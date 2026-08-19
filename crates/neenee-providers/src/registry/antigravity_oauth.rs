//! The `antigravity-oauth` provider template: Google-native models served
//! via Google Antigravity OAuth subscription.

use neenee_contracts::effort::{EFFORT_GEMINI_BUDGET, EFFORT_GEMINI_LEVEL};
use neenee_contracts::thinking::ThinkingSupport;
use neenee_contracts::{Model, WireFormat};

use super::ProviderTemplateSpec;

/// Models served by Google Antigravity OAuth (Google One AI Premium / Pro).
pub const ANTIGRAVITY_OAUTH_MODELS: &[&str] = &[
    "gemini-3.7-flash",
    "gemini-3.1-pro-high",
    "gemini-3.1-pro-low",
    "gemini-3-flash",
    "gemini-3.5-flash",
    "gemini-3-pro-preview",
    "gemini-3-flash-preview",
    "gemini-3.1-pro-preview",
    "gemini-2.5-flash",
    "gemini-2.5-pro",
];

/// Baseline capability metadata for the models this provider serves.
pub const MODELS: &[Model] = &[
    Model {
        id: "gemini-3.7-flash",
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
        id: "gemini-3.1-pro-high",
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
        id: "gemini-3.5-flash",
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
        family: "google",
        context_window: 1_000_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        format: WireFormat::Google,
        model_guidance: "",
        effort_levels: EFFORT_GEMINI_BUDGET,
    },
];

inventory::submit!(neenee_contracts::model::BaselineModels(MODELS));

pub(crate) const TEMPLATE_SPEC: ProviderTemplateSpec = ProviderTemplateSpec {
    id: "antigravity-oauth",
    baselines: MODELS,
    protocol: "google",
    discovery: false,
    fitting: false,
    models: ANTIGRAVITY_OAUTH_MODELS,
};
