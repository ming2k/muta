//! The `opencode-go` provider preset: the opencode.ai/zen/go relay's
//! OpenAI-compatible catalogue, served via the private models.opencode.ai catalog
//! (`DiscoveryProtocol::OpencodeGo`).

use muta_contracts::effort::{EFFORT_GLM_5, EFFORT_LOW_HIGH_MAX};
use muta_contracts::reasoning::ReasoningSupport;
use muta_contracts::{Model, WireProtocol};
use serde::Deserialize;
use std::collections::BTreeMap;

use super::{DiscoveryProtocol, ModelProviderSpec, RemoteCatalogSource};

/// Curated seed models offered by the OpenCode Go preset. A fresh connection
/// activates from this list before the first catalog fetch completes; the
/// live catalog then refreshes the served set (including relay models this
/// client has never heard of).
pub use muta_contracts::model_providers::OPENCODE_GO_MODELS;

/// Wire-format exceptions for the opencode-go relay. The relay's default route
/// is OpenAI chat-completions, but the `minimax-*` family is served over
/// Anthropic `/messages`. Declared here as data so `route_for_model` can route
/// them correctly even when the model is fitted from models.opencode.ai.
pub const WIRE_OVERRIDES: &[(&str, WireProtocol)] = &[
    ("minimax-m2.5", WireProtocol::AnthropicMessages),
    ("minimax-m2.7", WireProtocol::AnthropicMessages),
    ("minimax-m3", WireProtocol::AnthropicMessages),
];

/// Baseline capability metadata for the models this provider serves,
/// submitted to `muta_contracts`'s registry at link time (see
/// [`muta_contracts::model::BaselineModels`]).
pub const MODELS: &[Model] = &[
    // DeepSeek (opencode-go / direct)
    Model {
        id: "deepseek-flash",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: EFFORT_LOW_HIGH_MAX,
    },
    Model {
        id: "deepseek-v4-flash",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: EFFORT_LOW_HIGH_MAX,
    },
    Model {
        id: "deepseek-v4-pro",
        family: "deepseek",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: EFFORT_LOW_HIGH_MAX,
    },
    Model {
        id: "glm-5",
        family: "glm",
        context_window: 200_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "glm-5.1",
        family: "glm",
        context_window: 200_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    // GLM family (Zhipu / Z.AI / opencode-go)
    Model {
        id: "glm-5.2",
        family: "glm",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: EFFORT_GLM_5,
    },
    Model {
        id: "kimi-k2.5",
        family: "kimi",
        context_window: 262_144,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "kimi-k2.6",
        family: "kimi",
        context_window: 262_144,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "kimi-k2.7-code",
        family: "kimi",
        context_window: 262_144,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "mimo-v2-omni",
        family: "mimo",
        context_window: 262_144,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "mimo-v2-pro",
        family: "mimo",
        context_window: 1_048_576,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    // MiMo (Xiaomi / opencode-go, OpenAI format)
    Model {
        id: "mimo-v2.5",
        family: "mimo",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "mimo-v2.5-pro",
        family: "mimo",
        context_window: 1_048_576,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "minimax-m2.5",
        family: "minimax",
        context_window: 204_800,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::AnthropicMessages,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_COMMON,
    },
    Model {
        id: "minimax-m2.7",
        family: "minimax",
        context_window: 204_800,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::AnthropicMessages,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_COMMON,
    },
    // MiniMax (opencode-go, Anthropic /messages format)
    Model {
        id: "minimax-m3",
        family: "minimax",
        context_window: 512_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::AnthropicMessages,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_COMMON,
    },
    Model {
        id: "qwen3.5-plus",
        family: "qwen",
        context_window: 262_144,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_COMMON,
    },
    Model {
        id: "qwen3.6-plus",
        family: "qwen",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_COMMON,
    },
    // Qwen (opencode-go, OpenAI /chat/completions format)
    // models.dev records qwen3.* as `@ai-sdk/openai-compatible` under
    // opencode-go; this baseline table mirrors that so the offline
    // fallback path matches the live catalog.
    Model {
        id: "qwen3.7-max",
        family: "qwen",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_COMMON,
    },
    Model {
        id: "qwen3.7-plus",
        family: "qwen",
        context_window: 1_000_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_COMMON,
    },
];

inventory::submit!(muta_contracts::model::BaselineModels(MODELS));

pub(crate) const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    prompt_cache: super::unsupported_prompt_cache,
    id: "opencode-go",
    baselines: MODELS,
    // Endpoints are per-model by wire format (see `route_for_model`); the
    // instance-level default is the OpenAI chat-completions surface.
    base_url: "https://opencode.ai/zen/go/v1/chat/completions",
    user_agent: Some(muta_contracts::client_identity::OPENCODE_USER_AGENT),
    protocol: WireProtocol::ChatCompletions,
    // The served set comes from the relay's private models.opencode.ai catalog.
    // Every advertised id is materialized with its catalog metadata, so a newly
    // shipped relay model appears with zero client changes.
    catalog_source: RemoteCatalogSource::Endpoint(DiscoveryProtocol::OpencodeGo),
    default_client_profile: muta_contracts::ClientPreset::Native,
    client_profile_sensitive: false,
    wire_overrides: WIRE_OVERRIDES,
    models: OPENCODE_GO_MODELS,
};

#[derive(Debug, Clone, Deserialize)]
struct DevProvider {
    models: BTreeMap<String, DevModel>,
}

#[derive(Debug, Clone, Deserialize)]
struct DevModel {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    family: Option<String>,
    #[serde(default)]
    reasoning: bool,
    #[serde(default)]
    reasoning_options: Vec<DevReasoningOption>,
    #[serde(default)]
    tool_call: bool,
    #[serde(default)]
    limit: DevLimit,
    #[serde(default)]
    modalities: DevModalities,
}

#[derive(Debug, Clone, Deserialize)]
struct DevReasoningOption {
    r#type: String,
    #[serde(default)]
    values: Vec<Option<String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct DevLimit {
    #[serde(default)]
    context: Option<u64>,
    #[serde(default)]
    output: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct DevModalities {
    #[serde(default)]
    input: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)]
    output: Vec<String>,
}

pub(crate) fn parse_catalog(json: &serde_json::Value) -> Vec<crate::list_models::DiscoveredModel> {
    let Some(provider_json) = json.get("opencode-go") else {
        return Vec::new();
    };
    let Ok(provider) = serde_json::from_value::<DevProvider>(provider_json.clone()) else {
        return Vec::new();
    };
    let mut models: Vec<_> = provider.models.into_values().map(from_dev_model).collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models.dedup_by(|a, b| a.id == b.id);
    models
}

fn from_dev_model(m: DevModel) -> crate::list_models::DiscoveredModel {
    let modalities_in = &m.modalities.input;
    let reasoning = Some(m.reasoning);
    let thinking = Some(if m.reasoning {
        ReasoningSupport::ReasoningContent
    } else {
        ReasoningSupport::None
    });
    let effort_levels = m
        .reasoning_options
        .iter()
        .filter(|opt| opt.r#type == "effort")
        .flat_map(|opt| opt.values.iter().flatten())
        .cloned()
        .collect::<Vec<_>>();
    crate::list_models::DiscoveredModel {
        id: m.id,
        picker_enabled: None,
        protocol: None,
        family: m.family.clone(),
        name: (!m.name.trim().is_empty()).then(|| m.name.clone()),
        context_window: m.limit.context.map(|c| c as usize),
        max_output_tokens: m.limit.output.map(|o| o as u32),
        reasoning,
        thinking,
        tool_call: Some(m.tool_call),
        vision: if modalities_in.is_empty() {
            None
        } else {
            Some(modalities_in.iter().any(|m| m == "image"))
        },
        effort_levels: Some(effort_levels),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_model() -> DevModel {
        DevModel {
            id: "glm-5.3".to_string(),
            name: "GLM-5.3".to_string(),
            family: Some("glm".to_string()),
            reasoning: true,
            reasoning_options: vec![DevReasoningOption {
                r#type: "effort".to_string(),
                values: vec![
                    Some("low".to_string()),
                    Some("high".to_string()),
                    Some("max".to_string()),
                ],
            }],
            tool_call: true,
            limit: DevLimit {
                context: Some(1_000_000),
                output: Some(131_072),
            },
            modalities: DevModalities {
                input: vec!["text".to_string()],
                output: vec!["text".to_string()],
            },
        }
    }

    #[test]
    fn maps_capabilities_and_effort_ladder() {
        let dm = from_dev_model(sample_model());
        assert_eq!(dm.id, "glm-5.3");
        assert_eq!(dm.family.as_deref(), Some("glm"));
        assert_eq!(dm.context_window, Some(1_000_000));
        assert_eq!(dm.max_output_tokens, Some(131_072));
        assert_eq!(dm.thinking, Some(ReasoningSupport::ReasoningContent));
        assert_eq!(dm.reasoning, Some(true));
        assert_eq!(dm.tool_call, Some(true));
        assert_eq!(dm.vision, Some(false));
        assert_eq!(
            dm.effort_levels,
            Some(vec![
                "low".to_string(),
                "high".to_string(),
                "max".to_string()
            ])
        );
        assert_eq!(dm.protocol, None);
    }

    #[test]
    fn vision_is_derived_from_input_modalities() {
        let mut m = sample_model();
        m.modalities.input = vec!["text".to_string(), "image".to_string()];
        assert_eq!(from_dev_model(m).vision, Some(true));
    }

    #[test]
    fn parses_catalog_json_for_opencode_go() {
        let raw = serde_json::json!({
            "opencode-go": {
                "models": {
                    "m1": {
                        "id": "m1",
                        "name": "Model 1",
                        "reasoning": false,
                        "tool_call": true
                    }
                }
            }
        });
        let models = parse_catalog(&raw);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "m1");
        assert_eq!(models[0].name.as_deref(), Some("Model 1"));
    }
}
