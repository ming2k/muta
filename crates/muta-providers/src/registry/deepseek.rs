//! The built-in `deepseek` provider preset: DeepSeek V4 Flash, Pro, and the
//! experimental Flash Vision model over
//! the official Responses API, one key (`DEEPSEEK_API_KEY`).
//!
//! DeepSeek V4 (Flash + Pro) is served as one multi-model `deepseek` provider
//! built in the catalog layer (both models share one `DEEPSEEK_API_KEY`), not
//! as two single-model registry presets — so it has no entry in
//! [`OPENAI_PROVIDER_SPECS`](super::OPENAI_PROVIDER_SPECS).
//!
//! Both V4 models natively speak the OpenAI **Responses API**
//! (`https://api.deepseek.com/v1/responses`), so this preset's channels use
//! the Responses transport. Chat-completions remains available upstream, but
//! is no longer what the preset seeds.

use muta_contracts::effort::EFFORT_LOW_HIGH_MAX;
use muta_contracts::reasoning::ReasoningSupport;
use muta_contracts::{Model, WireProtocol};

use super::{DiscoveryProtocol, ModelProviderSpec, RemoteCatalogSource};

/// The model ids the built-in `deepseek` provider serves (V4 Flash, Pro, and
/// Flash Vision over the Responses API, one key). Each id exists in the model
/// registry and floats with the upstream latest.
pub use muta_contracts::model_providers::DEEPSEEK_BUILTIN_MODELS;

/// Baseline capability metadata for the models this provider serves,
/// submitted to `muta_contracts`'s registry at link time (see
/// [`muta_contracts::model::BaselineModels`]).
pub const MODELS: &[Model] = &[
    // DeepSeek (opencode-go / direct)
    Model {
        id: "deepseek-v4-flash",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: EFFORT_LOW_HIGH_MAX,
    },
    Model {
        id: "deepseek-v4-pro",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: EFFORT_LOW_HIGH_MAX,
    },
    Model {
        id: "deepseek-v4-flash-vision-exp",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: EFFORT_LOW_HIGH_MAX,
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
        reports_misses: true,
    }
}

pub(crate) const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    prompt_cache: prompt_cache_for_model,
    id: "deepseek",
    baselines: MODELS,
    base_url: "https://api.deepseek.com/v1/responses",
    user_agent: None,
    protocol: WireProtocol::Responses,
    models: DEEPSEEK_BUILTIN_MODELS,
    catalog_source: RemoteCatalogSource::Endpoint(DiscoveryProtocol::OpenAi),
    default_client_profile: muta_contracts::ClientPreset::Native,
    client_profile_sensitive: false,
    wire_overrides: &[],
};

#[cfg(test)]
mod tests {
    use super::{DEEPSEEK_BUILTIN_MODELS, MODELS};
    use crate::openai_provider_spec;

    #[test]
    fn deepseek_is_not_a_registry_preset() {
        // DeepSeek is now a multi-model catalog entry, not a single-model registry
        // preset: neither the merged id nor the old split ids resolve here.
        assert!(openai_provider_spec("deepseek").is_none());
        assert!(openai_provider_spec("deepseek-v4-flash").is_none());
        assert!(openai_provider_spec("deepseek-v4-pro").is_none());
        // Qwen was removed from the registry and must not resolve.
        assert!(openai_provider_spec("qwen").is_none());
    }

    #[test]
    fn vision_model_is_seeded_with_image_input_support() {
        let id = "deepseek-v4-flash-vision-exp";
        assert!(DEEPSEEK_BUILTIN_MODELS.contains(&id));
        let model = MODELS.iter().find(|model| model.id == id).unwrap();
        assert!(model.vision);
        assert!(model.tool_call);
        assert_eq!(model.context_window, 1_000_000);
    }
}
