//! The built-in `deepseek` provider template: DeepSeek V4 Flash + Pro over
//! the OpenAI-compatible API, one key (`DEEPSEEK_API_KEY`).
//!
//! DeepSeek V4 (Flash + Pro) is served as one multi-model `deepseek` provider
//! built in the catalog layer (both models share one `DEEPSEEK_API_KEY`), not
//! as two single-model registry presets — so it has no entry in
//! [`OPENAI_PROVIDER_SPECS`](super::OPENAI_PROVIDER_SPECS).

use neenee_core::thinking::ThinkingSupport;
use neenee_core::{Model, WireFormat};

use super::ProviderTemplateSpec;

/// The model ids the built-in `deepseek` provider serves (V4 Flash + Pro over
/// the OpenAI-compatible API, one key). Each id exists in the model registry.
pub const DEEPSEEK_BUILTIN_MODELS: &[&str] = &[
    "deepseek-v4-flash",
    "deepseek-v4-flash-0731",
    "deepseek-v4-pro",
];

/// Baseline capability metadata for the models this provider serves,
/// submitted to `neenee_core`'s registry at link time (see
/// [`neenee_core::model::BaselineModels`]).
pub const MODELS: &[Model] = &[
    // ── DeepSeek (opencode-go / direct) ────────────────────────────────────
    Model {
        id: "deepseek-v4-flash",
        name: "DeepSeek V4 Flash",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "deepseek-v4-flash-0731",
        name: "DeepSeek V4 Flash (0731)",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "deepseek-v4-pro",
        name: "DeepSeek V4 Pro",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: &[],
    },
];

inventory::submit!(neenee_core::model::BaselineModels(MODELS));

pub(crate) const TEMPLATE_SPEC: ProviderTemplateSpec = ProviderTemplateSpec {
    id: "deepseek",
    baselines: MODELS,
    protocol: "openai",
    models: DEEPSEEK_BUILTIN_MODELS,
    discovery: true,
    fitting: false,
};

#[cfg(test)]
mod tests {
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
}
