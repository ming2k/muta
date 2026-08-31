//! The `kimi-code` provider template and its legacy registry preset:
//! Moonshot AI's Kimi Code coding platform (`api.kimi.com/coding/v1`).

use muta_contracts::thinking::ThinkingSupport;
use muta_contracts::{Model, WireProtocol};

use super::{OpenAiProviderSpec, ProviderPresetSpec};

/// Models served by Moonshot's Kimi Code endpoint, in display/activation
/// order — the first entry is the initial active channel. `k3` is the
/// platform's current flagship; `kimi-k2.7-code` remains as the previous
/// pinned alias.
pub const KIMI_CODE_MODELS: &[&str] = &["k3", "kimi-k2.7-code"];

// Kimi Code — Moonshot AI's coding platform (api.kimi.com/coding/v1).
// The platform pins the model id to the fixed `k3` alias (Kimi K3, 1M
// context, always-on thinking); its live `GET /models` also lists the
// legacy `kimi-for-coding` (K2.7) ids, kept selectable via
// [`KIMI_CODE_MODELS`]. API key env still uses the MOONSHOT_API_KEY
// legacy name for config compatibility. The [`OPENCODE_USER_AGENT`]
// is borrowed on purpose: the endpoint was live-tested (2026-07) to
// accept any UA — including none — under OAuth auth, but whether the
// API-key path gates on a recognized coding-agent UA is unknown, so the
// recognized default stays as the zero-risk choice.
pub(crate) const PROVIDER_SPEC: OpenAiProviderSpec = OpenAiProviderSpec {
    id: "kimi-code",
    base_url: "https://api.kimi.com/coding/v1/chat/completions",
    default_model: "k3",
    env_api_key: "MOONSHOT_API_KEY",
    env_model: "MOONSHOT_MODEL",
    fixed_model: Some("k3"),
    default_user_agent: Some(muta_llm_client::OPENCODE_USER_AGENT),
};

/// Baseline capability metadata for the models this provider serves,
/// submitted to `muta_contracts`'s registry at link time (see
/// [`muta_contracts::model::BaselineModels`]).
pub const MODELS: &[Model] = &[
    // ── Kimi (Moonshot / opencode-go) ─────────────────────────────────────
    Model {
        // The Kimi Code platform's current flagship. The platform's live
        // `GET /models` advertises `k3` with a 1M context window, image/video
        // inputs, and always-on thinking (`supports_thinking_type: "only"`) —
        // over the OpenAI-compatible wire the always-on reasoning simply streams
        // back as `reasoning_content`, so there is no thinking switch to model.
        // The effort ladder is tunable: `reasoning_effort` accepts
        // `low`/`high`/`max` (platform default `high`), advertised so the
        // pickers/hint bar can show the effective level and the editor can cycle
        // it; the fitted overlay refreshes it from the live `/models` list.
        id: "k3",
        family: "kimi",
        context_window: 1_048_576,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_LOW_HIGH_MAX,
    },
    Model {
        id: "kimi-k2.7-code",
        family: "kimi",
        context_window: 262_144,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "kimi-k2.6",
        family: "kimi",
        context_window: 262_144,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "kimi-k2.5",
        family: "kimi",
        context_window: 262_144,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
];

inventory::submit!(muta_contracts::model::BaselineModels(MODELS));

fn prompt_cache_for_model(_: &str) -> muta_contracts::PromptCacheSpec {
    muta_contracts::PromptCacheSpec {
        modes: &[muta_contracts::PromptCacheMode::Implicit],
        default_mode: Some(muta_contracts::PromptCacheMode::Implicit),
        supported_retentions: &[],
        default_retention: None,
        disable_supported: false,
        routing_key_supported: false,
        max_breakpoints: None,
        min_cacheable_tokens: None,
        reports_reads: true,
        reports_writes: false,
        reports_misses: false,
    }
}

pub(crate) const PRESET_SPEC: ProviderPresetSpec = ProviderPresetSpec {
    prompt_cache: prompt_cache_for_model,
    id: "kimi-code",
    baselines: MODELS,
    base_url: "https://api.kimi.com/coding/v1/chat/completions",
    user_agent: Some(crate::OPENCODE_USER_AGENT),
    protocol: WireProtocol::OpenAiChatCompletions,
    // The Kimi Code platform exposes a live /models endpoint, so instances
    // created from this preset track the platform's actual model list.
    discovery: true,
    // It is also a trusted first-party endpoint whose /models advertises
    // real capability fields: platform-native ids the static registry does
    // not know (e.g. `kimi-for-coding`, and every future model) are fitted
    // with their advertised metadata instead of being intersected away —
    // new platform models become usable with zero client changes.
    fitting: true,
    models: KIMI_CODE_MODELS,
};

#[cfg(test)]
mod tests {
    use crate::openai_provider_spec;
    use muta_contracts::Provider;

    #[test]
    fn kimi_code_uses_kimi_code_platform() {
        let spec = openai_provider_spec("kimi-code").expect("kimi-code spec");
        // The Kimi Code platform pins the model id — overrides are ignored.
        assert_eq!(spec.resolve_model(None), "k3");
        assert_eq!(spec.resolve_model(Some("kimi-k2.7-code".to_string())), "k3");

        let provider = spec.build("test-key".to_string(), None, None);
        assert_eq!(
            provider.endpoint.base_url(),
            "https://api.kimi.com/coding/v1/chat/completions"
        );
        assert_eq!(provider.endpoint.model_id(), "k3");
        // The Kimi Code platform requires a recognized coding-agent UA.
        assert_eq!(provider.endpoint.user_agent(), crate::OPENCODE_USER_AGENT);
        // The registry stamps the preset id onto the concrete provider so
        // assistant responses can be attributed to "kimi-code".
        assert_eq!(provider.endpoint.id(), "kimi-code");
        assert_eq!(provider.provider_id(), "kimi-code");
        assert_eq!(provider.model(), "k3");
    }
}
