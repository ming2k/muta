//! The built-in `openai` provider preset: OpenAI's chat-completions API,
//! one key (`OPENAI_API_KEY`).

use muta_contracts::thinking::ThinkingSupport;
use muta_contracts::{Model, WireProtocol};

use super::ProviderPresetSpec;

/// The model ids the built-in `openai` provider serves over the OpenAI
/// chat-completions API, one key (`OPENAI_API_KEY`). Mirrors OpenAI's current
/// frontier chat lineup — the GPT-5.6 tier-named family (`gpt-5.6-sol`, the
/// flagship, leads) plus the GPT-5.x family; `gpt-5.6-sol` is the default.
/// The legacy `gpt-4o`/`gpt-4o-mini` ids stay registered for existing
/// configs but are no longer seeded for the official provider. Each id exists
/// in the model registry.
pub const OPENAI_BUILTIN_MODELS: &[&str] = &[
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-5.6-luna",
    "gpt-5.5",
    "gpt-5.4",
    "gpt-5.4-mini",
];

/// Baseline capability metadata for the models this provider serves,
/// submitted to `muta_contracts`'s registry at link time (see
/// [`muta_contracts::model::BaselineModels`]).
pub const MODELS: &[Model] = &[
    // ── GPT-5.6 (OpenAI) ───────────────────────────────────────────────────
    // The 2026-06-26 flagship family with OpenAI's tier naming scheme:
    // Sol (flagship) / Terra (balanced) / Luna (efficient, high-volume).
    // `gpt-5.6` is an alias that routes to `gpt-5.6-sol`. All speak the
    // standard OpenAI chat-completions API and reason via `reasoning_content`.
    // GPT-5.6 honors the `max` effort level, so these carry the 5.6-specific
    // effort set rather than the xhigh-capped `EFFORT_OPENAI_GPT`.
    // OpenAI has not published the context window; use the GPT-5.5-class 1M
    // window conservatively for all three tiers and the alias.
    Model {
        id: "gpt-5.6",
        family: "gpt",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_OPENAI_GPT_5_6,
    },
    Model {
        id: "gpt-5.6-sol",
        family: "gpt",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
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
        protocol: WireProtocol::OpenAiChatCompletions,
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
        protocol: WireProtocol::OpenAiChatCompletions,
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
        protocol: WireProtocol::OpenAiChatCompletions,
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
        protocol: WireProtocol::OpenAiChatCompletions,
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
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_OPENAI_GPT,
    },
    // Legacy GPT-4o family — no longer in OpenAI's frontier chat lineup (it
    // remains only behind the TTS/transcribe specialized models) but kept
    // registered so existing configs and older sessions still resolve metadata.
    Model {
        id: "gpt-4o",
        family: "gpt",
        context_window: 128_000,
        thinking: ThinkingSupport::None,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "gpt-4o-mini",
        family: "gpt",
        context_window: 128_000,
        thinking: ThinkingSupport::None,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "gpt-5.3-codex-spark",
        family: "gpt",
        context_window: 128_000,
        thinking: ThinkingSupport::ReasoningSummary,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_OPENAI_GPT,
    },
    Model {
        id: "gpt-5.2",
        family: "gpt",
        context_window: 0,
        thinking: ThinkingSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_OPENAI_GPT,
    },
    Model {
        id: "gpt-5.2-chat-latest",
        family: "gpt",
        context_window: 0,
        thinking: ThinkingSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_OPENAI_GPT,
    },
    Model {
        id: "gpt-5.2-pro",
        family: "gpt",
        context_window: 0,
        thinking: ThinkingSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_OPENAI_GPT,
    },
];

inventory::submit!(muta_contracts::model::BaselineModels(MODELS));

const OPENAI_GPT_56_CACHE: muta_contracts::PromptCacheSpec = muta_contracts::PromptCacheSpec {
    modes: &[
        muta_contracts::PromptCacheMode::Implicit,
        muta_contracts::PromptCacheMode::Explicit,
    ],
    default_mode: Some(muta_contracts::PromptCacheMode::Implicit),
    supported_retentions: &[muta_contracts::CacheRetention::ThirtyMinutes],
    default_retention: Some(muta_contracts::CacheRetention::ThirtyMinutes),
    disable_supported: false,
    routing_key_supported: true,
    max_breakpoints: Some(4),
    min_cacheable_tokens: Some(1024),
    reports_reads: true,
    reports_writes: true,
    reports_misses: false,
};

const OPENAI_24H_CACHE: muta_contracts::PromptCacheSpec = muta_contracts::PromptCacheSpec {
    modes: &[muta_contracts::PromptCacheMode::Implicit],
    default_mode: Some(muta_contracts::PromptCacheMode::Implicit),
    supported_retentions: &[muta_contracts::CacheRetention::TwentyFourHours],
    default_retention: None,
    disable_supported: false,
    routing_key_supported: true,
    max_breakpoints: None,
    min_cacheable_tokens: Some(2048),
    reports_reads: true,
    reports_writes: false,
    reports_misses: false,
};

const OPENAI_LEGACY_CACHE: muta_contracts::PromptCacheSpec = muta_contracts::PromptCacheSpec {
    modes: &[muta_contracts::PromptCacheMode::Implicit],
    default_mode: Some(muta_contracts::PromptCacheMode::Implicit),
    supported_retentions: &[
        muta_contracts::CacheRetention::InMemory,
        muta_contracts::CacheRetention::TwentyFourHours,
    ],
    default_retention: None,
    disable_supported: false,
    routing_key_supported: true,
    max_breakpoints: None,
    min_cacheable_tokens: Some(2048),
    reports_reads: true,
    reports_writes: false,
    reports_misses: false,
};

const OPENAI_IN_MEMORY_CACHE: muta_contracts::PromptCacheSpec = muta_contracts::PromptCacheSpec {
    modes: &[muta_contracts::PromptCacheMode::Implicit],
    default_mode: Some(muta_contracts::PromptCacheMode::Implicit),
    supported_retentions: &[muta_contracts::CacheRetention::InMemory],
    default_retention: None,
    disable_supported: false,
    routing_key_supported: true,
    max_breakpoints: None,
    min_cacheable_tokens: Some(2048),
    reports_reads: true,
    reports_writes: false,
    reports_misses: false,
};

fn prompt_cache_for_model(model: &str) -> muta_contracts::PromptCacheSpec {
    if model == "gpt-5.6" || model.starts_with("gpt-5.6-") {
        OPENAI_GPT_56_CACHE
    } else if model == "gpt-5.5" || model.starts_with("gpt-5.5-") {
        OPENAI_24H_CACHE
    } else if matches!(
        model,
        "gpt-5.4" | "gpt-5.4-mini" | "gpt-5.2" | "gpt-5.2-chat-latest" | "gpt-5.2-pro"
    ) {
        OPENAI_LEGACY_CACHE
    } else if matches!(model, "gpt-4o" | "gpt-4o-mini" | "gpt-5.3-codex-spark") {
        OPENAI_IN_MEMORY_CACHE
    } else {
        muta_contracts::PromptCacheSpec::UNSUPPORTED
    }
}

pub(crate) const PRESET_SPEC: ProviderPresetSpec = ProviderPresetSpec {
    prompt_cache: prompt_cache_for_model,
    id: "openai",
    baselines: MODELS,
    base_url: "https://api.openai.com/v1/chat/completions",
    user_agent: None,
    protocol: WireProtocol::OpenAiChatCompletions,
    models: OPENAI_BUILTIN_MODELS,
    discovery: true,
    fitting: false,
};
