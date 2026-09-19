//! The `qoder` provider template: Alibaba Qoder's subscription coding
//! platform (`api*.qoder.sh`, CN: `*.qoder.com.cn`), COSY-signed SSE wire.

use muta_contracts::reasoning::ReasoningSupport;
use muta_contracts::{Model, WireProtocol};

use super::{ModelProviderSpec, RemoteCatalogSource};

/// Models served by Qoder's inference endpoint, in display/activation order —
/// the first entry is the initial active channel. Qoder publishes no live
/// model-list endpoint, so this baseline is pinned; the ids mirror the
/// community protocol documentation (qoder3 family flagship + the qwen3
/// coding models the CLI bundles).
pub const QODER_MODELS: &[Model] = &[
    Model {
        // Qoder3 — the platform's current flagship. Advertised through the
        // CLI's bundled catalog with a large coding-tuned context; thinking
        // streams back as `reasoning_content` on the chat wire.
        id: "qoder3",
        family: "qoder",
        context_window: 262_144,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "qoder3-max",
        family: "qoder",
        context_window: 262_144,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "qoder3-base",
        family: "qoder",
        context_window: 262_144,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        // Qwen3-Coder — the open-weights coding model Qoder bundles as the
        // fast tier (the CLI's model key aliases it through X-Model-Key).
        id: "qwen3-coder-plus",
        family: "qwen",
        context_window: 262_144,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
];

inventory::submit!(muta_contracts::model::BaselineModels(QODER_MODELS));

pub(crate) const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: muta_contracts::ProviderDialect::Qoder,
    protocol_roots: std::borrow::Cow::Borrowed(&[]),
    catalog_root_url: None,
    prompt_cache: super::PromptCachePolicy::Compiled(super::unsupported_prompt_cache),
    id: std::borrow::Cow::Borrowed("qoder"),
    baselines: QODER_MODELS,
    // Base URL host only — the executor appends Qoder's fixed
    // `/algo/api/v2/service/pro/sse/agent_chat_generation?…` inference path
    // (see `muta_llm_client::...::qoder::inference_url`). The `.sh` host is
    // the international CLI line; CN accounts route through
    // `https://gateway.qoder.com.cn` (the OAuth preset tracks the issuer).
    root_url: std::borrow::Cow::Borrowed("https://api2.qoder.sh"),
    user_agent: None,
    protocol: WireProtocol::ChatCompletions,
    // No live /models endpoint: the baseline above is the whole universe.
    catalog_source: RemoteCatalogSource::None,
    default_client_profile: muta_contracts::ClientPreset::Native,
    client_profile_sensitive: false,
    models: &["qoder3", "qoder3-max", "qoder3-base", "qwen3-coder-plus"],
};

#[cfg(test)]
mod tests {
    use super::MODEL_PROVIDER_SPEC as SPEC;
    use super::{QODER_MODELS, RemoteCatalogSource};
    use muta_contracts::WireProtocol;

    #[test]
    fn spec_resolves_and_serves_the_chat_wire() {
        assert_eq!(SPEC.id, "qoder");
        assert_eq!(SPEC.protocol, WireProtocol::ChatCompletions);
        assert_eq!(SPEC.root_url.as_ref(), "https://api2.qoder.sh");
        // First model is the activation default.
        assert_eq!(SPEC.models[0], "qoder3");
        // Qoder serves the model list statically.
        assert_eq!(SPEC.catalog_source, RemoteCatalogSource::None);
    }

    #[test]
    fn baselines_are_registered_with_the_contract_registry() {
        for model in QODER_MODELS {
            let resolved = muta_contracts::resolve_model(&model.id);
            assert_eq!(resolved.family, model.family, "{}", model.id);
            assert_eq!(resolved.protocol, WireProtocol::ChatCompletions);
        }
    }
}
