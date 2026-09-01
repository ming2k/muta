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

use muta_contracts::{Effort, Message, Role};
use serde_json::{Value, json};

/// Inputs to [`body`]: the model id, whether this is a streaming request, the
/// prepared tool schemas (OpenAI function-spec shape, optional), the optional
/// reasoning-effort override, and the ChatGPT account id.
pub struct BodyInput<'a> {
    pub model: &'a str,
    pub stream: bool,
    /// OpenAI-shaped tool specs (`{type:"function", function:{...}}`), if any.
    pub tool_specs: Option<&'a [muta_contracts::ToolSpec]>,
    pub reasoning_effort: Option<Effort>,
    pub delivery: &'a muta_contracts::RequestDelivery,
    pub store: bool,
    pub cache_plan: &'a muta_contracts::ResolvedCachePlan,
}

/// Build the Responses request body.
///
/// System messages are folded into the top-level `instructions` field; the
/// remaining conversation is projected to Responses `input` items. Tool calls
/// and results are reconciled so the request is always wire-valid: every
/// `function_call_output` references a preceding `function_call`, and every
/// `function_call` has its output (mirrors the chat-completions builder).
pub fn body(messages: Vec<Message>, input: BodyInput<'_>) -> Value {
    let capabilities = muta_contracts::ModelCapabilities::for_channel(input.model, None);
    body_with_capabilities(messages, input, &capabilities)
}

/// Build a request with a provider-channel capability view. The provider calls
/// this for trusted remote metadata; [`body`] remains the static-baseline entry
/// point for standalone callers and tests.
pub fn body_with_capabilities(
    messages: Vec<Message>,
    input: BodyInput<'_>,
    capabilities: &muta_contracts::ModelCapabilities,
) -> Value {
    let BodyInput {
        model: model_id,
        stream,
        tool_specs,
        reasoning_effort,
        delivery,
        store,
        cache_plan,
    } = input;

    // Fold system messages into `instructions`, strip images on non-vision
    // models (the Responses API rejects `input_image` on them).
    let mut instructions = String::new();
    let mut working: Vec<Message> = Vec::with_capacity(messages.len());
    for (index, mut m) in messages.into_iter().enumerate() {
        match m.role {
            Role::System => {
                if !instructions.is_empty() {
                    instructions.push('\n');
                }
                instructions.push_str(&m.content);
            }
            _ => {
                if let muta_contracts::RequestDelivery::RemoteContinuation { input_start, .. } =
                    delivery
                    && index < *input_start
                {
                    continue;
                }
                if !capabilities.vision {
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
                if matches!(delivery, muta_contracts::RequestDelivery::OpaqueReplay)
                    && let Some(items) = m
                        .provider_meta
                        .as_ref()
                        .and_then(|meta| {
                            meta.get(muta_contracts::OPENAI_RESPONSE_OUTPUT_ARTIFACT_KEY)
                        })
                        .and_then(Value::as_array)
                {
                    input_items.extend(items.iter().cloned());
                    continue;
                }
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
        // Responses persistence mode:
        // - On ChatGPT subscription backend (`chatgpt.com/backend-api/codex/responses`),
        //   the backend requires `store: false`.
        // - On the OpenAI platform Responses API (`api.openai.com`), `store: false`
        //   disables response retention on the server, while `store: true` retains
        //   response objects and enables `previous_response_id` continuation chains.
        "store": store,
    });
    if !store {
        // Stateless Responses continuation requires opaque reasoning items in
        // the returned output. The next turn replays that output verbatim.
        body["include"] = json!(["reasoning.encrypted_content"]);
    }
    if let muta_contracts::RequestDelivery::RemoteContinuation {
        previous_response_id,
        ..
    } = delivery
    {
        body["previous_response_id"] = json!(previous_response_id);
    }
    if !instructions.is_empty() {
        body["instructions"] = json!(instructions);
    }
    if let Some(specs) = flatten_tools(tool_specs) {
        body["tools"] = specs;
        body["tool_choice"] = json!("auto");
        body["parallel_tool_calls"] = json!(true);
    }
    // Reasoning: GPT-5 models always reason. Request the most verbose summaries
    // the backend offers (`detailed`) so the reasoning trace carries real
    // detail — the raw chain-of-thought is never exposed (OpenAI encrypts it as
    // `reasoning.encrypted_content`), so `detailed` is the ceiling. codex
    // exposes the same lever (`model_reasoning_summary: detailed`). Attach the
    // effort override when the model exposes effort levels.
    let mut reasoning = serde_json::Map::new();
    reasoning.insert("summary".to_string(), json!("detailed"));
    if let Some(effort) = reasoning_effort
        && !capabilities.effort_levels.is_empty()
    {
        reasoning.insert(
            "effort".to_string(),
            json!(effort.clamp_to_levels(&capabilities.effort_levels).as_str()),
        );
    }
    body["reasoning"] = Value::Object(reasoning);
    super::super::cache::project_responses_instructions_for_explicit_mode(&mut body, cache_plan);
    super::super::cache::apply(&mut body, cache_plan, "input");
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
                "image_url": format!("data:{};base64,{}", image.mime, image.data)
            }));
        }
    }
    json!({ "type": "message", "role": role, "content": parts })
}

/// Flatten the OpenAI function-spec tool shape (`{type:"function",
/// Translate provider-neutral [`ToolSpec`]s into the OpenAI Responses tool
/// shape (`{type:"function", name, description, parameters, strict:false}`).
fn flatten_tools(tool_specs: Option<&[muta_contracts::ToolSpec]>) -> Option<Value> {
    let specs = tool_specs?;
    let out: Vec<Value> = specs
        .iter()
        .map(|spec| {
            json!({
                "type": "function",
                "name": spec.name,
                "description": spec.description,
                "parameters": spec.parameters.clone(),
                "strict": false,
            })
        })
        .collect();
    if out.is_empty() {
        None
    } else {
        Some(Value::Array(out))
    }
}

/// The per-request auth + provider headers for the Responses surface. Beyond
/// the always-present `Authorization: Bearer`:
/// - **ChatGPT/Codex mode** (`chatgpt == true`): `originator: muta` identifies
///   the client and the ChatGPT account id is attached as
///   `ChatGPT-Account-Id` when known.
/// - **Copilot mode** (`copilot == true`): GitHub Copilot's required headers
///   are attached instead — the client-identity headers
///   (`Copilot-Integration-Id`, `Editor-Version`, `Editor-Plugin-Version`)
///   that let the backend resolve the account's actual plan entitlements,
///   plus the per-turn headers `x-initiator` (treated as a user-initiated
///   turn by default; the harness does not currently distinguish agent
///   turns), `Openai-Intent: conversation-edits`, and `X-GitHub-Api-Version`.
///   The ChatGPT account-id header is omitted. Vision turns additionally need
///   `Copilot-Vision-Request: true`, but that depends on the request body
///   (whether an `input_image` part is present), so it is injected by the
///   provider's request builder (see [`has_input_image`]) rather than this
///   header list.
pub fn headers(
    access_token: &str,
    account_id: Option<&str>,
    copilot: bool,
    chatgpt: bool,
) -> Vec<(&'static str, String)> {
    let mut h = vec![("Authorization", format!("Bearer {access_token}"))];
    if copilot {
        for (name, value) in crate::COPILOT_CLIENT_HEADERS {
            h.push((*name, value.to_string()));
        }
        h.push(("x-initiator", "user".to_string()));
        h.push(("Openai-Intent", "conversation-edits".to_string()));
        h.push(("X-GitHub-Api-Version", "2026-06-01".to_string()));
        return h;
    }
    if !chatgpt {
        return h;
    }
    h.push(("originator", "muta".to_string()));
    if let Some(id) = account_id.filter(|id| !id.trim().is_empty()) {
        h.push(("ChatGPT-Account-Id", id.to_string()));
    }
    h
}

/// Whether a Responses request body carries an image input, i.e. any
/// `input_image` content part anywhere in the `input` items array. Copilot
/// requires `Copilot-Vision-Request: true` on such turns; this scan is the
/// signal the provider's request builder uses to set it. Defensive: a missing
/// or non-array `input` is treated as no image.
pub fn has_input_image(body: &Value) -> bool {
    body.get("input")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items.iter().any(|item| {
                item.get("content")
                    .and_then(Value::as_array)
                    .is_some_and(|parts| {
                        parts.iter().any(|part| {
                            part.get("type").and_then(Value::as_str) == Some("input_image")
                        })
                    })
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use muta_contracts::ToolCall;

    static DEFAULT_DELIVERY: muta_contracts::RequestDelivery =
        muta_contracts::RequestDelivery::FullReplay;
    static DEFAULT_CACHE_PLAN: muta_contracts::ResolvedCachePlan =
        muta_contracts::ResolvedCachePlan::Unsupported;

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
                delivery: &DEFAULT_DELIVERY,
                store: false,
                cache_plan: &DEFAULT_CACHE_PLAN,
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
                delivery: &DEFAULT_DELIVERY,
                store: false,
                cache_plan: &DEFAULT_CACHE_PLAN,
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
                delivery: &DEFAULT_DELIVERY,
                store: false,
                cache_plan: &DEFAULT_CACHE_PLAN,
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
                delivery: &DEFAULT_DELIVERY,
                store: false,
                cache_plan: &DEFAULT_CACHE_PLAN,
            },
        );
        // The most verbose summaries the backend offers — the raw chain of
        // thought is never exposed, so `detailed` is the ceiling.
        assert_eq!(body["reasoning"]["summary"], "detailed");
        assert_eq!(body["reasoning"]["effort"], "medium");
    }

    #[test]
    fn stateless_requests_preserve_encrypted_reasoning_and_parallel_tools() {
        let request = body_with_capabilities(
            vec![Message::new(Role::User, "hi")],
            BodyInput {
                model: "gpt-5.6-sol",
                tool_specs: Some(&[muta_contracts::ToolSpec {
                    name: "lookup".to_string(),
                    description: "lookup".to_string(),
                    parameters: serde_json::json!({"type": "object"}),
                }]),
                stream: true,
                reasoning_effort: None,
                delivery: &muta_contracts::RequestDelivery::OpaqueReplay,
                store: false,
                cache_plan: &muta_contracts::ResolvedCachePlan::Unsupported,
            },
            &muta_contracts::ModelCapabilities::for_channel("gpt-5.6-sol", None),
        );
        assert_eq!(
            request["include"],
            serde_json::json!(["reasoning.encrypted_content"])
        );
        assert_eq!(request["parallel_tool_calls"], true);
    }

    #[test]
    fn headers_include_account_id_when_present() {
        let h = headers("tok", Some("acct-1"), false, true);
        assert_eq!(h[0].0, "Authorization");
        assert_eq!(h[1], ("originator", "muta".to_string()));
        assert_eq!(h[2].0, "ChatGPT-Account-Id");
        assert_eq!(h[2].1, "acct-1");
        // No account id → no header.
        assert_eq!(headers("tok", None, false, true).len(), 2);
        // A third-party Responses endpoint gets neither Codex header.
        assert_eq!(headers("tok", Some("acct-1"), false, false).len(), 1);
    }

    #[test]
    fn headers_inject_copilot_set_and_drop_account_id() {
        // Copilot mode: the account id is ignored and Copilot's required
        // headers replace the ChatGPT account-id header.
        let h = headers("tok", Some("acct-1"), true, false);
        assert_eq!(h[0].0, "Authorization");
        assert_eq!(h[0].1, "Bearer tok");
        let names: Vec<&str> = h.iter().map(|(n, _)| *n).collect();
        assert_eq!(
            names,
            [
                "Authorization",
                "Copilot-Integration-Id",
                "Editor-Version",
                "Editor-Plugin-Version",
                "x-initiator",
                "Openai-Intent",
                "X-GitHub-Api-Version"
            ]
        );
        assert_eq!(h[1].1, "vscode-chat");
        assert_eq!(h[2].1, "vscode/1.107.0");
        assert_eq!(h[3].1, "copilot-chat/0.35.0");
        assert_eq!(h[4].1, "user");
        assert_eq!(h[5].1, "conversation-edits");
        assert_eq!(h[6].1, "2026-06-01");
        // No ChatGPT-Account-Id leaks through in Copilot mode.
        assert!(h.iter().all(|(n, _)| *n != "ChatGPT-Account-Id"));
    }

    #[test]
    fn has_input_image_detects_image_parts() {
        let with_image = json!({
            "input": [
                {"role": "user", "content": [
                    {"type": "input_text", "text": "what is this?"},
                    {"type": "input_image", "image_url": "data:..."}
                ]}
            ]
        });
        assert!(has_input_image(&with_image));

        let text_only = json!({
            "input": [{"role": "user", "content": [{"type": "input_text", "text": "hi"}]}]
        });
        assert!(!has_input_image(&text_only));

        // Defensive: a missing or non-array input is no image.
        assert!(!has_input_image(&json!({"model": "x"})));
        assert!(!has_input_image(&json!({"input": "not-an-array"})));
    }

    #[test]
    fn user_message_with_image_serializes_as_input_image_string_url() {
        let mut msg = Message::new(Role::User, "look at this");
        msg.images = Some(vec![muta_contracts::message::ImagePart {
            mime: "image/png".to_string(),
            data: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=".to_string(),
        }]);

        let remote = muta_contracts::RemoteModelMetadata {
            vision: Some(true),
            ..Default::default()
        };
        let caps = muta_contracts::ModelCapabilities::for_channel("gpt-4o", Some(&remote));

        let payload = body_with_capabilities(
            vec![msg],
            BodyInput {
                model: "gpt-4o",
                stream: false,
                tool_specs: None,
                reasoning_effort: None,
                delivery: &DEFAULT_DELIVERY,
                store: false,
                cache_plan: &DEFAULT_CACHE_PLAN,
            },
            &caps,
        );

        let input = payload["input"].as_array().expect("input array");
        assert_eq!(input.len(), 1);
        let content = input[0]["content"].as_array().expect("content array");
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(content[0]["text"], "look at this");

        assert_eq!(content[1]["type"], "input_image");
        assert_eq!(
            content[1]["image_url"],
            "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII="
        );
    }
}
