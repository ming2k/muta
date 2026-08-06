//! The `zai-code` provider template and its legacy registry preset: Z.AI
//! (Zhipu) coding-plan platform (`api.z.ai/api/coding/paas/v4`).

use neenee_core::thinking::ThinkingSupport;
use neenee_core::{Model, WireFormat};

use super::{OpenAiProviderSpec, ProviderTemplateSpec};

/// Models served by Z.AI's coding-plan endpoint.
pub const ZAI_CODE_MODELS: &[&str] = &["glm-5.2"];

// ZAI Code — Z.AI (Zhipu) coding-plan platform
// (api.z.ai/api/coding/paas/v4). A coding-agent membership endpoint that
// serves the GLM-5 family; glm-5.2 is the current flagship. Like the Kimi
// Code platform, it expects a recognized coding-agent User-Agent. Shares
// the ZHIPU_API_KEY legacy name for key compatibility with the broader
// Zhipu ecosystem, while ZAI_API_KEY is the preferred alias.
pub(crate) const PROVIDER_SPEC: OpenAiProviderSpec = OpenAiProviderSpec {
    id: "zai-code",
    base_url: "https://api.z.ai/api/coding/paas/v4/chat/completions",
    default_model: "glm-5.2",
    env_api_key: "ZAI_API_KEY",
    env_model: "ZAI_MODEL",
    fixed_model: None,
    default_user_agent: Some("opencode/1.17.10"),
};

/// Baseline capability metadata for the models this provider serves,
/// submitted to `neenee_core`'s registry at link time (see
/// [`neenee_core::model::BaselineModels`]).
pub const MODELS: &[Model] = &[
    // ── GLM family (Zhipu / Z.AI / opencode-go) ───────────────────────────
    Model {
        id: "glm-5.2",
        name: "GLM-5.2",
        family: "glm",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "glm-5.1",
        name: "GLM-5.1",
        family: "glm",
        context_window: 200_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "glm-5",
        name: "GLM-5",
        family: "glm",
        context_window: 200_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "glm-4.7",
        name: "GLM-4.7",
        family: "glm",
        context_window: 200_000,
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
    id: "zai-code",
    baselines: MODELS,
    protocol: "openai",
    // Same rationale as kimi-code: a single pinned membership-platform model.
    discovery: false,
    fitting: false,
    models: ZAI_CODE_MODELS,
};

#[cfg(test)]
mod tests {
    use crate::openai_provider_spec;

    #[test]
    fn openai_compat_spec_resolves_model_override_and_default() {
        let spec = openai_provider_spec("zai-code").expect("zai-code spec");
        assert_eq!(spec.resolve_model(None), "glm-5.2");
        assert_eq!(spec.resolve_model(Some("glm-5.1".to_string())), "glm-5.1");
    }
}
