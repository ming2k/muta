//! The `zai-code` provider template and its legacy registry preset: Z.AI /
//! Zhipu BigModel coding-plan platform (`open.bigmodel.cn/api/coding/paas/v4`).

use muta_contracts::effort::EFFORT_GLM_5;
use muta_contracts::thinking::ThinkingSupport;
use muta_contracts::{Model, WireFormat};

use super::{OpenAiProviderSpec, ProviderPresetSpec};

/// Models served by Z.AI's coding-plan endpoint, in display/activation
/// order — the first entry is the initial active channel. `glm-5.3-flash`
/// joined the plan alongside the flagship (native multimodal, 1M context,
/// ~1/3 the credit burn), so it is offered ahead of the older flagships.
pub const ZAI_CODE_MODELS: &[&str] = &["glm-5.3", "glm-5.3-flash", "glm-5.2"];

// ZAI Code (CN) — Zhipu BigModel / Z.AI coding-plan platform
// (open.bigmodel.cn/api/coding/paas/v4). A coding-agent membership endpoint
// that serves the GLM-5 family; glm-5.3 is the current flagship. Like the Kimi
// Code platform, it expects a recognized coding-agent User-Agent. Shares
// the ZHIPU_API_KEY legacy name for key compatibility with the broader
// Zhipu ecosystem, while ZAI_API_KEY is the preferred alias.
pub(crate) const PROVIDER_SPEC: OpenAiProviderSpec = OpenAiProviderSpec {
    id: "zai-code",
    base_url: "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions",
    default_model: "glm-5.3",
    env_api_key: "ZAI_API_KEY",
    env_model: "ZAI_MODEL",
    fixed_model: None,
    default_user_agent: Some(muta_llm_client::ZCODE_USER_AGENT),
};

/// Baseline capability metadata for the models this provider serves,
/// submitted to `muta_contracts`'s registry at link time (see
/// [`muta_contracts::model::BaselineModels`]).
pub const MODELS: &[Model] = &[
    // ── GLM family (Zhipu / Z.AI / opencode-go) ───────────────────────────
    Model {
        id: "glm-5.3",
        family: "glm",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: EFFORT_GLM_5,
    },
    Model {
        // GLM-5.3-Flash — the GLM-5 family's first natively multimodal model
        // (vision: screenshots, rendered UI, image inputs), served on the
        // coding plan at roughly a third of GLM-5.3's credit burn. Text
        // parameters are identical to GLM-5.3: 1M context, always-on thinking
        // (`thinking.type: enabled` only) with `reasoning_effort`
        // low/high/xhigh/max, streaming, and tool calls.
        id: "glm-5.3-flash",
        family: "glm",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: EFFORT_GLM_5,
    },
    Model {
        id: "glm-5.2",
        family: "glm",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: EFFORT_GLM_5,
    },
    Model {
        id: "glm-5.1",
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
        id: "glm-4.6",
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
        id: "glm-4.5",
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

inventory::submit!(muta_contracts::model::BaselineModels(MODELS));

pub(crate) const PRESET_SPEC: ProviderPresetSpec = ProviderPresetSpec {
    prompt_cache: muta_contracts::PromptCacheSpec::UNSUPPORTED,
    id: "zai-code",
    baselines: MODELS,
    base_url: "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions",
    user_agent: Some(crate::ZCODE_USER_AGENT),
    protocol: "openai",
    // Live-verified (2026-08): the coding endpoint serves GET /models and
    // returns the plan's current model ids (OpenAI list shape, ids only — no
    // capability metadata). Discovery intersects that live list against the
    // baseline table below, so models appear in the picker as Zhipu adds
    // them, while the baselines stay the single source of capability truth.
    discovery: true,
    fitting: false,
    models: ZAI_CODE_MODELS,
};

#[cfg(test)]
mod tests {
    use crate::openai_provider_spec;

    #[test]
    fn openai_compat_spec_resolves_model_override_and_default() {
        let spec = openai_provider_spec("zai-code").expect("zai-code spec");
        assert_eq!(spec.resolve_model(None), "glm-5.3");
        assert_eq!(spec.resolve_model(Some("glm-5.1".to_string())), "glm-5.1");
    }

    #[test]
    fn zai_code_uses_zcode_user_agent_and_identity() {
        let spec = openai_provider_spec("zai-code").expect("zai-code spec");
        let provider = spec.build("test-key".to_string(), None, None);
        assert_eq!(provider.endpoint.user_agent(), crate::ZCODE_USER_AGENT);
        let identity = provider.endpoint.client_identity();
        assert_eq!(identity, crate::ClientIdentity::ZCode);
        assert!(
            identity
                .headers()
                .iter()
                .any(|(k, v)| *k == "X-Title" && *v == "Z Code")
        );
        assert!(
            identity
                .headers()
                .iter()
                .any(|(k, v)| *k == "X-ZCode-Agent" && *v == "glm")
        );
    }

    #[test]
    fn flash_baseline_is_registered_multimodal() {
        // The plan now serves glm-5.3-flash (native multimodal, 1M context);
        // prove the offering list and the capability baseline stay in sync.
        let offered: Vec<&str> = crate::ZAI_CODE_MODELS.to_vec();
        assert!(offered.contains(&"glm-5.3-flash"), "flash is offered");
        let m = muta_contracts::resolve_model("glm-5.3-flash");
        assert_eq!(m.family, "glm");
        assert_eq!(m.context_window, 1_000_000);
        assert!(m.vision, "GLM-5.3-Flash is natively multimodal");
        assert!(m.tool_call);
        assert_eq!(m.effort_levels, muta_contracts::effort::EFFORT_GLM_5);
    }
}
