//! OpenAI-compatible chat completions — response parsing.
//!
//! Pure functions turning OpenAI's JSON response shape into the harness's
//! domain types: the assistant [`Message`] (with reasoning content and tool
//! calls), the top-level `usage` object, and the per-chunk stream events.

use muta_contracts::{Message, ProviderStreamEvent, Role, TokenUsage, ToolCall};
use serde_json::Value;

/// Parse an OpenAI top-level `usage` object (`prompt_tokens` /
/// `completion_tokens` / `total_tokens`) into a [`TokenUsage`]. Returns `None`
/// when the object is absent or has no numeric fields.
///
/// OpenAI auto-caches without explicit breakpoints. Its discount surfaces as
/// `prompt_tokens_details.cached_tokens` (Moonshot exposes the same number as a
/// top-level `cached_tokens`). That count is a **cache read** — served from the
/// auto-cache at a discount — and is now surfaced in
/// [`TokenUsage::cache_read_input_tokens`] so the token-source report shows the
/// hit rate and the cost is attributed correctly. `cache_creation_input_tokens`
/// stays zero: OpenAI-style auto-caching has no separate write counter.
pub fn usage(usage: &Value) -> Option<TokenUsage> {
    let cached = muta_contracts::cache::read_cached_tokens(usage);
    let prompt = usage["prompt_tokens"].as_i64();
    let completion = usage["completion_tokens"].as_i64();
    let total = usage["total_tokens"].as_i64();
    match (prompt, completion, total) {
        (Some(p), Some(c), _) => Some(TokenUsage {
            prompt_tokens: p,
            completion_tokens: c,
            total_tokens: total.unwrap_or(p + c),
            cache_read_input_tokens: cached.unwrap_or(0),
            ..Default::default()
        }),
        (Some(p), None, Some(t)) => Some(TokenUsage {
            prompt_tokens: p,
            completion_tokens: (t - p).max(0),
            total_tokens: t,
            cache_read_input_tokens: cached.unwrap_or(0),
            ..Default::default()
        }),
        _ => {
            // Fall back to total_tokens only.
            total.map(|t| TokenUsage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: t,
                cache_read_input_tokens: cached.unwrap_or(0),
                ..Default::default()
            })
        }
    }
}

/// Extract the tool calls from a response `choices[0].message.tool_calls`
/// array (if present).
pub fn tool_calls(choice: &Value) -> Option<Vec<ToolCall>> {
    choice.get("tool_calls").and_then(|tc| {
        tc.as_array().map(|arr| {
            arr.iter()
                .map(|t| ToolCall {
                    id: t["id"]
                        .as_str()
                        .filter(|value| !value.is_empty())
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("call_{}", uuid::Uuid::new_v4())),
                    name: t["function"]["name"].as_str().unwrap_or("").to_string(),
                    arguments: t["function"]["arguments"]
                        .as_str()
                        .unwrap_or("")
                        .to_string(),
                })
                .collect()
        })
    })
}

/// Assemble the `choices[0].message` of a non-streaming chat response into one
/// assistant [`Message`].
///
/// `content_filter` is applied to the text content: it receives the raw
/// `choice` text and the resolved tool calls, and returns the text safe to
/// show. This is the seam where the tool-call "echo" filter (GLM/Qwen models
/// that mirror a native tool call as text) is applied — see [`super::echo`].
pub fn message(choice: &Value, content_filter: impl FnOnce(&str, bool) -> String) -> Message {
    let reasoning_content = choice["reasoning_content"]
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    let tool_calls = tool_calls(choice);

    let raw_content = choice["content"].as_str().unwrap_or("");
    let had_native_tool_calls = tool_calls.as_ref().is_some_and(|calls| !calls.is_empty());
    let content = content_filter(raw_content, had_native_tool_calls);

    Message {
        role: Role::Assistant,
        content,
        content_blob: None,
        display_content: None,
        reasoning_content,
        provider_meta: None,
        tool_calls,
        tool_call_id: None,
        images: None,
        provider: None,
        model: None,
        effort: None,
        hidden: false,
        children: None,
        envoy_meta: None,
        origin: None,
        timestamp: Some(muta_contracts::todos::unix_now()),
        sent_at_ms: None,
    }
}

/// Parse one streaming chat-completion `data:` payload into provider stream
/// events. The terminal chunk (carrying `finish_reason`) may include a
/// top-level `usage` object when `stream_options: {include_usage: true}` was
/// set — forwarded as a [`ProviderStreamEvent::Usage`].
pub fn stream_events(data: &str) -> Vec<ProviderStreamEvent> {
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return Vec::new();
    };
    let mut events = Vec::new();
    if let Some(usage) = usage(&value["usage"]) {
        events.push(ProviderStreamEvent::Usage(usage));
    }
    let delta = &value["choices"][0]["delta"];
    if let Some(content) = delta["content"].as_str().filter(|value| !value.is_empty()) {
        events.push(ProviderStreamEvent::TextDelta(content.to_string()));
    }
    if let Some(reasoning) = delta["reasoning_content"]
        .as_str()
        .filter(|value| !value.is_empty())
    {
        events.push(ProviderStreamEvent::ReasoningDelta(reasoning.to_string()));
    }
    if let Some(tool_calls) = delta["tool_calls"].as_array() {
        for call in tool_calls {
            events.push(ProviderStreamEvent::ToolCallDelta {
                index: call["index"].as_u64().unwrap_or(0) as usize,
                id: call["id"].as_str().map(str::to_string),
                name: call["function"]["name"].as_str().map(str::to_string),
                arguments: call["function"]["arguments"]
                    .as_str()
                    .unwrap_or("")
                    .to_string(),
            });
        }
    }
    events
}

/// Extract the plain text delta from one stream payload (the simple
/// `stream_chat` path, which ignores reasoning/tool calls).
pub fn stream_text(data: &str) -> String {
    let Ok(v) = serde_json::from_str::<Value>(data) else {
        return String::new();
    };
    let mut content = String::new();
    if let Some(delta) = v["choices"][0]["delta"]["content"].as_str() {
        content.push_str(delta);
    }
    content
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_parser_preserves_tool_call_fragments() {
        let events = stream_events(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"read_","arguments":"{\"pa"}}]}}]}"#,
        );
        assert_eq!(
            events,
            vec![ProviderStreamEvent::ToolCallDelta {
                index: 0,
                id: Some("call_1".to_string()),
                name: Some("read_".to_string()),
                arguments: "{\"pa".to_string(),
            }]
        );
    }

    #[test]
    fn stream_text_extracts_content_delta() {
        let text = stream_text(r#"{"choices":[{"delta":{"content":"Hi there"}}]}"#);
        assert_eq!(text, "Hi there");
    }

    #[test]
    fn message_assembles_text_and_tool_calls() {
        let choice = serde_json::json!({
            "content": "hello",
            "tool_calls": [{
                "id": "call_1",
                "function": {"name": "bash", "arguments": "{\"cmd\":\"ls\"}"}
            }]
        });
        let msg = message(&choice, |raw, _| raw.to_string());
        assert_eq!(msg.content, "hello");
        assert_eq!(msg.tool_calls.as_ref().unwrap().len(), 1);
        assert_eq!(msg.tool_calls.unwrap()[0].name, "bash");
    }

    #[test]
    fn usage_surfaces_openai_cached_tokens_as_read() {
        let u = usage(&serde_json::json!({
            "prompt_tokens": 1000,
            "completion_tokens": 50,
            "total_tokens": 1050,
            "prompt_tokens_details": { "cached_tokens": 700 }
        }))
        .unwrap();
        assert_eq!(u.prompt_tokens, 1000);
        assert_eq!(u.cache_read_input_tokens, 700);
        // OpenAI auto-cache has no separate write counter.
        assert_eq!(u.cache_creation_input_tokens, 0);
    }

    #[test]
    fn usage_surfaces_moonshot_top_level_cached_tokens() {
        let u = usage(&serde_json::json!({
            "prompt_tokens": 1000,
            "completion_tokens": 50,
            "cached_tokens": 300
        }))
        .unwrap();
        assert_eq!(u.cache_read_input_tokens, 300);
    }

    #[test]
    fn usage_without_cache_field_has_zero_counters() {
        let u = usage(&serde_json::json!({
            "prompt_tokens": 1000,
            "completion_tokens": 50,
            "total_tokens": 1050
        }))
        .unwrap();
        assert_eq!(u.cache_read_input_tokens, 0);
        assert_eq!(u.cache_creation_input_tokens, 0);
    }
}
