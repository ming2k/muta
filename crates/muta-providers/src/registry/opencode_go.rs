//! The `opencode-go` provider preset: the opencode.ai/zen/go relay's
//! OpenAI-compatible catalogue, served via the models.dev third-party catalog
//! (`LiveCatalog::ModelsDev`).

use muta_contracts::effort::{EFFORT_GLM_5, EFFORT_LOW_HIGH_MAX};
use muta_contracts::thinking::ThinkingSupport;
use muta_contracts::{Model, WireProtocol};

use super::{LiveCatalog, ProviderPresetSpec};

/// Curated seed models offered by the OpenCode Go preset. A fresh connection
/// activates from this list before the first models.dev fetch completes; the
/// live catalog then refreshes the served set (including relay models this
/// client has never heard of).
pub const OPENCODE_GO_MODELS: &[&str] = &["glm-5.2", "kimi-k2.7-code", "deepseek-v4-flash"];

/// Wire-format exceptions for the opencode-go relay. The relay's default route
/// is OpenAI chat-completions, but the `minimax-*` family is served over
/// Anthropic `/messages`. Declared here as data so `route_for_model` can route
/// them correctly even when the model is fitted from models.dev (whose `npm`
/// field only says the SDK family, never the relay route).
pub const WIRE_OVERRIDES: &[(&str, WireProtocol)] = &[
    ("minimax-m2.5", WireProtocol::AnthropicMessages),
    ("minimax-m2.7", WireProtocol::AnthropicMessages),
    ("minimax-m3", WireProtocol::AnthropicMessages),
];

/// Baseline capability metadata for the models this provider serves,
/// submitted to `muta_contracts`'s registry at link time (see
/// [`muta_contracts::model::BaselineModels`]).
pub const MODELS: &[Model] = &[
    // ── DeepSeek (opencode-go / direct) ────────────────────────────────────
    Model {
        id: "deepseek-v4-flash",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: EFFORT_LOW_HIGH_MAX,
    },
    Model {
        id: "deepseek-v4-flash-0731",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: EFFORT_LOW_HIGH_MAX,
    },
    Model {
        id: "deepseek-v4-pro",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: EFFORT_LOW_HIGH_MAX,
    },
    Model {
        id: "glm-5",
        family: "glm",
        context_window: 200_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "glm-5.1",
        family: "glm",
        context_window: 200_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    // ── GLM family (Zhipu / Z.AI / opencode-go) ───────────────────────────
    Model {
        id: "glm-5.2",
        family: "glm",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: EFFORT_GLM_5,
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
        id: "mimo-v2-omni",
        family: "mimo",
        context_window: 262_144,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "mimo-v2-pro",
        family: "mimo",
        context_window: 1_048_576,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    // ── MiMo (Xiaomi / opencode-go, OpenAI format) ─────────────────────────
    Model {
        id: "mimo-v2.5",
        family: "mimo",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "mimo-v2.5-pro",
        family: "mimo",
        context_window: 1_048_576,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "minimax-m2.5",
        family: "minimax",
        context_window: 204_800,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::AnthropicMessages,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_COMMON,
    },
    Model {
        id: "minimax-m2.7",
        family: "minimax",
        context_window: 204_800,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::AnthropicMessages,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_COMMON,
    },
    // ── MiniMax (opencode-go, Anthropic /messages format) ──────────────────
    Model {
        id: "minimax-m3",
        family: "minimax",
        context_window: 512_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::AnthropicMessages,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_COMMON,
    },
    Model {
        id: "qwen3.5-plus",
        family: "qwen",
        context_window: 262_144,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_COMMON,
    },
    Model {
        id: "qwen3.6-plus",
        family: "qwen",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_COMMON,
    },
    // ── Qwen (opencode-go, OpenAI /chat/completions format) ────────────────
    // models.dev records qwen3.* as `@ai-sdk/openai-compatible` under
    // opencode-go; this baseline table mirrors that so the offline
    // fallback path matches the live catalog.
    Model {
        id: "qwen3.7-max",
        family: "qwen",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_COMMON,
    },
    Model {
        id: "qwen3.7-plus",
        family: "qwen",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_COMMON,
    },
];

inventory::submit!(muta_contracts::model::BaselineModels(MODELS));

pub(crate) const PRESET_SPEC: ProviderPresetSpec = ProviderPresetSpec {
    prompt_cache: super::unsupported_prompt_cache,
    id: "opencode-go",
    baselines: MODELS,
    // Endpoints are per-model by wire format (see `route_for_model`); the
    // instance-level default is the OpenAI chat-completions surface.
    base_url: "https://opencode.ai/zen/go/v1/chat/completions",
    user_agent: None,
    protocol: WireProtocol::OpenAiChatCompletions,
    // The served set comes from the models.dev third-party catalog (the relay
    // publishes there; its own `/models` is not authoritative). Fitting is
    // enabled because models.dev is the relay's own directory — every
    // advertised id is materialized with its catalog metadata, so a newly
    // shipped relay model appears with zero client changes.
    live_catalog: Some(LiveCatalog::ModelsDev {
        provider: "opencode-go",
    }),
    fitting: true,
    wire_overrides: WIRE_OVERRIDES,
    models: OPENCODE_GO_MODELS,
};
