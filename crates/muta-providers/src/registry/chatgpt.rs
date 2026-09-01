//! The `chatgpt-oauth` provider preset: GPT-5.x over the ChatGPT
//! subscription backend (the Codex Responses API).

use muta_contracts::thinking::ThinkingSupport;
use muta_contracts::{Model, WireProtocol};

use super::ProviderPresetSpec;

/// Entitlement-neutral seed for the ChatGPT subscription backend. Live Codex
/// discovery is authoritative and may add GPT-5.5 or Pro-only Spark for the
/// signed-in account; the static seed never guesses plan-specific access.
pub const CHATGPT_BUILTIN_MODELS: &[&str] = &["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"];

/// Baseline capability metadata for the models this provider serves,
/// submitted to `muta_contracts`'s registry at link time (see
/// [`muta_contracts::model::BaselineModels`]).
pub const MODELS: &[Model] = &[
    Model {
        id: "gpt-5.6-sol",
        family: "gpt",
        context_window: 1_050_000,
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
        context_window: 1_050_000,
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
        context_window: 1_050_000,
        thinking: ThinkingSupport::ReasoningSummary,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_OPENAI_GPT_5_6,
    },
    // Non-seeded models retained solely as metadata for ids returned by the
    // account-specific live Codex catalog.
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
];

inventory::submit!(muta_contracts::model::BaselineModels(MODELS));

pub(crate) const PRESET_SPEC: ProviderPresetSpec = ProviderPresetSpec {
    prompt_cache: super::openai::prompt_cache_for_model,
    id: "chatgpt-oauth",
    baselines: MODELS,
    base_url: "https://chatgpt.com/backend-api/codex/responses",
    user_agent: None,
    // The Responses transport is the OpenAI wire family. Discovery uses the
    // subscription-only `/backend-api/codex/models` catalog rather than the
    // public OpenAI `{data:[...]}` shape; the remote catalog is authoritative
    // for each account and its capability metadata is trusted.
    protocol: WireProtocol::OpenAiResponses,
    models: CHATGPT_BUILTIN_MODELS,
    discovery: true,
    fitting: true,
};

#[cfg(test)]
mod tests {
    use super::*;
    use muta_contracts::CacheRetention;

    #[test]
    fn seed_is_entitlement_neutral_and_uses_responses() {
        assert_eq!(
            CHATGPT_BUILTIN_MODELS,
            &["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"]
        );
        assert_eq!(PRESET_SPEC.protocol, WireProtocol::OpenAiResponses);
    }

    #[test]
    fn gpt_5_6_uses_codex_prompt_cache_controls() {
        let capabilities = (PRESET_SPEC.prompt_cache)("gpt-5.6-sol").materialize();
        assert_eq!(
            capabilities.default_retention,
            Some(CacheRetention::ThirtyMinutes)
        );
        assert!(capabilities.routing_key_supported);
    }
}
