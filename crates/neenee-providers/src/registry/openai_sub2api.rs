//! The `openai-sub2api` provider template: OpenAI-compatible sub2api relays.

use neenee_contracts::thinking::ThinkingSupport;
use neenee_contracts::{Model, WireFormat};

use super::ProviderTemplateSpec;

/// Text/chat models commonly served by OpenAI-compatible sub2api relays.
///
/// Keep stable aliases first. Dated snapshots and image/audio/realtime models
/// are intentionally omitted; callers can still add a relay-specific model id.
pub const OPENAI_SUB2API_MODELS: &[&str] = &[
    // GPT-5.6 family (Sol/Terra/Luna) — OpenAI's tier-named flagship line.
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-5.6-luna",
    "gpt-5.5",
    "gpt-5.4",
    "gpt-5.4-mini",
    "gpt-5.3-codex-spark",
    "gpt-5.2",
    "gpt-5.2-chat-latest",
    "gpt-5.2-pro",
];

/// Baseline capability metadata for the models this provider serves,
/// submitted to `neenee_contracts`'s registry at link time (see
/// [`neenee_contracts::model::BaselineModels`]).
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
        effort_levels: neenee_contracts::effort::EFFORT_OPENAI_GPT_5_6,
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
        effort_levels: neenee_contracts::effort::EFFORT_OPENAI_GPT_5_6,
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
        effort_levels: neenee_contracts::effort::EFFORT_OPENAI_GPT_5_6,
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
        effort_levels: neenee_contracts::effort::EFFORT_OPENAI_GPT,
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
        effort_levels: neenee_contracts::effort::EFFORT_OPENAI_GPT,
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
        effort_levels: neenee_contracts::effort::EFFORT_OPENAI_GPT,
    },
    // OpenAI sub2api relays can expose additional text aliases not used by the
    // official built-in template. Keep their metadata conservative when the
    // exact serving contract is relay-defined.
    Model {
        id: "gpt-5.3-codex-spark",
        name: "GPT-5.3 Codex Spark",
        family: "gpt",
        context_window: 0,
        thinking: ThinkingSupport::ReasoningSummary,
        tool_call: true,
        vision: false,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: neenee_contracts::effort::EFFORT_OPENAI_GPT,
    },
    Model {
        id: "gpt-5.2",
        name: "GPT-5.2",
        family: "gpt",
        context_window: 0,
        thinking: ThinkingSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: neenee_contracts::effort::EFFORT_OPENAI_GPT,
    },
    Model {
        id: "gpt-5.2-chat-latest",
        name: "GPT-5.2 Chat Latest",
        family: "gpt",
        context_window: 0,
        thinking: ThinkingSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: neenee_contracts::effort::EFFORT_OPENAI_GPT,
    },
    Model {
        id: "gpt-5.2-pro",
        name: "GPT-5.2 Pro",
        family: "gpt",
        context_window: 0,
        thinking: ThinkingSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: neenee_contracts::effort::EFFORT_OPENAI_GPT,
    },
];

inventory::submit!(neenee_contracts::model::BaselineModels(MODELS));

pub(crate) const TEMPLATE_SPEC: ProviderTemplateSpec = ProviderTemplateSpec {
    id: "openai-sub2api",
    baselines: MODELS,
    protocol: "openai",
    discovery: true,
    fitting: false,
    models: OPENAI_SUB2API_MODELS,
};
