//! The built-in `google` provider template: the native Google API, one key.

use neenee_core::effort::{EFFORT_GEMINI_BUDGET, EFFORT_GEMINI_LEVEL};
use neenee_core::thinking::ThinkingSupport;
use neenee_core::{Model, WireFormat};

use super::ProviderTemplateSpec;

/// The Gemini model ids the built-in `google` provider serves (native Google
/// API, one key). Each id exists in the model registry. The set is the
/// canonical text-generation family that Google plus common relays/中转站
/// advertise — image/embedding/video/audio-only models are excluded since an
/// agent only consumes the `generateContent` text surface.
pub const GOOGLE_BUILTIN_MODELS: &[&str] = &[
    // ── Gemini 3.x ──
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
/// submitted to `neenee_core`'s registry at link time (see
/// [`neenee_core::model::BaselineModels`]).
pub const MODELS: &[Model] = &[
    // ── Google (native) ────────────────────────────────────────────────────
    // Native Google REST surface (`generateContent`/`streamGenerateContent`).
    // The id strings mirror Google's official naming and the ids relay/中转站
    // gateways advertise — so a relay-served model resolves to real metadata
    // instead of a generic fallback. See ADR for the configurable
    // `google_base_url`.
    Model {
        id: "gemini-3.5-flash",
        name: "Gemini 3.5 Flash",
        family: "google",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        format: WireFormat::Google,
        model_guidance: "",
        effort_levels: EFFORT_GEMINI_LEVEL,
    },
    Model {
        id: "gemini-3-pro-preview",
        name: "Gemini 3 Pro Preview",
        family: "google",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        format: WireFormat::Google,
        model_guidance: "",
        effort_levels: EFFORT_GEMINI_LEVEL,
    },
    Model {
        id: "gemini-3-flash-preview",
        name: "Gemini 3 Flash Preview",
        family: "google",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        format: WireFormat::Google,
        model_guidance: "",
        effort_levels: EFFORT_GEMINI_LEVEL,
    },
    Model {
        id: "gemini-3.1-pro-preview",
        name: "Gemini 3.1 Pro Preview",
        family: "google",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        format: WireFormat::Google,
        model_guidance: "",
        effort_levels: EFFORT_GEMINI_LEVEL,
    },
    Model {
        // Custom-tools variant of 3.1 Pro Preview; serves the same REST surface.
        id: "gemini-3.1-pro-preview-customtools",
        name: "Gemini 3.1 Pro Preview (Custom Tools)",
        family: "google",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        format: WireFormat::Google,
        model_guidance: "",
        effort_levels: EFFORT_GEMINI_LEVEL,
    },
    Model {
        id: "gemini-2.5-flash",
        name: "Gemini 2.5 Flash",
        family: "google",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        format: WireFormat::Google,
        model_guidance: "",
        effort_levels: EFFORT_GEMINI_BUDGET,
    },
    Model {
        id: "gemini-2.5-pro",
        name: "Gemini 2.5 Pro",
        family: "google",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        format: WireFormat::Google,
        model_guidance: "",
        effort_levels: EFFORT_GEMINI_BUDGET,
    },
    Model {
        id: "gemini-2.5-flash-lite",
        name: "Gemini 2.5 Flash-Lite",
        family: "google",
        context_window: 1_000_000,
        thinking: ThinkingSupport::None,
        tool_call: true,
        vision: true,
        format: WireFormat::Google,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "gemini-2.0-flash",
        name: "Gemini 2.0 Flash",
        family: "google",
        context_window: 1_000_000,
        thinking: ThinkingSupport::None,
        tool_call: true,
        vision: true,
        format: WireFormat::Google,
        model_guidance: "",
        effort_levels: &[],
    },
];

inventory::submit!(neenee_core::model::BaselineModels(MODELS));

pub(crate) const TEMPLATE_SPEC: ProviderTemplateSpec = ProviderTemplateSpec {
    id: "google",
    baselines: MODELS,
    protocol: "google",
    models: GOOGLE_BUILTIN_MODELS,
    discovery: true,
    fitting: false,
};
