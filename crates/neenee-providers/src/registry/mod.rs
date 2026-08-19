//! OpenAI-compatible provider registry and the `Channel` → concrete `Provider`
//! factory consumed by the orchestration layer.
//!
//! The registry is split one file per provider: each module holds the
//! provider's model constants, its [`ProviderTemplateSpec`] entry, and (for
//! the two legacy OpenAI-compatible presets) its [`OpenAiProviderSpec`] entry.
//! This file keeps the shared types, the aggregate tables, and the factory.

use neenee_contracts::Provider;
use neenee_contracts::catalog::{Channel, Transport};
use std::sync::Arc;

use crate::{
    AnthropicMessagesProvider, GoogleProvider, NEENEE_USER_AGENT, OpenAiChatCompletionsProvider,
    OpenAiResponsesProvider, ThinkingConfig,
};

mod anthropic;
mod antigravity_oauth;
mod chatgpt;
mod copilot;
mod custom_openai;
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
// Provider templates — the seed spec for a user-added provider instance
// ═════════════════════════════════════════════════════════════════════════════

/// The reconciliation-relevant subset of an add-provider template.
///
/// A provider instance created from a template records the template's stable
/// [`id`](ProviderTemplateSpec::id) on its `UserProviderConfig`. At startup the
/// catalog uses the template protocol and seed `models` to reconcile the
/// instance. Fixed instances mirror the seed list; API-discovered instances
/// retain only provider-advertised ids known to the client for that protocol.
/// This struct is the source of truth for that mapping; it intentionally
/// lives in `neenee-providers` (where the model constants live) so the
/// reconciliation layer in `neenee-agent` and the UI templates in
/// `neenee-cli::tui` both read one table. The UI-only fields (label /
/// description / placeholders) are **not** duplicated here.
pub struct ProviderTemplateSpec {
    /// Stable identifier persisted on the instance (`template_id`). Never
    /// reused and never renamed once shipped — it is the durable join key
    /// between an instance and its template.
    pub id: &'static str,
    /// Baseline capability metadata for the models this provider serves.
    /// Lives beside the template (one table per provider) and is submitted to
    /// `neenee_contracts`'s baseline registry at link time; the reconciliation
    /// layer intersects live-discovered ids against this local table.
    pub baselines: &'static [neenee_contracts::Model],
    /// Wire protocol the template's channels speak: `"openai"` |
    /// `"openai-responses"` | `"anthropic"` | `"google"` (the legacy `"gemini"`
    /// label is still accepted). `"openai-responses"` is the OpenAI Responses
    /// API (`/responses` endpoint) over an ordinary API key — distinct from
    /// `"openai"` (chat completions) in transport only; model metadata and
    /// live discovery stay on the OpenAI shape.
    pub protocol: &'static str,
    /// The model ids the template initially seeds, in display/activation order.
    /// Fixed instances continue to mirror this list.
    pub models: &'static [&'static str],
    /// Whether this template supports **live model-list discovery** — fetching
    /// the provider's actual `GET /models` list at startup instead of mirroring
    /// the compiled-in [`models`](Self::models) snapshot.
    ///
    /// When `true`, an instance created from this template defaults to
    /// `ModelSource::Api` (live availability intersected with the client model
    /// registry, retaining the last valid subset on error). When `false`, the
    /// instance always uses the snapshot. A template is marked `false` when its
    /// model list is derived at runtime (opencode-go), since that would
    /// regress under a live overwrite.
    pub discovery: bool,
    /// Whether live discovery may **fit capability metadata** for model ids the
    /// client registry does not know — materializing them as channels with
    /// their advertised context window, reasoning, vision, and effort tiers
    /// (persisted per instance, then overlaid onto model resolution; see
    /// `neenee_contracts::model::register_fitted_models`).
    ///
    /// This is a trust decision, not a technical one: it is enabled only for
    /// official first-party endpoints whose `/models` advertises real
    /// capability fields (the Kimi Code platform). Arbitrary relays must never
    /// get it — a malicious or sloppy relay could otherwise inflate a model's
    /// context window or claim vision support the model lacks. When `false`,
    /// discovery keeps only registry-known ids (the historical behavior).
    pub fitting: bool,
}

/// The single registry of provider templates offered when adding a provider.
///
/// Each entry's `id` MUST be unique. The set is the source of truth shared by
/// the add-provider UI and the catalog's model reconciliation — a template id
/// recorded on a user instance resolves back to its entry here. Each entry
/// lives beside its provider's model constants in the per-provider modules.
pub const PROVIDER_TEMPLATE_SPECS: &[ProviderTemplateSpec] = &[
    openai::TEMPLATE_SPEC,
    anthropic::TEMPLATE_SPEC,
    google::TEMPLATE_SPEC,
    deepseek::TEMPLATE_SPEC,
    xai::TEMPLATE_SPEC,
    chatgpt::TEMPLATE_SPEC,
    copilot::TEMPLATE_SPEC,
    kimi::TEMPLATE_SPEC,
    zai::TEMPLATE_SPEC,
    opencode_go::TEMPLATE_SPEC,
    antigravity_oauth::TEMPLATE_SPEC,
    // The generic escape hatch: no seeded models — the Model field supplies
    // the one id, so an arbitrary OpenAI-compatible endpoint (third-party
    // relay, self-hosted gateway) works without a curated template.
    custom_openai::TEMPLATE_SPEC,
];

/// Look up a template spec by its stable id. Exact match only.
pub fn provider_template_spec(id: &str) -> Option<&'static ProviderTemplateSpec> {
    PROVIDER_TEMPLATE_SPECS.iter().find(|spec| spec.id == id)
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
            .unwrap_or_else(|| NEENEE_USER_AGENT.to_string());
        OpenAiChatCompletionsProvider::with_base_url_and_user_agent(
            api_key,
            model,
            self.base_url,
            &agent,
        )
        .with_id(self.id.to_string())
    }
}

/// Construct the concrete `Provider` for a [`neenee_contracts::catalog::Channel`].
///
/// This is the construction layer that knows about every concrete `Provider`
/// implementation; it lives in `neenee-providers` (not `neenee-contracts`) so the
/// domain crate stays free of HTTP I/O. `entry_id` becomes the provider's
/// attribution id (`Provider::provider_id`) so assistant responses are
/// attributed to the logical model even after a mid-session switch.
///
/// `session_id` participates in prompt-cache control (ADR-0067): when the
/// resolved [`neenee_contracts::CachePolicy`] for the model's family is
/// [`SessionKey`](neenee_contracts::CachePolicy::SessionKey) (Moonshot / Kimi), the
/// session id is stamped as the provider's `prompt_cache_key` so the server-side
/// cache namespaces per conversation and repeated prefixes hit at a discount.
/// Pass `None` when no session is known yet (shared bootstrap); the key is then
/// left unset and the provider caches at the server's default granularity.
pub fn build_provider_for_channel(
    channel: &Channel,
    entry_id: &str,
    session_id: Option<&str>,
) -> Arc<dyn Provider> {
    match &channel.transport {
        Transport::Google {
            base_url,
            user_agent,
            effort,
            project_id,
        } => {
            let capabilities = channel.capabilities();
            let mut provider = GoogleProvider::with_base_url_and_user_agent(
                channel.api_key.expose_secret().to_string(),
                channel.model.clone(),
                base_url,
                user_agent,
            )
            .with_reasoning_effort(*effort)
            .with_model_capabilities(capabilities)
            .with_id(entry_id.to_string());
            if let Some(pid) = project_id {
                provider = provider.with_project_id(pid.clone());
            }
            Arc::new(provider)
        }
        Transport::Anthropic {
            base_url,
            user_agent,
            effort,
            thinking,
            copilot,
        } => {
            let mut provider = AnthropicMessagesProvider::with_base_url_and_user_agent(
                channel.api_key.expose_secret().to_string(),
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
                ThinkingConfig::for_model(&neenee_contracts::model::resolve(&channel.model));
            if let Some(mode) = thinking {
                cfg = cfg.with_mode(*mode);
            }
            if let Some(effort) = effort {
                cfg = cfg.with_effort(*effort);
            }
            provider = provider
                .with_thinking(cfg)
                .with_model_capabilities(capabilities)
                .with_copilot(*copilot);
            Arc::new(provider)
        }
        Transport::OpenAi {
            base_url,
            user_agent,
            effort,
            copilot,
        } => {
            let capabilities = channel.capabilities();
            let policy = neenee_contracts::CachePolicy::for_family(&capabilities.family);
            let cache_key = if policy.injects_session_key() {
                session_id.map(str::to_string)
            } else {
                None
            };
            let provider = OpenAiChatCompletionsProvider::with_base_url_and_user_agent(
                channel.api_key.expose_secret().to_string(),
                channel.model.clone(),
                base_url,
                user_agent,
            )
            .with_reasoning_effort(*effort)
            .with_prompt_cache_key(cache_key)
            .with_model_capabilities(capabilities)
            .with_copilot(*copilot)
            .with_id(entry_id.to_string());
            Arc::new(provider)
        }
        Transport::OpenAiResponses {
            base_url,
            user_agent,
            effort,
            account_id,
            copilot,
        } => {
            let capabilities = channel.capabilities();
            let provider = OpenAiResponsesProvider::new(
                channel.api_key.expose_secret().to_string(),
                channel.model.clone(),
                base_url,
                account_id.clone(),
            )
            .with_user_agent(user_agent)
            .with_reasoning_effort(*effort)
            .with_model_capabilities(capabilities)
            .with_copilot(*copilot)
            .with_id(entry_id.to_string());
            Arc::new(provider)
        }
    }
}

#[cfg(test)]
mod baseline_fidelity_tests;

#[cfg(test)]
mod spec_tests {
    use super::*;

    #[test]
    fn provider_template_specs_have_unique_nonempty_ids() {
        // Template ids are the durable join key between an instance and its
        // template, so they must be unique and non-empty.
        let mut ids: Vec<&str> = PROVIDER_TEMPLATE_SPECS.iter().map(|spec| spec.id).collect();
        ids.sort_unstable();
        assert!(
            ids.iter().all(|id| !id.is_empty()),
            "template ids must be non-empty"
        );
        let dups: Vec<&[&str]> = ids.windows(2).filter(|pair| pair[0] == pair[1]).collect();
        assert!(dups.is_empty(), "duplicate template ids: {dups:?}");
    }

    #[test]
    fn provider_template_spec_resolves_each_known_id() {
        // The reconciliation layer resolves an instance's template_id back to a
        // spec here; every id in the table must round-trip.
        // `custom-openai` is the one template allowed to seed no models: the
        // free-text Model field supplies the one id at create time, so there
        // is no snapshot to mirror.
        const NO_SEED_TEMPLATES: &[&str] = &["custom-openai"];
        for spec in PROVIDER_TEMPLATE_SPECS {
            let resolved = provider_template_spec(spec.id).expect("id resolves");
            assert_eq!(resolved.id, spec.id);
            if NO_SEED_TEMPLATES.contains(&spec.id) {
                assert!(
                    resolved.models.is_empty(),
                    "{} is a no-seed template and must stay empty",
                    spec.id
                );
            } else {
                assert!(!resolved.models.is_empty(), "{} has no models", spec.id);
            }
        }
        // Unknown ids resolve to None (graceful: an unknown template_id leaves
        // the instance untouched).
        assert!(provider_template_spec("does-not-exist").is_none());
    }

    #[test]
    fn template_models_are_covered_by_the_local_baseline_table() {
        // Every id a template seeds must have baseline metadata in the same
        // provider file's local table — that table is what the reconciliation
        // layer intersects live discovery against.
        for spec in PROVIDER_TEMPLATE_SPECS {
            let baseline_ids: std::collections::HashSet<&str> =
                spec.baselines.iter().map(|m| m.id).collect();
            for id in spec.models {
                assert!(
                    baseline_ids.contains(id),
                    "{} template model {id} has no entry in the local baseline table",
                    spec.id
                );
            }
        }
    }

    #[test]
    fn opencode_go_served_ids_all_have_local_baselines() {
        // The go seed derives channels from the template's local table; every
        // advertised SERVED id must be present there.
        let spec = provider_template_spec("opencode-go").expect("opencode-go template");
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
                copilot: false,
            },
            api_key: "k".into(),
            model: "gpt-4o".to_string(),
            remote: None,
        };
        let provider = build_provider_for_channel(&channel, "openai", None);
        assert_eq!(provider.provider_id(), "openai");
        assert_eq!(provider.model(), "gpt-4o");
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
                copilot: false,
            },
            api_key: "go-key".into(),
            model: "minimax-m3".to_string(),
            remote: None,
        };
        let provider = build_provider_for_channel(&channel, "opencode-go", None);
        assert_eq!(provider.provider_id(), "opencode-go");
        assert_eq!(provider.model(), "minimax-m3");
    }

    #[test]
    fn builtin_provider_models_resolve_with_expected_wire_formats() {
        use neenee_contracts::WireFormat;
        // Every model a multi-model built-in serves must exist in the model
        // registry (so metadata resolves) and carry the wire format its provider
        // speaks.
        for (&id, expected) in crate::ANTHROPIC_BUILTIN_MODELS
            .iter()
            .map(|id| (id, WireFormat::AnthropicCompat))
            .chain(
                crate::GOOGLE_BUILTIN_MODELS
                    .iter()
                    .map(|id| (id, WireFormat::Google)),
            )
            .chain(
                crate::DEEPSEEK_BUILTIN_MODELS
                    .iter()
                    .map(|id| (id, WireFormat::OpenAi)),
            )
            .chain(
                crate::OPENAI_BUILTIN_MODELS
                    .iter()
                    .map(|id| (id, WireFormat::OpenAi)),
            )
            .chain(
                crate::XAI_BUILTIN_MODELS
                    .iter()
                    .map(|id| (id, WireFormat::OpenAi)),
            )
            .chain(
                crate::CHATGPT_BUILTIN_MODELS
                    .iter()
                    .map(|id| (id, WireFormat::OpenAi)),
            )
            .chain(
                crate::COPILOT_SEED_MODELS
                    .iter()
                    .map(|id| (id, WireFormat::OpenAi)),
            )
            .chain(
                crate::KIMI_CODE_MODELS
                    .iter()
                    .map(|id| (id, WireFormat::OpenAi)),
            )
            .chain(
                crate::ZAI_CODE_MODELS
                    .iter()
                    .map(|id| (id, WireFormat::OpenAi)),
            )
            .chain(
                crate::OPENCODE_GO_MODELS
                    .iter()
                    .map(|id| (id, WireFormat::OpenAi)),
            )
            .chain(
                crate::ANTIGRAVITY_OAUTH_MODELS
                    .iter()
                    .map(|id| (id, WireFormat::Google)),
            )
        {
            let model = neenee_contracts::model::resolve(id);
            assert_eq!(model.id, id, "model {id} must be registered");
            assert_eq!(model.format, expected, "{id} wire format");
        }
    }
}
