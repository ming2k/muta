//! The built-in `deepseek` provider template: DeepSeek V4 Flash + Pro over
//! the official Responses API, one key (`DEEPSEEK_API_KEY`).
//!
//! DeepSeek V4 (Flash + Pro) is served as one multi-model `deepseek` provider
//! built in the catalog layer (both models share one `DEEPSEEK_API_KEY`), not
//! as two single-model registry presets — so it has no entry in
//! [`OPENAI_PROVIDER_SPECS`](super::OPENAI_PROVIDER_SPECS).
//!
//! Both V4 models natively speak the OpenAI **Responses API**
//! (`https://api.deepseek.com/v1/responses`): Flash gained it with the 0731 GA
//! (adapted for Codex), Pro with the 0813 release — so this template's
//! channels use the Responses transport. Chat-completions remains available
//! upstream, but is no longer what the template seeds.

use neenee_contracts::effort::EFFORT_LOW_HIGH_MAX;
use neenee_contracts::thinking::ThinkingSupport;
use neenee_contracts::{Model, WireFormat};

use super::ProviderTemplateSpec;

/// The model ids the built-in `deepseek` provider serves (V4 Flash + Pro over
/// the Responses API, one key). Each id exists in the model registry. The
/// dated ids pin a snapshot (`-0731` / `-0813`); the bare ids float with the
/// upstream latest.
pub const DEEPSEEK_BUILTIN_MODELS: &[&str] = &[
    "deepseek-v4-flash",
    "deepseek-v4-flash-0731",
    "deepseek-v4-pro",
    "deepseek-v4-pro-0813",
];

/// Baseline capability metadata for the models this provider serves,
/// submitted to `neenee_contracts`'s registry at link time (see
/// [`neenee_contracts::model::BaselineModels`]).
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
        effort_levels: EFFORT_LOW_HIGH_MAX,
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
        effort_levels: EFFORT_LOW_HIGH_MAX,
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
        effort_levels: EFFORT_LOW_HIGH_MAX,
    },
    Model {
        id: "deepseek-v4-pro-0813",
        name: "DeepSeek V4 Pro (0813)",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: EFFORT_LOW_HIGH_MAX,
    },
];

inventory::submit!(neenee_contracts::model::BaselineModels(MODELS));

pub(crate) const TEMPLATE_SPEC: ProviderTemplateSpec = ProviderTemplateSpec {
    id: "deepseek",
    baselines: MODELS,
    protocol: "openai-responses",
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
