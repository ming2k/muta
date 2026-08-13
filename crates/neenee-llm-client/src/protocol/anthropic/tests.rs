//! Tests for the Anthropic provider module.
//!
//! Adapted from the original monolithic `anthropic_compat.rs` tests to the new
//! layered API: tests call the pure `request::body` / `response::*` functions
//! directly rather than the old `provider.request_body` method.

use super::request::{self, BodyInput};
use super::response;
use super::*;
use neenee_core::{Effort, Role, ThinkingMode, Tool};
use serde_json::{Value, json};
use std::sync::Arc;

// ── request body shape ────────────────────────────────────────────────────

fn body_input<'a>(provider: &'a AnthropicMessagesProvider, stream: bool) -> BodyInput<'a> {
    BodyInput {
        model: &provider.endpoint.model,
        stream,
        tool_specs: None,
        max_tokens: provider.max_tokens,
        thinking: provider.thinking,
    }
}

#[test]
fn request_body_lifts_system_to_top_level() {
    let provider =
        AnthropicMessagesProvider::new("k".to_string(), "minimax-m3".to_string(), "https://x");
    let body = request::body(
        vec![
            Message::new(Role::System, "you are concise"),
            Message::new(Role::User, "hi"),
        ],
        body_input(&provider, false),
    );
    assert_eq!(body["system"][0]["type"], "text");
    assert_eq!(body["system"][0]["text"], "you are concise");
    let msgs = body["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0]["role"], "user");
}

#[test]
fn request_body_serializes_tool_result_as_user_block() {
    let provider =
        AnthropicMessagesProvider::new("k".to_string(), "minimax-m3".to_string(), "https://x");
    let body = request::body(
        vec![
            Message::new(Role::User, "run it"),
            Message {
                role: Role::Assistant,
                content: String::new(),
                tool_calls: Some(vec![neenee_core::ToolCall {
                    id: "toolu_1".to_string(),
                    name: "bash".to_string(),
                    arguments: "{}".to_string(),
                }]),
                ..Message::new(Role::Assistant, "")
            },
            Message {
                role: Role::Tool,
                content: "done".to_string(),
                tool_call_id: Some("toolu_1".to_string()),
                ..Message::new(Role::Tool, "")
            },
        ],
        body_input(&provider, false),
    );
    let msgs = body["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[2]["role"], "user");
    assert_eq!(msgs[2]["content"][0]["type"], "tool_result");
    assert_eq!(msgs[2]["content"][0]["tool_use_id"], "toolu_1");
}

struct DummyTool;
#[async_trait]
impl Tool for DummyTool {
    fn name(&self) -> &str {
        "dummy"
    }
    fn description(&self) -> &str {
        "test"
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{}})
    }
    async fn call(&self, _args: &str) -> Result<String, String> {
        Ok("ok".to_string())
    }
}

struct DummyTool2;
#[async_trait]
impl Tool for DummyTool2 {
    fn name(&self) -> &str {
        "dummy2"
    }
    fn description(&self) -> &str {
        "test2"
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{}})
    }
    async fn call(&self, _args: &str) -> Result<String, String> {
        Ok("ok".to_string())
    }
}

#[test]
fn request_body_includes_tools_in_anthropic_shape() {
    let provider =
        AnthropicMessagesProvider::new("k".to_string(), "minimax-m3".to_string(), "https://x");
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(DummyTool)];
    let request =
        neenee_core::ModelRequest::with_tools(vec![Message::new(Role::User, "hi")], &tools);
    let (messages, tool_specs) = request.into_parts();
    let body = request::body(
        messages,
        BodyInput {
            model: &provider.endpoint.model,
            stream: false,
            tool_specs: Some(&tool_specs),
            max_tokens: provider.max_tokens,
            thinking: provider.thinking,
        },
    );
    let tool = &body["tools"][0];
    assert_eq!(tool["name"], "dummy");
    assert!(tool.get("input_schema").is_some(), "needs input_schema");
    assert!(tool.get("function").is_none());
}

// ── prompt-caching breakpoints ────────────────────────────────────────────

/// Count every `cache_control` breakpoint across `tools` + `system` +
/// `messages`.
fn count_cache_breakpoints(body: &Value) -> usize {
    let mut n = 0;
    if let Some(tools) = body["tools"].as_array() {
        n += tools
            .iter()
            .filter(|t| t.get("cache_control").is_some())
            .count();
    }
    if let Some(system) = body["system"].as_array() {
        n += system
            .iter()
            .filter(|b| b.get("cache_control").is_some())
            .count();
    }
    if let Some(msgs) = body["messages"].as_array() {
        for msg in msgs {
            if let Some(blocks) = msg["content"].as_array() {
                n += blocks
                    .iter()
                    .filter(|b| b.get("cache_control").is_some())
                    .count();
            }
        }
    }
    n
}

#[test]
fn cache_breakpoints_hit_all_four_zones() {
    let provider =
        AnthropicMessagesProvider::new("k".to_string(), "minimax-m3".to_string(), "https://x");
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(DummyTool)];
    let request = neenee_core::ModelRequest::with_tools(
        vec![
            Message::new(Role::System, "you are a coding agent"),
            Message::new(Role::User, "do task A"),
            Message::new(Role::Assistant, "ok"),
            Message::new(Role::User, "now task B"),
            Message::new(Role::Assistant, "done"),
            Message::new(Role::User, "task C"),
        ],
        &tools,
    );
    let (messages, tool_specs) = request.into_parts();
    let body = request::body(
        messages,
        BodyInput {
            model: &provider.endpoint.model,
            stream: false,
            tool_specs: Some(&tool_specs),
            max_tokens: provider.max_tokens,
            thinking: provider.thinking,
        },
    );
    assert_eq!(body["tools"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    let msgs = body["messages"].as_array().unwrap();
    let last = &msgs[msgs.len() - 1]["content"][0];
    let prev = &msgs[msgs.len() - 2]["content"][0];
    assert_eq!(last["cache_control"]["type"], "ephemeral");
    assert_eq!(prev["cache_control"]["type"], "ephemeral");
    let earlier = &msgs[0]["content"][0];
    assert!(earlier.get("cache_control").is_none());
    assert_eq!(count_cache_breakpoints(&body), 4);
}

#[test]
fn cache_breakpoints_never_exceed_four_cap() {
    let provider =
        AnthropicMessagesProvider::new("k".to_string(), "minimax-m3".to_string(), "https://x");
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(DummyTool), Arc::new(DummyTool2)];
    let history: Vec<Message> = (0..8)
        .flat_map(|i| {
            vec![
                Message::new(Role::User, format!("u{i}")),
                Message::new(Role::Assistant, format!("a{i}")),
            ]
        })
        .collect();
    let request = neenee_core::ModelRequest::with_tools(history, &tools);
    let (messages, tool_specs) = request.into_parts();
    let body = request::body(
        messages,
        BodyInput {
            model: &provider.endpoint.model,
            stream: false,
            tool_specs: Some(&tool_specs),
            max_tokens: provider.max_tokens,
            thinking: provider.thinking,
        },
    );
    assert!(
        count_cache_breakpoints(&body) <= 4,
        "must not exceed the 4-breakpoint cap"
    );
}

#[test]
fn cache_breakpoints_use_default_five_minute_ttl() {
    let provider =
        AnthropicMessagesProvider::new("k".to_string(), "minimax-m3".to_string(), "https://x");
    let body = request::body(
        vec![
            Message::new(Role::System, "sys"),
            Message::new(Role::User, "hi"),
            Message::new(Role::Assistant, "hey"),
            Message::new(Role::User, "bye"),
        ],
        body_input(&provider, false),
    );
    let breakpoint_with_ttl = ["tools", "system"]
        .iter()
        .filter_map(|key| body[*key].as_array())
        .flatten()
        .chain(
            body["messages"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|m| m["content"].as_array().into_iter().flatten()),
        )
        .find(|b| b.get("cache_control").is_some() && b["cache_control"].get("ttl").is_some());
    assert!(
        breakpoint_with_ttl.is_none(),
        "no breakpoint should carry a ttl override"
    );
}

#[test]
fn cache_breakpoints_degrade_when_zones_absent() {
    let provider =
        AnthropicMessagesProvider::new("k".to_string(), "minimax-m3".to_string(), "https://x");
    let body = request::body(
        vec![
            Message::new(Role::User, "first"),
            Message::new(Role::Assistant, "second"),
            Message::new(Role::User, "third"),
        ],
        body_input(&provider, false),
    );
    assert!(body.get("system").is_none());
    assert!(body.get("tools").is_none());
    assert_eq!(count_cache_breakpoints(&body), 2);
}

#[test]
fn cache_breakpoints_skip_non_stampable_system_shape() {
    let provider =
        AnthropicMessagesProvider::new("k".to_string(), "minimax-m3".to_string(), "https://x");
    let body = request::body(
        vec![
            Message::new(Role::System, ""),
            Message::new(Role::User, "hi"),
            Message::new(Role::Assistant, "yo"),
        ],
        body_input(&provider, false),
    );
    assert!(body.get("system").is_none() || body["system"].as_array().is_none());
    assert_eq!(count_cache_breakpoints(&body), 2);
}

// ── extended-thinking / effort stamping ───────────────────────────────────

#[test]
fn claude_request_body_omits_thinking_by_default() {
    let provider =
        AnthropicMessagesProvider::new("k".to_string(), "claude-opus-4-8".to_string(), "https://x");
    let body = request::body(
        vec![Message::new(Role::User, "hi")],
        body_input(&provider, false),
    );
    assert!(
        body.get("thinking").is_none(),
        "Claude defaults to thinking off (opt-in)"
    );
    assert!(
        body.get("output_config").is_none(),
        "no explicit effort omits output_config"
    );
}

#[test]
fn claude_request_body_injects_adaptive_thinking_when_opted_in() {
    let provider =
        AnthropicMessagesProvider::new("k".to_string(), "claude-opus-4-8".to_string(), "https://x")
            .with_thinking(ThinkingConfig::default().with_mode(ThinkingMode::Adaptive));
    let body = request::body(
        vec![Message::new(Role::User, "hi")],
        body_input(&provider, false),
    );
    assert_eq!(body["thinking"]["type"], "adaptive");
    assert_eq!(body["thinking"]["display"], "summarized");
    assert!(
        body.get("output_config").is_none(),
        "default high effort omits output_config"
    );
}

#[test]
fn haiku_uses_manual_thinking_not_adaptive_when_opted_in() {
    let provider = AnthropicMessagesProvider::new(
        "k".to_string(),
        "claude-haiku-4-5-20251001".to_string(),
        "https://x",
    )
    .with_thinking(
        ThinkingConfig::default()
            .with_mode(ThinkingMode::Adaptive)
            .with_effort(Effort::Max),
    );
    let body = request::body(
        vec![Message::new(Role::User, "hi")],
        body_input(&provider, false),
    );
    assert_eq!(body["thinking"]["type"], "enabled", "manual, not adaptive");
    let budget = body["thinking"]["budget_tokens"].as_u64().unwrap();
    assert!(budget > 0 && budget < u64::from(provider.max_tokens));
    assert!(
        body.get("output_config").is_none(),
        "Haiku rejects effort — output_config must be dropped"
    );
    assert_eq!(
        request::beta_header(&provider.capabilities, provider.thinking),
        Some("interleaved-thinking-2025-05-14"),
    );
}

#[test]
fn haiku_omits_thinking_and_beta_when_off() {
    let provider = AnthropicMessagesProvider::new(
        "k".to_string(),
        "claude-haiku-4-5-20251001".to_string(),
        "https://x",
    );
    let body = request::body(
        vec![Message::new(Role::User, "hi")],
        body_input(&provider, false),
    );
    assert!(body.get("thinking").is_none());
    assert_eq!(
        request::beta_header(&provider.capabilities, provider.thinking),
        None
    );
}

#[test]
fn sonnet_46_clamps_xhigh_to_high_but_opus_48_keeps_it() {
    let sonnet = AnthropicMessagesProvider::new(
        "k".to_string(),
        "claude-sonnet-4-6".to_string(),
        "https://x",
    )
    .with_thinking(
        ThinkingConfig::default()
            .with_mode(ThinkingMode::Adaptive)
            .with_effort(Effort::Xhigh),
    );
    let body = request::body(
        vec![Message::new(Role::User, "hi")],
        body_input(&sonnet, false),
    );
    assert_eq!(body["output_config"]["effort"], "high");

    let opus =
        AnthropicMessagesProvider::new("k".to_string(), "claude-opus-4-8".to_string(), "https://x")
            .with_thinking(
                ThinkingConfig::default()
                    .with_mode(ThinkingMode::Adaptive)
                    .with_effort(Effort::Xhigh),
            );
    let body = request::body(
        vec![Message::new(Role::User, "hi")],
        body_input(&opus, false),
    );
    assert_eq!(body["output_config"]["effort"], "xhigh");
}

#[test]
fn unknown_relay_model_omits_thinking_by_default() {
    let provider = AnthropicMessagesProvider::new(
        "k".to_string(),
        "some-unknown-relay-model".to_string(),
        "https://x",
    );
    let body = request::body(
        vec![Message::new(Role::User, "hi")],
        body_input(&provider, false),
    );
    assert!(
        body.get("thinking").is_none(),
        "unknown relay model defaults to thinking off"
    );
}

#[test]
fn known_relay_model_also_defaults_to_thinking_off() {
    let provider =
        AnthropicMessagesProvider::new("k".to_string(), "minimax-m3".to_string(), "https://x");
    let body = request::body(
        vec![Message::new(Role::User, "hi")],
        body_input(&provider, false),
    );
    assert!(
        body.get("thinking").is_none(),
        "known relay model also defaults to thinking off (opt-in)"
    );
}

#[test]
fn non_default_effort_is_stamped_into_output_config() {
    let provider =
        AnthropicMessagesProvider::new("k".to_string(), "claude-opus-4-8".to_string(), "https://x")
            .with_thinking(
                ThinkingConfig::default()
                    .with_mode(ThinkingMode::Adaptive)
                    .with_effort(Effort::Max),
            );
    let body = request::body(
        vec![Message::new(Role::User, "hi")],
        body_input(&provider, false),
    );
    assert_eq!(body["output_config"]["effort"], "max");
    assert_eq!(body["thinking"]["type"], "adaptive");
}

#[test]
fn effort_clamps_to_model_support_levels() {
    let cfg = ThinkingConfig::default().with_effort(Effort::Xhigh);
    let common: Vec<neenee_core::EffortLevel> = neenee_core::EFFORT_COMMON
        .iter()
        .copied()
        .map(Into::into)
        .collect();
    let resolved = cfg.resolve_for(&common);
    assert_eq!(
        resolved.effort,
        Some(Effort::High),
        "xhigh clamps to high on a common-only model"
    );
    let claude: Vec<neenee_core::EffortLevel> = neenee_core::EFFORT_CLAUDE_FULL
        .iter()
        .copied()
        .map(Into::into)
        .collect();
    let resolved_claude = cfg.resolve_for(&claude);
    assert_eq!(resolved_claude.effort, Some(Effort::Xhigh));
}

#[test]
fn explicit_high_effort_is_honored_not_swallowed() {
    let provider =
        AnthropicMessagesProvider::new("k".to_string(), "claude-opus-4-8".to_string(), "https://x")
            .with_thinking(ThinkingConfig::default().with_effort(Effort::High));
    let body = request::body(
        vec![Message::new(Role::User, "hi")],
        body_input(&provider, false),
    );
    assert_eq!(
        body["output_config"]["effort"], "high",
        "an explicitly-pinned high effort must be emitted, not swallowed"
    );
}

#[test]
fn effort_without_thinking_stays_decoupled() {
    let provider =
        AnthropicMessagesProvider::new("k".to_string(), "claude-opus-4-8".to_string(), "https://x")
            .with_thinking(ThinkingConfig::default().with_effort(Effort::Medium));
    let body = request::body(
        vec![Message::new(Role::User, "hi")],
        body_input(&provider, false),
    );
    assert_eq!(body["output_config"]["effort"], "medium");
    assert!(
        body.get("thinking").is_none(),
        "effort alone must not enable thinking — the two stay decoupled"
    );
}

// ── extended-thinking replay (message conversion) ─────────────────────────

#[test]
fn assistant_message_replays_signed_thinking_block() {
    let prior = Message {
        role: Role::Assistant,
        content: "answer".to_string(),
        reasoning_content: Some("let me think".to_string()),
        provider_meta: Some({
            let mut m = serde_json::Map::new();
            m.insert(
                "thinking_signature".to_string(),
                Value::String("sig_abc".to_string()),
            );
            m
        }),
        ..Message::new(Role::Assistant, "")
    };
    let wire = request::message_obj(prior);
    let blocks = wire["content"]
        .as_array()
        .expect("content is a block array");
    assert_eq!(blocks[0]["type"], "thinking");
    assert_eq!(blocks[0]["thinking"], "let me think");
    assert_eq!(
        blocks[0]["signature"], "sig_abc",
        "signature must round-trip"
    );
    assert_eq!(blocks[1]["type"], "text");
    assert_eq!(blocks[1]["text"], "answer");
}

#[test]
fn assistant_message_replays_unsigned_thinking_without_signature() {
    let prior = Message {
        role: Role::Assistant,
        content: "x".to_string(),
        reasoning_content: Some("hmm".to_string()),
        provider_meta: None,
        ..Message::new(Role::Assistant, "")
    };
    let wire = request::message_obj(prior);
    let block = &wire["content"][0];
    assert_eq!(block["type"], "thinking");
    assert_eq!(block["thinking"], "hmm");
    assert!(
        block.get("signature").is_none(),
        "no signature key when none was captured"
    );
}

#[test]
fn assistant_message_omits_thinking_block_when_no_reasoning() {
    let prior = Message::new(Role::Assistant, "just text");
    let wire = request::message_obj(prior);
    let blocks = wire["content"].as_array().unwrap();
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0]["type"], "text");
}

// ── usage parsing ─────────────────────────────────────────────────────────

#[test]
fn anthropic_usage_folds_cache_tokens_into_prompt_total() {
    let usage = json!({
        "input_tokens": 200,
        "output_tokens": 50,
        "cache_creation_input_tokens": 5000,
        "cache_read_input_tokens": 8000,
    });
    let parsed = response::usage(&usage).expect("usage parses");
    assert_eq!(
        parsed.prompt_tokens, 13200,
        "cache tokens folded into prompt"
    );
    assert_eq!(parsed.completion_tokens, 50);
    assert_eq!(parsed.total_tokens, 13250);
    assert_eq!(parsed.cache_creation_input_tokens, 5000);
    assert_eq!(parsed.cache_read_input_tokens, 8000);
}

#[test]
fn anthropic_usage_without_cache_fields_defaults_to_zero() {
    let usage = json!({"input_tokens": 100, "output_tokens": 30});
    let parsed = response::usage(&usage).expect("usage parses");
    assert_eq!(parsed.prompt_tokens, 100);
    assert_eq!(parsed.total_tokens, 130);
    assert_eq!(parsed.cache_creation_input_tokens, 0);
    assert_eq!(parsed.cache_read_input_tokens, 0);
}

#[test]
fn anthropic_usage_absent_returns_none() {
    assert!(response::usage(&json!({})).is_none());
    let parsed = response::usage(&json!({"output_tokens": 5})).unwrap();
    assert_eq!(parsed.prompt_tokens, 0);
    assert_eq!(parsed.completion_tokens, 5);
    assert_eq!(parsed.total_tokens, 5);
}

// ── signature stash ───────────────────────────────────────────────────────

#[test]
fn take_last_provider_meta_drains_thinking_signature() {
    let provider =
        AnthropicMessagesProvider::new("k".to_string(), "claude-opus-4-8".to_string(), "https://x");
    provider.last_thinking_signature.set("sig_xyz".to_string());
    let meta = provider.take_last_provider_meta().expect("some meta");
    assert_eq!(meta["thinking_signature"], "sig_xyz");
    assert!(
        provider.take_last_provider_meta().is_none(),
        "stash drained on first take"
    );
}

#[test]
fn prompt_hints_emit_no_system_guidance() {
    let provider = AnthropicMessagesProvider::new(
        "k".to_string(),
        "claude-sonnet-4-6".to_string(),
        "https://x",
    );
    // No protocol note: thinking signatures travel as opaque provider_meta
    // and replay into the wire thinking block only — never into content the
    // model can read, so there is nothing for a prompt note to guard.
    assert!(provider.prompt_hints().system_guidance.is_empty());
}
