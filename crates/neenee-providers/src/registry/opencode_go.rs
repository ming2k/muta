//! The `opencode-go` provider template: the opencode.ai/zen/go relay's
//! curated OpenAI-compatible catalogue.

use neenee_core::effort::{EFFORT_GLM_5, EFFORT_LOW_HIGH_MAX};
use neenee_core::thinking::ThinkingSupport;
use neenee_core::{Model, WireFormat};

use super::ProviderTemplateSpec;

/// Curated OpenAI-compatible models offered by the OpenCode Go template.
pub const OPENCODE_GO_MODELS: &[&str] = &["glm-5.2", "kimi-k2.7-code", "deepseek-v4-flash"];

/// The full catalogue the opencode-go relay (opencode.ai/zen/go) actually
/// serves — mirrors the opencode-go entries on models.dev, the same source
/// `ANTHROPIC_MODEL_MAX_TOKENS` follows. The legacy-config migration seeds
/// one channel per entry it knows (intersected with the client model
/// registry, which supplies each model's wire format and metadata).
///
/// Keeping this as an explicit allowlist — rather than deriving the seed from
/// registry families — is deliberate: a newly registered model must NOT
/// appear on the relay until the relay advertises it, otherwise users get a
/// channel that only ever answers "model not found". (Kimi `k3` and `glm-4.7`
/// are registered for other providers but unserved by go, for example.)
pub const OPENCODE_GO_SERVED_MODELS: &[&str] = &[
    "deepseek-v4-flash",
    "deepseek-v4-flash-0731",
    "deepseek-v4-pro",
    "glm-5",
    "glm-5.1",
    "glm-5.2",
    "kimi-k2.5",
    "kimi-k2.6",
    "kimi-k2.7-code",
    "mimo-v2-omni",
    "mimo-v2-pro",
    "mimo-v2.5",
    "mimo-v2.5-pro",
    "minimax-m2.5",
    "minimax-m2.7",
    "minimax-m3",
    "qwen3.5-plus",
    "qwen3.6-plus",
    "qwen3.7-max",
    "qwen3.7-plus",
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
        effort_levels: EFFORT_GLM_5,
    },
    Model {
        id: "kimi-k2.5",
        name: "Kimi K2.5",
        family: "kimi",
        context_window: 262_144,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "kimi-k2.6",
        name: "Kimi K2.6",
        family: "kimi",
        context_window: 262_144,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "kimi-k2.7-code",
        name: "Kimi K2.7 Code",
        family: "kimi",
        context_window: 262_144,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "mimo-v2-omni",
        name: "MiMo V2 Omni",
        family: "mimo",
        context_window: 262_144,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "mimo-v2-pro",
        name: "MiMo V2 Pro",
        family: "mimo",
        context_window: 1_048_576,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: &[],
    },
    // ── MiMo (Xiaomi / opencode-go, OpenAI format) ─────────────────────────
    Model {
        id: "mimo-v2.5",
        name: "MiMo V2.5",
        family: "mimo",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "mimo-v2.5-pro",
        name: "MiMo V2.5 Pro",
        family: "mimo",
        context_window: 1_048_576,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "minimax-m2.5",
        name: "MiniMax M2.5",
        family: "minimax",
        context_window: 204_800,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        format: WireFormat::AnthropicCompat,
        model_guidance: "",
        effort_levels: neenee_core::effort::EFFORT_COMMON,
    },
    Model {
        id: "minimax-m2.7",
        name: "MiniMax M2.7",
        family: "minimax",
        context_window: 204_800,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        format: WireFormat::AnthropicCompat,
        model_guidance: "",
        effort_levels: neenee_core::effort::EFFORT_COMMON,
    },
    // ── MiniMax (opencode-go, Anthropic /messages format) ──────────────────
    Model {
        id: "minimax-m3",
        name: "MiniMax M3",
        family: "minimax",
        context_window: 512_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        format: WireFormat::AnthropicCompat,
        model_guidance: "",
        effort_levels: neenee_core::effort::EFFORT_COMMON,
    },
    Model {
        id: "qwen3.5-plus",
        name: "Qwen3.5 Plus",
        family: "qwen",
        context_window: 262_144,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: neenee_core::effort::EFFORT_COMMON,
    },
    Model {
        id: "qwen3.6-plus",
        name: "Qwen3.6 Plus",
        family: "qwen",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: neenee_core::effort::EFFORT_COMMON,
    },
    // ── Qwen (opencode-go, OpenAI /chat/completions format) ────────────────
    // models.dev records qwen3.* as `@ai-sdk/openai-compatible` under
    // opencode-go; this baseline table mirrors that so the offline
    // fallback path matches the live catalog.
    Model {
        id: "qwen3.7-max",
        name: "Qwen3.7 Max",
        family: "qwen",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: neenee_core::effort::EFFORT_COMMON,
    },
    Model {
        id: "qwen3.7-plus",
        name: "Qwen3.7 Plus",
        family: "qwen",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: neenee_core::effort::EFFORT_COMMON,
    },
];

inventory::submit!(neenee_core::model::BaselineModels(MODELS));

pub(crate) const TEMPLATE_SPEC: ProviderTemplateSpec = ProviderTemplateSpec {
    id: "opencode-go",
    baselines: MODELS,
    protocol: "openai",
    // opencode-go's model list is derived at runtime from the baseline
    // registry and spans multiple transports; a live overwrite would regress it.
    discovery: false,
    fitting: false,
    models: OPENCODE_GO_MODELS,
};
