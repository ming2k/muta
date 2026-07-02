//! Anthropic Messages — response parsing.
//!
//! Pure functions turning Anthropic's `/messages` response shape into the
//! harness's domain types: content-block assembly (text + thinking + tool_use),
//! the `usage` object (with cache-token folding), and the per-event stream
//! parsing.

use neenee_core::{Message, ProviderStreamEvent, Role, TokenUsage, ToolCall};
use serde_json::{Map, Value};

/// The assembled pieces of an Anthropic assistant response: the text content,
/// the reasoning content, and any tool calls. The provider also extracts a
/// thinking signature (captured separately for replay) and stashes the usage.
pub struct AssembledMessage {
    pub content: String,
    pub reasoning_content: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
    /// The `signature` of the most recent `thinking` block, to stash in
    /// `provider_meta` for replay on the next turn.
    pub thinking_signature: Option<String>,
}

/// Parse an Anthropic `usage` object into a [`TokenUsage`]. Returns `None`
/// when the object is absent or has no numeric fields.
///
/// `prompt_tokens` ← `input_tokens` + `cache_creation_input_tokens` +
/// `cache_read_input_tokens` (Anthropic's `input_tokens` is ONLY the uncached
/// dynamic suffix; the cache counts must be folded in or every cached turn is
/// undercounted). `completion_tokens` ← `output_tokens`. The cache counters are
/// kept verbatim for the ledger's separate cache accounting.
pub fn usage(usage: &Value) -> Option<TokenUsage> {
    let input = usage["input_tokens"].as_i64();
    let output = usage["output_tokens"].as_i64();
    let cache_creation = usage["cache_creation_input_tokens"].as_i64().unwrap_or(0);
    let cache_read = usage["cache_read_input_tokens"].as_i64().unwrap_or(0);
    let (uncached_input, c) = match (input, output) {
        (Some(p), Some(c)) => (p, c),
        (Some(p), None) => (p, 0),
        (None, Some(c)) => (0, c),
        (None, None) => return None,
    };
    let prompt = uncached_input + cache_creation + cache_read;
    Some(TokenUsage {
        prompt_tokens: prompt,
        completion_tokens: c,
        total_tokens: prompt + c,
        cache_creation_input_tokens: cache_creation,
        cache_read_input_tokens: cache_read,
    })
}

/// Assemble the `content` block array of a non-streaming response into one
/// assistant message's pieces.
pub fn assemble_message(response: &Value) -> Result<AssembledMessage, String> {
    let mut content = String::new();
    let mut reasoning_content: Option<String> = None;
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut thinking_signature: Option<String> = None;

    if let Some(err) = response.get("error") {
        return Err(format!("Anthropic Error: {}", err));
    }

    if let Some(blocks) = response["content"].as_array() {
        for block in blocks {
            match block["type"].as_str().unwrap_or("") {
                "text" => {
                    if let Some(text) = block["text"].as_str() {
                        content.push_str(text);
                    }
                }
                "thinking" => {
                    if let Some(text) = block["thinking"].as_str() {
                        reasoning_content = Some(text.to_string());
                    }
                    // The thinking block carries a `signature` the server needs
                    // to reconstruct it on the next replay.
                    if let Some(sig) = block["signature"].as_str()
                        && !sig.is_empty()
                    {
                        thinking_signature = Some(sig.to_string());
                    }
                }
                "tool_use" => {
                    tool_calls.push(ToolCall {
                        id: block["id"]
                            .as_str()
                            .filter(|v| !v.is_empty())
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("call_{}", uuid::Uuid::new_v4())),
                        name: block["name"].as_str().unwrap_or("").to_string(),
                        arguments: block
                            .get("input")
                            .map(|v| v.to_string())
                            .unwrap_or_default(),
                    });
                }
                _ => {}
            }
        }
    }

    Ok(AssembledMessage {
        content,
        reasoning_content,
        tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
        thinking_signature,
    })
}

/// Turn an [`AssembledMessage`] into a domain [`Message`], stamping the
/// thinking signature into `provider_meta` when present.
pub fn into_message(
    AssembledMessage {
        content,
        reasoning_content,
        tool_calls,
        thinking_signature,
    }: AssembledMessage,
) -> Message {
    let provider_meta = thinking_signature.map(|sig| {
        let mut map = Map::new();
        map.insert("thinking_signature".to_string(), Value::String(sig));
        map
    });
    Message {
        role: Role::Assistant,
        content,
        content_blob: None,
        display_content: None,
        reasoning_content,
        provider_meta,
        tool_calls,
        tool_call_id: None,
        images: None,
        provider: None,
        model: None,
        hidden: false,
        children: None,
        envoy_meta: None,
        origin: None,
    }
}

/// Parse one SSE `data:` payload into provider stream events. Anthropic wraps
/// each event in `{type, ...}`; the `type` discriminator selects the block/delta
/// shape.
///
/// Returns `Err` only for an in-stream `error` event; other non-content events
/// are no-ops that yield no events.
pub fn stream_events(data: &str) -> Result<Vec<ProviderStreamEvent>, String> {
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return Ok(Vec::new());
    };
    let event_type = value["type"].as_str().unwrap_or("");
    match event_type {
        "error" => {
            let message = value["error"]["message"]
                .as_str()
                .unwrap_or("Anthropic stream error")
                .to_string();
            Err(message)
        }
        "content_block_start" => {
            let index = value["index"].as_u64().unwrap_or(0) as usize;
            let block = &value["content_block"];
            let block_type = block["type"].as_str().unwrap_or("");
            if block_type == "tool_use" {
                Ok(vec![ProviderStreamEvent::ToolCallDelta {
                    index,
                    id: block["id"].as_str().map(str::to_string),
                    name: block["name"].as_str().map(str::to_string),
                    arguments: String::new(),
                }])
            } else {
                Ok(Vec::new())
            }
        }
        "content_block_delta" => {
            let index = value["index"].as_u64().unwrap_or(0) as usize;
            let delta = &value["delta"];
            match delta["type"].as_str().unwrap_or("") {
                "text_delta" => Ok(delta["text"]
                    .as_str()
                    .filter(|t| !t.is_empty())
                    .map(|t| ProviderStreamEvent::TextDelta(t.to_string()))
                    .into_iter()
                    .collect()),
                "thinking_delta" => Ok(delta["thinking"]
                    .as_str()
                    .filter(|t| !t.is_empty())
                    .map(|t| ProviderStreamEvent::ReasoningDelta(t.to_string()))
                    .into_iter()
                    .collect()),
                "input_json_delta" => {
                    let frag = delta["partial_json"].as_str().unwrap_or("");
                    Ok(vec![ProviderStreamEvent::ToolCallDelta {
                        index,
                        id: None,
                        name: None,
                        arguments: frag.to_string(),
                    }])
                }
                _ => Ok(Vec::new()),
            }
        }
        // message_delta carries the final cumulative `usage` right before
        // message_stop. Forward it as a Usage event so the harness books
        // authoritative counts instead of estimating.
        "message_delta" => {
            if let Some(usage) = usage(&value["usage"]) {
                Ok(vec![ProviderStreamEvent::Usage(usage)])
            } else {
                Ok(Vec::new())
            }
        }
        _ => Ok(Vec::new()),
    }
}

/// Extract the plain text delta from one stream payload (the simple
/// `stream_chat` path, which ignores reasoning/tool calls).
pub fn stream_text(data: &str) -> String {
    let Ok(v) = serde_json::from_str::<Value>(data) else {
        return String::new();
    };
    let mut text = String::new();
    if v["type"].as_str() == Some("content_block_delta")
        && v["delta"]["type"].as_str() == Some("text_delta")
        && let Some(t) = v["delta"]["text"].as_str()
    {
        text.push_str(t);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_parser_extracts_text_and_tool_deltas() {
        // A text delta event.
        let text_events = stream_events(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
        )
        .expect("text delta parses");
        assert_eq!(
            text_events,
            vec![ProviderStreamEvent::TextDelta("Hello".to_string())]
        );

        // A tool_use block opening: id and name arrive up front.
        let open_events = stream_events(
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"bash"}}"#,
        )
        .expect("content_block_start parses");
        assert_eq!(
            open_events,
            vec![ProviderStreamEvent::ToolCallDelta {
                index: 1,
                id: Some("toolu_1".to_string()),
                name: Some("bash".to_string()),
                arguments: String::new(),
            }]
        );

        // Argument JSON fragments arrive as input_json_delta; the harness
        // concatenates them.
        let frag_events = stream_events(
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"comm"}}"#,
        )
        .expect("input_json_delta parses");
        assert_eq!(
            frag_events,
            vec![ProviderStreamEvent::ToolCallDelta {
                index: 1,
                id: None,
                name: None,
                arguments: "{\"comm".to_string(),
            }]
        );
    }

    #[test]
    fn stream_parser_extracts_reasoning_deltas() {
        let events = stream_events(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"hm"}}"#,
        )
        .expect("thinking_delta parses");
        assert_eq!(
            events,
            vec![ProviderStreamEvent::ReasoningDelta("hm".to_string())]
        );
    }

    #[test]
    fn stream_parser_surfaces_error_events_as_err() {
        let result = stream_events(
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        );
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("Overloaded"),
            "error message must be surfaced"
        );
    }

    #[test]
    fn stream_parser_ignores_non_content_events() {
        for payload in [
            r#"{"type":"message_start","message":{"id":"msg_1"}}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"}}"#,
            r#"{"type":"message_stop"}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"not-json-at-all"#,
        ] {
            let events = stream_events(payload).expect("non-content event is ok");
            assert!(
                events.is_empty(),
                "non-content event must yield nothing: {payload}"
            );
        }
    }
}
