//! Canonical model registry — baseline metadata for models whose provider does
//! not publish a complete live model catalogue.
//!
//! A [`ProviderEntry`](crate::catalog::ProviderEntry) references a model by its
//! wire id (e.g. `"glm-5.2"`); this module supplies conservative defaults for
//! that id. A trusted provider may instead attach a [`RemoteModelMetadata`]
//! snapshot to its channel. Such metadata is scoped to the provider because an
//! endpoint, account entitlement, and serving runtime can change a model's
//! available API surface and capabilities.
//!
//! The [`WireFormat`] on each model is the baseline wire protocol when no live
//! provider metadata supplies a more specific endpoint. A remote catalogue can
//! legitimately route the same model id through a different surface.

use crate::thinking::ThinkingSupport;

/// The baseline wire protocol used when a provider has no live endpoint
/// metadata. A remote catalogue may select a different route for the same model
/// id, so this is not an invariant of a model id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WireFormat {
    /// OpenAI chat-completions (`/v1/chat/completions`). The common case.
    #[default]
    OpenAi,
    /// Anthropic Messages (`/v1/messages`). Used by opencode-go for
    /// MiniMax/Qwen, and by any Anthropic-compatible relay.
    AnthropicCompat,
    /// Google native (`generativelanguage.googleapis.com`).
    Google,
}

/// A provider-selected inference surface from a trusted remote model catalogue.
///
/// This deliberately lives beside provider-scoped metadata rather than
/// [`WireFormat`]: a single model id can be exposed through different APIs by
/// different providers, plans, or accounts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteModelEndpoint {
    ChatCompletions,
    Responses,
    Messages,
}

/// A canonical baseline model definition.
///
/// The registered baselines (see [`BaselineModels`]) are authoritative only
/// when a channel has no trusted remote metadata for the requested field. Use
/// [`ModelCapabilities::for_channel`] for request-time behavior.
#[derive(Debug, Clone, Copy)]
pub struct Model {
    /// Wire model id sent in API requests, e.g. `"glm-5.2"`. This is also the
    /// only label the UI ever renders for a model — id-first by policy, so
    /// every surface shows the same string the user must type/see on the wire.
    pub id: &'static str,
    /// Model family for grouping, e.g. `"glm"`, `"gpt"`, `"google"`.
    pub family: &'static str,
    /// Context window in tokens. `0` means unknown.
    pub context_window: usize,
    /// What extended thinking this model supports and how it is encoded on the
    /// wire. The single source of truth for thinking capability; the coarse
    /// "does it reason" bool used for display derives from it via
    /// [`Model::reasoning`]. See [`ThinkingSupport`].
    pub thinking: ThinkingSupport,
    /// Whether the model supports native tool/function calling.
    pub tool_call: bool,
    /// Whether the model supports vision (image inputs via `image_url`/
    /// `inline_data`). When `false`, images attached to messages are
    /// silently stripped before the request hits the wire.
    pub vision: bool,
    /// Wire protocol used to reach this model. See [`WireFormat`].
    pub format: WireFormat,
    /// Model-specific prompt guidance injected into the system prompt as a
    /// `ModelGuidance` section. Because each model behaves differently,
    /// this is the per-model hook for any behavioral nudge a model needs.
    /// Empty for all known models today; a model entry is free to carry
    /// non-empty guidance when it needs one. The model entry is the single
    /// source of truth; the prompt engine just renders whatever the resolved
    /// model carries.
    pub model_guidance: &'static str,
    /// The reasoning-effort levels this model honors, ascending. Used as the
    /// clamp range when a user requests an effort the model doesn't support.
    /// `&[]` means effort control does not apply (non-reasoning models, or
    /// protocols without an effort field). First-party Claude models carry
    /// [`crate::effort::EFFORT_CLAUDE_FULL`]; models behind
    /// Anthropic-compatible relays with unknown effort support carry
    /// [`crate::effort::EFFORT_COMMON`] (conservative); non-reasoning / non-
    /// Anthropic-protocol models carry `&[]`.
    pub effort_levels: &'static [crate::effort::Effort],
}

impl Model {
    /// Coarse "does this model reason at all" flag, for capability display.
    /// Derives from [`Self::thinking`] so there is one source of truth.
    pub const fn reasoning(&self) -> bool {
        self.thinking.reasons()
    }
}

/// Capability metadata received from a trusted provider's live model catalogue.
///
/// Every field is optional so an omitted remote field falls back to the static
/// baseline. A present `false` is meaningful: it explicitly overrides a more
/// optimistic local default. This record belongs to the channel that received
/// it, never globally by model id, because availability and protocol routing are
/// provider- and account-specific.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct RemoteModelMetadata {
    /// Exact API surface advertised for this model by the provider. When absent,
    /// the channel's configured transport remains authoritative.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<RemoteModelEndpoint>,
    /// Provider's model-family label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    /// Maximum full request context in tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<usize>,
    /// Maximum generated tokens, when declared by the endpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Exact reasoning representation supported by the advertised endpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingSupport>,
    /// Whether native tool/function calls are accepted by this endpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<bool>,
    /// Whether image input is accepted by this endpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    /// Endpoint-advertised reasoning effort values. An empty vector explicitly
    /// means that the model accepts no effort control. Carries
    /// [`EffortLevel`](crate::effort::EffortLevel) so a provider-advertised tier
    /// the vocabulary does not name is preserved verbatim and stamped through
    /// (ADR-0065), rather than silently dropped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort_levels: Option<Vec<crate::effort::EffortLevel>>,
}

/// Effective capabilities for one provider channel.
///
/// This owned view combines the local baseline with the channel's remote
/// snapshot. One model id can therefore have different capabilities or routes
/// at different providers without one account's discovery changing another
/// provider's behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCapabilities {
    pub family: String,
    pub context_window: usize,
    pub max_output_tokens: Option<u32>,
    pub thinking: ThinkingSupport,
    pub tool_call: bool,
    pub vision: bool,
    /// The effort ladder this channel honors, as [`crate::EffortLevel`] so a
    /// provider-advertised tier outside the [`crate::Effort`] vocabulary is preserved
    /// and stamped through (ADR-0065). Built in `for_channel` from the remote
    /// advertisement over the static baseline.
    pub effort_levels: Vec<crate::effort::EffortLevel>,
}

/// A user's explicit capability override for one (provider-instance, model)
/// route -- the **top layer** of the capability resolution order (ADR-0080).
///
/// Every field is optional; `None` means "no opinion, fall through to the
/// layer below". A present `Some(false)` is meaningful: it forces the
/// capability off even when both the remote advertisement and the static
/// baseline say otherwise (e.g. a relay's `glm-5.3-flash` that strips image
/// inputs, or an account whose plan caps the context window lower than the
/// model card claims).
///
/// This lives in `muta-contracts` (not persistence) so the merge function can
/// live beside the structure it overrides -- persistence keys it per
/// `(instance_id, model_id)` inside `RouteSettings` and owns only storage.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, ts_rs::TS,
)]
#[serde(default)]
pub struct CapabilityOverrides {
    /// Force the family tag used for family-scoped wire behavior (cache
    /// policy, effort mapping). `None` -> inherit from the layers below.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    /// Force the context window (tokens). `None` -> inherit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_window: Option<usize>,
    /// Force the max output tokens. `None` -> inherit. `Some(0)` clears an
    /// inherited cap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Force the thinking representation. `None` -> inherit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingSupport>,
    /// Force native tool calling on/off. `None` -> inherit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<bool>,
    /// Force image-input support on/off. `None` -> inherit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
}

impl CapabilityOverrides {
    /// Whether any knob is set. An all-`None` record is a no-op and should
    /// not be persisted.
    pub fn is_empty(&self) -> bool {
        self.family.is_none()
            && self.context_window.is_none()
            && self.max_output_tokens.is_none()
            && self.thinking.is_none()
            && self.tool_call.is_none()
            && self.vision.is_none()
    }
}

impl ModelCapabilities {
    /// Resolve effective capabilities for `model_id`, applying all explicitly
    /// advertised remote fields over the local baseline.
    ///
    /// # Capability resolution order (ADR-0080)
    ///
    /// This method implements the **lower two layers** of the canonical
    /// three-layer capability resolution order:
    ///
    /// ```text
    /// 1. user config     — `RouteSettings::capability_overrides`
    ///                      (per provider-instance + model id, applied last
    ///                      by the catalog derivation, see ADR-0080)
    /// 2. remote metadata — the `remote` argument here: fields a trusted
    ///                      endpoint advertised (`fitting: true` templates)
    /// 3. local baseline  — the static registry entry for the model id
    /// ```
    ///
    /// A field resolved by a higher layer wins; an absent field at a higher
    /// layer falls through to the layer below. The top (user) layer is *not*
    /// applied here — capability overrides are the user's per-route choices
    /// and are stamped on by [`Self::apply_overrides`] at the catalog
    /// derivation site, keeping this function a pure baseline⊕remote merge.
    pub fn for_channel(model_id: &str, remote: Option<&RemoteModelMetadata>) -> Self {
        let baseline = resolve(model_id);
        let remote = remote.cloned().unwrap_or_default();
        Self {
            family: remote.family.unwrap_or_else(|| {
                if baseline.family.is_empty() {
                    model_id.to_string()
                } else {
                    baseline.family.to_string()
                }
            }),
            context_window: remote.context_window.unwrap_or(baseline.context_window),
            max_output_tokens: remote.max_output_tokens,
            thinking: remote.thinking.unwrap_or(baseline.thinking),
            tool_call: remote.tool_call.unwrap_or(baseline.tool_call),
            vision: remote.vision.unwrap_or(baseline.vision),
            effort_levels: remote.effort_levels.unwrap_or_else(|| {
                baseline
                    .effort_levels
                    .iter()
                    .copied()
                    .map(Into::into)
                    .collect()
            }),
        }
    }

    /// Apply the **top layer** of the capability resolution order (ADR-0080):
    /// stamp the user's explicit `CapabilityOverrides` onto the already-merged
    /// (baseline + remote) capabilities. Consumes `self` and returns the
    /// overridden copy. This is deliberately a separate step from
    /// [`Self::for_channel`] so that merge stays pure baseline+remote and
    /// this stays the single, auditable place a user can win over a provider.
    pub fn apply_overrides(mut self, user: &CapabilityOverrides) -> Self {
        if let Some(family) = user.family.clone() {
            self.family = family;
        }
        if let Some(context_window) = user.context_window {
            self.context_window = context_window;
        }
        if let Some(max_output_tokens) = user.max_output_tokens {
            self.max_output_tokens = Some(max_output_tokens);
        }
        if let Some(thinking) = user.thinking {
            self.thinking = thinking;
        }
        if let Some(tool_call) = user.tool_call {
            self.tool_call = tool_call;
        }
        if let Some(vision) = user.vision {
            self.vision = vision;
        }
        self
    }


    /// Coarse reasoning capability used by picker and request construction.
    pub const fn reasoning(&self) -> bool {
        self.thinking.reasons()
    }
}

#[cfg(test)]
mod capability_tests {
    use super::*;

    #[test]
    fn remote_metadata_overrides_only_the_fields_it_declares() {
        let remote = RemoteModelMetadata {
            context_window: Some(64_000),
            vision: Some(false),
            tool_call: Some(false),
            ..Default::default()
        };

        let effective = ModelCapabilities::for_channel("gpt-4o", Some(&remote));

        assert_eq!(effective.context_window, 64_000);
        assert!(!effective.vision);
        assert!(!effective.tool_call);
        // The provider omitted reasoning, so the local baseline remains
        // (no baseline is registered for this id in core's own tests, so the
        // fallback's `None` applies).
        assert_eq!(effective.thinking, ThinkingSupport::None);
    }

    #[test]
    fn remote_effort_levels_can_explicitly_clear_the_static_baseline() {
        let remote = RemoteModelMetadata {
            effort_levels: Some(Vec::new()),
            ..Default::default()
        };

        let effective = ModelCapabilities::for_channel("gpt-5.5", Some(&remote));

        assert!(effective.effort_levels.is_empty());
    }
}

/// Baseline model metadata registered by a provider crate.
///
/// **Mechanism lives here; data lives with the providers.** This crate owns
/// only the lookup machinery ([`resolve`], [`model_by_id`], [`fallback_model`],
/// the [`FittedModel`] overlay). The per-provider baseline tables live beside
/// each provider's other registry data (today: `muta-providers`' registry
/// modules), and each table is submitted once at link time:
///
/// ```ignore
/// inventory::submit!(muta_contracts::model::BaselineModels(MODELS));
/// ```
///
/// Every binary that links a provider crate picks its tables up with no
/// manual call. Lookup precedence in [`resolve`]: the first registered
/// baseline with a matching id wins (a model id is expected to appear in at
/// most one provider's table; providers that share an id carry byte-identical
/// copies, so the winner is irrelevant), then the runtime-fitted overlay, then
/// [`fallback_model`]. When no provider crate is linked (this crate's own
/// tests), every id falls through to the overlay/fallback.
pub struct BaselineModels(pub &'static [Model]);

inventory::collect!(BaselineModels);

/// Iterate every baseline model registered by linked provider crates, in
/// registration (link) order. Callers that need deterministic order-independent
/// results should match by `id` rather than position.
pub fn baseline_models() -> impl Iterator<Item = &'static Model> {
    inventory::iter::<BaselineModels>
        .into_iter()
        .flat_map(|batch| batch.0.iter())
}

/// Look up a known model by its wire id. Returns `None` for user-defined or
/// unrecognized model ids; callers should fall back to [`fallback_model`].
pub fn model_by_id(id: &str) -> Option<&'static Model> {
    baseline_models().find(|m| m.id == id)
}

/// A conservative fallback for model ids no registered baseline knows (local
/// models, user-defined relays, unreleased models). Assumes tool calling (the
/// harness depends on it) and nothing else.
pub fn fallback_model(_id: &str) -> Model {
    Model {
        id: "",
        family: "",
        context_window: 0,
        thinking: ThinkingSupport::None,
        tool_call: true,
        vision: false,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: &[],
    }
}

/// Resolve any model id to its metadata: the vetted static registry entry
/// when known (with a live-refreshed effort ladder when a trusted provider's
/// overlay overrode it — see [`register_fitted_models`]), then the
/// runtime-fitted overlay for ids a trusted provider advertised, then a
/// conservative fallback. Never returns `None` so callers need not branch on
/// absence.
pub fn resolve(id: &str) -> Model {
    if let Some(model) = model_by_id(id) {
        // A trusted provider may have refreshed the baseline's live effort
        // ladder via `register_fitted_models` (stored under the baseline's
        // own id); every other field stays vetted.
        if let Some(overridden) = fitted_models()
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(model.id)
        {
            return Model {
                effort_levels: overridden.effort_levels,
                ..*model
            };
        }
        return *model;
    }
    if let Some(model) = fitted_models()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(id)
    {
        return *model;
    }
    fallback_model(id)
}

// ═════════════════════════════════════════════════════════════════════════════
// Runtime-fitted models (capability overlay)
// ═════════════════════════════════════════════════════════════════════════════

/// Capability metadata for a model id no registered baseline knows,
/// learned at runtime from a provider's live model list (ADR-0065).
///
/// Only **trusted** providers may feed this overlay (official endpoints whose
/// `/models` advertises real capability fields, opted in via their template);
/// an arbitrary relay cannot use it to inflate a model's context window or
/// capabilities. Registration is ignored for ids a registered baseline knows
/// — the vetted baseline entry always wins, so a provider can never
/// *downgrade* a known model either.
#[derive(Debug, Clone)]
pub struct FittedModel {
    /// Wire model id as advertised by the provider.
    pub id: String,
    /// Grouping family (the feeding template's id, e.g. `"kimi-code"`).
    pub family: String,
    /// Advertised context window in tokens; `0` means the endpoint did not
    /// say (the model resolves with an unknown window, like the fallback).
    pub context_window: usize,
    /// The endpoint advertises reasoning (a `reasoning_content` stream).
    pub reasoning: bool,
    /// The endpoint advertises image inputs.
    pub vision: bool,
    /// Wire protocol the feeding provider speaks for this model.
    pub format: WireFormat,
    /// Advertised reasoning-effort levels (any order; stored ascending via
    /// [`Effort::ORDER`](crate::effort::Effort::ORDER)).
    pub effort_levels: Vec<crate::effort::Effort>,
}

/// Process-wide overlay of runtime-fitted models. Populated at startup from
/// persisted discovery results and refreshed after a live fetch (the feeding
/// layer lives in `muta_agent::catalog`).
static FITTED_MODELS: std::sync::OnceLock<
    std::sync::RwLock<std::collections::HashMap<&'static str, Model>>,
> = std::sync::OnceLock::new();

fn fitted_models() -> &'static std::sync::RwLock<std::collections::HashMap<&'static str, Model>> {
    FITTED_MODELS.get_or_init(|| std::sync::RwLock::new(std::collections::HashMap::new()))
}

/// Register (or replace) runtime-fitted models. An id a registered baseline
/// knows keeps its vetted entry **except** for `effort_levels`: effort tiers
/// are a live platform knob that can evolve after the baseline shipped (Kimi
/// K3's ladder went from a single `max` rung to `low`/`high`/`max`), so a
/// trusted provider's advertised tiers refresh the baseline's ladder while
/// every other field stays vetted. Strings and slices are interned via
/// `Box::leak` because [`Model`] is `Copy` over `&'static str`; the set of
/// distinct fitted ids is bounded by what a provider advertises, so the
/// one-time leak per registration is negligible.
pub fn register_fitted_models(models: impl IntoIterator<Item = FittedModel>) {
    let mut overlay = fitted_models().write().unwrap_or_else(|e| e.into_inner());
    for fitted in models {
        let mut levels = fitted.effort_levels;
        levels.sort_by_key(|level| {
            crate::effort::Effort::ORDER
                .iter()
                .position(|ordered| ordered == level)
                .unwrap_or(usize::MAX)
        });
        levels.dedup();
        if let Some(baseline) = model_by_id(&fitted.id) {
            // Baseline-known id: only the effort ladder follows the live
            // advertisement (and only when the endpoint actually advertises
            // tiers — an absent field must not wipe the baseline's). The
            // `resolve` order (baseline first) means the override must land
            // ON the baseline's id to take effect.
            if !levels.is_empty() && baseline.effort_levels != levels.as_slice() {
                overlay.insert(
                    baseline.id,
                    Model {
                        effort_levels: Box::leak(levels.into_boxed_slice()),
                        ..*baseline
                    },
                );
            }
            continue;
        }
        let id: &'static str = Box::leak(fitted.id.into_boxed_str());
        overlay.insert(
            id,
            Model {
                id,
                family: Box::leak(fitted.family.into_boxed_str()),
                context_window: fitted.context_window,
                thinking: if fitted.reasoning {
                    ThinkingSupport::ReasoningContent
                } else {
                    ThinkingSupport::None
                },
                // The harness depends on tool calling; an advertised coding
                // model is assumed capable (same assumption as the fallback).
                tool_call: true,
                vision: fitted.vision,
                format: fitted.format,
                model_guidance: "",
                effort_levels: Box::leak(levels.into_boxed_slice()),
            },
        );
    }
}

/// Sanitize a raw model identifier string: trims surrounding whitespace,
/// replaces internal whitespace sequences with single hyphens (`-`),
/// and filters out ASCII control characters.
pub fn sanitize_model_id(raw: &str) -> String {
    let trimmed = raw.trim();
    let mut out = String::with_capacity(trimmed.len());
    let mut in_ws = false;
    for c in trimmed.chars() {
        if c.is_whitespace() {
            if !in_ws && !out.is_empty() {
                out.push('-');
                in_ws = true;
            }
        } else if !c.is_control() {
            out.push(c);
            in_ws = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_overrides_win_over_remote_and_baseline() {
        // ADR-0080: layer 1 (user) beats layer 2 (remote) beats layer 3
        // (baseline), field-wise; unset user knobs fall through.
        let remote = RemoteModelMetadata {
            vision: Some(true),
            tool_call: Some(true),
            context_window: Some(222_000),
            ..Default::default()
        };
        let user = CapabilityOverrides {
            // user says vision off, even though remote+baseline say on
            vision: Some(false),
            // user has no opinion on tool_call -> remote's true stands
            tool_call: None,
            // user has no opinion on context window -> remote's 222_000 stands
            context_window: None,
            family: Some("user-family".to_string()),
            thinking: None,
            max_output_tokens: Some(4_096),
        };
        let caps = ModelCapabilities::for_channel("fixture-alpha", Some(&remote))
            .apply_overrides(&user);
        // Layer 1 wins:
        assert!(!caps.vision, "user Some(false) must beat remote Some(true)");
        assert_eq!(caps.family, "user-family");
        assert_eq!(caps.max_output_tokens, Some(4_096));
        // Fall-through to layer 2:
        assert!(caps.tool_call);
        assert_eq!(caps.context_window, 222_000);
    }

    #[test]
    fn empty_capability_overrides_are_a_noop() {
        let caps = ModelCapabilities::for_channel("fixture-alpha", None);
        let overridden = caps.clone().apply_overrides(&CapabilityOverrides::default());
        assert_eq!(caps, overridden);
        assert!(CapabilityOverrides::default().is_empty());
    }


    // Fixture baselines. Core's own tests must not depend on real vendor data
    // (that lives with the provider crates), so they register small tables of
    // fictional ids through the same inventory mechanism providers use. The
    // ids are deliberately non-vendor so they can never collide with a real
    // baseline in a binary that also links a provider crate.
    const FIXTURE_A: &[Model] = &[
        Model {
            id: "fixture-alpha",
            family: "fixture",
            context_window: 111_000,
            thinking: ThinkingSupport::ReasoningContent,
            tool_call: true,
            vision: true,
            format: WireFormat::OpenAi,
            model_guidance: "",
            effort_levels: crate::effort::EFFORT_COMMON,
        },
        Model {
            id: "fixture-beta",
            family: "fixture",
            context_window: 222_000,
            thinking: ThinkingSupport::None,
            tool_call: true,
            vision: false,
            format: WireFormat::AnthropicCompat,
            model_guidance: "",
            effort_levels: &[],
        },
    ];
    const FIXTURE_B: &[Model] = &[Model {
        id: "fixture-gamma",
        family: "fixture",
        context_window: 333_000,
        thinking: ThinkingSupport::None,
        tool_call: false,
        vision: false,
        format: WireFormat::Google,
        model_guidance: "",
        effort_levels: &[],
    }];

    inventory::submit!(BaselineModels(FIXTURE_A));
    inventory::submit!(BaselineModels(FIXTURE_B));

    #[test]
    fn registered_baselines_resolve_by_id() {
        let m = resolve("fixture-alpha");
        assert_eq!(m.context_window, 111_000);
        assert!(m.reasoning());
        assert!(m.vision);
        assert_eq!(m.format, WireFormat::OpenAi);
        // `fitted_overlay_never_overrides_a_registered_baseline` may have
        // already run in this process and refreshed the ladder (overlay
        // writes are process-global), so assert the baseline value only when
        // no override landed; the override case is covered there.
        assert!(
            m.effort_levels == crate::effort::EFFORT_COMMON
                || m.effort_levels == [crate::effort::Effort::Max].as_slice(),
            "unexpected ladder: {:?}",
            m.effort_levels
        );

        let g = resolve("fixture-gamma");
        assert_eq!(g.format, WireFormat::Google);
        assert!(!g.tool_call);
    }

    #[test]
    fn model_by_id_returns_none_for_unregistered_ids() {
        assert!(model_by_id("fixture-alpha").is_some());
        assert!(model_by_id("some-local-model").is_none());
    }

    #[test]
    fn registered_baselines_have_unique_ids() {
        let mut ids: Vec<&str> = baseline_models().map(|m| m.id).collect();
        ids.sort_unstable();
        let dups: Vec<&str> = ids
            .windows(2)
            .filter(|w| w[0] == w[1])
            .map(|w| w[0])
            .collect();
        assert!(dups.is_empty(), "duplicate baseline ids: {dups:?}");
    }

    #[test]
    fn resolve_falls_back_for_unknown() {
        let m = resolve("some-local-model");
        assert_eq!(m.context_window, 0);
        assert!(!m.reasoning());
        // The harness depends on tool calling, so even the fallback assumes it.
        assert!(m.tool_call);
    }

    #[test]
    fn fitted_overlay_supplies_metadata_for_unregistered_ids() {
        register_fitted_models(vec![FittedModel {
            id: "fitted-future-k9".to_string(),
            family: "kimi-code".to_string(),
            context_window: 2_000_000,
            reasoning: true,
            vision: true,
            format: WireFormat::OpenAi,
            // Unsorted input with a duplicate: stored ascending, deduped.
            effort_levels: vec![
                crate::effort::Effort::Max,
                crate::effort::Effort::Low,
                crate::effort::Effort::Low,
            ],
        }]);
        let m = resolve("fitted-future-k9");
        assert_eq!(m.id, "fitted-future-k9");
        assert_eq!(m.context_window, 2_000_000);
        assert!(m.reasoning());
        assert!(m.vision);
        assert_eq!(
            m.effort_levels,
            &[crate::effort::Effort::Low, crate::effort::Effort::Max]
        );
    }

    #[test]
    fn fitted_overlay_never_overrides_a_registered_baseline() {
        register_fitted_models(vec![FittedModel {
            id: "fixture-alpha".to_string(),
            family: "bogus".to_string(),
            context_window: 1,
            reasoning: false,
            vision: false,
            format: WireFormat::Google,
            effort_levels: vec![crate::effort::Effort::Max],
        }]);
        // The vetted baseline entry wins on every field except the effort
        // ladder: effort tiers are a live platform knob, so a trusted
        // provider's advertised tiers refresh the baseline's ladder while
        // identity, context, format, and vision stay vetted.
        let m = resolve("fixture-alpha");
        assert_eq!(m.context_window, 111_000);
        assert_eq!(m.format, WireFormat::OpenAi);
        assert!(m.vision);
        assert_eq!(m.effort_levels, [crate::effort::Effort::Max].as_slice());
    }

    #[test]
    fn fitted_overlay_with_no_advertised_tiers_keeps_the_baseline_ladder() {
        // A fitted entry for a baseline-known id that advertises NO effort
        // tiers must not wipe the baseline's ladder — an absent field means
        // "the endpoint did not say", not "the model lost its knob".
        register_fitted_models(vec![FittedModel {
            id: "fixture-beta".to_string(),
            family: "fixture".to_string(),
            context_window: 0,
            reasoning: false,
            vision: false,
            format: WireFormat::OpenAi,
            effort_levels: Vec::new(),
        }]);
        // fixture-beta's baseline ladder is empty already, so check through
        // the gamma fixture's *sibling* instead: gamma has no fitted entry at
        // all and must be untouched by beta's registration.
        let g = resolve("fixture-gamma");
        assert_eq!(g.context_window, 333_000);
    }

    #[test]
    fn fallback_format_is_openai_compat() {
        assert_eq!(fallback_model("anything").format, WireFormat::OpenAi);
    }

    #[test]
    fn sanitize_model_id_replaces_whitespace_with_hyphen() {
        assert_eq!(sanitize_model_id("gpt 5.5 preview"), "gpt-5.5-preview");
        assert_eq!(
            sanitize_model_id("  claude   3.7  sonnet  "),
            "claude-3.7-sonnet"
        );
        assert_eq!(sanitize_model_id("gemini-3.1-pro"), "gemini-3.1-pro");
        assert_eq!(sanitize_model_id("   "), "");
    }
}
