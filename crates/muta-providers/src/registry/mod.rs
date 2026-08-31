//! OpenAI-compatible provider registry and the `Channel` → concrete `Provider`
//! factory consumed by the orchestration layer.
//!
//! The registry is split one file per provider: each module holds the
//! provider's model constants, its [`ProviderPresetSpec`] entry, and (for
//! the two legacy OpenAI-compatible presets) its [`OpenAiProviderSpec`] entry.
//! This file keeps the shared types, the aggregate tables, and the factory.

use muta_contracts::Provider;
use muta_contracts::catalog::{Channel, Transport};
use std::sync::Arc;

use crate::{
    AnthropicMessagesProvider, GoogleProvider, MUTA_USER_AGENT, OpenAiChatCompletionsProvider,
    OpenAiResponsesProvider, ThinkingConfig,
};

mod anthropic;
mod antigravity_oauth;
mod chatgpt;
mod copilot;
mod custom_baselines;
mod deepseek;
mod google;
mod kimi;
mod openai;
mod opencode_go;
mod xai;
mod zai;

pub use anthropic::ANTHROPIC_BUILTIN_MODELS;
pub use antigravity_oauth::ANTIGRAVITY_OAUTH_MODELS;
pub use chatgpt::CHATGPT_BUILTIN_MODELS;
pub use copilot::COPILOT_SEED_MODELS;
pub use deepseek::DEEPSEEK_BUILTIN_MODELS;
pub use google::GOOGLE_BUILTIN_MODELS;
pub use kimi::KIMI_CODE_MODELS;
pub use openai::OPENAI_BUILTIN_MODELS;
pub use opencode_go::{OPENCODE_GO_MODELS, OPENCODE_GO_SERVED_MODELS};
pub use xai::XAI_BUILTIN_MODELS;
pub use zai::ZAI_CODE_MODELS;

use anthropic::anthropic_model_max_tokens;

// ═════════════════════════════════════════════════════════════════════════════
// OpenAI-compatible provider wrappers for popular Chinese & global services
// ═════════════════════════════════════════════════════════════════════════════

/// Specification for an OpenAI-compatible provider.
///
/// Every provider in [`OPENAI_PROVIDER_SPECS`] speaks the OpenAI
/// chat-completions wire format and differs only in endpoint, default model,
/// the environment variables consulted, and (rarely) a pinned model or a
/// required user agent. Modelling them as *data* rather than one delegating
/// newtype per vendor means adding a provider is a single table entry instead
/// of ~30 lines of boilerplate trait delegation.
pub struct OpenAiProviderSpec {
    /// Stable identifier used in config (`default_provider`) and the TUI.
    pub id: &'static str,
    /// Full chat-completions endpoint URL.
    pub base_url: &'static str,
    /// Model used when neither config nor environment specifies one.
    pub default_model: &'static str,
    /// Environment variable consulted for the API key.
    pub env_api_key: &'static str,
    /// Environment variable consulted for a model override.
    pub env_model: &'static str,
    /// When set, the endpoint pins this model and ignores any override
    /// (e.g. the Kimi coding endpoint).
    pub fixed_model: Option<&'static str>,
    /// When set, the endpoint requires this user agent unless overridden.
    pub default_user_agent: Option<&'static str>,
}

/// The single registry of OpenAI-compatible providers — the source of truth for
/// their endpoints, default models, and environment variables. The entries
/// live beside their provider's other data (`kimi`, `zai`).
pub const OPENAI_PROVIDER_SPECS: &[OpenAiProviderSpec] = &[
    kimi::PROVIDER_SPEC,
    // DeepSeek V4 (Flash + Pro) is served as one multi-model `deepseek` provider
    // built in the catalog layer (both models share one DEEPSEEK_API_KEY), not as
    // two single-model registry presets.
    zai::PROVIDER_SPEC,
];

/// Look up an OpenAI-compatible provider spec by its identifier. Exact match
/// only; preset ids are unique and do not have alias mappings.
pub fn openai_provider_spec(id: &str) -> Option<&'static OpenAiProviderSpec> {
    OPENAI_PROVIDER_SPECS.iter().find(|spec| spec.id == id)
}

// ═════════════════════════════════════════════════════════════════════════════
// Provider presets — the seed spec for a user-added connection
// ═════════════════════════════════════════════════════════════════════════════

/// The reconciliation-relevant subset of an add-connection preset.
///
/// A connection created from a preset records the preset's stable
/// [`id`](ProviderPresetSpec::id) on its `UserProviderConfig`. At startup the
/// catalog uses the preset protocol and seed `models` to reconcile the
/// connection. Fixed connections mirror the seed list; API-discovered
/// connections retain only provider-advertised ids known to the client for
/// that protocol. This struct is the source of truth for that mapping; it
/// intentionally lives in `muta-providers` (where the model constants live)
/// so the reconciliation layer in `muta-agent` and the UI presets in
/// `mutx::tui` both read one table. The UI-only fields (label /
/// description / placeholders) are **not** duplicated here.
pub struct ProviderPresetSpec {
    /// Stable identifier persisted on the connection (`preset_id`). Never
    /// reused and never renamed once shipped — it is the durable join key
    /// between a connection and its preset.
    pub id: &'static str,
    /// The connection-level default endpoint this preset's routes reach.
    pub base_url: &'static str,
    /// The `User-Agent` header this preset's routes must send, when the
    /// provider requires a specific one (the coding-plan endpoints validate
    /// this header). `None` → the shared [`crate::MUTA_USER_AGENT`].
    pub user_agent: Option<&'static str>,
    /// Baseline capability metadata for the models this provider serves.
    /// Lives beside the preset (one table per provider) and is submitted to
    /// `muta_contracts`'s baseline registry at link time; the reconciliation
    /// layer intersects live-discovered ids against this local table.
    pub baselines: &'static [muta_contracts::Model],
    /// Exact inference protocol spoken by the preset's default route.
    pub protocol: muta_contracts::WireProtocol,
    /// The model ids the preset initially seeds, in display/activation order.
    /// Fixed connections continue to mirror this list.
    pub models: &'static [&'static str],
    /// Whether this preset supports **live model-list discovery** — fetching
    /// the provider's actual `GET /models` list at startup instead of mirroring
    /// the compiled-in [`models`](Self::models) snapshot.
    ///
    /// When `true`, a connection created from this preset defaults to
    /// `ModelSource::Api` (live availability intersected with the client model
    /// registry, retaining the last valid subset on error). When `false`, the
    /// connection always uses the snapshot. A preset is marked `false` when
    /// its model list is derived at runtime (opencode-go), since that would
    /// regress under a live overwrite.
    pub discovery: bool,
    /// Whether live discovery may **fit capability metadata** for model ids the
    /// client registry does not know — materializing them as channels with
    /// their advertised context window, reasoning, vision, and effort tiers
    /// (persisted per connection, then overlaid onto model resolution; see
    /// `muta_contracts::model::register_fitted_models`).
    ///
    /// This is a trust decision, not a technical one: it is enabled only for
    /// official first-party endpoints whose `/models` advertises real
    /// capability fields (the Kimi Code platform). Arbitrary relays must never
    /// get it — a malicious or sloppy relay could otherwise inflate a model's
    /// context window or claim vision support the model lacks. When `false`,
    /// discovery keeps only registry-known ids (the historical behavior).
    pub fitting: bool,
    /// Resolve prompt-cache behavior for one exact preset route and model.
    /// Protocol compatibility and provider identity alone never grant cache
    /// controls; model generations may expose different wire fields.
    pub prompt_cache: fn(&str) -> muta_contracts::PromptCacheSpec,
}

pub(crate) const fn unsupported_prompt_cache(_: &str) -> muta_contracts::PromptCacheSpec {
    muta_contracts::PromptCacheSpec::UNSUPPORTED
}

/// The single registry of provider presets offered when adding a connection.
///
/// Each entry's `id` MUST be unique. The set is the source of truth shared by
/// the add-connection UI and the catalog's model reconciliation — a preset id
/// recorded on a user connection resolves back to its entry here. Each entry
/// lives beside its provider's model constants in the per-provider modules.
pub const PROVIDER_PRESET_SPECS: &[ProviderPresetSpec] = &[
    openai::PRESET_SPEC,
    anthropic::PRESET_SPEC,
    google::PRESET_SPEC,
    deepseek::PRESET_SPEC,
    xai::PRESET_SPEC,
    chatgpt::PRESET_SPEC,
    copilot::PRESET_SPEC,
    kimi::PRESET_SPEC,
    zai::PRESET_SPEC,
    opencode_go::PRESET_SPEC,
    antigravity_oauth::PRESET_SPEC,
];

/// Look up a preset spec by its stable id. Exact match only.
pub fn provider_preset_spec(id: &str) -> Option<&'static ProviderPresetSpec> {
    PROVIDER_PRESET_SPECS.iter().find(|spec| spec.id == id)
}

/// Resolve the transport endpoint for **one model** of a preset — the route
/// the catalog materializes at runtime (routes are derived, never persisted).
///
/// Returns `(protocol, base_url, user_agent)` where `protocol` is one of the
/// wire-protocol labels `"openai"` / `"openai-responses"` / `"anthropic"` /
/// `"google"`. Most presets serve every model over one endpoint; the
/// `opencode-go` relay routes models by their registered wire format (OpenAI
/// chat / Anthropic `/messages` / Google `/v1beta`), so its base URL and
/// protocol vary per model. `None` means the preset id is unknown.
pub fn route_for_model(
    preset_id: &str,
    model_id: &str,
) -> Option<(
    muta_contracts::WireProtocol,
    &'static str,
    Option<&'static str>,
)> {
    let spec = provider_preset_spec(preset_id)?;
    if spec.id == "opencode-go" {
        let protocol = muta_contracts::model::resolve(model_id).protocol;
        let base_url = match protocol {
            muta_contracts::WireProtocol::AnthropicMessages => {
                "https://opencode.ai/zen/go/v1/messages"
            }
            muta_contracts::WireProtocol::GoogleGenerateContent => {
                "https://opencode.ai/zen/go/v1beta"
            }
            muta_contracts::WireProtocol::OpenAiResponses => {
                "https://opencode.ai/zen/go/v1/responses"
            }
            muta_contracts::WireProtocol::OpenAiChatCompletions => {
                "https://opencode.ai/zen/go/v1/chat/completions"
            }
        };
        return Some((protocol, base_url, spec.user_agent));
    }
    Some((spec.protocol, spec.base_url, spec.user_agent))
}

impl OpenAiProviderSpec {
    /// Resolve the model to use: a pinned `fixed_model` always wins, otherwise
    /// the caller's override, otherwise the provider default.
    pub fn resolve_model(&self, override_model: Option<String>) -> String {
        if let Some(fixed) = self.fixed_model {
            return fixed.to_string();
        }
        override_model.unwrap_or_else(|| self.default_model.to_string())
    }

    /// Build a concrete [`OpenAiChatCompletionsProvider`] for this spec. `user_agent` overrides
    /// the spec default (used by the Kimi coding endpoint).
    pub fn build(
        &self,
        api_key: String,
        override_model: Option<String>,
        user_agent: Option<String>,
    ) -> OpenAiChatCompletionsProvider {
        let model = self.resolve_model(override_model);
        let agent = user_agent
            .or_else(|| self.default_user_agent.map(str::to_string))
            .unwrap_or_else(|| MUTA_USER_AGENT.to_string());
        OpenAiChatCompletionsProvider::with_base_url_and_user_agent(
            api_key,
            model,
            self.base_url,
            &agent,
        )
        .with_id(self.id.to_string())
    }
}

/// Construct the concrete `Provider` for a [`muta_contracts::catalog::Channel`].
///
/// This is the construction layer that knows about every concrete `Provider`
/// implementation; it lives in `muta-providers` (not `muta-contracts`) so the
/// domain crate stays free of HTTP I/O. `entry_id` becomes the provider's
/// attribution id (`Provider::provider_id`) so assistant responses are
/// attributed to the logical model even after a mid-session switch.
///
/// `session_id` is offered as a routing key only when this exact channel
/// declares routing-key support. A protocol or model-family resemblance never
/// grants that capability to a relay.
pub fn build_provider_for_channel(
    channel: &Channel,
    entry_id: &str,
    session_id: Option<&str>,
) -> Arc<dyn Provider> {
    let credentials = channel.credentials_source();
    let prompt_cache = muta_llm_client::PromptCacheConfig::new(
        channel.prompt_cache.clone(),
        channel.prompt_cache_preference,
        session_id.map(str::to_string),
    );
    match &channel.transport {
        Transport::Google {
            base_url,
            user_agent,
            effort,
            dialect,
        } => {
            let capabilities = channel.capabilities();
            let provider = GoogleProvider::with_credentials(
                credentials,
                channel.model.clone(),
                base_url,
                user_agent,
            )
            .with_reasoning_effort(*effort)
            .with_model_capabilities(capabilities)
            .with_prompt_cache(prompt_cache)
            .with_dialect(*dialect)
            .with_id(entry_id.to_string());
            Arc::new(provider)
        }
        Transport::Anthropic {
            base_url,
            user_agent,
            effort,
            thinking,
            dialect,
        } => {
            let mut provider = AnthropicMessagesProvider::with_credentials(
                credentials,
                channel.model.clone(),
                base_url,
                user_agent,
            )
            .with_id(entry_id.to_string());
            // Cap the response length at the model's registered output limit so
            // high-output models (MiniMax M3) are not truncated by the default.
            let capabilities = channel.capabilities();
            if let Some(max_tokens) = capabilities
                .max_output_tokens
                .or_else(|| anthropic_model_max_tokens(&channel.model))
            {
                provider = provider.with_max_tokens(max_tokens);
            }
            // Apply the two reasoning knobs INDEPENDENTLY. effort (depth) and
            // thinking (on/off) are orthogonal on the wire, so we never couple
            // them: setting effort must not implicitly turn thinking on, and an
            // explicit thinking override must not change effort. Each is an
            // optional override layered onto the model-derived default
            // (`for_model`: thinking **off** unless the user opts in — ADR-0046);
            // anything unset keeps that default. Effort is clamped to the
            // model's registered levels at request-build time.
            let mut cfg =
                ThinkingConfig::for_model(&muta_contracts::model::resolve(&channel.model));
            if let Some(mode) = thinking {
                cfg = cfg.with_mode(*mode);
            }
            if let Some(effort) = effort {
                cfg = cfg.with_effort(*effort);
            }
            provider = provider
                .with_thinking(cfg)
                .with_model_capabilities(capabilities)
                .with_prompt_cache(prompt_cache)
                .with_dialect(*dialect);
            Arc::new(provider)
        }
        Transport::OpenAi {
            base_url,
            user_agent,
            effort,
            dialect,
        } => {
            let capabilities = channel.capabilities();
            // For OpenAI-family transports the effort knob IS the reasoning
            // control: a model that advertises a ladder (GLM-5.x — always-on
            // thinking gated only by `reasoning_effort`) must never send an
            // absent effort, or the endpoint falls back to its server-side
            // default, which can reason far deeper than the tier the picker
            // displays. Default the wire to the same `Effort::channel_default`
            // the picker shows (GPT→medium, others→high clamped to the
            // ladder); an explicit channel override still wins.
            let effective_effort = effective_channel_effort(*effort, &capabilities);
            let provider = OpenAiChatCompletionsProvider::with_credentials(
                credentials,
                channel.model.clone(),
                base_url,
                user_agent,
            )
            .with_reasoning_effort(effective_effort)
            .with_prompt_cache(prompt_cache)
            .with_model_capabilities(capabilities)
            .with_dialect(*dialect)
            .with_id(entry_id.to_string());
            Arc::new(provider)
        }
        Transport::OpenAiResponses {
            base_url,
            user_agent,
            effort,
            dialect,
        } => {
            let capabilities = channel.capabilities();
            // Same wire-level default as the chat-completions arm above —
            // see the comment there.
            let effective_effort = effective_channel_effort(*effort, &capabilities);
            let provider = OpenAiResponsesProvider::with_credentials(
                credentials,
                channel.model.clone(),
                base_url,
            )
            .with_user_agent(user_agent)
            .with_reasoning_effort(effective_effort)
            .with_model_capabilities(capabilities)
            .with_prompt_cache(prompt_cache)
            .with_dialect(*dialect)
            .with_id(entry_id.to_string());
            Arc::new(provider)
        }
    }
}

/// Resolve the effort a channel's request actually carries: the explicit
/// override when set, otherwise the shared [`muta_contracts::Effort::channel_default`]
/// for a model that advertises an effort ladder. Used by the OpenAI-family
/// factory arms so the wire can never omit the reasoning control the picker
/// already promises (`None` remains `None` for ladder-less models — no
/// `reasoning_effort` field is stamped for them at request-build time).
fn effective_channel_effort(
    override_effort: Option<muta_contracts::Effort>,
    capabilities: &muta_contracts::ModelCapabilities,
) -> Option<muta_contracts::Effort> {
    override_effort.or_else(|| {
        let known: Vec<muta_contracts::Effort> = capabilities
            .effort_levels
            .iter()
            .filter_map(muta_contracts::EffortLevel::as_known)
            .collect();
        muta_contracts::Effort::channel_default(&capabilities.family, &known)
    })
}

#[cfg(test)]
mod baseline_fidelity_tests;

#[cfg(test)]
mod spec_tests {
    use super::*;

    #[test]
    fn provider_preset_specs_have_unique_nonempty_ids() {
        // Preset ids are the durable join key between a connection and its
        // preset, so they must be unique and non-empty.
        let mut ids: Vec<&str> = PROVIDER_PRESET_SPECS.iter().map(|spec| spec.id).collect();
        ids.sort_unstable();
        assert!(
            ids.iter().all(|id| !id.is_empty()),
            "preset ids must be non-empty"
        );
        let dups: Vec<&[&str]> = ids.windows(2).filter(|pair| pair[0] == pair[1]).collect();
        assert!(dups.is_empty(), "duplicate preset ids: {dups:?}");
    }

    #[test]
    fn provider_preset_spec_resolves_each_known_id() {
        // The reconciliation layer resolves a connection's preset_id back to a
        // spec here; every id in the table must round-trip.
        for spec in PROVIDER_PRESET_SPECS {
            let resolved = provider_preset_spec(spec.id).expect("id resolves");
            assert_eq!(resolved.id, spec.id);
            assert!(!resolved.models.is_empty(), "{} has no models", spec.id);
        }
        // Unknown ids resolve to None (graceful: an unknown preset_id leaves
        // the connection untouched).
        assert!(provider_preset_spec("does-not-exist").is_none());
    }

    #[test]
    fn shared_baseline_ids_are_identical_across_provider_tables() {
        // `resolve_model` (baseline_models().find) returns the first table that
        // declares an id, so when several provider files carry the same id
        // (zai/opencode-go both list glm-5.2; kimi/opencode-go both list
        // kimi-k2.7-code) the copies MUST be field-identical -- otherwise which
        // copy wins depends on link order, an invisible behavior change.
        // `Model` is not `PartialEq`, so compare a derived signature instead.
        use std::collections::HashMap;

        fn signature(m: &muta_contracts::Model) -> String {
            format!(
                "{:?}|{:?}|{}|{}|{:?}|{:?}|{:?}",
                m.context_window,
                m.thinking,
                m.tool_call,
                m.vision,
                m.protocol,
                m.model_guidance,
                m.effort_levels,
            )
        }

        let mut seen: HashMap<&str, (&str, String)> = HashMap::new();
        for spec in PROVIDER_PRESET_SPECS {
            for m in spec.baselines {
                let sig = signature(m);
                if let Some((first_provider, first_sig)) = seen.insert(m.id, (spec.id, sig)) {
                    assert_eq!(
                        first_sig,
                        seen[&m.id].1,
                        "{id}: baseline declared by {first_provider} and {} disagree \
                         (context_window/thinking/tool_call/vision/format/guidance/effort)",
                        spec.id,
                        id = m.id
                    );
                }
            }
        }
    }

    #[test]
    fn preset_models_are_covered_by_the_local_baseline_table() {
        // Every id a preset seeds must have baseline metadata in the same
        // provider file's local table — that table is what the reconciliation
        // layer intersects live discovery against.
        for spec in PROVIDER_PRESET_SPECS {
            let baseline_ids: std::collections::HashSet<&str> =
                spec.baselines.iter().map(|m| m.id).collect();
            for id in spec.models {
                assert!(
                    baseline_ids.contains(id),
                    "{} preset model {id} has no entry in the local baseline table",
                    spec.id
                );
            }
        }
    }

    #[test]
    fn opencode_go_served_ids_all_have_local_baselines() {
        // The go seed derives channels from the preset's local table; every
        // advertised SERVED id must be present there.
        let spec = provider_preset_spec("opencode-go").expect("opencode-go preset");
        let baseline_ids: std::collections::HashSet<&str> =
            spec.baselines.iter().map(|m| m.id).collect();
        for id in OPENCODE_GO_SERVED_MODELS {
            assert!(
                baseline_ids.contains(id),
                "served id {id} missing from opencode_go's baseline table"
            );
        }
    }
}

#[cfg(test)]
mod build_tests {
    use super::*;

    #[test]
    fn build_provider_stamps_entry_id_on_openai_compat() {
        let channel = Channel {
            id: "default".to_string(),
            label: "OpenAI".to_string(),
            transport: Transport::OpenAi {
                base_url: "https://api.openai.com/v1/chat/completions".to_string(),
                user_agent: "agent".to_string(),
                effort: None,
                dialect: Default::default(),
            },
            credentials: muta_contracts::static_credential("k"),
            model: "gpt-4o".to_string(),
            remote: None,
            user_overrides: None,
            prompt_cache_preference: muta_contracts::PromptCachePreference::default(),
            prompt_cache: muta_contracts::PromptCacheCapabilities::unsupported(),
        };
        let provider = build_provider_for_channel(&channel, "openai", None);
        assert_eq!(provider.provider_id(), "openai");
        assert_eq!(provider.model(), "gpt-4o");
    }

    #[test]
    fn openai_channel_without_override_defaults_effort_on_the_wire() {
        // GLM-5.3 advertises a ladder (low/high/xhigh/max) and its endpoint
        // runs always-on thinking gated only by `reasoning_effort`. A channel
        // with no explicit override must default to `high` — the same tier
        // the picker displays — instead of omitting the field and eating the
        // server's (much deeper) default.
        let channel = Channel {
            id: "default".to_string(),
            label: "ZAI Code".to_string(),
            transport: Transport::OpenAi {
                base_url: "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions"
                    .to_string(),
                user_agent: "agent".to_string(),
                effort: None,
                dialect: Default::default(),
            },
            credentials: muta_contracts::static_credential("k"),
            model: "glm-5.3".to_string(),
            remote: None,
            user_overrides: None,
            prompt_cache_preference: muta_contracts::PromptCachePreference::default(),
            prompt_cache: muta_contracts::PromptCacheCapabilities::unsupported(),
        };
        let provider = build_provider_for_channel(&channel, "zai-code", None);
        assert_eq!(provider.effort(), Some(muta_contracts::Effort::High));

        // An explicit override still wins verbatim.
        let mut pinned = channel;
        if let Transport::OpenAi { effort, .. } = &mut pinned.transport {
            *effort = Some(muta_contracts::Effort::Low);
        }
        let provider = build_provider_for_channel(&pinned, "zai-code", None);
        assert_eq!(provider.effort(), Some(muta_contracts::Effort::Low));
    }

    #[test]
    fn ladderless_model_keeps_absent_effort_on_the_wire() {
        // A model with NO effort ladder must keep `None`: stamping a
        // `reasoning_effort` the endpoint never advertised would be noise at
        // best and a 400 at worst.
        let channel = Channel {
            id: "default".to_string(),
            label: "OpenAI".to_string(),
            transport: Transport::OpenAi {
                base_url: "https://api.openai.com/v1/chat/completions".to_string(),
                user_agent: "agent".to_string(),
                effort: None,
                dialect: Default::default(),
            },
            credentials: muta_contracts::static_credential("k"),
            model: "gpt-4o".to_string(),
            remote: None,
            user_overrides: None,
            prompt_cache_preference: muta_contracts::PromptCachePreference::default(),
            prompt_cache: muta_contracts::PromptCacheCapabilities::unsupported(),
        };
        let provider = build_provider_for_channel(&channel, "openai", None);
        assert_eq!(provider.effort(), None);
    }

    #[test]
    fn build_provider_dispatches_anthropic_transport() {
        // opencode-go's MiniMax/Qwen models reach an Anthropic /messages
        // endpoint; the catalog builds an Anthropic transport for them, and
        // build_provider_for_channel must dispatch it to the messages provider.
        let channel = Channel {
            id: "minimax-m3".to_string(),
            label: "MiniMax M3".to_string(),
            transport: Transport::Anthropic {
                base_url: "https://opencode.ai/zen/go/v1/messages".to_string(),
                user_agent: "agent".to_string(),
                effort: None,
                thinking: None,
                dialect: Default::default(),
            },
            credentials: muta_contracts::static_credential("go-key"),
            model: "minimax-m3".to_string(),
            remote: None,
            user_overrides: None,
            prompt_cache_preference: muta_contracts::PromptCachePreference::default(),
            prompt_cache: muta_contracts::PromptCacheCapabilities::unsupported(),
        };
        let provider = build_provider_for_channel(&channel, "opencode-go", None);
        assert_eq!(provider.provider_id(), "opencode-go");
        assert_eq!(provider.model(), "minimax-m3");
    }

    #[test]
    fn builtin_provider_models_resolve_with_expected_wire_formats() {
        use muta_contracts::WireProtocol;
        // Every model a multi-model built-in serves must exist in the model
        // registry (so metadata resolves) and carry the wire format its provider
        // speaks.
        for (&id, expected) in crate::ANTHROPIC_BUILTIN_MODELS
            .iter()
            .map(|id| (id, WireProtocol::AnthropicMessages))
            .chain(
                crate::GOOGLE_BUILTIN_MODELS
                    .iter()
                    .map(|id| (id, WireProtocol::GoogleGenerateContent)),
            )
            .chain(
                crate::DEEPSEEK_BUILTIN_MODELS
                    .iter()
                    .map(|id| (id, WireProtocol::OpenAiChatCompletions)),
            )
            .chain(
                crate::OPENAI_BUILTIN_MODELS
                    .iter()
                    .map(|id| (id, WireProtocol::OpenAiChatCompletions)),
            )
            .chain(
                crate::XAI_BUILTIN_MODELS
                    .iter()
                    .map(|id| (id, WireProtocol::OpenAiChatCompletions)),
            )
            .chain(
                crate::CHATGPT_BUILTIN_MODELS
                    .iter()
                    .map(|id| (id, WireProtocol::OpenAiChatCompletions)),
            )
            .chain(
                crate::COPILOT_SEED_MODELS
                    .iter()
                    .map(|id| (id, WireProtocol::OpenAiChatCompletions)),
            )
            .chain(
                crate::KIMI_CODE_MODELS
                    .iter()
                    .map(|id| (id, WireProtocol::OpenAiChatCompletions)),
            )
            .chain(
                crate::ZAI_CODE_MODELS
                    .iter()
                    .map(|id| (id, WireProtocol::OpenAiChatCompletions)),
            )
            .chain(
                crate::OPENCODE_GO_MODELS
                    .iter()
                    .map(|id| (id, WireProtocol::OpenAiChatCompletions)),
            )
            .chain(
                crate::ANTIGRAVITY_OAUTH_MODELS
                    .iter()
                    .map(|id| (id, WireProtocol::GoogleGenerateContent)),
            )
        {
            let model = muta_contracts::model::resolve(id);
            assert_eq!(model.id, id, "model {id} must be registered");
            assert_eq!(model.protocol, expected, "{id} wire format");
        }
    }
}
