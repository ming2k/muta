//! The `xai-oauth` provider template: xAI Grok over OpenAI-compatible chat
//! completions (SuperGrok OAuth or `XAI_API_KEY`).

use neenee_contracts::thinking::ThinkingSupport;
use neenee_contracts::{Model, WireFormat};

use super::ProviderTemplateSpec;

/// xAI Grok models over OpenAI-compatible chat completions (SuperGrok OAuth or
/// `XAI_API_KEY`).
pub const XAI_BUILTIN_MODELS: &[&str] = &["grok-4.5", "grok-4.20", "grok-4.3", "grok-build-0.1"];

/// Baseline capability metadata for the models this provider serves,
/// submitted to `neenee_contracts`'s registry at link time (see
/// [`neenee_contracts::model::BaselineModels`]).
pub const MODELS: &[Model] = &[
    // ── xAI Grok (OpenAI-compatible; SuperGrok OAuth or XAI_API_KEY) ──
    Model {
        id: "grok-4.5",
        family: "grok",
        context_window: 256_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: neenee_contracts::effort::EFFORT_XAI_GROK,
    },
    Model {
        id: "grok-4.20",
        family: "grok",
        context_window: 256_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: neenee_contracts::effort::EFFORT_XAI_GROK,
    },
    Model {
        id: "grok-4.3",
        family: "grok",
        context_window: 256_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: neenee_contracts::effort::EFFORT_XAI_GROK,
    },
    Model {
        id: "grok-build-0.1",
        family: "grok",
        context_window: 256_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: neenee_contracts::effort::EFFORT_XAI_GROK,
    },
];

inventory::submit!(neenee_contracts::model::BaselineModels(MODELS));

pub(crate) const TEMPLATE_SPEC: ProviderTemplateSpec = ProviderTemplateSpec {
    id: "xai-oauth",
    baselines: MODELS,
    base_url: "https://api.x.ai/v1/chat/completions",
    user_agent: None,
    protocol: "openai",
    models: XAI_BUILTIN_MODELS,
    discovery: true,
    fitting: false,
};
