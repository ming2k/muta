//! The `chatgpt-oauth` provider preset: GPT-5.x over the ChatGPT
//! subscription backend (the Codex Responses API).

use muta_contracts::thinking::ThinkingSupport;
use muta_contracts::{Model, WireFormat};

use super::ProviderPresetSpec;

/// GPT-5.x models served over the ChatGPT subscription backend (the Codex
/// Responses API). These are the models a ChatGPT Pro/PLUS plan unlocks; the
/// Responses transport routes them to `chatgpt.com/backend-api/codex/responses`.
/// Each id exists in the model registry.
pub const CHATGPT_BUILTIN_MODELS: &[&str] = &[
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-5.6-luna",
    "gpt-5.5",
    "gpt-5.3-codex-spark",
    "gpt-5.4",
    "gpt-5.4-mini",
];

/// Baseline capability metadata for the models this provider serves,
/// submitted to `muta_contracts`'s registry at link time (see
/// [`muta_contracts::model::BaselineModels`]).
pub const MODELS: &[Model] = &[
    Model {
        id: "gpt-5.6-sol",
        family: "gpt",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_OPENAI_GPT_5_6,
    },
    Model {
        id: "gpt-5.6-terra",
        family: "gpt",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_OPENAI_GPT_5_6,
    },
    Model {
        id: "gpt-5.6-luna",
        family: "gpt",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_OPENAI_GPT_5_6,
    },
    // ── GPT (OpenAI) ───────────────────────────────────────────────────────
    // The current frontier chat family served over the OpenAI chat-completions
    // API. All reason (surfaced via the `reasoning_content` stream) and take
    // text+image input. Context windows and pricing per OpenAI's model docs;
    // `gpt-5.5`/`gpt-5.4` share a 1M window, `gpt-5.4-mini` a 400K window.
    Model {
        id: "gpt-5.5",
        family: "gpt",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_OPENAI_GPT,
    },
    Model {
        id: "gpt-5.3-codex-spark",
        family: "gpt",
        context_window: 128_000,
        thinking: ThinkingSupport::ReasoningSummary,
        tool_call: true,
        vision: false,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_OPENAI_GPT,
    },
    Model {
        id: "gpt-5.4",
        family: "gpt",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_OPENAI_GPT,
    },
    Model {
        id: "gpt-5.4-mini",
        family: "gpt",
        context_window: 400_000,
        thinking: ThinkingSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_OPENAI_GPT,
    },
];

inventory::submit!(muta_contracts::model::BaselineModels(MODELS));

pub(crate) const PRESET_SPEC: ProviderPresetSpec = ProviderPresetSpec {
    id: "chatgpt-oauth",
    baselines: MODELS,
    base_url: "https://chatgpt.com/backend-api/codex/responses",
    user_agent: None,
    // The Responses transport is the OpenAI wire family. Discovery uses the
    // subscription-only `/backend-api/codex/models` catalog rather than the
    // public OpenAI `{data:[...]}` shape; the remote catalog is authoritative
    // for each account and its capability metadata is trusted.
    protocol: "openai",
    models: CHATGPT_BUILTIN_MODELS,
    discovery: true,
    fitting: true,
};
