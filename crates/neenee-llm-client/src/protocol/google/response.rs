//! Google Google — response parsing.
//!
//! Pure functions that turn Google's JSON response shape into the harness's
//! domain types. Google reports usage under `usageMetadata`
//! (`promptTokenCount` / `candidatesTokenCount` / `totalTokenCount`), and
//! content lives in `candidates[0].content.parts[]` as either `text` or
//! `functionCall`.

use neenee_core::{Message, ProviderStreamEvent, Role, TokenUsage, ToolCall};
use serde_json::{Map, Value};

pub const THOUGHT_SIGNATURES_META_KEY: &str = "gemini_thought_signatures";
pub const TEXT_THOUGHT_SIGNATURE_META_KEY: &str = "gemini_text_thought_signature";

#[derive(Debug, Clone)]
pub struct StreamPayload {
    pub events: Vec<ProviderStreamEvent>,
    pub thought_signatures: Vec<(String, String)>,
    pub text_thought_signature: Option<String>,
}

/// Parse Google's `usageMetadata` into a [`TokenUsage`]. Returns `None` when
/// the object is absent or has no numeric fields. Google's implicit context
/// caching discount surfaces as `cachedContentTokenCount`; it is surfaced in
/// [`TokenUsage::cache_read_input_tokens`] so the token-source report shows the
/// hit rate. Google exposes no separate cache-write counter.
pub fn usage(usage: &Value) -> Option<TokenUsage> {
    let prompt = usage["promptTokenCount"].as_i64();
    let completion = usage["candidatesTokenCount"].as_i64();
    let total = usage["totalTokenCount"].as_i64();
    // Route cache-read accounting through the shared helper so the cache
    // policy is enforced in one place (ADR-0067). Google hides the discount in
    // `cachedContentTokenCount`, which the helper reads.
    let cached = neenee_core::cache::read_cached_tokens(usage);
    match (prompt, completion, total) {
        (Some(p), Some(c), _) => Some(TokenUsage {
            prompt_tokens: p,
            completion_tokens: c,
            total_tokens: total.unwrap_or(p + c),
            cache_read_input_tokens: cached.unwrap_or(0),
            ..Default::default()
        }),
        _ => total.map(|t| TokenUsage {
            prompt_tokens: prompt.unwrap_or(0),
            completion_tokens: completion.unwrap_or(0),
            total_tokens: t,
            cache_read_input_tokens: cached.unwrap_or(0),
            ..Default::default()
        }),
    }
}

/// Assemble the candidate content of a non-streaming `generateContent`
/// response into one assistant [`Message`]. Text parts become content and
/// `functionCall` parts become native tool calls. Errors when the response has
/// no candidates or no `parts` array.
pub fn message(response: &Value) -> Result<Message, String> {
    let candidates = response
        .get("candidates")
        .and_then(|c| c.as_array())
        .ok_or_else(|| format!("Invalid Google response: {}", response))?;

    if candidates.is_empty() {
        return Err("Google returned no candidates".to_string());
    }

    let parts = candidates[0]["content"]
        .get("parts")
        .and_then(|p| p.as_array())
        .ok_or_else(|| "Missing parts in Google response".to_string())?;

    let mut content_text = String::new();
    let mut reasoning_text = String::new();
    let mut tool_calls = Vec::new();
    let mut thought_signatures = Map::new();
    let mut text_thought_signature = None;
    for part in parts {
        if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
            if part_is_thought(part) {
                reasoning_text.push_str(text);
            } else {
                content_text.push_str(text);
            }
            if let Some(signature) = thought_signature(part) {
                text_thought_signature = Some(signature);
            }
        }
        if let Some(call) = part.get("functionCall").and_then(function_call) {
            if let Some(signature) = thought_signature(part) {
                thought_signatures.insert(call.id.clone(), Value::String(signature));
            }
            tool_calls.push(call);
        }
    }

    let mut message = Message::new(Role::Assistant, content_text);
    message.tool_calls = (!tool_calls.is_empty()).then_some(tool_calls);
    if !reasoning_text.is_empty() {
        message.reasoning_content = Some(reasoning_text);
    }
    if !thought_signatures.is_empty() {
        let provider_meta = message.provider_meta.get_or_insert_with(Map::new);
        provider_meta.insert(
            THOUGHT_SIGNATURES_META_KEY.to_string(),
            Value::Object(thought_signatures),
        );
    }
    if let Some(signature) = text_thought_signature {
        let mut provider_meta = Map::new();
        provider_meta.insert(
            TEXT_THOUGHT_SIGNATURE_META_KEY.to_string(),
            Value::String(signature),
        );
        if let Some(existing) = message.provider_meta.as_mut() {
            existing.extend(provider_meta);
        } else {
            message.provider_meta = Some(provider_meta);
        }
    }
    Ok(message)
}

/// Parse one `streamGenerateContent` SSE payload and concatenate the text from
/// `candidates[0].content.parts[].text`. Returns an empty string when the
/// payload carries no text part (e.g. a finish-reason-only chunk).
pub fn stream_text(payload: &str) -> String {
    stream_events(payload)
        .into_iter()
        .filter_map(|event| match event {
            ProviderStreamEvent::TextDelta(text) => Some(text),
            _ => None,
        })
        .collect()
}

/// Parse one `streamGenerateContent` SSE payload into normalized provider
/// events. Google may stream text, usage metadata, and whole `functionCall`
/// parts. Function calls are emitted as one delta per part because the REST
/// stream does not use OpenAI-style argument fragments.
pub fn stream_events(payload: &str) -> Vec<ProviderStreamEvent> {
    stream_payload(payload).events
}

/// Parse one `streamGenerateContent` SSE payload into normalized provider
/// events plus any Google thought signatures attached to function-call parts.
pub fn stream_payload(payload: &str) -> StreamPayload {
    let value: Value = match serde_json::from_str(payload) {
        Ok(value) => value,
        Err(_) => {
            return StreamPayload {
                events: Vec::new(),
                thought_signatures: Vec::new(),
                text_thought_signature: None,
            };
        }
    };
    let mut events = Vec::new();
    let mut thought_signatures = Vec::new();
    let mut text_thought_signature = None;
    if let Some(usage) = usage(&value["usageMetadata"]) {
        events.push(ProviderStreamEvent::Usage(usage));
    }
    if let Some(parts) = value
        .get("candidates")
        .and_then(|candidates| candidates.as_array())
        .and_then(|candidates| candidates.first())
        .and_then(|candidate| candidate["content"]["parts"].as_array())
    {
        let mut call_index = 0usize;
        for part in parts {
            if let Some(text) = part.get("text").and_then(|text| text.as_str())
                && !text.is_empty()
            {
                if let Some(signature) = thought_signature(part) {
                    text_thought_signature = Some(signature);
                }
                // Route reasoning summaries to ReasoningDelta so the harness
                // surfaces them as a thinking trace, distinct from the answer.
                if part_is_thought(part) {
                    events.push(ProviderStreamEvent::ReasoningDelta(text.to_string()));
                } else {
                    events.push(ProviderStreamEvent::TextDelta(text.to_string()));
                }
            }
            if let Some(call) = part.get("functionCall").and_then(function_call) {
                if let Some(signature) = thought_signature(part) {
                    thought_signatures.push((call.id.clone(), signature));
                }
                events.push(ProviderStreamEvent::ToolCallDelta {
                    index: call_index,
                    id: Some(call.id),
                    name: Some(call.name),
                    arguments: call.arguments,
                });
                call_index += 1;
            }
        }
    }
    StreamPayload {
        events,
        thought_signatures,
        text_thought_signature,
    }
}

fn function_call(value: &Value) -> Option<ToolCall> {
    let name = value.get("name")?.as_str()?.to_string();
    if name.is_empty() {
        return None;
    }
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("call_{}", uuid::Uuid::new_v4()));
    let arguments = value
        .get("args")
        .filter(|args| !args.is_null())
        .map(Value::to_string)
        .unwrap_or_else(|| "{}".to_string());
    Some(ToolCall {
        id,
        name,
        arguments,
    })
}

fn thought_signature(part: &Value) -> Option<String> {
    part.get("thoughtSignature")
        .or_else(|| part.get("thought_signature"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Whether this part is a Google reasoning *summary* — the model's thinking
/// text, surfaced only when the request set
/// `generationConfig.thinkingConfig.includeThoughts`. Such parts carry
/// `"thought": true` and MUST be routed to `ReasoningDelta` /
/// `reasoning_content`, never folded into the answer `content` (otherwise the
/// reasoning leaks into the visible reply).
fn part_is_thought(part: &Value) -> bool {
    part.get("thought")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Augment a transport-layer error with model-specific guidance. For native
/// Google, a `404 NOT_FOUND` almost always means the upstream does not serve
/// this model id — not a transient fault. Other errors pass through unchanged.
pub fn clarify_error(err: String, model: &str, base_url: &str) -> String {
    if err.contains("HTTP 404") || err.contains("\"status\": \"NOT_FOUND\"") {
        format!(
            "{err}\n\n\
             Google returned 404 for model `{model}`. The upstream at {base_url} does \
             not serve this model — it may advertise it in /v1beta/models but still \
             reject it, or the id may be deprecated/preview-only. Switch to a model the \
             relay actually serves (e.g. gemini-2.5-flash / gemini-2.5-pro), or pick a \
             different provider."
        )
    } else {
        err
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_text_concatenates_parts_and_preserves_multibyte() {
        let payload = r#"{"candidates":[{"content":{"parts":[{"text":"😀😁"},{"text":"😃😄"}]}}]}"#;
        assert_eq!(stream_text(payload), "😀😁😃😄");
    }

    #[test]
    fn stream_text_returns_empty_for_non_text_payload() {
        assert_eq!(
            stream_text(r#"{"candidates":[{"finishReason":"STOP"}]}"#),
            ""
        );
        assert_eq!(stream_text("not json"), "");
    }

    #[test]
    fn message_extracts_function_calls() {
        let value: Value = serde_json::from_str(
            r#"{
              "candidates": [{
                "content": {
                  "parts": [
                    {
                      "thoughtSignature": "sig-1",
                      "functionCall": {"id": "abc", "name": "list_dir", "args": {"path": "."}}
                    },
                    {"thoughtSignature": "text-sig", "text": "done"}
                  ]
                }
              }]
            }"#,
        )
        .unwrap();

        let message = super::message(&value).unwrap();
        assert_eq!(message.content, "done");
        let calls = message.tool_calls.unwrap();
        assert_eq!(calls[0].id, "abc");
        assert_eq!(calls[0].name, "list_dir");
        assert_eq!(calls[0].arguments, r#"{"path":"."}"#);
        assert_eq!(
            message.provider_meta.unwrap()[THOUGHT_SIGNATURES_META_KEY]["abc"],
            "sig-1"
        );
        let value: Value = serde_json::from_str(
            r#"{"candidates":[{"content":{"parts":[{"thoughtSignature":"text-sig","text":"done"}]}}]}"#,
        )
        .unwrap();
        let message = super::message(&value).unwrap();
        assert_eq!(
            message.provider_meta.unwrap()[TEXT_THOUGHT_SIGNATURE_META_KEY],
            "text-sig"
        );
    }

    #[test]
    fn stream_events_extract_function_calls_and_usage() {
        let events = stream_events(
            r#"{
              "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 2,
                "totalTokenCount": 12
              },
              "candidates": [{
                "content": {
                  "parts": [
                    {"text": "thinking"},
                    {
                      "thoughtSignature": "sig-2",
                      "functionCall": {"id": "abc", "name": "grep", "args": {"query": "x"}}
                    }
                  ]
                }
              }]
            }"#,
        );

        assert!(matches!(events[0], ProviderStreamEvent::Usage(_)));
        assert_eq!(
            events[1],
            ProviderStreamEvent::TextDelta("thinking".to_string())
        );
        assert_eq!(
            events[2],
            ProviderStreamEvent::ToolCallDelta {
                index: 0,
                id: Some("abc".to_string()),
                name: Some("grep".to_string()),
                arguments: r#"{"query":"x"}"#.to_string(),
            }
        );

        let payload = stream_payload(
            r#"{"candidates":[{"content":{"parts":[{"thought_signature":"sig-3","functionCall":{"id":"xyz","name":"list_dir","args":{}}}]}}]}"#,
        );
        assert_eq!(
            payload.thought_signatures,
            vec![("xyz".to_string(), "sig-3".to_string())]
        );
    }

    #[test]
    fn stream_routes_thought_parts_to_reasoning_delta() {
        // A response with a thought summary followed by the answer. Only the
        // `"thought": true` part must become ReasoningDelta; the plain text
        // part stays a TextDelta.
        let events = stream_events(
            r#"{
              "candidates": [{
                "content": {
                  "parts": [
                    {"text": "let me consider the angles...", "thought": true},
                    {"text": "The answer is 42."}
                  ]
                }
              }]
            }"#,
        );
        assert_eq!(
            events[0],
            ProviderStreamEvent::ReasoningDelta("let me consider the angles...".to_string())
        );
        assert_eq!(
            events[1],
            ProviderStreamEvent::TextDelta("The answer is 42.".to_string())
        );
    }

    #[test]
    fn message_routes_thought_parts_to_reasoning_content() {
        // Non-streaming: thought text goes to reasoning_content, never content.
        let value: Value = serde_json::from_str(
            r#"{
              "candidates": [{
                "content": {
                  "parts": [
                    {"text": "reasoning step by step", "thought": true},
                    {"text": "final answer"}
                  ]
                }
              }]
            }"#,
        )
        .unwrap();
        let message = super::message(&value).unwrap();
        assert_eq!(message.content, "final answer");
        assert_eq!(
            message.reasoning_content.as_deref(),
            Some("reasoning step by step")
        );
    }

    #[test]
    fn message_without_thought_flag_keeps_all_text_in_content() {
        // No `thought` flag → nothing is reasoning; all text is the answer.
        // Guards against accidentally treating ordinary text as thinking.
        let value: Value = serde_json::from_str(
            r#"{
              "candidates": [{
                "content": {
                  "parts": [
                    {"text": "part one "},
                    {"text": "part two"}
                  ]
                }
              }]
            }"#,
        )
        .unwrap();
        let message = super::message(&value).unwrap();
        assert_eq!(message.content, "part one part two");
        assert!(message.reasoning_content.is_none());
    }

    #[test]
    fn usage_surfaces_google_cached_content_tokens_as_read() {
        let u = usage(&serde_json::json!({
            "promptTokenCount": 900,
            "candidatesTokenCount": 30,
            "totalTokenCount": 930,
            "cachedContentTokenCount": 600
        }))
        .unwrap();
        assert_eq!(u.prompt_tokens, 900);
        assert_eq!(u.cache_read_input_tokens, 600);
        assert_eq!(u.cache_creation_input_tokens, 0);
    }
}
