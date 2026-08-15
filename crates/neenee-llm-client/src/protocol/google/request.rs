//! Google Google — request construction.
//!
//! Pure functions that turn the harness's `Vec<Message>` into the Google
//! `generateContent` request shape: a `contents` array of `{role, parts}`,
//! with `systemInstruction` lifted to the top level and OpenAI-shaped tool
//! schemas converted to Google `functionDeclarations`. No `reqwest`, no
//! `async`, no I/O — these are testable in isolation.
//!
//! Google's wire shape:
//! - URL: `{base}/models/{model}:generateContent?key={key}`
//!   (streaming: `:streamGenerateContent?alt=sse&key={key}`). The key is a
//!   query param, never a header — unlike OpenAI/Anthropic.
//! - Body:
//!   `{contents: [{role, parts}], systemInstruction?: {parts:[{text}]}, tools?}`.
//! - Roles are only `user` and `model` (assistant). Tool calls are assistant
//!   `functionCall` parts; tool results are user `functionResponse` parts.
//! - Auth: none beyond the `?key=` query param.

use neenee_contracts::{Message, Role};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

use super::response::{TEXT_THOUGHT_SIGNATURE_META_KEY, THOUGHT_SIGNATURES_META_KEY};

/// The resolved reasoning-depth directive to stamp into `thinkingConfig` — the
/// wire form of a channel's [`neenee_contracts::Effort`] override once it has been
/// clamped and translated for this Gemini model.
///
/// Gemini exposes **two mutually exclusive** depth surfaces, so a single field
/// is insufficient and the request never sends both (doing so is a 400):
/// - [`GoogleThinking::Level`] — Gemini 3.x `thinkingLevel` enum.
/// - [`GoogleThinking::Budget`] — Gemini 2.5 `thinkingBudget` integer tokens.
///
/// `None` here means "no override; let the server default stand" and is
/// distinct from an explicit off (Gemini 2.5 Flash honors a `0` budget; Gemini
/// 3.x cannot disable thinking at all, so there is no off path for it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoogleThinking {
    /// `thinkingLevel` for a Gemini 3.x model: one of `minimal`/`low`/
    /// `medium`/`high`. Never `none`/`xhigh`/`max` (Gemini 3.x rejects them);
    /// the resolver clamps those down before constructing this variant.
    Level(neenee_contracts::Effort),
    /// `thinkingBudget` for a Gemini 2.5 model: a token count in the model's
    /// supported range. `0` disables thinking (Flash/Lite only — Pro's floor is
    /// `128`, so the resolver only emits `0` when the model's ladder allows
    /// [`neenee_contracts::Effort::None`]).
    Budget(i64),
}

/// Resolve a channel's raw reasoning-effort override into the wire directive
/// this Gemini model expects, clamping it to the model's supported levels.
///
/// The model's **effort ladder** decides which surface applies: a
/// `thinkingLevel` ladder ([`neenee_contracts::effort::EFFORT_GEMINI_LEVEL`]) maps each rung
/// to a level enum; a `thinkingBudget` ladder
/// ([`neenee_contracts::effort::EFFORT_GEMINI_BUDGET`]) maps each rung to a token bucket
/// (the bucket cap is `max_budget`, e.g. `24576` for Gemini 2.5 Flash or
/// `32768` for Pro). Returns `None` when there is nothing to stamp — an empty
/// ladder (non-reasoning / unknown model) or an unset override — leaving the
/// server default in place.
///
/// `max_budget` is only consulted for the budget surface; pass any value for a
/// level-only model.
pub fn resolve_thinking(
    effort: Option<neenee_contracts::Effort>,
    effort_levels: &[neenee_contracts::EffortLevel],
    max_budget: u32,
) -> Option<GoogleThinking> {
    // A model that does not advertise a depth ladder has no thinking surface to
    // stamp (non-reasoning models like Gemini 2.5 Flash-Lite / 2.0 Flash, or an
    // unknown model with a default empty ladder). Fall back to the server
    // default rather than guessing a form the upstream may reject.
    if effort_levels.is_empty() {
        return None;
    }
    let requested = effort?;
    // Gemini's ladders are always compiled-in known rungs, so extract the known
    // subset for ranking; `Other` (a provider tier outside the vocabulary) has
    // no Gemini surface to map onto and is ignored here.
    let known: Vec<neenee_contracts::Effort> = effort_levels
        .iter()
        .filter_map(neenee_contracts::EffortLevel::as_known)
        .collect();
    let clamped = requested.clamp_to(&known);
    // Decide the surface from the ladder's deepest rung: the budget ladder tops
    // out at `max`, the level ladder at `high`. This keeps the model-specific
    // choice in one place and never inspects the free-form model id string.
    let is_budget = known.contains(&neenee_contracts::Effort::Max);
    Some(if is_budget {
        GoogleThinking::Budget(clamped.gemini_thinking_budget(max_budget))
    } else {
        GoogleThinking::Level(clamped)
    })
}

/// Inputs to [`body`]: the prepared tool schemas in OpenAI function-spec
/// shape, if any.
pub struct BodyInput<'a> {
    pub tool_specs: Option<&'a [neenee_contracts::ToolSpec]>,
    /// Stamp `generationConfig.thinkingConfig.includeThoughts`. The Google API
    /// only returns reasoning *text* when the request asks for it, and rejects
    /// the flag with HTTP 400 on models that do not think — so this MUST be
    /// `true` only for reasoning models (`model.thinking.reasons()`).
    pub include_thoughts: bool,
    /// Resolved reasoning-depth directive for this model, or `None` to leave
    /// the server default in place. Built by [`resolve_thinking`] from the
    /// channel's effort override and the model's ladder; never carries both a
    /// level and a budget (Gemini 400s on that combination).
    pub thinking: Option<GoogleThinking>,
}

/// Build the Google `generateContent` request body from a message list.
pub fn body(messages: Vec<Message>, input: BodyInput<'_>) -> Value {
    let mut system = Vec::new();
    let mut contents: Vec<Value> = Vec::new();
    let mut call_names: HashMap<String, String> = HashMap::new();
    let messages = reconcile_tool_history(messages);

    for message in messages {
        if message.role == Role::System {
            system.push(message.content);
            continue;
        }

        let role = if message.role == Role::Assistant {
            "model"
        } else {
            "user"
        };
        let new_parts = message_parts(message, &mut call_names);

        // Coalesce consecutive same-role turns into one `contents` entry:
        // Google rejects two adjacent `user` objects, so a tool-result-then-user
        // pair must merge into a single `user` with two parts.
        if let Some(previous) = contents.last_mut()
            && previous.get("role").and_then(Value::as_str) == Some(role)
            && let Some(parts) = previous.get_mut("parts").and_then(Value::as_array_mut)
        {
            parts.extend(new_parts);
            continue;
        }
        contents.push(json!({
            "role": role,
            "parts": new_parts
        }));
    }

    let mut body = json!({ "contents": contents });
    if !system.is_empty() {
        body["systemInstruction"] = json!({
            "parts": [{ "text": system.join("\n\n") }]
        });
    }
    if let Some(tools) = google_tools(input.tool_specs) {
        body["tools"] = tools;
    }
    // Request the model's reasoning *text* so it can be routed to
    // `ReasoningDelta` / `reasoning_content` instead of being withheld. The
    // flag is only stamped for reasoning models — the API 400s otherwise.
    if input.include_thoughts {
        body["generationConfig"]["thinkingConfig"]["includeThoughts"] = json!(true);
    }
    // Reasoning depth — the resolved surface for this Gemini model, clamped to
    // its supported levels. Level (Gemini 3.x) and budget (Gemini 2.5) are
    // mutually exclusive on the wire, so only one is ever stamped. Both live
    // under `thinkingConfig` alongside `includeThoughts`.
    if let Some(thinking) = input.thinking {
        match thinking {
            GoogleThinking::Level(effort) => {
                body["generationConfig"]["thinkingConfig"]["thinkingLevel"] =
                    json!(effort.as_str());
            }
            GoogleThinking::Budget(tokens) => {
                body["generationConfig"]["thinkingConfig"]["thinkingBudget"] = json!(tokens);
            }
        }
    }
    body
}

/// Keep only tool-call history Google can replay: tool results must reference
/// a known previous assistant call, and assistant calls must have a result.
fn reconcile_tool_history(messages: Vec<Message>) -> Vec<Message> {
    let mut known_ids = HashSet::new();
    let mut messages: Vec<Message> = messages
        .into_iter()
        .filter(|message| match message.role {
            Role::Assistant => {
                if let Some(calls) = message.tool_calls.as_ref() {
                    for call in calls {
                        known_ids.insert(call.id.clone());
                    }
                }
                !assistant_is_empty(message)
            }
            Role::Tool => message
                .tool_call_id
                .as_ref()
                .is_some_and(|id| !id.is_empty() && known_ids.contains(id)),
            _ => true,
        })
        .collect();

    let answered: HashSet<String> = messages
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
            calls.retain(|call| answered.contains(&call.id));
            if calls.is_empty() {
                m.tool_calls = None;
            }
        }
        !assistant_is_empty(m)
    });
    messages
}

fn assistant_is_empty(message: &Message) -> bool {
    message.content.is_empty()
        && message.images.as_ref().is_none_or(Vec::is_empty)
        && message.tool_calls.as_ref().is_none_or(Vec::is_empty)
}

fn message_parts(message: Message, call_names: &mut HashMap<String, String>) -> Vec<Value> {
    match message.role {
        Role::Assistant => assistant_parts(message, call_names),
        Role::Tool => tool_result_parts(message, call_names),
        _ => user_parts(message),
    }
}

fn user_parts(message: Message) -> Vec<Value> {
    let mut parts = text_and_image_parts(message.content, message.images.unwrap_or_default(), true);
    if parts.is_empty() {
        parts.push(json!({ "text": "" }));
    }
    parts
}

fn assistant_parts(message: Message, call_names: &mut HashMap<String, String>) -> Vec<Value> {
    let thought_signatures = thought_signatures_by_call(&message);
    let text_thought_signature = text_thought_signature(&message);
    let mut parts =
        text_and_image_parts(message.content, message.images.unwrap_or_default(), false);
    if let Some(signature) = text_thought_signature
        && let Some(part) = parts
            .iter_mut()
            .find(|part| part.get("text").and_then(Value::as_str).is_some())
    {
        part["thoughtSignature"] = json!(signature);
    }
    if let Some(calls) = message.tool_calls {
        for call in calls {
            call_names.insert(call.id.clone(), call.name.clone());
            let mut function_call = json!({
                "name": call.name,
                "args": parse_json_object(&call.arguments),
            });
            if !call.id.is_empty() {
                function_call["id"] = json!(call.id);
            }
            let mut part = json!({ "functionCall": function_call });
            if let Some(signature) = thought_signatures.get(&call.id) {
                part["thoughtSignature"] = json!(signature);
            }
            parts.push(part);
        }
    }
    if parts.is_empty() {
        parts.push(json!({ "text": "" }));
    }
    parts
}

fn text_thought_signature(message: &Message) -> Option<String> {
    message
        .provider_meta
        .as_ref()
        .and_then(|meta| meta.get(TEXT_THOUGHT_SIGNATURE_META_KEY))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn thought_signatures_by_call(message: &Message) -> HashMap<String, String> {
    message
        .provider_meta
        .as_ref()
        .and_then(|meta| meta.get(THOUGHT_SIGNATURES_META_KEY))
        .and_then(Value::as_object)
        .map(|signatures| {
            signatures
                .iter()
                .filter_map(|(id, signature)| {
                    signature
                        .as_str()
                        .map(|signature| (id.clone(), signature.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn tool_result_parts(message: Message, call_names: &HashMap<String, String>) -> Vec<Value> {
    let id = message.tool_call_id.unwrap_or_default();
    let name = call_names.get(&id).cloned().unwrap_or_default();
    let mut function_response = json!({
        "name": name,
        "response": tool_response_payload(&message.content),
    });
    if !id.is_empty() {
        function_response["id"] = json!(id);
    }
    vec![json!({ "functionResponse": function_response })]
}

fn text_and_image_parts(
    text: String,
    images: Vec<neenee_contracts::ImagePart>,
    include_empty_text: bool,
) -> Vec<Value> {
    let mut parts = Vec::new();
    if !text.is_empty() || (include_empty_text && images.is_empty()) {
        parts.push(json!({ "text": text }));
    }
    for image in images {
        parts.push(json!({
            "inline_data": {
                "mime_type": image.mime,
                "data": image.data,
            }
        }));
    }
    parts
}

fn google_tools(tool_specs: Option<&[neenee_contracts::ToolSpec]>) -> Option<Value> {
    let specs = tool_specs?;
    if specs.is_empty() {
        return None;
    }
    let declarations = specs
        .iter()
        .map(|spec| {
            json!({
                "name": spec.name,
                "description": spec.description,
                "parameters": spec.parameters.clone(),
            })
        })
        .collect::<Vec<_>>();
    Some(json!([{ "functionDeclarations": declarations }]))
}

fn parse_json_object(text: &str) -> Value {
    if text.trim().is_empty() {
        return json!({});
    }
    match serde_json::from_str::<Value>(text) {
        Ok(value) if value.is_object() => value,
        _ => json!({}),
    }
}

fn tool_response_payload(text: &str) -> Value {
    match serde_json::from_str::<Value>(text) {
        Ok(value) if value.is_object() => value,
        Ok(value) => json!({ "result": value }),
        Err(_) => json!({ "content": text }),
    }
}

/// The non-streaming generateContent URL: key passed as a query param.
pub fn url(base_url: &str, model: &str, api_key: &str) -> String {
    format!("{base_url}/models/{model}:generateContent?key={api_key}")
}

/// The streaming streamGenerateContent URL (SSE framing via `alt=sse`).
pub fn stream_url(base_url: &str, model: &str, api_key: &str) -> String {
    format!("{base_url}/models/{model}:streamGenerateContent?alt=sse&key={api_key}")
}

/// The maximum `thinkingBudget` a Gemini 2.5 model accepts, in tokens — the cap
/// [`neenee_contracts::Effort::gemini_thinking_budget`] buckets against. Gemini 2.5 Flash tops
/// out at `24576`; Pro at `32768`; Flash-Lite (floor `512`) also at `24576`.
/// Any other model id (Gemini 3.x, non-reasoning) returns `0`, signaling "no
/// budget surface" so [`resolve_thinking`] never constructs a `Budget` for it.
///
/// Kept free-form on the model id rather than a registry field because the cap
/// is an intrinsic property of the model family, not a capability the static
/// `Model` carries today, and it only matters for the 2.5 budget path.
pub fn max_thinking_budget(model: &str) -> u32 {
    if model.starts_with("gemini-2.5-pro") {
        32768
    } else if model.starts_with("gemini-2.5") {
        // 2.5-flash and 2.5-flash-lite share the 24576 cap.
        24576
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_system_harness_context() {
        let body = body(
            vec![
                Message::new(Role::System, "pursuit and tools"),
                Message::new(Role::User, "continue"),
            ],
            BodyInput {
                tool_specs: None,
                include_thoughts: false,
                thinking: None,
            },
        );

        assert_eq!(
            body["systemInstruction"]["parts"][0]["text"],
            "pursuit and tools"
        );
        assert_eq!(body["contents"][0]["role"], "user");
    }

    #[test]
    fn fallback_tool_results_are_user_context() {
        let body = body(
            vec![
                Message::new(Role::Assistant, "{\"tool\":\"read_text\"}"),
                Message::new(Role::Tool, "file contents"),
                Message::new(Role::User, "next"),
            ],
            BodyInput {
                tool_specs: None,
                include_thoughts: false,
                thinking: None,
            },
        );

        assert_eq!(body["contents"][1]["role"], "user");
        assert_eq!(body["contents"][1]["parts"][0]["text"], "next");
    }

    #[test]
    fn declares_google_function_tools() {
        let body = body(
            vec![Message::new(Role::User, "list files")],
            BodyInput {
                tool_specs: Some(&[neenee_contracts::ToolSpec {
                    name: "list_dir".to_string(),
                    description: "List files".to_string(),
                    parameters: json!({
                        "type": "object",
                        "properties": {"path": {"type": "string"}},
                        "required": ["path"]
                    }),
                }]),
                include_thoughts: false,
                thinking: None,
            },
        );

        assert_eq!(
            body["tools"][0]["functionDeclarations"][0]["name"],
            "list_dir"
        );
        assert_eq!(
            body["tools"][0]["functionDeclarations"][0]["parameters"]["properties"]["path"]["type"],
            "string"
        );
    }

    #[test]
    fn include_thoughts_stamps_thinking_config() {
        let body = body(
            vec![Message::new(Role::User, "why is the sky blue?")],
            BodyInput {
                tool_specs: None,
                include_thoughts: true,
                thinking: None,
            },
        );
        // Reasoning text is only surfaced when explicitly requested, and the
        // flag lives under generationConfig.thinkingConfig.
        assert_eq!(
            body["generationConfig"]["thinkingConfig"]["includeThoughts"],
            true
        );
    }

    #[test]
    fn include_thoughts_off_omits_thinking_config() {
        let body = body(
            vec![Message::new(Role::User, "hi")],
            BodyInput {
                tool_specs: None,
                include_thoughts: false,
                thinking: None,
            },
        );
        assert!(body.get("generationConfig").is_none());
    }

    #[test]
    fn replays_tool_calls_and_results_as_function_parts() {
        let call = neenee_contracts::ToolCall {
            id: "call_1".to_string(),
            name: "list_dir".to_string(),
            arguments: r#"{"path":"."}"#.to_string(),
        };
        let mut provider_meta = serde_json::Map::new();
        provider_meta.insert(
            THOUGHT_SIGNATURES_META_KEY.to_string(),
            json!({ "call_1": "sig-1" }),
        );
        provider_meta.insert(
            TEXT_THOUGHT_SIGNATURE_META_KEY.to_string(),
            json!("text-sig"),
        );
        let body = body(
            vec![
                Message::new(Role::User, "inspect"),
                Message {
                    tool_calls: Some(vec![call.clone()]),
                    provider_meta: Some(provider_meta),
                    ..Message::new(Role::Assistant, "checking")
                },
                Message::tool_result(&call, "Cargo.toml"),
            ],
            BodyInput {
                tool_specs: None,
                include_thoughts: false,
                thinking: None,
            },
        );

        assert_eq!(
            body["contents"][1]["parts"][1]["functionCall"]["name"],
            "list_dir"
        );
        assert_eq!(
            body["contents"][1]["parts"][1]["functionCall"]["args"]["path"],
            "."
        );
        assert_eq!(
            body["contents"][1]["parts"][0]["thoughtSignature"],
            "text-sig"
        );
        assert_eq!(body["contents"][1]["parts"][1]["thoughtSignature"], "sig-1");
        assert_eq!(
            body["contents"][2]["parts"][0]["functionResponse"]["name"],
            "list_dir"
        );
        assert_eq!(
            body["contents"][2]["parts"][0]["functionResponse"]["response"]["content"],
            "Cargo.toml"
        );
    }

    #[test]
    fn strips_unanswered_tool_calls_and_orphan_results() {
        let answered = neenee_contracts::ToolCall {
            id: "answered".to_string(),
            name: "list_dir".to_string(),
            arguments: "{}".to_string(),
        };
        let unanswered = neenee_contracts::ToolCall {
            id: "unanswered".to_string(),
            name: "grep".to_string(),
            arguments: "{}".to_string(),
        };
        let body = body(
            vec![
                Message::new(Role::User, "go"),
                Message {
                    tool_calls: Some(vec![answered.clone(), unanswered]),
                    ..Message::new(Role::Assistant, "")
                },
                Message::tool_result(&answered, "{}"),
                Message {
                    tool_call_id: Some("orphan".to_string()),
                    ..Message::new(Role::Tool, "ignored")
                },
            ],
            BodyInput {
                tool_specs: None,
                include_thoughts: false,
                thinking: None,
            },
        );

        let parts = body["contents"][1]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["functionCall"]["id"], "answered");
        assert_eq!(
            body["contents"][2]["parts"][0]["functionResponse"]["id"],
            "answered"
        );
    }

    #[test]
    fn resolve_thinking_level_for_gemini_3x() {
        // Gemini 3.x uses a level ladder; each rung maps to thinkingLevel,
        // clamping down from unsupported depths.
        use neenee_contracts::effort::{EFFORT_GEMINI_LEVEL, Effort};
        let level: Vec<neenee_contracts::EffortLevel> = EFFORT_GEMINI_LEVEL
            .iter()
            .copied()
            .map(Into::into)
            .collect();
        assert_eq!(
            resolve_thinking(Some(Effort::High), &level, 0),
            Some(GoogleThinking::Level(Effort::High))
        );
        // max/xhigh clamp down to high (Gemini 3.x has no deeper rung).
        assert_eq!(
            resolve_thinking(Some(Effort::Max), &level, 0),
            Some(GoogleThinking::Level(Effort::High))
        );
        assert_eq!(
            resolve_thinking(Some(Effort::Minimal), &level, 0),
            Some(GoogleThinking::Level(Effort::Minimal))
        );
        // No override → server default.
        assert_eq!(resolve_thinking(None, &level, 0), None);
    }

    #[test]
    fn resolve_thinking_budget_for_gemini_2_5() {
        // Gemini 2.5 uses a budget ladder; rungs map to token buckets against
        // the model's max (Flash: 24576).
        use neenee_contracts::effort::{EFFORT_GEMINI_BUDGET, Effort};
        let budget: Vec<neenee_contracts::EffortLevel> = EFFORT_GEMINI_BUDGET
            .iter()
            .copied()
            .map(Into::into)
            .collect();
        assert_eq!(
            resolve_thinking(Some(Effort::Medium), &budget, 24576),
            Some(GoogleThinking::Budget(12288))
        );
        assert_eq!(
            resolve_thinking(Some(Effort::Max), &budget, 24576),
            Some(GoogleThinking::Budget(24576))
        );
        // Pro's larger cap scales the bucket.
        assert_eq!(
            resolve_thinking(Some(Effort::Medium), &budget, 32768),
            Some(GoogleThinking::Budget(16384))
        );
    }

    #[test]
    fn resolve_thinking_empty_ladder_is_no_override() {
        // A non-reasoning / unknown model (empty ladder) never stamps thinking.
        assert_eq!(
            resolve_thinking(Some(neenee_contracts::Effort::High), &[], 24576),
            None
        );
    }

    #[test]
    fn body_stamps_thinking_level_and_budget() {
        // Level → thinkingLevel string.
        let level_body = body(
            vec![Message::new(Role::User, "think")],
            BodyInput {
                tool_specs: None,
                include_thoughts: false,
                thinking: Some(GoogleThinking::Level(neenee_contracts::Effort::Medium)),
            },
        );
        assert_eq!(
            level_body["generationConfig"]["thinkingConfig"]["thinkingLevel"],
            "medium"
        );
        // Budget → thinkingBudget integer.
        let budget_body = body(
            vec![Message::new(Role::User, "think")],
            BodyInput {
                tool_specs: None,
                include_thoughts: false,
                thinking: Some(GoogleThinking::Budget(8192)),
            },
        );
        assert_eq!(
            budget_body["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            8192
        );
    }

    #[test]
    fn max_thinking_budget_per_gemini_2_5_model() {
        assert_eq!(max_thinking_budget("gemini-2.5-pro"), 32768);
        assert_eq!(max_thinking_budget("gemini-2.5-flash"), 24576);
        assert_eq!(max_thinking_budget("gemini-2.5-flash-lite"), 24576);
        // Gemini 3.x and non-reasoning models have no budget surface.
        assert_eq!(max_thinking_budget("gemini-3.5-flash"), 0);
        assert_eq!(max_thinking_budget("gemini-2.0-flash"), 0);
    }
}
