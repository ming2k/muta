//! Provider-declared prompt-cache capabilities and request resolution.
//!
//! Prompt caching is a wire-route capability, not a model-family property.
//! The same model name can be served by the vendor API, a compatibility relay,
//! or a subscription endpoint with different controls. Callers therefore
//! resolve a user preference against capabilities reported by the concrete
//! route. Unsupported controls are errors; they are never silently ignored.

use serde::{Deserialize, Serialize};

/// User intent for one model request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CachePreference {
    /// Let the concrete route choose its documented default.
    #[default]
    ProviderDefault,
    /// Prefer the shortest explicitly supported lifetime.
    Short,
    /// Prefer the longest explicitly supported lifetime.
    Long,
    /// Write only at explicit breakpoints supplied by the request projection.
    ExplicitOnly,
    /// Do not write prompt-cache state. Valid only when the route declares an
    /// actual disable mechanism.
    Disabled,
}

/// TTL values that have stable, cross-request semantics in supported routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheTtl {
    FiveMinutes,
    ThirtyMinutes,
    OneHour,
    TwentyFourHours,
}

/// How a route can activate prompt caching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheActivation {
    /// The upstream caches eligible prefixes without a request field.
    Implicit,
    /// A top-level request control follows the growing conversation.
    Automatic,
    /// Content blocks may mark cumulative prefix boundaries.
    ExplicitBreakpoints,
    /// A stable routing/namespace key improves prefix affinity.
    RoutingKey,
    /// Requests refer to a separately managed cached-content resource.
    Resource,
}

/// Prompt-cache support of one concrete provider route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptCacheCapabilities {
    pub activations: Vec<CacheActivation>,
    pub supported_ttls: Vec<CacheTtl>,
    pub default_ttl: Option<CacheTtl>,
    pub disable_supported: bool,
    pub max_breakpoints: Option<u8>,
    pub min_cacheable_tokens: Option<u32>,
    /// Whether the provider reports cache reads separately from ordinary input.
    pub reports_reads: bool,
    /// Whether the provider reports newly written cache tokens.
    pub reports_writes: bool,
    /// Whether the provider reports a distinct uncached/miss count.
    pub reports_misses: bool,
}

/// Const-friendly declaration embedded in provider route registries. Runtime
/// channels own the materialized [`PromptCacheCapabilities`] snapshot so a
/// hot-reloaded catalog never borrows registry storage.
#[derive(Debug, Clone, Copy)]
pub struct PromptCacheSpec {
    pub activations: &'static [CacheActivation],
    pub supported_ttls: &'static [CacheTtl],
    pub default_ttl: Option<CacheTtl>,
    pub disable_supported: bool,
    pub max_breakpoints: Option<u8>,
    pub min_cacheable_tokens: Option<u32>,
    pub reports_reads: bool,
    pub reports_writes: bool,
    pub reports_misses: bool,
}

impl PromptCacheSpec {
    pub const UNSUPPORTED: Self = Self {
        activations: &[],
        supported_ttls: &[],
        default_ttl: None,
        disable_supported: false,
        max_breakpoints: None,
        min_cacheable_tokens: None,
        reports_reads: false,
        reports_writes: false,
        reports_misses: false,
    };

    pub fn materialize(self) -> PromptCacheCapabilities {
        PromptCacheCapabilities {
            activations: self.activations.to_vec(),
            supported_ttls: self.supported_ttls.to_vec(),
            default_ttl: self.default_ttl,
            disable_supported: self.disable_supported,
            max_breakpoints: self.max_breakpoints,
            min_cacheable_tokens: self.min_cacheable_tokens,
            reports_reads: self.reports_reads,
            reports_writes: self.reports_writes,
            reports_misses: self.reports_misses,
        }
    }
}

impl PromptCacheCapabilities {
    /// A conservative route that accepts no cache-control claims.
    pub const fn unsupported() -> Self {
        Self {
            activations: Vec::new(),
            supported_ttls: Vec::new(),
            default_ttl: None,
            disable_supported: false,
            max_breakpoints: None,
            min_cacheable_tokens: None,
            reports_reads: false,
            reports_writes: false,
            reports_misses: false,
        }
    }

    pub fn has(&self, activation: CacheActivation) -> bool {
        self.activations.contains(&activation)
    }

    /// Resolve request intent without inventing unsupported behavior.
    pub fn resolve(
        &self,
        preference: CachePreference,
        routing_key: Option<String>,
    ) -> Result<ResolvedCachePlan, CacheResolutionError> {
        if self.activations.is_empty() {
            return match preference {
                CachePreference::ProviderDefault => Ok(ResolvedCachePlan::Unsupported),
                _ => Err(CacheResolutionError::CachingUnsupported),
            };
        }

        if preference == CachePreference::Disabled {
            return self
                .disable_supported
                .then_some(ResolvedCachePlan::Disabled)
                .ok_or(CacheResolutionError::DisableUnsupported);
        }

        if preference == CachePreference::ExplicitOnly
            && !self.has(CacheActivation::ExplicitBreakpoints)
        {
            return Err(CacheResolutionError::ExplicitBreakpointsUnsupported);
        }

        let ttl = match preference {
            CachePreference::ProviderDefault | CachePreference::ExplicitOnly => self.default_ttl,
            CachePreference::Short => Some(
                self.supported_ttls
                    .first()
                    .copied()
                    .ok_or(CacheResolutionError::TtlUnsupported { preference })?,
            ),
            CachePreference::Long => Some(
                self.supported_ttls
                    .last()
                    .copied()
                    .ok_or(CacheResolutionError::TtlUnsupported { preference })?,
            ),
            CachePreference::Disabled => unreachable!("handled above"),
        };

        let mode = if preference == CachePreference::ExplicitOnly {
            ResolvedCacheMode::ExplicitOnly
        } else if self.has(CacheActivation::Automatic) {
            ResolvedCacheMode::Automatic
        } else if self.has(CacheActivation::ExplicitBreakpoints) {
            ResolvedCacheMode::Explicit
        } else {
            ResolvedCacheMode::Implicit
        };

        Ok(ResolvedCachePlan::Enabled {
            mode,
            ttl,
            routing_key: self
                .has(CacheActivation::RoutingKey)
                .then_some(routing_key)
                .flatten(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolvedCacheMode {
    Implicit,
    Automatic,
    Explicit,
    ExplicitOnly,
}

/// Fully validated cache instructions consumed by a protocol encoder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ResolvedCachePlan {
    Unsupported,
    Disabled,
    Enabled {
        mode: ResolvedCacheMode,
        ttl: Option<CacheTtl>,
        routing_key: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheResolutionError {
    CachingUnsupported,
    DisableUnsupported,
    ExplicitBreakpointsUnsupported,
    TtlUnsupported { preference: CachePreference },
}

impl std::fmt::Display for CacheResolutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CachingUnsupported => {
                write!(f, "this provider route exposes no prompt-cache controls")
            }
            Self::DisableUnsupported => {
                write!(f, "this provider route cannot disable prompt caching")
            }
            Self::ExplicitBreakpointsUnsupported => write!(
                f,
                "this provider route does not support explicit cache breakpoints"
            ),
            Self::TtlUnsupported { preference } => write!(
                f,
                "this provider route does not support the requested {preference:?} cache lifetime"
            ),
        }
    }
}

impl std::error::Error for CacheResolutionError {}

/// Provider-neutral cache telemetry parsed from an upstream usage object.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptCacheUsage {
    pub read_tokens: i64,
    pub write_tokens: i64,
    pub miss_tokens: Option<i64>,
}

/// Parse all supported upstream cache counters without conflating absent,
/// zero, and miss telemetry.
pub fn read_prompt_cache_usage(usage: &serde_json::Value) -> PromptCacheUsage {
    let read_tokens = first_non_negative(&[
        usage["cached_tokens"].as_i64(),
        usage["prompt_tokens_details"]["cached_tokens"].as_i64(),
        usage["input_tokens_details"]["cached_tokens"].as_i64(),
        usage["cachedContentTokenCount"].as_i64(),
        usage["prompt_cache_hit_tokens"].as_i64(),
        usage["cache_read_input_tokens"].as_i64(),
    ])
    .unwrap_or(0);
    let write_tokens = first_non_negative(&[
        usage["input_tokens_details"]["cache_write_tokens"].as_i64(),
        usage["cache_creation_input_tokens"].as_i64(),
    ])
    .unwrap_or(0);
    let miss_tokens = first_non_negative(&[usage["prompt_cache_miss_tokens"].as_i64()]);

    PromptCacheUsage {
        read_tokens,
        write_tokens,
        miss_tokens,
    }
}

fn first_non_negative(values: &[Option<i64>]) -> Option<i64> {
    values.iter().flatten().copied().find(|value| *value >= 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unknown_route_rejects_claimed_controls() {
        let caps = PromptCacheCapabilities::unsupported();
        assert_eq!(
            caps.resolve(CachePreference::ProviderDefault, None),
            Ok(ResolvedCachePlan::Unsupported)
        );
        assert_eq!(
            caps.resolve(CachePreference::Long, None),
            Err(CacheResolutionError::CachingUnsupported)
        );
    }

    #[test]
    fn resolves_longest_declared_ttl_and_routing_key() {
        let caps = PromptCacheCapabilities {
            activations: vec![CacheActivation::Automatic, CacheActivation::RoutingKey],
            supported_ttls: vec![CacheTtl::FiveMinutes, CacheTtl::OneHour],
            default_ttl: Some(CacheTtl::FiveMinutes),
            disable_supported: false,
            max_breakpoints: Some(4),
            min_cacheable_tokens: Some(1024),
            reports_reads: true,
            reports_writes: true,
            reports_misses: false,
        };
        assert_eq!(
            caps.resolve(CachePreference::Long, Some("session-42".into())),
            Ok(ResolvedCachePlan::Enabled {
                mode: ResolvedCacheMode::Automatic,
                ttl: Some(CacheTtl::OneHour),
                routing_key: Some("session-42".into()),
            })
        );
    }

    #[test]
    fn parses_openai_read_and_write_counters() {
        assert_eq!(
            read_prompt_cache_usage(&json!({
                "input_tokens_details": {
                    "cached_tokens": 800,
                    "cache_write_tokens": 200,
                }
            })),
            PromptCacheUsage {
                read_tokens: 800,
                write_tokens: 200,
                miss_tokens: None,
            }
        );
    }

    #[test]
    fn parses_deepseek_hit_and_miss_counters() {
        assert_eq!(
            read_prompt_cache_usage(&json!({
                "prompt_cache_hit_tokens": 600,
                "prompt_cache_miss_tokens": 400,
            })),
            PromptCacheUsage {
                read_tokens: 600,
                write_tokens: 0,
                miss_tokens: Some(400),
            }
        );
    }
}
