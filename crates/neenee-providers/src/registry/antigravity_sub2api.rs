//! The `antigravity-sub2api` provider template: Google-native models served
//! by Antigravity sub2api relays.

use neenee_contracts::thinking::ThinkingSupport;
use neenee_contracts::{Model, WireFormat};

use super::ProviderTemplateSpec;

/// Google-native models advertised by Antigravity sub2api relays.
///
/// The order is deliberate: callers use the first model as the initial active
/// channel, while some relays reject the `-high` variant.
pub const ANTIGRAVITY_SUB2API_MODELS: &[&str] = &[
    "gemini-3-flash",
    "gemini-3.1-pro-low",
    "gemini-3.1-pro-high",
];

/// Baseline capability metadata for the models this provider serves,
/// submitted to `neenee_contracts`'s registry at link time (see
/// [`neenee_contracts::model::BaselineModels`]).
pub const MODELS: &[Model] = &[
    // ── sub2api / antigravity relay models ────────────────────────────────
    // Google-native 中转站 variants that advertise effort-tiered 3.1 Pro
    // models (`-high`/`-low`) and a non-preview `gemini-3-flash`. Same REST
    // surface (`/v1beta/models/{id}:generateContent`), so the metadata mirrors
    // the Google family; the relay forwards the model id verbatim. The wire
    // responses include `thoughtSignature`/`thoughtsTokenCount`, so these
    // reason like the rest of the 3.x family.
    Model {
        id: "gemini-3.1-pro-high",
        name: "Gemini 3.1 Pro High",
        family: "google",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        format: WireFormat::Google,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "gemini-3.1-pro-low",
        name: "Gemini 3.1 Pro Low",
        family: "google",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        format: WireFormat::Google,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "gemini-3-flash",
        name: "Gemini 3 Flash",
        family: "google",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        format: WireFormat::Google,
        model_guidance: "",
        effort_levels: &[],
    },
];

inventory::submit!(neenee_contracts::model::BaselineModels(MODELS));

pub(crate) const TEMPLATE_SPEC: ProviderTemplateSpec = ProviderTemplateSpec {
    id: "antigravity-sub2api",
    baselines: MODELS,
    protocol: "google",
    discovery: true,
    fitting: false,
    models: ANTIGRAVITY_SUB2API_MODELS,
};
