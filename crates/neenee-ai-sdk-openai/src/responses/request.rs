//! OpenAI **Responses** API — request construction.
//!
//! Pure functions turning the harness's `Vec<Message>` into the Responses
//! request shape: top-level `instructions` (the system prompt) + an `input`
//! array of typed items (`message`, `function_call`, `function_call_output`),
//! plus optional `tools`/`reasoning`. No `reqwest`, no `async`, no I/O.
//!
//! Responses wire shape (ChatGPT subscription backend):
//! - URL: the `/responses` endpoint (e.g. `https://chatgpt.com/backend-api/codex/responses`).
//! - Auth: `Authorization: Bearer <oauth access_token>` plus the optional
//!   `ChatGPT-Account-Id` header (applied by the provider, not here).
//! - Body: `{model, instructions?, input, tools?, reasoning?, tool_choice?, stream}`.

use neenee_core::{Effort, Message, Role};
use serde_json::{Value, json};

/// Inputs to [`body`]: the model id, whether this is a streaming request, the
/// prepared tool schemas (OpenAI function-spec shape, optional), the optional
/// reasoning-effort override, and the ChatGPT account id.
pub struct BodyInput<'a> {
    pub model: &'a str,
    pub stream: bool,
    /// OpenAI-shaped tool specs (`{type:"function", function:{...}}`), if any.
    pub tool_specs: Option<&'a [Value]>,
    pub reasoning_effort: Option<Effort>,
}

/// Build the Responses request body.
///
/// System messages are folded into the top-level `instructions` field; the
/// remaining conversation is projected to Responses `input` items. Tool calls
/// and results are reconciled so the request is always wire-valid: every
/// `function_call_output` references a preceding `function_call`, and every
/// `function_call` has its output (mirrors the chat-completions builder).
pub fn body(messages: Vec<Message>, input: BodyInput<'_>) -> Value {
    let BodyInput {
        model: model_id,
        stream,
        tool_specs,
        reasoning_effort,
    } = input;

    let model = neenee_core::model::resolve(model_id);

    // Fold system messages into `instructions`, strip images on non-vision
    // models (the Responses API rejects `input_image` on them).
    let mut instructions = String::new();
    let mut working: Vec<Message> = Vec::with_capacity(messages.len());
    for mut m in messages {
        match m.role {
            Role::System => {
                if !instructions.is_empty() {
                    instructions.push('\n');
                }
                instructions.push_str(&m.content);
            }
            _ => {
                if !model.vision {
                    m.images = None;
                }
                working.push(m);
            }
        }
    }

    // Reconcile tool calls/results so the request is wire-valid (same rules as
    // the chat-completions builder): drop orphan tool results and strip
    // unanswered tool calls.
    let mut known_ids = std::collections::HashSet::new();
    let working: Vec<Message> = working
        .into_iter()
        .filter(valid_message)
        .filter(|message| match message.role {
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
        })
        .collect();

    let answered: std::collections::HashSet<String> = working
        .iter()
        .filter_map(|m| {
            if m.role == Role::Tool {
                m.tool_call_id.clone()
            } else {
                None
            }
        })
        .collect();

    let mut input_items: Vec<Value> = Vec::new();
    for mut m in working {
        match m.role {
            Role::User => {
                input_items.push(message_item("user", &m, "input_text"));
            }
            Role::Assistant => {
                // Strip unanswered tool calls from this assistant turn.
                if let Some(calls) = m.tool_calls.as_mut() {
                    calls.retain(|c| answered.contains(&c.id));
                }
                // Emit an assistant message item for any text content...
                if !m.content.is_empty() {
                    input_items.push(message_item("assistant", &m, "output_text"));
                }
                // ...then one function_call item per surviving call, in order.
                // The Responses API requires a function_call item to precede its
                // function_call_output, and interleaves them with the message
                // flow, so emitting them right after the assistant text matches.
                if let Some(calls) = m.tool_calls.as_ref() {
                    for call in calls {
                        input_items.push(json!({
                            "type": "function_call",
                            "call_id": call.id,
                            "name": call.name,
                            "arguments": call.arguments,
                        }));
                    }
                }
            }
            Role::Tool => {
                let call_id = m.tool_call_id.clone().unwrap_or_default();
                input_items.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": m.content,
                }));
            }
            Role::System => {} // folded into instructions above
        }
    }

    let mut body = json!({
        "model": model_id,
        "input": input_items,
        "stream": stream,
        // The ChatGPT subscription backend (chatgpt.com/backend-api/codex/
        // responses) refuses to persist responses and rejects the request with
        // `{"detail":"Store must be set to false"}` unless this is explicitly
        // false. The platform Responses API (api.openai.com) ignores it.
        "store": false,
    });
    if !instructions.is_empty() {
        body["instructions"] = json!(instructions);
    }
    if let Some(specs) = flatten_tools(tool_specs) {
        body["tools"] = specs;
        body["tool_choice"] = json!("auto");
    }
    // Reasoning: GPT-5 models always reason. Request summaries so the reasoning
    // trace streams back (clean, unlike relay-reconstructed `reasoning_content`),
    // and attach the effort override when the model exposes effort levels.
    let mut reasoning = serde_json::Map::new();
    reasoning.insert("summary".to_string(), json!("auto"));
    if let Some(effort) = reasoning_effort
        && !model.effort_levels.is_empty()
    {
        reasoning.insert(
            "effort".to_string(),
            json!(effort.clamp_to(model.effort_levels).as_str()),
        );
    }
    body["reasoning"] = Value::Object(reasoning);
    body
}

/// Discard assistant turns that are completely empty (no content, no tool
/// calls). System messages never reach here (folded into `instructions`).
fn valid_message(message: &Message) -> bool {
    if message.role == Role::Assistant {
        let empty = message.content.is_empty()
            && message
                .tool_calls
                .as_ref()
                .map(|calls| calls.is_empty())
                .unwrap_or(true);
        return !empty;
    }
    true
}

/// Build a Responses `message` item: `{type:"message", role, content:[...]}`.
/// `text_part` selects `input_text` (user) vs `output_text` (assistant). Inline
/// images become `input_image` parts.
fn message_item(role: &str, m: &Message, text_part: &str) -> Value {
    let mut parts: Vec<Value> = Vec::new();
    if !m.content.is_empty() {
        parts.push(json!({ "type": text_part, "text": m.content }));
    }
    if let Some(images) = m.images.as_ref() {
        for image in images {
            parts.push(json!({
                "type": "input_image",
                "image_url": { "url": format!("data:{};base64,{}", image.mime, image.data) }
            }));
        }
    }
    json!({ "type": "message", "role": role, "content": parts })
}

/// Flatten the OpenAI function-spec tool shape (`{type:"function",
/// function:{name,description,parameters}}`) into the Responses tool shape
/// (`{type:"function", name, description, parameters, strict:false}`). Tolerant
/// of either nesting so the shared `to_openai_function()` output works as-is.
fn flatten_tools(tool_specs: Option<&[Value]>) -> Option<Value> {
    let specs = tool_specs?;
    let out: Vec<Value> = specs
        .iter()
        .filter_map(|spec| {
            let func = spec.get("function").unwrap_or(spec);
            let name = func.get("name")?.as_str()?;
            let description = func
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let parameters = func
                .get("parameters")
                .cloned()
                .unwrap_or(json!({"type":"object"}));
            Some(json!({
                "type": "function",
                "name": name,
                "description": description,
                "parameters": parameters,
                "strict": false,
            }))
        })
        .collect();
    if out.is_empty() {
        None
    } else {
        Some(Value::Array(out))
    }
}

/// The per-request auth + account headers for the Responses surface. Beyond the
/// always-present `User-Agent`, the bearer is required and the ChatGPT account
/// id is attached when known.
pub fn headers(access_token: &str, account_id: Option<&str>) -> Vec<(&'static str, String)> {
    let mut h = vec![("Authorization", format!("Bearer {access_token}"))];
    if let Some(id) = account_id.filter(|id| !id.trim().is_empty()) {
        h.push(("ChatGPT-Account-Id", id.to_string()));
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use neenee_core::ToolCall;

    fn assistant_with_call(call: ToolCall, content: &str) -> Message {
        Message {
            tool_calls: Some(vec![call]),
            ..Message::new(Role::Assistant, content)
        }
    }

    #[test]
    fn system_messages_fold_into_instructions() {
        let body = body(
            vec![
                Message::new(Role::System, "be concise"),
                Message::new(Role::User, "hi"),
            ],
            BodyInput {
                model: "gpt-5.6-sol",
                stream: true,
                tool_specs: None,
                reasoning_effort: None,
            },
        );
        assert_eq!(body["instructions"], "be concise");
        let input = body["input"].as_array().unwrap();
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0]["type"], "input_text");
    }

    #[test]
    fn function_call_and_output_round_trip_as_items() {
        let call = ToolCall {
            id: "call_1".into(),
            name: "bash".into(),
            arguments: "{\"command\":\"ls\"}".into(),
        };
        let result = Message {
            tool_call_id: Some("call_1".into()),
            ..Message::new(Role::Tool, "file.txt")
        };
        let body = body(
            vec![
                Message::new(Role::User, "list files"),
                assistant_with_call(call, "running it"),
                result,
            ],
            BodyInput {
                model: "gpt-5.6-sol",
                stream: true,
                tool_specs: None,
                reasoning_effort: None,
            },
        );
        let input = body["input"].as_array().unwrap();
        // user, assistant(message), function_call, function_call_output.
        assert_eq!(input.len(), 4);
        assert_eq!(input[2]["type"], "function_call");
        assert_eq!(input[2]["call_id"], "call_1");
        assert_eq!(input[3]["type"], "function_call_output");
        assert_eq!(input[3]["output"], "file.txt");
    }

    #[test]
    fn unanswered_function_calls_are_stripped() {
        let call = ToolCall {
            id: "call_x".into(),
            name: "bash".into(),
            arguments: "{}".into(),
        };
        let body = body(
            vec![
                Message::new(Role::User, "go"),
                assistant_with_call(call, "thinking"),
            ],
            BodyInput {
                model: "gpt-5.6-sol",
                stream: false,
                tool_specs: None,
                reasoning_effort: None,
            },
        );
        let input = body["input"].as_array().unwrap();
        // Only user + assistant message survive; the unanswered call is dropped.
        assert_eq!(input.len(), 2);
        assert!(input.iter().all(|i| i["type"] != "function_call"));
    }

    #[test]
    fn reasoning_summary_always_requested_with_effort_when_supported() {
        let body = body(
            vec![Message::new(Role::User, "hi")],
            BodyInput {
                model: "gpt-5.6-sol",
                stream: true,
                tool_specs: None,
                reasoning_effort: Some(Effort::Medium),
            },
        );
        assert_eq!(body["reasoning"]["summary"], "auto");
        assert_eq!(body["reasoning"]["effort"], "medium");
    }

    #[test]
    fn headers_include_account_id_when_present() {
        let h = headers("tok", Some("acct-1"));
        assert_eq!(h[0].0, "Authorization");
        assert_eq!(h[1].0, "ChatGPT-Account-Id");
        assert_eq!(h[1].1, "acct-1");
        // No account id → no header.
        assert_eq!(headers("tok", None).len(), 1);
    }
}
