//! Baselines for case-sensitive model ids used by custom OpenAI-compatible
//! routes. Custom connections are declarations, not provider presets.

use muta_contracts::thinking::ThinkingSupport;
use muta_contracts::{Model, WireProtocol};

pub const MODELS: &[Model] = &[
    Model {
        id: "GLM-5.2",
        family: "glm",
        context_window: 200_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_GLM_5,
    },
    Model {
        id: "Deepseek-v4-flash",
        family: "deepseek",
        context_window: 200_000,
        thinking: ThinkingSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::OpenAiChatCompletions,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_LOW_HIGH_MAX,
    },
];

inventory::submit!(muta_contracts::model::BaselineModels(MODELS));

#[cfg(test)]
mod tests {
    #[test]
    fn cased_third_party_ids_remain_exact() {
        let glm = muta_contracts::model::resolve("GLM-5.2");
        let lowercase = muta_contracts::model::resolve("glm-5.2");
        assert_eq!(glm.context_window, 200_000);
        assert_eq!(lowercase.context_window, 1_000_000);
    }
}
