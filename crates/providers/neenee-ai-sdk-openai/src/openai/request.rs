//! OpenAI-compatible chat completions — request construction.
//!
//! Pure functions turning the harness's `Vec<Message>` into the OpenAI
//! chat-completions request shape: a `messages` array of `{role, content,
//! tool_calls?, tool_call_id?}`, plus optional `tools`/`stream_options`. No
//! `reqwest`, no `async`, no I/O.
//!
//! OpenAI wire shape:
//! - URL: the chat-completions endpoint as configured (default
//!   `https://api.openai.com/v1/chat/completions`).
//! - Auth: `Authorization: Bearer <key>` — but **only when a key is set**. A
//!   keyless relay (empty key) sends no auth header at all, because some
//!   relays reject a malformed bearer token even when they'd otherwise ignore
//!   the key.
//! - Body: `{model, messages, stream, reasoning_effort?, tools?, stream_options?}`.
//!
//! Two correctness transformations live here:
//! 1. Images are stripped from non-vision models (the API rejects `image_url`
//!    on them; text content is preserved).
//! 2. Orphan tool results and unanswered tool calls are reconciled so the
//!    request is always wire-valid: every `tool` result references a known
//!    preceding `tool_call`, and every assistant `tool_calls` has its results.

use neenee_core::{Effort, Message, Role};
use serde_json::{Value, json};

/// The headers this wire format requires on every request, beyond the
/// always-present `User-Agent`. For OpenAI that is just the bearer auth header
/// — omitted when the key is empty (keyless relay).
pub fn headers(api_key: &str) -> Vec<(&'static str, String)> {
    if api_key.trim().is_empty() {
        Vec::new()
    } else {
        vec![("Authorization", format!("Bearer {api_key}"))]
    }
}

/// Inputs to [`body`]: the model id, whether this is a streaming request, and
/// the prepared tool schemas (OpenAI function-spec shape, optional).
pub struct BodyInput<'a> {
    pub model: &'a str,
    pub stream: bool,
    /// OpenAI-shaped tool specs (`{type:"function", function:{...}}`), if any.
    pub tool_specs: Option<&'a [Value]>,
    /// Optional OpenAI reasoning-effort override. `None` omits the field and
    /// keeps the model/provider default.
    pub reasoning_effort: Option<Effort>,
    /// Optional session-scoped prompt-cache key (Moonshot / Kimi). When set, the
    /// body carries `prompt_cache_key` so the server-side cache namespaces per
    /// session and repeated prefixes (system prompt, recent turns) hit at a
    /// discount. Resolved from the model's [`neenee_core::CachePolicy`] by the
    /// provider adapter; `None` omits the field entirely (OpenAI ignores it
    /// harmlessly, but we still don't send it unless the policy is `SessionKey`).
    pub prompt_cache_key: Option<&'a str>,
}

/// Build the chat-completions request body.
///
/// Strips images for non-vision models and reconciles tool calls/results so
/// the request is always wire-valid (see the module docs).
///
/// Projection contract (ADR-0048): the `messages` passed here are a clone of
/// the round's working scratch, itself cloned from the session's authoritative
/// `model_window` at turn start. This builder reads only wire-relevant fields
/// via [`message_obj`] — `role`, `content`, `tool_calls`, `tool_call_id`,
/// `images` — so durable sidecars (`children`, `envoy_meta`, `origin`) never
/// reach the wire. Serialization is therefore a pure projection of the
/// session: no field on the wire exists that the session did not produce.
pub fn body(messages: Vec<Message>, input: BodyInput<'_>) -> Value {
    let BodyInput {
        model: model_id,
        stream,
        tool_specs,
        reasoning_effort,
        prompt_cache_key,
    } = input;

    // If the model doesn't support vision, strip inline images so the API
    // doesn't reject the request with "unknown variant `image_url`". The
    // text content is preserved — the model just doesn't see the pixels.
    let model = neenee_core::model::resolve(model_id);
    let messages: Vec<Message> = if model.vision {
        messages
    } else {
        messages
            .into_iter()
            .map(|mut m| {
                if m.images.is_some() {
                    tracing::debug!(
                        target: "neenee_core::provider",
                        model = %model_id,
                        vision = false,
                        "stripping images from message — model does not support vision",
                    );
                    m.images = None;
                }
                m
            })
            .collect()
    };

    // OpenAI rejects any `tool` message whose `tool_call_id` does not match
    // a `tool_call` on a preceding assistant message. Drop orphan tool
    // results (e.g. from text-fallback calls or older saved sessions) so the
    // request can never fail with "tool_call_id is not found".
    let mut known_ids = std::collections::HashSet::new();
    let mut messages: Vec<Message> = messages
        .into_iter()
        .filter(|message| {
            if !valid_provider_message(message) {
                return false;
            }
            match message.role {
                Role::Assistant => {
                    if let Some(calls) = message.tool_calls.as_ref() {
                        for call in calls {
                            known_ids.insert(call.id.clone());
                        }
                    }
                    true
                }
                Role::Tool => message
                    .tool_call_id
                    .as_ref()
                    .is_some_and(|id| !id.is_empty() && known_ids.contains(id)),
                _ => true,
            }
        })
        .collect();

    // Every assistant `tool_calls` must be followed by a corresponding
    // `tool` result message. Collect the ids that *did* get a result, then
    // strip unanswered calls from every assistant message so the request is
    // always valid — whether the turn was interrupted, the session was
    // mid-tool when saved, or older turns lost their results.
    let answered: std::collections::HashSet<String> = messages
        .iter()
        .filter_map(|m| {
            if m.role == Role::Tool {
                m.tool_call_id.clone()
            } else {
                None
            }
        })
        .collect();
    messages.retain_mut(|m| {
        if m.role != Role::Assistant {
            return true;
        }
        if let Some(calls) = m.tool_calls.as_mut() {
            calls.retain(|c| answered.contains(&c.id));
            if calls.is_empty() {
                m.tool_calls = None;
            }
        }
        // Keep the message only if it still carries content or at least one
        // surviving tool call; a completely empty assistant message is illegal
        // on the wire.
        !m.content.is_empty() || m.tool_calls.as_ref().is_some_and(|calls| !calls.is_empty())
    });

    let tool_specs = tool_specs.map(|specs| {
        json!(
            specs
                .iter()
                .map(|spec| {
                    let mut spec = spec.clone();
                    if let Some(obj) = spec.as_object_mut() {
                        obj.insert("type".to_string(), Value::String("function".to_string()));
                    }
                    spec
                })
                .collect::<Vec<_>>()
        )
    });

    let mut body = json!({
        "model": model_id,
        "messages": messages.into_iter().map(message_obj).collect::<Vec<_>>(),
        "stream": stream,
    });
    if stream {
        // Ask the endpoint to include a terminal `usage` chunk so the
        // streaming path can book real token counts. Standard OpenAI & most
        // OpenAI-compatible relays honour this; relays that don't recognise it
        // ignore the unknown field harmlessly.
        body["stream_options"] = json!({ "include_usage": true });
    }
    if let Some(effort) = reasoning_effort
        && !model.effort_levels.is_empty()
    {
        body["reasoning_effort"] = json!(effort.clamp_to(model.effort_levels).as_str());
    }
    if let Some(specs) = tool_specs {
        body["tools"] = specs;
    }
    if let Some(key) = prompt_cache_key
        && !key.is_empty()
    {
        // Moonshot / Kimi: a session-scoped cache key namespaces the server-side
        // prompt cache so repeated prefixes (system prompt + recent turns) hit
        // across steps in a session. Relays that don't recognise the field
        // ignore it harmlessly.
        body["prompt_cache_key"] = json!(key);
    }
    body
}

/// Discard messages the OpenAI endpoint rejects or misuses: empty assistant
/// turns (no content, no tool calls) and the system role when tool calls are
/// present (Kimi/Qwen interleave system content with tool execution and refuse
/// a leading system message in that case).
fn valid_provider_message(message: &Message) -> bool {
    if message.role == Role::Assistant {
        let empty = message.content.is_empty()
            && message
                .tool_calls
                .as_ref()
                .map(|calls| calls.is_empty())
                .unwrap_or(true);
        return !empty;
    }
    if message.role == Role::System {
        return message
            .tool_calls
            .as_ref()
            .is_none_or(|calls| calls.is_empty());
    }
    true
}

/// Convert a harness [`Message`] to an OpenAI message object (role + content,
/// with optional `tool_calls` / `tool_call_id`).
pub fn message_obj(m: Message) -> Value {
    let mut map = json!({
        "role": match m.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::System => "system",
            Role::Tool => "tool",
        },
        "content": content(&m),
    });
    if let Some(tool_calls) = m.tool_calls {
        map["tool_calls"] = json!(
            tool_calls
                .into_iter()
                .map(|tc| {
                    json!({
                        "id": tc.id,
                        "type": "function",
                        "function": {"name": tc.name, "arguments": tc.arguments}
                    })
                })
                .collect::<Vec<_>>()
        );
    }
    if let Some(tool_call_id) = m.tool_call_id {
        map["tool_call_id"] = json!(tool_call_id);
    }
    map
}

/// Render a message's `content` field: an array of typed parts when images are
/// present (text + `image_url` data URLs), otherwise a plain string.
pub fn content(m: &Message) -> Value {
    match &m.images {
        Some(images) if !images.is_empty() => {
            let mut parts = Vec::new();
            if !m.content.is_empty() {
                parts.push(json!({ "type": "text", "text": m.content }));
            }
            for image in images {
                parts.push(json!({
                    "type": "image_url",
                    "image_url": {
                        "url": format!("data:{};base64,{}", image.mime, image.data)
                    }
                }));
            }
            Value::Array(parts)
        }
        _ => Value::String(m.content.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use neenee_core::ToolCall;
    #[test]
    fn request_filters_empty_assistant_history() {
        let body = super::body(
            vec![
                Message::new(Role::User, "hello"),
                Message::new(Role::Assistant, ""),
                Message::new(Role::User, "again"),
            ],
            BodyInput {
                model: "test-model",
                stream: true,
                tool_specs: None,
                reasoning_effort: None,
                prompt_cache_key: None,
            },
        );

        assert_eq!(body["messages"].as_array().unwrap().len(), 2);
        assert_eq!(body["messages"][1]["content"], "again");
    }

    #[test]
    fn request_includes_reasoning_effort_when_configured() {
        let body = super::body(
            vec![Message::new(Role::User, "think")],
            BodyInput {
                model: "gpt-5.5",
                stream: false,
                tool_specs: None,
                reasoning_effort: Some(Effort::Xhigh),
                prompt_cache_key: None,
            },
        );

        assert_eq!(body["reasoning_effort"], "xhigh");
    }

    #[test]
    fn request_injects_prompt_cache_key_when_present() {
        let body = super::body(
            vec![Message::new(Role::User, "hi")],
            BodyInput {
                model: "kimi-k2.7-code",
                stream: false,
                tool_specs: None,
                reasoning_effort: None,
                prompt_cache_key: Some("session-42"),
            },
        );
        assert_eq!(body["prompt_cache_key"], "session-42");
    }

    #[test]
    fn request_omits_prompt_cache_key_when_absent() {
        let body = super::body(
            vec![Message::new(Role::User, "hi")],
            BodyInput {
                model: "gpt-5.5",
                stream: false,
                tool_specs: None,
                reasoning_effort: None,
                prompt_cache_key: None,
            },
        );
        assert!(body.get("prompt_cache_key").is_none());
    }

    #[test]
    fn request_drops_orphan_tool_results() {
        let matched = ToolCall {
            id: "call_matched".to_string(),
            name: "read_text".to_string(),
            arguments: "{}".to_string(),
        };
        let assistant_with_call = Message {
            role: Role::Assistant,
            content: String::new(),
            content_blob: None,
            display_content: None,
            reasoning_content: None,
            provider_meta: None,
            tool_calls: Some(vec![matched.clone()]),
            tool_call_id: None,
            images: None,
            provider: None,
            model: None,
            hidden: false,
            children: None,
            envoy_meta: None,
            origin: None,
            timestamp: None,
            sent_at_ms: None,
        };
        let good_result = Message {
            role: Role::Tool,
            content: "ok".to_string(),
            tool_call_id: Some("call_matched".to_string()),
            ..Message::new(Role::Tool, "")
        };
        let orphan_result = Message {
            tool_call_id: Some("call_orphan".to_string()),
            ..Message::new(Role::Tool, "orphan")
        };
        let empty_id_result = Message {
            tool_call_id: Some(String::new()),
            ..Message::new(Role::Tool, "empty id")
        };

        let body = super::body(
            vec![
                Message::new(Role::User, "hi"),
                assistant_with_call,
                good_result,
                orphan_result,
                empty_id_result,
            ],
            BodyInput {
                model: "test-model",
                stream: false,
                tool_specs: None,
                reasoning_effort: None,
                prompt_cache_key: None,
            },
        );

        let messages = body["messages"].as_array().unwrap();
        // user, assistant(tool_calls), and only the matched tool result survive.
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[2]["tool_call_id"], "call_matched");
    }

    #[test]
    fn strips_unanswered_tool_calls() {
        let call_a = ToolCall {
            id: "a".into(),
            name: "bash".into(),
            arguments: "{}".into(),
        };
        let call_b = ToolCall {
            id: "b".into(),
            name: "read_text".into(),
            arguments: "{}".into(),
        };
        let call_c = ToolCall {
            id: "c".into(),
            name: "grep".into(),
            arguments: "{}".into(),
        };

        // --- Case 1: trailing unanswered assistant (the original bug) ---
        let body = super::body(
            vec![
                Message::new(Role::User, "go"),
                Message {
                    tool_calls: Some(vec![call_a.clone()]),
                    ..Message::new(Role::Assistant, "")
                },
            ],
            BodyInput {
                model: "test-model",
                stream: false,
                tool_specs: None,
                reasoning_effort: None,
                prompt_cache_key: None,
            },
        );
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(
            msgs.len(),
            1,
            "trailing unanswered assistant must be dropped"
        );

        // --- Case 2: assistant with content but no tool result ---
        let body = super::body(
            vec![
                Message::new(Role::User, "go"),
                Message {
                    tool_calls: Some(vec![call_a.clone()]),
                    ..Message::new(Role::Assistant, "let me think")
                },
            ],
            BodyInput {
                model: "test-model",
                stream: false,
                tool_specs: None,
                reasoning_effort: None,
                prompt_cache_key: None,
            },
        );
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert!(
            msgs[1]["content"]
                .as_str()
                .unwrap_or("")
                .contains("let me think")
        );
        assert!(
            msgs[1].get("tool_calls").is_none(),
            "unanswered calls must be stripped"
        );

        // --- Case 3: partially answered call set ---
        let body = super::body(
            vec![
                Message::new(Role::User, "go"),
                Message {
                    tool_calls: Some(vec![call_a.clone(), call_b.clone()]),
                    ..Message::new(Role::Assistant, "")
                },
                Message {
                    tool_call_id: Some("a".into()),
                    ..Message::new(Role::Tool, "result a")
                },
            ],
            BodyInput {
                model: "test-model",
                stream: false,
                tool_specs: None,
                reasoning_effort: None,
                prompt_cache_key: None,
            },
        );
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        let calls = msgs[1]["tool_calls"].as_array().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["id"], "a");

        // --- Case 4: multiple consecutive unanswered assistants ---
        let body = super::body(
            vec![
                Message::new(Role::User, "go"),
                Message {
                    tool_calls: Some(vec![call_a.clone()]),
                    ..Message::new(Role::Assistant, "first")
                },
                Message {
                    tool_calls: Some(vec![call_b.clone()]),
                    ..Message::new(Role::Assistant, "second")
                },
            ],
            BodyInput {
                model: "test-model",
                stream: false,
                tool_specs: None,
                reasoning_effort: None,
                prompt_cache_key: None,
            },
        );
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3, "both assistants kept for their content");
        assert!(msgs[1].get("tool_calls").is_none());
        assert!(msgs[2].get("tool_calls").is_none());

        // --- Case 5: fully healthy conversation (no stripping) ---
        let body = super::body(
            vec![
                Message::new(Role::User, "go"),
                Message {
                    tool_calls: Some(vec![call_a.clone(), call_c.clone()]),
                    ..Message::new(Role::Assistant, "")
                },
                Message {
                    tool_call_id: Some("a".into()),
                    ..Message::new(Role::Tool, "ok")
                },
                Message {
                    tool_call_id: Some("c".into()),
                    ..Message::new(Role::Tool, "ok")
                },
                Message::new(Role::Assistant, "all done"),
            ],
            BodyInput {
                model: "test-model",
                stream: false,
                tool_specs: None,
                reasoning_effort: None,
                prompt_cache_key: None,
            },
        );
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 5, "healthy conversation untouched");
        assert_eq!(msgs[1]["tool_calls"].as_array().unwrap().len(), 2);
        assert_eq!(msgs[4]["content"], "all done");
    }
}
