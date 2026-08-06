//! The `chatgpt-oauth` provider template: GPT-5.x over the ChatGPT
//! subscription backend (the Codex Responses API).

use neenee_core::thinking::ThinkingSupport;
use neenee_core::{Model, WireFormat};

use super::ProviderTemplateSpec;

/// GPT-5.x models served over the ChatGPT subscription backend (the Codex
/// Responses API). These are the models a ChatGPT Pro/PLUS plan unlocks; the
/// Responses transport routes them to `chatgpt.com/backend-api/codex/responses`.
/// Each id exists in the model registry.
pub const CHATGPT_BUILTIN_MODELS: &[&str] = &[
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-5.6-luna",
    "gpt-5.5",
    "gpt-5.4",
    "gpt-5.4-mini",
];

/// Baseline capability metadata for the models this provider serves,
/// submitted to `neenee_core`'s registry at link time (see
/// [`neenee_core::model::BaselineModels`]).
pub const MODELS: &[Model] = &[
    Model {
        id: "gpt-5.6-sol",
        name: "GPT-5.6 Sol",
        family: "gpt",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: neenee_core::effort::EFFORT_OPENAI_GPT_5_6,
    },
    Model {
        id: "gpt-5.6-terra",
        name: "GPT-5.6 Terra",
        family: "gpt",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: neenee_core::effort::EFFORT_OPENAI_GPT_5_6,
    },
    Model {
        id: "gpt-5.6-luna",
        name: "GPT-5.6 Luna",
        family: "gpt",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: neenee_core::effort::EFFORT_OPENAI_GPT_5_6,
    },
    // ── GPT (OpenAI) ───────────────────────────────────────────────────────
    // The current frontier chat family served over the OpenAI chat-completions
    // API. All reason (surfaced via the `reasoning_content` stream) and take
    // text+image input. Context windows and pricing per OpenAI's model docs;
    // `gpt-5.5`/`gpt-5.4` share a 1M window, `gpt-5.4-mini` a 400K window.
    Model {
        id: "gpt-5.5",
        name: "GPT-5.5",
        family: "gpt",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: neenee_core::effort::EFFORT_OPENAI_GPT,
    },
    Model {
        id: "gpt-5.4",
        name: "GPT-5.4",
        family: "gpt",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: neenee_core::effort::EFFORT_OPENAI_GPT,
    },
    Model {
        id: "gpt-5.4-mini",
        name: "GPT-5.4 Mini",
        family: "gpt",
        context_window: 400_000,
        thinking: ThinkingSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: neenee_core::effort::EFFORT_OPENAI_GPT,
    },
];

inventory::submit!(neenee_core::model::BaselineModels(MODELS));

pub(crate) const TEMPLATE_SPEC: ProviderTemplateSpec = ProviderTemplateSpec {
    id: "chatgpt-oauth",
    baselines: MODELS,
    // The Responses transport is the OpenAI wire family; discovery is
    // disabled because the ChatGPT subscription backend does not expose a
    // standard `GET /models` list, and the plan-unlocked set is fixed.
    protocol: "openai",
    models: CHATGPT_BUILTIN_MODELS,
    discovery: false,
    fitting: false,
};
