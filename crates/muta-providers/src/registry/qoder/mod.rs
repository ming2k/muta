//! The `qoder` provider template: Alibaba Qoder's subscription coding
//! platform (`api*.qoder.sh`, CN: `*.qoder.com.cn`), COSY-signed SSE wire.

pub mod identity;
pub mod pipeline;
pub mod surface;
pub mod wire;

pub use identity::{QoderRequestIdentity, QoderStoredIdentity};
pub use pipeline::build_qoder_pipeline;
pub use wire::*;

use super::{ModelProviderSpec, RemoteCatalogSource};
use muta_contracts::WireProtocol;
use muta_contracts::model::Model;
use muta_contracts::reasoning::ReasoningSupport;
use serde_json::Value;

/// Offline seed for Qoder: the two platform flagships.
pub const QODER_MODELS: &[Model] = &[
    Model {
        id: "qmodel_38max",
        family: "qwen",
        context_window: 200_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
    Model {
        id: "qfmodel",
        family: "qwen",
        context_window: 200_000,
        thinking: ReasoningSupport::ReasoningContent,
        tool_call: true,
        vision: true,
        protocol: WireProtocol::ChatCompletions,
        model_guidance: "",
        effort_levels: &[],
    },
];

inventory::submit!(muta_contracts::model::BaselineModels(QODER_MODELS));

pub(crate) const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    dialect: muta_contracts::ProviderDialect::Qoder,
    protocol_roots: std::borrow::Cow::Borrowed(&[]),
    catalog_root_url: None,
    prompt_cache: super::PromptCachePolicy::Compiled(super::unsupported_prompt_cache),
    id: std::borrow::Cow::Borrowed("qoder"),
    baselines: QODER_MODELS,
    root_url: std::borrow::Cow::Borrowed("https://api2.qoder.sh"),
    user_agent: None,
    protocol: WireProtocol::ChatCompletions,
    catalog_source: RemoteCatalogSource::Endpoint(
        muta_contracts::provider_surface::CatalogShape::SceneMap,
    ),
    default_client_profile: muta_contracts::ClientPreset::Native,
    client_profile_sensitive: false,
    models: &["qmodel_38max", "qfmodel"],
};

/// Sign a catalog request with the connection's COSY identity.
pub(crate) fn sign_catalog_request(
    identity: &QoderRequestIdentity,
    bearer: &str,
    signed_path: &str,
    now_secs: u64,
) -> Result<wire::signer::PreparedCosy, String> {
    let cosy = wire::signer::CosyIdentity::parse(identity.machine_key_hex.expose_secret())
        .ok_or_else(|| "the connection's machine key is malformed".to_string())?;
    let identity_json = identity.identity_payload_json(bearer, "");
    Ok(wire::signer::prepare_request_for_path(
        &cosy,
        &identity_json,
        &identity.uid,
        "",
        signed_path,
        now_secs,
    ))
}

/// Build the Qoder catalog signer from the persisted identity.
pub async fn build_catalog_signer(
    connection_id: &str,
    bearer: &str,
) -> Option<Box<dyn super::super::CatalogSigning>> {
    let identity = crate::oauth::stored_qoder_request_identity(connection_id).await?;
    Some(Box::new(QoderCatalogSigning::new(
        identity,
        bearer.to_string(),
    )))
}

/// The Qoder catalog signer.
pub struct QoderCatalogSigning {
    identity: QoderRequestIdentity,
    bearer: String,
}

impl QoderCatalogSigning {
    pub fn new(identity: QoderRequestIdentity, bearer: String) -> Self {
        Self { identity, bearer }
    }
}

impl super::super::CatalogSigning for QoderCatalogSigning {
    fn identity_headers(&self) -> Vec<(String, String)> {
        surface::QODER_SURFACE
            .identity
            .headers_with_version()
            .into_iter()
            .map(|(name, value)| (name.to_string(), value))
            .collect()
    }

    fn identity_subject_headers(&self) -> Vec<(String, String)> {
        if self.identity.uid.is_empty() {
            Vec::new()
        } else {
            vec![("Cosy-User".to_string(), self.identity.uid.clone())]
        }
    }

    fn sign(&self, signed_path: &str) -> Result<super::super::CatalogSignature, String> {
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0);
        let signed = sign_catalog_request(&self.identity, &self.bearer, signed_path, now_secs)?;
        Ok(super::super::CatalogSignature {
            authorization: signed.authorization,
            date: signed.date,
            key: signed.key,
        })
    }
}

/// Parse Qoder's scene-keyed catalog into the requested scene's models.
pub(crate) fn parse_scene_catalog(json: &Value, scene: &str) -> Vec<super::super::DiscoveredModel> {
    let Some(scenes) = json.as_object() else {
        return Vec::new();
    };
    let entries = scenes
        .get(scene)
        .or_else(|| scenes.get("assistant"))
        .and_then(Value::as_array);
    let entries = match entries {
        Some(entries) => entries,
        None => {
            let mut all = Vec::new();
            for value in scenes.values() {
                if let Some(array) = value.as_array() {
                    all.extend(array.iter());
                }
            }
            return all
                .into_iter()
                .filter_map(discovered_from_scene_entry)
                .collect();
        }
    };
    entries
        .iter()
        .filter_map(discovered_from_scene_entry)
        .collect()
}

fn discovered_from_scene_entry(entry: &Value) -> Option<super::super::DiscoveredModel> {
    let key = entry.get("key").and_then(Value::as_str)?.trim().to_string();
    if key.is_empty() {
        return None;
    }
    let picker_enabled = match entry.get("enable") {
        Some(Value::Bool(enabled)) => Some(*enabled),
        Some(Value::Number(number)) => Some(number.as_i64().unwrap_or(0) != 0),
        _ => None,
    };
    if picker_enabled == Some(false) {
        return None;
    }
    let context_window = entry
        .get("context_config")
        .and_then(Value::as_object)
        .and_then(|configs| {
            configs
                .values()
                .find(|tier| tier.get("is_default").and_then(Value::as_bool) == Some(true))
                .and_then(|tier| tier.get("token_count"))
                .and_then(Value::as_u64)
        })
        .or_else(|| entry.get("max_input_tokens").and_then(Value::as_u64))
        .and_then(|tokens| usize::try_from(tokens).ok());
    let effort_levels = entry
        .get("thinking_config")
        .and_then(|config| config.get("enabled"))
        .and_then(|enabled| enabled.get("efforts"))
        .and_then(Value::as_object)
        .map(|efforts| efforts.keys().cloned().collect());
    Some(super::super::DiscoveredModel {
        id: key,
        picker_enabled,
        protocol: None,
        family: entry
            .get("family")
            .and_then(Value::as_str)
            .map(str::to_string),
        name: entry
            .get("display_name")
            .and_then(Value::as_str)
            .map(str::to_string),
        context_window,
        max_output_tokens: None,
        reasoning: entry.get("is_reasoning").and_then(Value::as_bool),
        thinking: entry
            .get("is_reasoning")
            .and_then(Value::as_bool)
            .map(|reasoning| {
                if reasoning {
                    ReasoningSupport::ReasoningContent
                } else {
                    ReasoningSupport::None
                }
            }),
        tool_call: None,
        vision: entry.get("is_vl").and_then(Value::as_bool),
        effort_levels,
        catalog_source: entry
            .get("source")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::MODEL_PROVIDER_SPEC as SPEC;
    use muta_contracts::WireProtocol;

    #[test]
    fn spec_resolves_and_serves_the_chat_wire() {
        assert_eq!(SPEC.id, "qoder");
        assert_eq!(SPEC.protocol, WireProtocol::ChatCompletions);
        assert_eq!(SPEC.baselines.len(), 2);
    }
}
