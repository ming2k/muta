//! The built-in `google` provider preset: the native Google API, one key.

use muta_contracts::effort::{EFFORT_GEMINI_BUDGET, EFFORT_GEMINI_LEVEL};
use muta_contracts::thinking::ThinkingSupport;
use muta_contracts::{Model, WireProtocol};

use super::{DiscoveryProtocol, LiveCatalog, ProviderPresetSpec};

/// The Gemini model ids the built-in `google` provider serves (native Google
/// API, one key). Each id exists in the model registry. The set is the
/// canonical text-generation family that Google plus common relays/中转站
/// advertise — image/embedding/video/audio-only models are excluded since an
/// agent only consumes the `generateContent` text surface.
pub const GOOGLE_BUILTIN_MODELS: &[&str] = &[
    // ── Gemini 3.x ──
    "gemini-3.8-flash",
    "gemini-3.7-flash",
    "gemini-3.5-flash",
    "gemini-3-pro-preview",
    "gemini-3-flash-preview",
    "gemini-3.1-pro-preview",
    "gemini-3.1-pro-preview-customtools",
    // ── Gemini 2.5 ──
    "gemini-2.5-flash",
    "gemini-2.5-pro",
    "gemini-2.5-flash-lite",
    // ── Gemini 2.0 (still widely served by relays) ──
    "gemini-2.0-flash",
];

/// Baseline capability metadata for the models this provider serves,
/// submitted to `muta_contracts`'s registry at link time (see
/// [`muta_contracts::model::BaselineModels`]).
pub const MODELS: &[Model] = &[
    // ── Google (native) ────────────────────────────────────────────────────
    // Native Google REST surface (`generateContent`/`streamGenerateContent`).
    // The id strings mirror Google's official naming and the ids relay/中转站
    // gateways advertise — so a relay-served model resolves to real metadata
    // instead of a generic fallback. See ADR for the configurable
    // `google_base_url`.
    Model {
        id: "gemini-3.8-flash",
        family: "google",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGenerateContent,
        model_guidance: "",
        effort_levels: EFFORT_GEMINI_LEVEL,
    },
    Model {
        id: "gemini-3.7-flash",
        family: "google",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGenerateContent,
        model_guidance: "",
        effort_levels: EFFORT_GEMINI_LEVEL,
    },
    Model {
        id: "gemini-3.5-flash",
        family: "google",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGenerateContent,
        model_guidance: "",
        effort_levels: EFFORT_GEMINI_LEVEL,
    },
    Model {
        id: "gemini-3-pro-preview",
        family: "google",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGenerateContent,
        model_guidance: "",
        effort_levels: EFFORT_GEMINI_LEVEL,
    },
    Model {
        id: "gemini-3-flash-preview",
        family: "google",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGenerateContent,
        model_guidance: "",
        effort_levels: EFFORT_GEMINI_LEVEL,
    },
    Model {
        id: "gemini-3.1-pro-preview",
        family: "google",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGenerateContent,
        model_guidance: "",
        effort_levels: EFFORT_GEMINI_LEVEL,
    },
    Model {
        // Custom-tools variant of 3.1 Pro Preview; serves the same REST surface.
        id: "gemini-3.1-pro-preview-customtools",
        family: "google",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGenerateContent,
        model_guidance: "",
        effort_levels: EFFORT_GEMINI_LEVEL,
    },
    Model {
        id: "gemini-2.5-flash",
        family: "google",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGenerateContent,
        model_guidance: "",
        effort_levels: EFFORT_GEMINI_BUDGET,
    },
    Model {
        id: "gemini-2.5-pro",
        family: "google",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGenerateContent,
        model_guidance: "",
        effort_levels: EFFORT_GEMINI_BUDGET,
    },
    Model {
        id: "gemini-2.5-flash-lite",
        family: "google",
        context_window: 1_000_000,
        thinking: ThinkingSupport::None,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGenerateContent,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "gemini-2.0-flash",
        family: "google",
        context_window: 1_000_000,
        thinking: ThinkingSupport::None,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::GoogleGenerateContent,
        model_guidance: "",
        effort_levels: &[],
    },
];

inventory::submit!(muta_contracts::model::BaselineModels(MODELS));

fn prompt_cache_for_model(_: &str) -> muta_contracts::PromptCacheSpec {
    muta_contracts::PromptCacheSpec {
        modes: &[muta_contracts::PromptCacheMode::Implicit],
        default_mode: Some(muta_contracts::PromptCacheMode::Implicit),
        supported_retentions: &[],
        default_retention: None,
        disable_supported: false,
        routing_key_supported: false,
        max_breakpoints: None,
        min_cacheable_tokens: None,
        reports_reads: true,
        reports_writes: false,
        reports_misses: false,
    }
}

pub(crate) const PRESET_SPEC: ProviderPresetSpec = ProviderPresetSpec {
    prompt_cache: prompt_cache_for_model,
    id: "google",
    baselines: MODELS,
    base_url: "https://generativelanguage.googleapis.com/v1beta",
    user_agent: None,
    protocol: WireProtocol::GoogleGenerateContent,
    models: GOOGLE_BUILTIN_MODELS,
    live_catalog: Some(LiveCatalog::ProviderEndpoint(DiscoveryProtocol::Google)),
    fitting: false,
    wire_overrides: &[],
};
