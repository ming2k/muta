//! The `opencode-go` provider preset: the OpenCode **Console** subscription
//! surface (`opencode.ai/inference/…`), cataloged and routed by the
//! account-scoped `/console/api/config` catalog
//! ([`CatalogShape::OpencodeConsole`], ADR-0269). The catalog advertises the
//! served models *and* their routing: a per-model `provider.{npm,api}` override
//! selects the wire protocol and inference root, so the live catalog — not a
//! compiled relay guess — is authoritative for both.

use muta_contracts::effort::{EFFORT_GLM_5, EFFORT_LOW_HIGH_MAX};
use muta_contracts::reasoning::ReasoningSupport;
use muta_contracts::{Model, WireProtocol};
use serde::Deserialize;
use std::collections::BTreeMap;

use super::{CatalogShape, ModelProviderSpec, RemoteCatalogSource};

/// Curated seed models offered by the OpenCode Go preset. A fresh connection
/// activates from this list before the first catalog fetch completes; the
/// live catalog then refreshes the served set (including relay models this
/// client has never heard of).
pub use muta_contracts::model_providers::OPENCODE_GO_MODELS;

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
        protocol: WireProtocol::ChatCompletions,
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
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_COMMON,
    },
    // MiniMax (opencode-go, openai-compatible chat on the Console surface)
    // The zen-era relay served MiniMax over Anthropic /messages; the Console
    // account catalog advertises no provider override for it, which pins the
    // `@ai-sdk/openai-compatible` default — chat completions (ADR-0269).
    Model {
        id: "minimax-m3",
        family: "minimax",
        context_window: 512_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: false,
        protocol: WireProtocol::ChatCompletions,
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
        protocol: WireProtocol::AnthropicMessages,
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
        protocol: WireProtocol::AnthropicMessages,
        model_guidance: "",
        effort_levels: muta_contracts::effort::EFFORT_COMMON,
    },
    // Qwen (opencode-go) — the Console catalog routes qwen3.5-plus/3.6-plus
    // through `@ai-sdk/anthropic`; the older qwen3.7 entries were
    // `@ai-sdk/openai-compatible` zen-era listings and stay on chat
    // completions until the live catalog says otherwise.
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
    dialect: muta_contracts::ProviderDialect::Standard,
    protocol_roots: std::borrow::Cow::Borrowed(&[
        (
            WireProtocol::AnthropicMessages,
            std::borrow::Cow::Borrowed("https://opencode.ai/inference/anthropic/v1"),
        ),
        (
            WireProtocol::GoogleGemini,
            std::borrow::Cow::Borrowed("https://opencode.ai/inference/google/v1beta"),
        ),
    ]),
    catalog_root_url: Some(std::borrow::Cow::Borrowed("https://opencode.ai/console")),
    prompt_cache: super::PromptCachePolicy::Compiled(super::unsupported_prompt_cache),
    id: std::borrow::Cow::Borrowed("opencode-go"),
    baselines: MODELS,
    // Per-model routes come from the account catalog (protocol + optional
    // root override); the default root serves OpenAI chat/responses surfaces
    // and any model the catalog does not override.
    root_url: std::borrow::Cow::Borrowed("https://opencode.ai/inference/openai/v1"),
    user_agent: Some(std::borrow::Cow::Borrowed(
        muta_contracts::client_identity::OPENCODE_USER_AGENT,
    )),
    protocol: WireProtocol::ChatCompletions,
    // The served set comes from the Console account catalog. Every advertised
    // id is materialized with its catalog metadata — including per-model wire
    // and root overrides — so a newly shipped model appears with zero client
    // changes (ADR-0269).
    catalog_source: RemoteCatalogSource::Endpoint(CatalogShape::OpencodeConsole),
    default_client_profile: muta_contracts::ClientPreset::Native,
    client_profile_sensitive: false,
    models: OPENCODE_GO_MODELS,
};

#[derive(Debug, Clone, Deserialize)]
struct DevProvider {
    /// The provider-default npm; models without a `provider` override ride it.
    #[serde(default)]
    npm: Option<String>,
    #[serde(default)]
    models: BTreeMap<String, DevModel>,
}

#[derive(Debug, Clone, Deserialize)]
struct DevModel {
    /// models.dev carries the id inside the entry; the Console config keys
    /// the entries by id and omits the field, so it defaults and the parser
    /// fills it from the map key.
    #[serde(default)]
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
    /// Per-model routing override: npm selects the wire protocol, api is an
    /// inference **root** override (ADR-0269).
    #[serde(default)]
    provider: Option<DevModelProvider>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct DevModelProvider {
    #[serde(default)]
    npm: Option<String>,
    #[serde(default)]
    api: Option<String>,
}

/// The `@ai-sdk/*` package the Console catalog names per model selects the
/// wire protocol. Unknown npm → `None` (no override; the spec default holds),
/// never a guess.
fn protocol_for_npm(npm: &str) -> Option<WireProtocol> {
    match npm {
        "@ai-sdk/anthropic" => Some(WireProtocol::AnthropicMessages),
        "@ai-sdk/google" => Some(WireProtocol::GoogleGemini),
        "@ai-sdk/openai" => Some(WireProtocol::Responses),
        "@ai-sdk/openai-compatible" => Some(WireProtocol::ChatCompletions),
        _ => None,
    }
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

/// Extract the account catalog from a Console `/api/config` response:
/// `{"config":{"provider":{"opencode":{npm, models:{…}}}}}` (ADR-0269).
pub(crate) fn parse_config_catalog(
    json: &serde_json::Value,
) -> Vec<crate::list_models::DiscoveredModel> {
    let Some(provider_json) = json
        .get("config")
        .and_then(|config| config.get("provider"))
        .and_then(|providers| providers.get("opencode"))
    else {
        return Vec::new();
    };
    let Ok(provider) = serde_json::from_value::<DevProvider>(provider_json.clone()) else {
        return Vec::new();
    };
    let default_protocol = provider.npm.as_deref().and_then(protocol_for_npm);
    let mut models: Vec<_> = provider
        .models
        .into_iter()
        .map(|(key, model)| from_dev_model(key, model, default_protocol))
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models.dedup_by(|a, b| a.id == b.id);
    models
}

fn from_dev_model(
    key: String,
    m: DevModel,
    default_protocol: Option<WireProtocol>,
) -> crate::list_models::DiscoveredModel {
    let id = if m.id.trim().is_empty() { key } else { m.id };
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
    // The model's own npm override selects the wire; without one the
    // provider-default npm applies; an npm outside the known set declares
    // nothing (the spec route holds) rather than guessing.
    let protocol = m
        .provider
        .as_ref()
        .and_then(|p| p.npm.as_deref())
        .and_then(protocol_for_npm)
        .or(default_protocol);
    crate::list_models::DiscoveredModel {
        id,
        picker_enabled: None,
        protocol,
        endpoint: m.provider.as_ref().and_then(|p| p.api.clone()),
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
        // The Console config advertises no effort vocabulary; undeclared keeps
        // the baseline ladder, an empty vec would explicitly negate it.
        effort_levels: (!effort_levels.is_empty()).then_some(effort_levels),
        catalog_source: None,
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
            provider: None,
        }
    }

    #[test]
    fn maps_capabilities_and_effort_ladder() {
        let dm = from_dev_model("k".to_string(), sample_model(), None);
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
        assert_eq!(dm.endpoint, None);
    }

    #[test]
    fn vision_is_derived_from_input_modalities() {
        let mut m = sample_model();
        m.modalities.input = vec!["text".to_string(), "image".to_string()];
        assert_eq!(from_dev_model("k".to_string(), m, None).vision, Some(true));
    }

    #[test]
    fn parses_console_config_catalog_with_routing_overrides() {
        let raw = serde_json::json!({
            "config": {
                "provider": {
                    "opencode": {
                        "npm": "@ai-sdk/openai-compatible",
                        "models": {
                            "m1": {
                                "name": "Model 1",
                                "reasoning": false,
                                "tool_call": true
                            },
                            "c1": {
                                "name": "Claude 1",
                                "reasoning": true,
                                "tool_call": true,
                                "provider": {
                                    "npm": "@ai-sdk/anthropic",
                                    "api": "https://opencode.ai/inference/anthropic/v1"
                                }
                            },
                            "g1": {
                                "name": "GPT 1",
                                "provider": { "npm": "@ai-sdk/openai" }
                            },
                            "u1": {
                                "name": "Unknown npm",
                                "provider": { "npm": "@ai-sdk/mystery" }
                            }
                        }
                    }
                }
            }
        });
        let models = parse_config_catalog(&raw);
        assert_eq!(models.len(), 4);
        // Keyed entries carry no `id` field; the map key is the wire id.
        let ids: Vec<_> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["c1", "g1", "m1", "u1"]);
        let find = |id: &str| models.iter().find(|m| m.id == id).unwrap();
        // No per-model override → the provider-default npm selects the wire.
        assert_eq!(find("m1").protocol, Some(WireProtocol::ChatCompletions));
        assert_eq!(find("m1").endpoint, None);
        // npm selects the wire; api overrides the inference root.
        assert_eq!(find("c1").protocol, Some(WireProtocol::AnthropicMessages));
        assert_eq!(
            find("c1").endpoint.as_deref(),
            Some("https://opencode.ai/inference/anthropic/v1")
        );
        assert_eq!(find("g1").protocol, Some(WireProtocol::Responses));
        assert_eq!(find("g1").endpoint, None);
        // An unknown npm declares no wire instead of guessing one; the
        // api/root defaults still apply at route time.
        assert_eq!(find("u1").protocol, Some(WireProtocol::ChatCompletions));
        // Console config carries no effort vocabulary: undeclared, so the
        // static baseline ladder survives instead of being negated.
        assert_eq!(find("m1").effort_levels, None);
    }

    #[test]
    fn empty_models_map_is_an_empty_catalog() {
        let raw = serde_json::json!({
            "config": { "provider": { "opencode": { "npm": "@ai-sdk/openai-compatible", "models": {} } } }
        });
        assert!(parse_config_catalog(&raw).is_empty());
    }

    #[test]
    fn non_console_shape_parses_to_nothing() {
        let raw = serde_json::json!({ "opencode-go": { "models": {} } });
        assert!(parse_config_catalog(&raw).is_empty());
    }
}
