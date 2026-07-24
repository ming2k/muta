//! The `copilot-oauth` provider template: GitHub Copilot subscription models
//! over OpenAI-compatible chat completions against `api.githubcopilot.com`.

use neenee_core::thinking::ThinkingSupport;
use neenee_core::{Model, WireFormat};

use super::ProviderTemplateSpec;

/// The minimal model seed for a fresh GitHub Copilot instance, before its
/// first live discovery completes. A Copilot instance uses `discovery: true`
/// and `fitting: true` (see [`COPILOT`](crate::oauth::COPILOT) / the `copilot-oauth`
/// template), so its real channel set is populated from
/// `GET api.githubcopilot.com/models` at runtime — this seed only needs one
/// universally available id so a brand-new instance activates without a 400.
/// `gpt-4o-mini` is unlocked on every Copilot plan (incl. Free/Student).
pub const COPILOT_SEED_MODELS: &[&str] = &["gpt-4o-mini"];

/// Baseline capability metadata for the models this provider serves,
/// submitted to `neenee_core`'s registry at link time (see
/// [`neenee_core::model::BaselineModels`]).
pub const MODELS: &[Model] = &[

    Model {
        id: "gpt-4o-mini",
        name: "GPT-4o Mini",
        family: "gpt",
        context_window: 128_000,
        thinking: ThinkingSupport::None,
        tool_call: true,
        vision: true,
        format: WireFormat::OpenAi,
        model_guidance: "",
        effort_levels: &[],
    },
];

inventory::submit!(neenee_core::model::BaselineModels(MODELS));

pub(crate) const TEMPLATE_SPEC: ProviderTemplateSpec = ProviderTemplateSpec {
    id: "copilot-oauth",
    baselines: MODELS,
    // Copilot speaks the OpenAI chat-completions wire family against
    // api.githubcopilot.com. Discovery + fitting are enabled so the
    // instance tracks the user's actual plan-unlocked model set (which
    // varies by plan: Free/Student get only the GPT-4o chat family, Pro+
    // unlocks GPT-5) without a hardcoded model list — every advertised id
    // the client registry does not know is fitted with its advertised
    // capability metadata, mirroring the kimi-code flow.
    protocol: "openai",
    discovery: true,
    fitting: true,
    // Minimal seed: the id a fresh Copilot instance activates before the
    // first live discovery completes. `gpt-4o-mini` is universally
    // available across every Copilot plan, so the seed never 400s.
    models: COPILOT_SEED_MODELS,
};
