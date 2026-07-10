//! OpenAI **Responses** API — response & streaming-event parsing.
//!
//! Pure functions turning a Responses payload into the harness's domain types:
//! usage extraction, the non-streaming assistant [`Message`], and the
//! per-`data:`-payload stream events. No `reqwest`, no `async`, no I/O.
//!
//! Streaming shape: each SSE `data:` payload is a JSON object with a `type`
//! field identifying the event. The harness-relevant events are:
//! - `response.output_text.delta` → `TextDelta`
//! - `response.reasoning_summary_text.delta` → `ReasoningDelta`
//! - `response.function_call_arguments.delta` → accumulating tool-call args
//! - `response.function_call_arguments.done` → the complete tool call
//! - `response.output_item.added` (function_call) → captures id/name/call_id
//! - `response.completed` → terminal usage

use neenee_core::{Message, ProviderStreamEvent, Role, TokenUsage, ToolCall};
use serde_json::Value;

/// Parse a Responses `usage` object (`input_tokens` / `output_tokens` /
/// `total_tokens`) into a [`TokenUsage`]. Returns `None` when absent or without
/// numeric fields. The Responses API reports reasoning tokens under
/// `output_tokens_details.reasoning_tokens`; they are folded into the
/// completion count (mirroring how chat-completions reports them).
pub fn usage(usage: &Value) -> Option<TokenUsage> {
    let input = usage["input_tokens"].as_i64();
    let output = usage["output_tokens"].as_i64();
    let total = usage["total_tokens"].as_i64();
    let prompt = input;
    let completion = output.or_else(|| total.zip(prompt).map(|(t, p)| (t - p).max(0)));
    match (prompt, completion, total) {
        (Some(p), Some(c), _) => Some(TokenUsage {
            prompt_tokens: p,
            completion_tokens: c,
            total_tokens: total.unwrap_or(p + c),
            ..Default::default()
        }),
        _ => total.map(|t| TokenUsage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: t,
            ..Default::default()
        }),
    }
}

/// Assemble a non-streaming Responses `output` array into one assistant
/// [`Message`]. Tolerant: gathers `output_text` parts from message items,
/// reasoning summary text from reasoning items, and every `function_call` item
/// into `tool_calls`. Items it does not recognize are ignored.
pub fn message(output: &Value) -> Message {
    let mut content = String::new();
    let mut reasoning = String::new();
    let mut calls: Vec<ToolCall> = Vec::new();
    if let Some(items) = output.as_array() {
        for item in items {
            match item["type"].as_str().unwrap_or("") {
                "message" => {
                    if let Some(parts) = item["content"].as_array() {
                        for part in parts {
                            if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                content.push_str(text);
                            }
                        }
                    }
                }
                "reasoning" => {
                    if let Some(summary) = item["summary"].as_array() {
                        for part in summary {
                            if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                if !reasoning.is_empty() {
                                    reasoning.push('\n');
                                }
                                reasoning.push_str(text);
                            }
                        }
                    }
                }
                "function_call" => {
                    calls.push(ToolCall {
                        id: item["call_id"].as_str().unwrap_or("").to_string(),
                        name: item["name"].as_str().unwrap_or("").to_string(),
                        arguments: item["arguments"].as_str().unwrap_or("").to_string(),
                    });
                }
                _ => {}
            }
        }
    }
    Message {
        role: Role::Assistant,
        content,
        reasoning_content: (!reasoning.is_empty()).then_some(reasoning),
        tool_calls: (!calls.is_empty()).then_some(calls),
        ..Message::new(Role::Assistant, "")
    }
}

/// Stateful accumulator that turns a sequence of Responses stream events into
/// [`ProviderStreamEvent`]s. The Responses API streams a function call across
/// several events (`output_item.added` carrying the id/name, then argument
/// deltas, then `.done`), and a single response may emit several calls, so this
/// tracks per-`item_id` state and assigns a stable tool-call `index`.
///
/// The harness *appends* every `ToolCallDelta` it receives (`id`/`name`/`
/// `arguments` via `push_str`), so this accumulator MUST emit each fragment
/// exactly once: the `output_item.added` event supplies id+name, the
/// `function_call_arguments.delta` events supply the argument text, and
/// `function_call_arguments.done` is treated as a completion signal — it only
/// re-emits a field as a fallback when that field's incremental events never
/// arrived (a degenerate backend), never as a duplicate.
#[derive(Debug, Default)]
pub struct ResponsesStream {
    /// `item_id` → tool-call index, assigned in first-appearance order.
    call_index: std::collections::HashMap<String, usize>,
    /// `item_id`s whose `output_item.added` already emitted id + name.
    seen_item: std::collections::HashSet<String>,
    /// `item_id`s that already received argument deltas.
    seen_args: std::collections::HashSet<String>,
}

impl ResponsesStream {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse one `data:` payload (a JSON object with a `type` field) into zero
    /// or more harness stream events. Unknown event types are ignored.
    pub fn parse(&mut self, data: &str) -> Vec<ProviderStreamEvent> {
        let Ok(value) = serde_json::from_str::<Value>(data) else {
            return Vec::new();
        };
        let mut events = Vec::new();
        let event_type = value["type"].as_str().unwrap_or("");
        match event_type {
            "response.output_text.delta" => {
                if let Some(delta) = value["delta"].as_str() {
                    events.push(ProviderStreamEvent::TextDelta(delta.to_string()));
                }
            }
            "response.reasoning_summary_text.delta" => {
                if let Some(delta) = value["delta"].as_str() {
                    events.push(ProviderStreamEvent::ReasoningDelta(delta.to_string()));
                }
            }
            "response.output_item.added" => {
                // A function_call item announces its id/name/call_id before its
                // argument deltas begin. Register its index and surface the call
                // id + name immediately so the harness can render it.
                let item = &value["item"];
                if item["type"].as_str() == Some("function_call")
                    && let Some(item_id) = item["id"].as_str()
                {
                    let idx = self.assign_index(item_id);
                    self.seen_item.insert(item_id.to_string());
                    events.push(ProviderStreamEvent::ToolCallDelta {
                        index: idx,
                        id: item["call_id"].as_str().map(str::to_string),
                        name: item["name"].as_str().map(str::to_string),
                        arguments: String::new(),
                    });
                }
            }
            "response.function_call_arguments.delta" => {
                if let (Some(item_id), Some(delta)) =
                    (value["item_id"].as_str(), value["delta"].as_str())
                {
                    let idx = self.assign_index(item_id);
                    self.seen_args.insert(item_id.to_string());
                    events.push(ProviderStreamEvent::ToolCallDelta {
                        index: idx,
                        id: None,
                        name: None,
                        arguments: delta.to_string(),
                    });
                }
            }
            "response.function_call_arguments.done" => {
                // Completion signal only: the argument deltas above already
                // built the args. Emit a field solely as a fallback when its
                // incremental events never arrived, so a well-behaved stream is
                // never doubled.
                if let Some(item_id) = value["item_id"].as_str() {
                    let idx = self.assign_index(item_id);
                    let had_item = self.seen_item.contains(item_id);
                    let had_args = self.seen_args.contains(item_id);
                    let id = (!had_item)
                        .then(|| value["call_id"].as_str().map(str::to_string))
                        .flatten();
                    let name = (!had_item)
                        .then(|| value["name"].as_str().map(str::to_string))
                        .flatten();
                    let arguments = if !had_args {
                        value["arguments"].as_str().unwrap_or("").to_string()
                    } else {
                        Default::default()
                    };
                    if id.is_some() || name.is_some() || !arguments.is_empty() {
                        events.push(ProviderStreamEvent::ToolCallDelta {
                            index: idx,
                            id,
                            name,
                            arguments,
                        });
                    }
                }
            }
            "response.completed" => {
                if let Some(u) = usage(&value["response"]["usage"]) {
                    events.push(ProviderStreamEvent::Usage(u));
                }
            }
            // `response.created`, `response.in_progress`, `*.added`/`*.done`
            // for text/reasoning parts, and `response.failed` carry no harness-
            // relevant payload; they are intentionally ignored.
            _ => {}
        }
        events
    }

    /// Assign (or look up) the tool-call index for an `item_id`, in
    /// first-appearance order.
    fn assign_index(&mut self, item_id: &str) -> usize {
        let next = self.call_index.len();
        *self.call_index.entry(item_id.to_string()).or_insert(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_output_text_delta() {
        let mut s = ResponsesStream::new();
        let ev = s.parse(r#"{"type":"response.output_text.delta","delta":"hel"}"#);
        assert_eq!(ev.len(), 1);
        assert!(matches!(
            &ev[0],
            ProviderStreamEvent::TextDelta(t) if t == "hel"
        ));
    }

    #[test]
    fn parses_reasoning_summary_delta() {
        let mut s = ResponsesStream::new();
        let ev = s.parse(r#"{"type":"response.reasoning_summary_text.delta","delta":"thinking"}"#);
        assert_eq!(ev.len(), 1);
        assert!(matches!(
            &ev[0],
            ProviderStreamEvent::ReasoningDelta(t) if t == "thinking"
        ));
    }

    #[test]
    fn assembles_function_call_across_events_with_stable_index() {
        let mut s = ResponsesStream::new();
        // output_item.added announces the call.
        let added = s.parse(
            r#"{"type":"response.output_item.added","item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"bash"}}"#,
        );
        assert_eq!(added.len(), 1);
        assert!(matches!(
            &added[0],
            ProviderStreamEvent::ToolCallDelta { index: 0, id, name, arguments }
            if id.as_deref() == Some("call_1") && name.as_deref() == Some("bash") && arguments.is_empty()
        ));
        // Argument deltas accumulate under the same index.
        let d1 = s.parse(
            r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"{\"comm"}"#,
        );
        let d2 = s.parse(
            r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","delta":"and\":\"ls\"}"}"#,
        );
        assert_eq!(d1.len() + d2.len(), 2);
        // A second call gets index 1.
        let added2 = s.parse(
            r#"{"type":"response.output_item.added","item":{"id":"fc_2","type":"function_call","call_id":"call_2","name":"grep"}}"#,
        );
        assert!(matches!(
            &added2[0],
            ProviderStreamEvent::ToolCallDelta { index: 1, .. }
        ));
    }

    #[test]
    fn emits_usage_on_completion() {
        let mut s = ResponsesStream::new();
        let ev = s.parse(
            r#"{"type":"response.completed","response":{"usage":{"input_tokens":12,"output_tokens":7,"total_tokens":19}}}"#,
        );
        assert_eq!(ev.len(), 1);
        match &ev[0] {
            ProviderStreamEvent::Usage(u) => {
                assert_eq!(u.prompt_tokens, 12);
                assert_eq!(u.completion_tokens, 7);
                assert_eq!(u.total_tokens, 19);
            }
            _ => panic!("expected usage"),
        }
    }

    #[test]
    fn done_event_does_not_double_arguments_or_id() {
        // Regression: a well-behaved stream emits output_item.added (id+name),
        // then argument deltas, then function_call_arguments.done. The harness
        // appends every delta, so the .done event MUST NOT re-emit the full
        // arguments/id/name — otherwise they double (e.g.
        // `{"path":"."}{"path":"."}` and `call_1call_1`).
        let mut s = ResponsesStream::new();
        s.parse(
            r#"{"type":"response.output_item.added","item":{"id":"fc_1","type":"function_call","call_id":"call_1","name":"list_dir"}}"#,
        );
        let delta = serde_json::json!({
            "type": "response.function_call_arguments.delta",
            "item_id": "fc_1",
            "delta": "{\"path\":\".\",\"max_results\":100}"
        })
        .to_string();
        let done = serde_json::json!({
            "type": "response.function_call_arguments.done",
            "item_id": "fc_1",
            "call_id": "call_1",
            "name": "list_dir",
            "arguments": "{\"path\":\".\",\"max_results\":100}"
        })
        .to_string();
        s.parse(&delta);
        // The .done event must emit nothing — args and id/name already flowed.
        let done_events = s.parse(&done);
        assert!(
            done_events.is_empty(),
            "done must not re-emit after deltas; got {done_events:?}"
        );
    }

    #[test]
    fn done_emits_full_call_as_fallback_when_no_deltas() {
        // A degenerate backend that sends only .done (no added, no deltas):
        //the .done must surface the complete call exactly once.
        let mut s = ResponsesStream::new();
        let done = serde_json::json!({
            "type": "response.function_call_arguments.done",
            "item_id": "fc_1",
            "call_id": "call_1",
            "name": "list_dir",
            "arguments": "{\"path\":\".\"}"
        })
        .to_string();
        let ev = s.parse(&done);
        assert_eq!(ev.len(), 1);
        match &ev[0] {
            ProviderStreamEvent::ToolCallDelta {
                index: 0,
                id,
                name,
                arguments,
            } => {
                assert_eq!(id.as_deref(), Some("call_1"));
                assert_eq!(name.as_deref(), Some("list_dir"));
                assert_eq!(arguments, "{\"path\":\".\"}");
            }
            _ => panic!("expected one tool-call delta"),
        }
    }

    #[test]
    fn non_streaming_message_gathers_text_reasoning_and_calls() {
        let output = serde_json::json!([
            {"type":"reasoning","summary":[{"type":"summary_text","text":"deliberating"}]},
            {"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello "},{"type":"output_text","text":"world"}]},
            {"type":"function_call","call_id":"call_1","name":"bash","arguments":"{\"command\":\"ls\"}"}
        ]);
        let m = message(&output);
        assert_eq!(m.content, "hello world");
        assert_eq!(m.reasoning_content.as_deref(), Some("deliberating"));
        let calls = m.tool_calls.as_ref().unwrap();
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].name, "bash");
    }

    #[test]
    fn unknown_event_types_are_ignored() {
        let mut s = ResponsesStream::new();
        let ev = s.parse(r#"{"type":"response.created"}"#);
        assert!(ev.is_empty());
        // Garbage JSON is ignored too.
        assert!(s.parse("not json").is_empty());
    }
}
