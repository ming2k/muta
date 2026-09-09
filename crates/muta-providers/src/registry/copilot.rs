//! The `copilot-oauth` provider preset: GitHub Copilot subscription models
//! over OpenAI-compatible chat completions against `api.githubcopilot.com`.

use muta_contracts::reasoning::ReasoningSupport;
use muta_contracts::{Model, WireProtocol};

use super::{DiscoveryProtocol, ModelProviderSpec, RemoteCatalogSource};

/// The minimal model seed for a fresh GitHub Copilot instance, before its
/// first live discovery completes. A Copilot instance uses `discovery: true`
/// and `fitting: true` (see [`COPILOT`](crate::oauth::COPILOT) / the `copilot-oauth`
/// preset), so its real channel set is populated from
/// `GET api.githubcopilot.com/models` at runtime — this seed only needs one
/// universally available id so a brand-new instance activates without a 400.
/// `gpt-4o-mini` is unlocked on every Copilot plan (incl. Free/Student).
pub use muta_contracts::model_providers::COPILOT_SEED_MODELS;

/// Baseline capability metadata for the models this provider serves,
/// submitted to `muta_contracts`'s registry at link time (see
/// [`muta_contracts::model::BaselineModels`]).
pub const MODELS: &[Model] = &[Model {
    id: "gpt-4o-mini",
    family: "gpt",
    context_window: 128_000,
    thinking: ReasoningSupport::None,
    tool_call: true,
    vision: true,
    protocol: WireProtocol::OpenAiChatCompletions,
    model_guidance: "",
    effort_levels: &[],
}];

inventory::submit!(muta_contracts::model::BaselineModels(MODELS));

pub(crate) const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    prompt_cache: super::unsupported_prompt_cache,
    id: "github-copilot",
    baselines: MODELS,
    base_url: "https://api.githubcopilot.com/chat/completions",
    user_agent: None,
    // Copilot speaks the OpenAI chat-completions wire family against
    // api.githubcopilot.com. Live catalog + fitting are enabled so the
    // instance tracks the user's actual plan-unlocked model set (which
    // varies by plan: Free/Student get only the GPT-4o chat family, Pro+
    // unlocks GPT-5) without a hardcoded model list — every advertised id
    // the client registry does not know is fitted with its advertised
    // capability metadata, mirroring the kimi-code flow.
    protocol: WireProtocol::OpenAiChatCompletions,
    catalog_source: RemoteCatalogSource::Endpoint(DiscoveryProtocol::OpenAi),
    default_client_profile: muta_contracts::ClientPreset::Copilot,
    client_profile_sensitive: true,
    wire_overrides: &[],
    // Minimal seed: the id a fresh Copilot instance activates before the
    // first live discovery completes. `gpt-4o-mini` is universally
    // available across every Copilot plan, so the seed never 400s.
    models: COPILOT_SEED_MODELS,
};
