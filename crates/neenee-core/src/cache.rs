//! Prompt-cache control policy (ADR-0067).
//!
//! Prompt caching is the dominant cost lever for multi-turn agents: a cached
//! prefix is billed at ~0.1× input (Anthropic) or folded into a discount
//! (OpenAI / Google / Moonshot). Different providers expose wildly different
//! surfaces, so this module is the **single classifier** that the provider
//! adapters and the token ledger consult to know how a model family caches and
//! how its discount surfaces.
//!
//! Three strategies cover every provider we ship:
//!
//! - [`CachePolicy::Breakpoints`] — the client *stamps* explicit
//!   `cache_control: {"type":"ephemeral"}` breakpoints (Anthropic). The request
//!   builder owns stamping; the response parser splits write vs read counters.
//! - [`CachePolicy::SessionKey`] — the client supplies a session-scoped
//!   `prompt_cache_key` so the server cache namespaces per session (Moonshot /
//!   Kimi). The request builder injects the key; the response parser reports
//!   `cached_tokens` as a read.
//! - [`CachePolicy::Automatic`] — the server auto-caches with no client control
//!   (OpenAI, Google). Nothing is stamped; the response parser surfaces the
//!   discount as a read counter only.
//!
//! This module is **pure domain**: it knows no `reqwest`, no `serde_json` beyond
//! a single read helper. The per-provider request/response adapters call into
//! it; the agent never does. Keep it free of I/O so it unit-tests without a
//! provider.

use serde::{Deserialize, Serialize};

/// How a model family interacts with prompt caching.
///
/// Resolved from the model's `family` by [`CachePolicy::for_family`]. Provider
/// adapters branch on this to decide what (if anything) to stamp on the request
/// and how to parse the response's discount.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CachePolicy {
    /// Client stamps explicit `cache_control: {"type":"ephemeral"}` breakpoints
    /// (Anthropic Messages API). The request builder stamps up to 4 breakpoints
    /// across tools → system → messages; the response parser carries separate
    /// `cache_creation_input_tokens` (write, billed at a premium) and
    /// `cache_read_input_tokens` (read, billed at ~0.1×).
    Breakpoints,
    /// Client supplies a session-scoped `prompt_cache_key` (Moonshot / Kimi).
    /// The key namespaces the server-side cache per session so repeated prefixes
    /// hit. The response surfaces the discount as `cached_tokens` (read only).
    SessionKey,
    /// The server auto-caches with no client control (OpenAI, Google). Nothing
    /// is stamped. The discount surfaces as a read counter when the provider
    /// reports it.
    Automatic,
}

impl CachePolicy {
    /// Resolve the cache policy for a model family.
    ///
    /// Families are matched case-insensitively against the known set. Unknown
    /// families default to [`CachePolicy::Automatic`] — the safest choice, since
    /// it stamps nothing on the request and merely surfaces any discount the
    /// server happens to report.
    pub fn for_family(family: &str) -> Self {
        match family.to_ascii_lowercase().as_str() {
            // Anthropic: explicit breakpoint stamping.
            "claude" | "anthropic" => CachePolicy::Breakpoints,
            // Moonshot / Kimi: session-scoped prompt_cache_key.
            "kimi" | "moonshot" | "kimi-code" => CachePolicy::SessionKey,
            // Everything else (openai, google, qwen, deepseek, …): auto-cache.
            _ => CachePolicy::Automatic,
        }
    }

    /// Whether this policy asks the request builder to stamp breakpoints.
    ///
    /// Only [`Breakpoints`](CachePolicy::Breakpoints) does; the other two add
    /// either a key or nothing.
    pub const fn stamps_breakpoints(self) -> bool {
        matches!(self, CachePolicy::Breakpoints)
    }

    /// Whether this policy asks the request builder to inject a session key.
    pub const fn injects_session_key(self) -> bool {
        matches!(self, CachePolicy::SessionKey)
    }
}

impl Default for CachePolicy {
    /// The safe default: stamp nothing, surface any server-reported discount.
    fn default() -> Self {
        CachePolicy::Automatic
    }
}

/// Extract the cache-read discount from a provider `usage` object, regardless of
/// where the provider hides it.
///
/// Each OpenAI-compatible relay / provider puts the cached count in a slightly
/// different place. This checks them in order of specificity and returns the
/// first positive hit:
///
/// - `cached_tokens` (top-level) — Moonshot proprietary.
/// - `prompt_tokens_details.cached_tokens` — OpenAI chat-completions.
/// - `input_tokens_details.cached_tokens` — OpenAI Responses API.
/// - `cachedContentTokenCount` — Google (`usageMetadata`).
///
/// **Every** per-protocol `usage()` parser in the SDK layer MUST route through
/// this helper rather than reading its field inline. Routing here is the single
/// safety lever against billing drift: if the cache-accounting policy ever
/// changes (coefficient folding, zero-count-as-miss auditing, a new relay's
/// field), it changes in one place and cannot be silently missed by a protocol
/// that forked its own read. A missed cache field is a missed discount, which
/// is a direct cost/billing error (ADR-0067).
///
/// Returns `None` when no cache field is present or it is non-positive. The
/// caller folds the result into [`crate::TokenUsage::cache_read_input_tokens`].
/// OpenAI-style auto-caching has no separate write counter, so
/// [`crate::TokenUsage::cache_creation_input_tokens`] stays zero for those
/// providers.
pub fn read_cached_tokens(usage: &serde_json::Value) -> Option<i64> {
    let top = usage["cached_tokens"].as_i64();
    let prompt_detail = usage["prompt_tokens_details"]["cached_tokens"].as_i64();
    let input_detail = usage["input_tokens_details"]["cached_tokens"].as_i64();
    let google = usage["cachedContentTokenCount"].as_i64();
    top.or(prompt_detail)
        .or(input_detail)
        .or(google)
        .filter(|n| *n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn family_resolves_known_strategies() {
        assert_eq!(CachePolicy::for_family("claude"), CachePolicy::Breakpoints);
        assert_eq!(CachePolicy::for_family("Claude"), CachePolicy::Breakpoints);
        assert_eq!(CachePolicy::for_family("kimi"), CachePolicy::SessionKey);
        assert_eq!(
            CachePolicy::for_family("Kimi-Code"),
            CachePolicy::SessionKey
        );
        assert_eq!(CachePolicy::for_family("moonshot"), CachePolicy::SessionKey);
        assert_eq!(CachePolicy::for_family("openai"), CachePolicy::Automatic);
        assert_eq!(CachePolicy::for_family("google"), CachePolicy::Automatic);
        // Legacy family id still resolves (backward-compat for old configs).
        assert_eq!(CachePolicy::for_family("gemini"), CachePolicy::Automatic);
        assert_eq!(CachePolicy::for_family("qwen"), CachePolicy::Automatic);
        assert_eq!(CachePolicy::for_family(""), CachePolicy::Automatic);
    }

    #[test]
    fn strategy_predicates_partition() {
        assert!(CachePolicy::Breakpoints.stamps_breakpoints());
        assert!(!CachePolicy::Breakpoints.injects_session_key());
        assert!(!CachePolicy::SessionKey.stamps_breakpoints());
        assert!(CachePolicy::SessionKey.injects_session_key());
        assert!(!CachePolicy::Automatic.stamps_breakpoints());
        assert!(!CachePolicy::Automatic.injects_session_key());
    }

    #[test]
    fn reads_moonshot_top_level_cached_tokens() {
        let usage = json!({ "prompt_tokens": 1000, "cached_tokens": 600 });
        assert_eq!(read_cached_tokens(&usage), Some(600));
    }

    #[test]
    fn reads_openai_prompt_tokens_details() {
        let usage = json!({
            "prompt_tokens": 1000,
            "prompt_tokens_details": { "cached_tokens": 250 }
        });
        assert_eq!(read_cached_tokens(&usage), Some(250));
    }

    #[test]
    fn reads_openai_responses_input_tokens_details() {
        // The Responses API uses `input_tokens_details.cached_tokens`, distinct
        // from chat-completions' `prompt_tokens_details`. Regression guard: this
        // key was once read inline in the Responses parser and missed by the
        // helper — a billing-drift footgun (ADR-0067).
        let usage = json!({
            "input_tokens": 800,
            "input_tokens_details": { "cached_tokens": 500 }
        });
        assert_eq!(read_cached_tokens(&usage), Some(500));
    }

    #[test]
    fn reads_google_cached_content_token_count() {
        let usage = json!({ "cachedContentTokenCount": 42 });
        assert_eq!(read_cached_tokens(&usage), Some(42));
    }

    #[test]
    fn prefers_first_present_field() {
        // When both Moonshot top-level and OpenAI detail are present, the more
        // specific (top-level Moonshot) value wins.
        let usage = json!({
            "cached_tokens": 7,
            "prompt_tokens_details": { "cached_tokens": 99 }
        });
        assert_eq!(read_cached_tokens(&usage), Some(7));
    }

    #[test]
    fn returns_none_when_absent_or_non_positive() {
        assert_eq!(read_cached_tokens(&json!({ "prompt_tokens": 10 })), None);
        assert_eq!(read_cached_tokens(&json!({ "cached_tokens": 0 })), None);
        assert_eq!(read_cached_tokens(&json!({ "cached_tokens": -3 })), None);
        assert_eq!(read_cached_tokens(&json!({})), None);
    }

    #[test]
    fn default_is_automatic() {
        assert_eq!(CachePolicy::default(), CachePolicy::Automatic);
    }
}
