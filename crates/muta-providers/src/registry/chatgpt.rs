//! The `chatgpt-oauth` provider preset: GPT-5.x over the ChatGPT
//! Subscription backend (the Codex Responses API).

use muta_contracts::reasoning::ReasoningSupport;
use muta_contracts::{Model, WireProtocol};

use super::{DiscoveryProtocol, ModelProviderSpec, RemoteCatalogSource};

/// Empty seed: the ChatGPT Subscription backend's model set is fully
/// discovery-derived from the account's live Codex catalog
/// (`/backend-api/codex/models`). The static seed never guesses
/// plan-specific access — the entitlement-aware endpoint is the single source
/// of truth, and the picker is intentionally empty until that first fetch
/// completes. Baseline capability metadata for ids the catalog returns still
/// resolves through the model registry (`MODELS` below).
pub use muta_contracts::model_providers::CHATGPT_BUILTIN_MODELS;

/// Baseline capability metadata for the models this provider serves,
/// submitted to `muta_contracts`'s registry at link time (see
/// [`muta_contracts::model::BaselineModels`]).
pub const MODELS: &[Model] = &[
    Model {
        id: "gpt-6-astra",
        family: "gpt",
        context_window: 872_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_OPENAI_GPT_6,
    },
    Model {
        id: "gpt-5.6-sol",
        family: "gpt",
        context_window: 1_050_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_OPENAI_GPT_5_6,
    },
    Model {
        id: "gpt-5.6-terra",
        family: "gpt",
        context_window: 1_050_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_OPENAI_GPT_5_6,
    },
    Model {
        id: "gpt-5.6-luna",
        family: "gpt",
        context_window: 1_050_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_OPENAI_GPT_5_6,
    },
    // Non-seeded models retained solely as metadata for ids returned by the
    // account-specific live Codex catalog.
    Model {
        id: "gpt-5.5",
        family: "gpt",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_OPENAI_GPT,
    },
    Model {
        id: "gpt-5.3-codex-spark",
        family: "gpt",
        context_window: 128_000,
        thinking: ReasoningSupport::ReasoningSummary,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_OPENAI_GPT,
    },
];

inventory::submit!(muta_contracts::model::BaselineModels(MODELS));

// The ChatGPT subscription endpoint is Responses-shaped, but it is not the
// public OpenAI Responses API. It accepts Codex's stable `prompt_cache_key`
// while rejecting the GPT-5.6 platform-only `prompt_cache_options` control.
// Keep this route implicit and retention-neutral so the shared encoder emits
// only the affinity key.
const CHATGPT_IMPLICIT_CACHE: muta_contracts::PromptCacheSpec = muta_contracts::PromptCacheSpec {
    modes: &[muta_contracts::PromptCacheMode::Implicit],
    default_mode: Some(muta_contracts::PromptCacheMode::Implicit),
    supported_retentions: &[],
    default_retention: None,
    disable_supported: false,
    routing_key_supported: true,
    max_breakpoints: None,
    min_cacheable_tokens: None,
    reports_reads: true,
    reports_writes: true,
    reports_misses: false,
};

const fn prompt_cache_for_model(_: &str) -> muta_contracts::PromptCacheSpec {
    CHATGPT_IMPLICIT_CACHE
}

pub(crate) const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    prompt_cache: prompt_cache_for_model,
    id: "openai-subscription",
    baselines: MODELS,
    base_url: "https://chatgpt.com/backend-api/codex/responses",
    user_agent: Some(muta_contracts::client_identity::CODEX_USER_AGENT),
    // The Responses transport is the OpenAI wire family. Discovery uses the
    // subscription-only `/backend-api/codex/models` catalog rather than the
    // public OpenAI `{data:[...]}` shape; the remote catalog is authoritative
    // for each account and its capability metadata is trusted.
    protocol: WireProtocol::Responses,
    models: CHATGPT_BUILTIN_MODELS,
    catalog_source: RemoteCatalogSource::Endpoint(DiscoveryProtocol::Codex),
    default_client_profile: muta_contracts::ClientPreset::Codex,
    client_profile_sensitive: true,
    wire_overrides: &[],
};

#[cfg(test)]
mod tests {
    use super::*;
    use muta_contracts::PromptCacheMode;

    #[test]
    fn seed_is_empty_and_uses_responses() {
        // The picker seed is intentionally empty: Codex /backend-api/codex/models
        // (the entitlement-aware endpoint) is authoritative, so nothing is
        // hardcoded — see the module doc for why.
        assert_eq!(CHATGPT_BUILTIN_MODELS, &[] as &[&str]);
        assert_eq!(MODEL_PROVIDER_SPEC.protocol, WireProtocol::Responses);
    }

    #[test]
    fn chatgpt_uses_affinity_without_platform_cache_options() {
        let capabilities = (MODEL_PROVIDER_SPEC.prompt_cache)("gpt-5.6-sol").materialize();
        assert_eq!(capabilities.modes, vec![PromptCacheMode::Implicit]);
        assert_eq!(capabilities.default_mode, Some(PromptCacheMode::Implicit));
        assert!(capabilities.supported_retentions.is_empty());
        assert_eq!(capabilities.default_retention, None);
        assert!(capabilities.routing_key_supported);
    }
}
