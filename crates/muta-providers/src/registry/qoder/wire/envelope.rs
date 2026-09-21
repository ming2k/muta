//! The `agent_chat_generation` request envelope (Qoder's agent surface).

use muta_contracts::Effort;
use muta_contracts::model::ModelCapabilities;
use muta_contracts::wire_surface::{AgentChatSpec, IdentityValue, ModelBinding, ModelCarrier};
use muta_contracts::ProviderError;
use muta_llm_client::pipeline::{EnvelopePhase, ReshapedEnvelope};
use serde_json::{Map, Value, json};

use super::super::surface::{AGENT_CHAT, COSY_VERSION, MODEL_BINDINGS};

/// Everything the envelope needs that the flat body does not already carry.
pub struct EnvelopeInput<'a> {
    pub model: &'a str,
    pub catalog_source: &'a str,
    pub display_name: &'a str,
    #[allow(dead_code)]
    pub capabilities: &'a ModelCapabilities,
    #[allow(dead_code)]
    pub reasoning_effort: Option<Effort>,
    pub session_id: Option<&'a str>,
    pub fresh_uuids: &'a [String],
    pub now_ms: u64,
    pub emulated_version: &'a str,
    pub first_user_text: &'a str,
}

pub fn apply_model_bindings(
    spec: &[ModelBinding],
    input: &EnvelopeInput<'_>,
    body: &mut Value,
) -> (Vec<(String, String)>, Option<String>) {
    let mut headers = Vec::new();
    let mut path_segment = None;
    for binding in spec {
        let value = match binding.value {
            IdentityValue::WireId => input.model.to_string(),
            IdentityValue::CatalogSource => input.catalog_source.to_string(),
            IdentityValue::DisplayName => input.display_name.to_string(),
            IdentityValue::Constant(c) => c.to_string(),
        };
        match &binding.carrier {
            ModelCarrier::Header(name) => {
                if !value.is_empty() {
                    headers.push((name.to_string(), value));
                }
            }
            ModelCarrier::PathSegment => {
                path_segment = Some(value);
            }
            ModelCarrier::BodyField(field) => {
                if let Some(obj) = body.as_object_mut() {
                    obj.insert(field.to_string(), Value::String(value));
                }
            }
            ModelCarrier::BodyPointer(ptr) => {
                write_pointer(body, ptr, Value::String(value));
            }
            ModelCarrier::QueryParam(_) => {}
        }
    }
    (headers, path_segment)
}

fn write_pointer(root: &mut Value, pointer: &str, value: Value) {
    let segments: Vec<&str> = pointer.trim_start_matches('/').split('/').collect();
    if segments.is_empty() {
        return;
    }
    let mut cursor = root;
    for (i, segment) in segments.iter().enumerate() {
        let is_last = i + 1 == segments.len();
        if is_last {
            if let Some(obj) = cursor.as_object_mut() {
                obj.insert((*segment).to_string(), value);
            }
            return;
        }
        if !cursor.is_object() {
            *cursor = Value::Object(Map::new());
        }
        let obj = cursor.as_object_mut().expect("cursor was made an object");
        if !obj.contains_key(*segment) || !obj.get(*segment).is_some_and(Value::is_object) {
            obj.insert((*segment).to_string(), Value::Object(Map::new()));
        }
        cursor = obj.get_mut(*segment).expect("entry was just inserted");
    }
}

pub fn first_user_text(body: &Value) -> &str {
    body.get("messages")
        .and_then(Value::as_array)
        .and_then(|msgs| {
            msgs.iter().find_map(|m| {
                if m.get("role").and_then(Value::as_str) == Some("user") {
                    m.get("content").and_then(Value::as_str)
                } else {
                    None
                }
            })
        })
        .unwrap_or("")
}

pub fn wrap(body: &Value, spec: &AgentChatSpec, input: &EnvelopeInput<'_>) -> Value {
    let mut root = json!({
        "chat_task": spec.chat_task,
        "agent_id": spec.agent_id,
        "session_type": spec.session_type,
        "task_id": spec.task_id,
        "source": spec.source,
        "version": spec.version,
        "model_format": spec.model_format,
        "business": {
            "product": spec.business_product,
            "type": spec.business_type,
            "stage": spec.business_stage,
            "name": input.first_user_text,
            "begin_at": input.now_ms,
            "version": input.emulated_version,
        },
    });

    if let Some(sid) = input.session_id {
        write_pointer(&mut root, "session_id", Value::String(sid.to_string()));
    }

    for (ptr, uuid) in spec.fresh_uuid_pointers.iter().zip(input.fresh_uuids) {
        write_pointer(&mut root, ptr, Value::String(uuid.clone()));
    }

    apply_model_bindings(MODEL_BINDINGS, input, &mut root);

    if let Some(obj) = root.as_object_mut() {
        if let Some(msgs) = body.get("messages") {
            obj.insert("messages".to_string(), msgs.clone());
        }
        if let Some(tools) = body.get("tools") {
            obj.insert("tools".to_string(), tools.clone());
        }
        if let Some(system) = body.get("system") {
            obj.insert("system".to_string(), system.clone());
        }
    }

    root
}

/// EnvelopePhase implementation for Qoder.
#[derive(Debug, Clone, Default)]
pub struct QoderAgentEnvelope;

impl EnvelopePhase for QoderAgentEnvelope {
    fn reshape_body(
        &self,
        body: &serde_json::Value,
    ) -> Result<ReshapedEnvelope, ProviderError> {
        let model = body.get("model").and_then(Value::as_str).unwrap_or("qoder3");
        let first_text = first_user_text(body);
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let fresh_uuids: Vec<String> = (0..AGENT_CHAT.fresh_uuid_pointers.len())
            .map(|_| uuid::Uuid::new_v4().to_string())
            .collect();

        let caps = ModelCapabilities::for_channel(model, None);
        let input = EnvelopeInput {
            model,
            catalog_source: "system",
            display_name: model,
            capabilities: &caps,
            reasoning_effort: None,
            session_id: None,
            fresh_uuids: &fresh_uuids,
            now_ms,
            emulated_version: COSY_VERSION,
            first_user_text: first_text,
        };

        let wrapped = wrap(body, &AGENT_CHAT, &input);
        // One identity resolution feeds both artifacts: the body slots were
        // just stamped from `input`, so the header carriers resolve from the
        // same values — consistent by construction.
        let (identity_headers, _) =
            apply_model_bindings(MODEL_BINDINGS, &input, &mut Value::Null);

        Ok(ReshapedEnvelope {
            body: wrapped,
            identity_headers,
        })
    }
}
