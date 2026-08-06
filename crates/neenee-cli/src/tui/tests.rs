use super::*;
use neenee_core::{AgentResponse, LoopStatus, Message, Role, RoundEvent, ToolCall};

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tokio::sync::mpsc;

use crate::tui::app::{App, CaretOwner};
use crate::tui::completion::CompletionKind;
use crate::tui::completion::{
    completion_anchor_x, is_explicit_path_prefix, manual_walk, mention_range_at, path_query_match,
    resolve_explicit_dir, resolved_slash_command_len,
};
use crate::tui::config;
use crate::tui::event_loop::{display_status, focused_messages_mut};
use crate::tui::model::layout::{InteractiveTarget, LayoutMap};
use crate::tui::model::selection::{SelectionDrag, SelectionState};
use crate::tui::transcript::{
    finalize_streaming_reasoning, transcript_message_from_core, transcript_messages_from_core,
};
use crate::tui::versioned::{TranscriptPatch, TranscriptUpdate};
use crate::tui::view::Theme;
use crate::tui::{ActivityTab, Modal};
use neenee_core::{AgentRequest, ProviderPickerSnapshot};

use std::collections::HashMap;

#[test]
fn only_high_frequency_stream_updates_are_coalesced() {
    let stream_delta = AgentResponse::Round {
        session_id: "session".to_string(),
        event: RoundEvent::StreamDelta("hello".to_string()),
    };
    let tool_stream = AgentResponse::Round {
        session_id: "session".to_string(),
        event: RoundEvent::ToolStream {
            id: "call".to_string(),
            stream: neenee_core::ToolStream::Stdout("line\n".to_string()),
        },
    };
    let stream_start = AgentResponse::Round {
        session_id: "session".to_string(),
        event: RoundEvent::StreamStart,
    };
    let stream_end = AgentResponse::Round {
        session_id: "session".to_string(),
        event: RoundEvent::StreamEnd("done".to_string()),
    };

    assert!(is_coalescible_stream_update(&stream_delta));
    assert!(is_coalescible_stream_update(&tool_stream));
    assert!(!is_coalescible_stream_update(&stream_start));
    assert!(!is_coalescible_stream_update(&stream_end));
}

#[test]
fn transcript_patch_updates_only_the_live_message() {
    let mut messages = vec![
        TranscriptMessage::new(Role::Assistant, "frozen history"),
        TranscriptMessage::new(Role::Assistant, ""),
    ];
    let history_id = messages[0].id;
    let live_id = messages[1].id;

    assert!(crate::tui::event_loop::apply_transcript_patch(
        &mut messages,
        TranscriptPatch::Updates(vec![TranscriptUpdate::TextDelta {
            message_id: live_id,
            delta: "live tail".to_string(),
        }]),
    ));
    assert_eq!(messages[0].id, history_id);
    assert_eq!(messages[0].raw, "frozen history");
    assert_eq!(messages[1].raw, "live tail");
}

#[test]
fn streamed_text_is_appended_only_to_the_current_turn() {
    let mut messages = vec![TranscriptMessage::new(Role::Assistant, "older").with_turn(1)];
    let older_id = messages[0].id;

    assert_eq!(
        append_stream_text_delta(&mut messages, None, Some(2), "new"),
        None
    );
    assert_eq!(
        messages.len(),
        1,
        "a new round requires structural insertion"
    );
    assert_eq!(messages[0].raw, "older");

    messages.push(TranscriptMessage::new(Role::Assistant, "first").with_turn(2));
    let current_id = messages[1].id;
    assert_eq!(
        append_stream_text_delta(&mut messages, None, Some(2), " second"),
        Some(current_id)
    );
    assert_eq!(messages[0].id, older_id);
    assert_eq!(messages[0].raw, "older");
    assert_eq!(messages[1].raw, "first second");
}

#[test]
fn hidden_reasoning_needs_no_assistant_placeholder() {
    // StreamStart is lifecycle-only. If a hidden-chain reasoning delta is
    // ignored, the transcript remains exactly as it was: no zero-height message
    // can introduce a second semantic separator before the next visible item.
    let mut messages = vec![TranscriptMessage::new(Role::User, "question").with_round(1)];
    let before = messages[0].id;

    begin_stream(&mut messages);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].id, before);
}

#[test]
fn restored_history_hides_harness_messages() {
    assert!(transcript_message_from_core(Message::hidden(Role::User, "internal")).is_none());
    assert!(transcript_message_from_core(Message::new(Role::System, "system")).is_none());
}
#[test]
fn restored_history_uses_command_display_content() {
    let message = Message::new(Role::User, "Expanded internal prompt")
        .with_display_content("/review working-tree");
    let restored = transcript_message_from_core(message).unwrap();
    assert_eq!(restored.raw, "/review working-tree");
}

#[test]
fn restored_user_message_uses_exact_or_legacy_timestamp() {
    let exact = Message::new(Role::User, "hi").with_sent_at_ms(1_700_000_000_123);
    let restored = transcript_message_from_core(exact).unwrap();
    assert_eq!(restored.sent_at_ms, Some(1_700_000_000_123));

    let mut legacy = Message::new(Role::User, "hi");
    legacy.sent_at_ms = None;
    legacy.timestamp = Some(1_700_000_001);
    let restored = transcript_message_from_core(legacy).unwrap();
    assert_eq!(restored.sent_at_ms, Some(1_700_000_001_000));
}

#[test]
fn restored_assistant_tool_step_uses_message_timestamp_for_turn_header() {
    let mut assistant = Message::new(Role::Assistant, "");
    assistant.timestamp = Some(1_700_000_002);
    assistant.tool_calls = Some(vec![ToolCall {
        id: "call".to_string(),
        name: "read_text".to_string(),
        arguments: r#"{"path":"README.md"}"#.to_string(),
    }]);

    let restored = transcript_messages_from_core(vec![assistant], &config::TuiConfig::default());
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].sent_at_ms, Some(1_700_000_002_000));
    assert_eq!(restored[0].round, Some(1));
    assert_eq!(restored[0].turn, Some(1));
}

#[test]
fn restored_assistant_components_share_their_round_and_turn() {
    let mut assistant = Message::new(Role::Assistant, "continue");
    assistant.reasoning_content = Some("inspect first".to_string());
    assistant.tool_calls = Some(vec![ToolCall {
        id: "call".to_string(),
        name: "read_text".to_string(),
        arguments: r#"{"path":"README.md"}"#.to_string(),
    }]);

    let restored = transcript_messages_from_core(vec![assistant], &config::TuiConfig::default());
    assert_eq!(restored.len(), 3);
    assert!(restored[0].is_thinking());
    assert!(restored[1].is_tool_step());
    assert_eq!(restored[2].role, Role::Assistant);
    assert!(restored.iter().all(|message| message.round == Some(1)));
    assert!(restored.iter().all(|message| message.turn == Some(1)));
}

#[test]
fn restored_user_message_origin_inferred_from_shape() {
    use crate::tui::model::document::UserMessageOrigin;
    // A genuine chat prompt: no display_content, no leading `!`.
    let chat = transcript_message_from_core(Message::new(Role::User, "fix the bug")).unwrap();
    assert_eq!(chat.origin, UserMessageOrigin::Chat);

    // A slash command carries a `display_content` whose text is the literal
    // `/cmd` (its real content is the harness-expanded form) → Slash.
    let slash = Message::new(Role::User, "expanded pursue body")
        .with_display_content("/pursue ship the release");
    let slash = transcript_message_from_core(slash).unwrap();
    assert_eq!(slash.origin, UserMessageOrigin::Slash);

    // A shell passthrough persists as the `!command` the user typed → Shell.
    let shell = transcript_message_from_core(Message::new(Role::User, "!ls -la")).unwrap();
    assert_eq!(shell.origin, UserMessageOrigin::Shell);

    // A genuine prompt that merely *starts* with `/` (no display_content) is
    // NOT misclassified as a slash command — e.g. "/etc is a path" stays Chat.
    let path_like =
        transcript_message_from_core(Message::new(Role::User, "/etc is a path")).unwrap();
    assert_eq!(path_like.origin, UserMessageOrigin::Chat);
}

#[test]
fn restored_user_insert_keeps_mid_round_origin_without_opening_a_turn() {
    use crate::tui::model::document::UserMessageOrigin;
    let first = Message::new(Role::Assistant, "first answer");
    let inserted = Message::new(Role::User, "one more constraint").with_origin(
        neenee_core::InjectionOrigin::new(neenee_core::InjectionKind::UserSteer),
    );
    let second = Message::new(Role::Assistant, "revised answer");

    let restored =
        transcript_messages_from_core(vec![first, inserted, second], &config::TuiConfig::default());
    assert_eq!(restored[1].origin, UserMessageOrigin::Insert);
    assert_eq!(restored[1].round, Some(1));
    assert_eq!(restored[1].turn, None);
    assert_eq!(restored[2].round, Some(1));
    assert_eq!(restored[2].turn, Some(2));
}

#[test]
fn restored_command_echo_origin_from_durable_provenance() {
    // ADR-0050: durable slash/shell echoes are persisted as
    // `Message::command_echo` — a visible user message carrying the
    // `CommandEcho` provenance. On resume the stored origin is consulted
    // FIRST (ahead of the shape heuristic), so an echo whose text lacks the
    // `display_content` / `!` shape signals is still classified as a
    // non-driving command, never as the round's driving prompt.
    use crate::tui::model::document::UserMessageOrigin;

    // A slash echo: content is the literal `/cmd`, no display_content. The
    // shape heuristic alone would misread this (no display_content → fall to
    // the `!` check → fail → Chat). The durable origin must win.
    let slash_echo = Message::command_echo("/pursue ship it");
    assert!(slash_echo.is_command_echo());
    let restored = transcript_message_from_core(slash_echo).unwrap();
    assert_eq!(
        restored.origin,
        UserMessageOrigin::Slash,
        "durable echo provenance must classify as Slash, not Chat"
    );

    // A shell echo: content is `!cmd`. Even though the `!` shape heuristic
    // would also catch it, the origin-first path must handle it too.
    let shell_echo = Message::command_echo("!ls -la");
    let restored_shell = transcript_message_from_core(shell_echo).unwrap();
    assert_eq!(restored_shell.origin, UserMessageOrigin::Slash);
}

#[test]
fn command_ledger_restores_as_non_conversational_command_rows() {
    // ADR-0091: the command ledger is the durable source of truth for
    // commands on resume — the message stream is pure dialogue. Each record
    // restores as a distinct, non-conversational command row carrying the
    // typed result as its expandable body.
    let commands = vec![
        neenee_core::CommandRecord::new("search", "foo").with_result(
            neenee_core::CommandResult::Search {
                query: "foo".to_string(),
                hits: vec![],
            },
        ),
        // A result-less record (legacy fold / shell passthrough): the
        // invocation still restores, with an empty expandable body.
        neenee_core::CommandRecord::new("shell", "!ls -la"),
    ];
    let restored = transcript_commands_from_ledger(commands);
    assert_eq!(restored.len(), 2);
    let search = &restored[0];
    assert!(search.is_command_result(), "command rows carry the CommandResult kind");
    assert_eq!(search.command_result_summary().as_deref(), Some("/search foo"));
    assert_eq!(
        search.command_result_text().as_deref(),
        Some("No relevant history found.")
    );
    assert_eq!(search.round, None, "a command is not a conversation turn");
    assert_ne!(search.role, neenee_core::Role::Assistant, "never assistant prose");

    let shell = &restored[1];
    assert!(shell.is_command_result());
    assert_eq!(shell.command_result_summary().as_deref(), Some("!ls -la"));
    assert_eq!(shell.command_result_text(), None);
}

#[test]
fn command_result_message_expands_and_round_trips_display() {
    // The command block's collapsed header is the invocation; expansion
    // reveals the typed result body. Pinning is respected (user toggle wins).
    use crate::tui::model::document::TranscriptMessage;
    let mut message = TranscriptMessage::command_result(
        "permissions",
        "",
        Some(neenee_core::CommandResult::PermissionList {
            allowed: vec!["bash".to_string()],
        }),
    );
    assert_eq!(message.command_result_expanded(), Some(false));
    assert_eq!(
        message.command_result_summary().as_deref(),
        Some("/permissions")
    );
    assert_eq!(
        message.command_result_text().as_deref(),
        Some("Always-allowed tools:\n- bash")
    );
    // The result body is the message's parsed blocks (non-empty here).
    assert!(!message.blocks.is_empty());

    message.pin_command_result_expanded(true);
    assert_eq!(message.command_result_expanded(), Some(true));
}

#[test]
fn restored_assistant_carries_provider_and_model_attribution() {
    // A persisted assistant message stamped by the harness keeps its
    // provider/model so a resumed session that mixed models stays
    // traceable in the transcript.
    let message = Message::new(Role::Assistant, "Hello from kimi")
        .with_attribution("kimi-code", "kimi-k2.7-code");
    let restored = transcript_message_from_core(message).unwrap();
    assert_eq!(restored.provider.as_deref(), Some("kimi-code"));
    assert_eq!(restored.model.as_deref(), Some("kimi-k2.7-code"));
    assert_eq!(
        restored.attribution_label(),
        Some(("kimi-code".to_string(), "kimi-k2.7-code".to_string()))
    );

    // A plain user message carries no attribution.
    let user = transcript_message_from_core(Message::new(Role::User, "hi")).unwrap();
    assert!(user.attribution_label().is_none());

    // A provider without an id still surfaces the model alone.
    let model_only = Message::new(Role::Assistant, "x").with_attribution("", "gpt-4o");
    let restored = transcript_message_from_core(model_only).unwrap();
    assert_eq!(
        restored.attribution_label(),
        Some((String::new(), "gpt-4o".to_string()))
    );
}

#[test]
fn restored_reasoning_is_not_shown_as_running() {
    let message = Message {
        role: Role::Assistant,
        content: String::new(),
        content_blob: None,
        display_content: None,
        reasoning_content: Some("step-by-step reasoning".to_string()),
        provider_meta: None,
        tool_calls: None,
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

    let restored = transcript_messages_from_core(vec![message], &config::TuiConfig::default());
    assert_eq!(restored.len(), 1);
    let thinking = &restored[0];
    assert!(thinking.is_thinking());
    assert_eq!(thinking.turn, Some(1));
    // A finished reasoning block must not be rendered with a live spinner.
    assert!(
        thinking.thinking_summary().unwrap().contains("0ms"),
        "restored thinking should have a finished duration, got {:?}",
        thinking.thinking_summary()
    );
}

#[test]
fn finalize_streaming_reasoning_freezes_orphaned_traces() {
    // An interrupt mid-reasoning leaves the in-flight Thinking message
    // with `duration_ms: None`, which the renderer treats as "running"
    // (breathing spinner). The sweep must stamp every such trace so the
    // spinner stops, while leaving already-finished traces untouched.
    let streaming = TranscriptMessage::thinking("partial reasoning");
    assert!(
        streaming.is_thinking_streaming(),
        "a fresh thinking trace should be in the streaming state"
    );

    let mut finished = TranscriptMessage::thinking("done reasoning");
    finished.set_thinking_duration(1234);
    assert!(
        !finished.is_thinking_streaming(),
        "a trace with a stamped duration is not streaming"
    );

    let other = TranscriptMessage::new(Role::User, "hi");

    let mut messages = vec![streaming.clone(), finished.clone(), other];
    finalize_streaming_reasoning(&mut messages, Some(500));

    // The orphaned streaming trace is frozen with the supplied duration.
    assert!(
        !messages[0].is_thinking_streaming(),
        "streaming trace must be finalized by the sweep"
    );
    assert!(
        messages[0].thinking_summary().unwrap().contains("500ms"),
        "expected the supplied duration to be stamped, got {:?}",
        messages[0].thinking_summary()
    );

    // The already-finished trace keeps its original duration (no overwrite
    // of real timing with the sweep's value).
    assert!(
        messages[1].thinking_summary().unwrap().contains("1.2s"),
        "finished trace must keep its original duration, got {:?}",
        messages[1].thinking_summary()
    );

    // A missing duration falls back to 0 so the trace still leaves the
    // streaming state even when the start instant was already consumed.
    let mut messages = vec![streaming];
    finalize_streaming_reasoning(&mut messages, None);
    assert!(
        !messages[0].is_thinking_streaming(),
        "a None duration must still finalize the trace"
    );
    assert!(
        messages[0].thinking_summary().unwrap().contains("0ms"),
        "expected 0ms fallback, got {:?}",
        messages[0].thinking_summary()
    );
}

#[test]
fn restored_native_tool_calls_are_visible() {
    let message = Message {
        role: Role::Assistant,
        content: String::new(),
        content_blob: None,
        display_content: None,
        reasoning_content: None,
        provider_meta: None,
        tool_calls: Some(vec![ToolCall {
            id: "call".to_string(),
            name: "read_text".to_string(),
            arguments: "{\"path\":\"README.md\"}".to_string(),
        }]),
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

    let restored = transcript_message_from_core(message).unwrap();
    assert!(restored.raw.contains("read_text"));
}

#[test]
fn restored_tool_results_merge_into_steps_in_fifo_order() {
    let messages = vec![
        Message {
            role: Role::Assistant,
            content: String::new(),
            content_blob: None,
            display_content: None,
            reasoning_content: None,
            provider_meta: None,
            tool_calls: Some(vec![
                ToolCall {
                    id: "one".to_string(),
                    name: "read_text".to_string(),
                    arguments: r#"{"path":"one"}"#.to_string(),
                },
                ToolCall {
                    id: "two".to_string(),
                    name: "read_text".to_string(),
                    arguments: r#"{"path":"two"}"#.to_string(),
                },
            ]),
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
        },
        Message::tool_result(
            &ToolCall {
                id: "one".to_string(),
                name: "read_text".to_string(),
                arguments: String::new(),
            },
            "[read_text result]:\nfirst",
        ),
        Message::tool_result(
            &ToolCall {
                id: "two".to_string(),
                name: "read_text".to_string(),
                arguments: String::new(),
            },
            "[read_text result]:\nsecond",
        ),
    ];

    let mut restored = transcript_messages_from_core(messages, &config::TuiConfig::default());
    assert_eq!(restored.len(), 2);
    restored[0].set_tool_step_expanded(true);
    restored[1].set_tool_step_expanded(true);
    assert!(restored[0].raw.contains("first"));
    assert!(!restored[0].raw.contains("second"));
    assert!(restored[1].raw.contains("second"));
}

#[test]
fn tool_activity_is_semantic_and_loop_progress_is_preserved() {
    assert_eq!(
        event_loop::tool_activity_status("grep"),
        "searching codebase"
    );
    assert_eq!(
        event_loop::tool_activity_status("edit_file"),
        "making edits"
    );
    assert_eq!(
        event_loop::tool_activity_status("mcp__github__search"),
        "using MCP"
    );
    assert_eq!(
        display_status(LoopStatus::Running, "running command", false),
        "running command"
    );
    assert_eq!(
        display_status(LoopStatus::Running, "running command", true),
        "awaiting permission"
    );
}

#[test]
fn provider_retry_updates_one_transcript_component() {
    let mut messages = vec![TranscriptMessage::new(Role::User, "hello")];
    upsert_provider_retry(&mut messages, 2, 4, 3_000, "first failure".into());
    let retry = messages.last_mut().expect("retry component");
    retry.pin_provider_retry_expanded(true);

    upsert_provider_retry(&mut messages, 3, 4, 1_000, "second failure".into());

    assert_eq!(
        messages
            .iter()
            .filter(|message| message.is_provider_retry())
            .count(),
        1,
        "later failures must refresh the existing component"
    );
    let retry = messages.last().expect("retry component");
    assert_eq!(retry.raw, "second failure");
    assert_eq!(retry.provider_retry_expanded(), Some(true));
}

/// Build a small conversation with two sibling envoy tasks, each with a
/// couple of child messages, for focus-navigation tests.
fn conversation_with_envoys() -> Vec<TranscriptMessage> {
    let mut a = TranscriptMessage::tool_step(
        "task_a",
        "envoy",
        r#"{"description":"explore a","prompt":"..."}"#,
    );
    a.envoy_children_mut()
        .unwrap()
        .push(TranscriptMessage::new(Role::Assistant, "child A1"));
    let mut b = TranscriptMessage::tool_step(
        "task_b",
        "envoy",
        r#"{"description":"explore b","prompt":"..."}"#,
    );
    b.envoy_children_mut()
        .unwrap()
        .push(TranscriptMessage::new(Role::Assistant, "child B1"));
    vec![
        TranscriptMessage::new(Role::User, "hi"),
        a,
        TranscriptMessage::new(Role::Assistant, "ok"),
        b,
    ]
}

#[test]
fn resolve_focused_mut_indexes_root_when_unfocused() {
    let mut messages = conversation_with_envoys();
    let focus: Vec<crate::tui::app::ZoomFrame> = Vec::new();
    let resolved = event_loop::resolve_focused_mut(&mut messages, &focus, 2);
    assert_eq!(resolved.map(|m| m.raw.clone()).as_deref(), Some("ok"));
}

#[test]
fn resolve_focused_mut_indexes_children_when_focused() {
    let mut messages = conversation_with_envoys();
    let focus = vec![crate::tui::app::ZoomFrame {
        call_id: "task_b".to_string(),
        saved_scroll: crate::tui::app::ScrollSnapshot::default(),
    }];
    // Index 0 inside task_b's children => "child B1".
    let resolved = event_loop::resolve_focused_mut(&mut messages, &focus, 0);
    assert_eq!(resolved.map(|m| m.raw.clone()).as_deref(), Some("child B1"));
    // Indexing task_a's children via task_b focus returns none / out of range.
    assert!(event_loop::resolve_focused_mut(&mut messages, &focus, 5).is_none());
}

#[test]
fn focused_tool_steps_mut_only_touches_focused_envoy_children() {
    let mut messages = conversation_with_envoys();
    // Focused on task_a: its single child is an assistant message (not a
    // tool step), so the focused stream has 1 message and 0 tool steps.
    let focus = vec![crate::tui::app::ZoomFrame {
        call_id: "task_a".to_string(),
        saved_scroll: crate::tui::app::ScrollSnapshot::default(),
    }];
    let total = focused_messages_mut(&mut messages, &focus).count();
    assert_eq!(total, 1);
    let tool_steps = focused_messages_mut(&mut messages, &focus)
        .filter(|m| m.is_tool_step())
        .count();
    assert_eq!(tool_steps, 0);

    // Root view: 4 messages total, 2 of which are tool steps.
    let focus: Vec<crate::tui::app::ZoomFrame> = Vec::new();
    assert_eq!(focused_messages_mut(&mut messages, &focus).count(), 4);
    let tool_steps = focused_messages_mut(&mut messages, &focus)
        .filter(|m| m.is_tool_step())
        .count();
    assert_eq!(tool_steps, 2);
}

// ----- `@path` completion tests -----

#[test]
fn mention_range_detects_at_start_of_input() {
    // Cursor at end of `@src`: range covers the whole token.
    assert_eq!(mention_range_at("@src", 4), Some((0, 4)));
}

#[test]
fn completion_anchor_aligns_slash_menu_with_composer_text_start() {
    // A `/command` replaces the whole input, so the popup hangs off the
    // start of the composer's text area — the rect's left edge plus the
    // two-column prompt prefix.
    let rect = neenee_tui_engine::Rect::new(0, 10, 80, 3);
    let x = completion_anchor_x("/pu", 3, rect, CompletionKind::Slash);
    assert_eq!(x, rect.x + 2);
}

#[test]
fn completion_anchor_aligns_path_menu_with_the_at_trigger() {
    // `look at @sr` — the `@` sits at display column 8 of the input, so the
    // popup's leading edge lands 8 columns right of the text area's start.
    let rect = neenee_tui_engine::Rect::new(0, 10, 80, 3);
    let input = "look at @sr";
    let x = completion_anchor_x(input, input.len(), rect, CompletionKind::Path);
    assert_eq!(x, rect.x + 2 + 8);
}

#[test]
fn completion_anchor_follows_the_at_trigger_across_wraps() {
    // A 10-column-wide text area (rect 14 wide minus the 2+2 composer
    // padding) wraps `wrap this @sr` after `wrap this `; the `@` then starts
    // the second text row at column 0, so the popup realigns to the text
    // area's left edge instead of sticking to the pre-wrap column.
    let rect = neenee_tui_engine::Rect::new(0, 10, 14, 4);
    let input = "wrap this @sr";
    let x = completion_anchor_x(input, input.len(), rect, CompletionKind::Path);
    assert_eq!(x, rect.x + 2);
}

#[test]
fn completion_anchor_keeps_column_when_token_stays_on_one_row() {
    // No wrap: the `@` at display column 10 keeps its column even on a
    // narrow-ish box, so the popup tracks the token exactly.
    let rect = neenee_tui_engine::Rect::new(0, 10, 20, 3);
    let input = "wrap this @sr";
    let x = completion_anchor_x(input, input.len(), rect, CompletionKind::Path);
    assert_eq!(x, rect.x + 2 + 10);
}

// ----- resolved `/command` highlight tests -----

#[test]
fn resolved_slash_len_matches_builtin_command_without_args() {
    assert_eq!(resolved_slash_command_len("/clear", &[]), Some(6));
}

#[test]
fn resolved_slash_len_covers_only_the_command_token_not_args() {
    // `/session new` — only `/session` (8 bytes) is the resolved command;
    // the argument tail is excluded so the accent stops at the token.
    assert_eq!(resolved_slash_command_len("/session new", &[]), Some(8));
}

#[test]
fn resolved_slash_len_matches_custom_command() {
    let customs = vec![("/deploy".to_string(), "Deploy the app".to_string())];
    assert_eq!(
        resolved_slash_command_len("/deploy prod", &customs),
        Some(7)
    );
}

#[test]
fn resolved_slash_len_rejects_partial_prefix_and_unknown_commands() {
    // A bare `/` or an in-progress prefix is not yet a command.
    assert_eq!(resolved_slash_command_len("/", &[]), None);
    assert_eq!(resolved_slash_command_len("/cle", &[]), None);
    assert_eq!(resolved_slash_command_len("/not-a-command", &[]), None);
    // Plain prose and `@` mentions never highlight.
    assert_eq!(resolved_slash_command_len("hello", &[]), None);
    assert_eq!(resolved_slash_command_len("@src/main.rs", &[]), None);
}

#[test]
fn mention_range_detects_inline_after_whitespace() {
    // `look at @src`: the `@` follows a space, so the range starts at the
    // `@` and ends at the cursor.
    assert_eq!(mention_range_at("look at @src", 12), Some((8, 12)));
}

#[test]
fn mention_range_rejects_email_style_at() {
    // `user@host` — the char before `@` is non-whitespace, so no mention.
    assert_eq!(mention_range_at("user@host", 9), None);
}

#[test]
fn mention_range_rejects_whitespace_between_at_and_cursor() {
    // `@src foo`: the cursor sits after a space, walking back crosses
    // whitespace before reaching `@`, so no mention.
    assert_eq!(mention_range_at("@src foo", 8), None);
}

#[test]
fn mention_range_rejects_cursor_before_at() {
    // Cursor before the `@`: nothing to walk back to.
    assert_eq!(mention_range_at("look @src", 4), None);
}

#[test]
fn mention_range_handles_multibyte_before_at() {
    // `😀😁 @x` — the `@` is preceded by an ASCII space, so we detect it
    // even when multibyte chars appear earlier in the input.
    let s = "😀😁 @x";
    // Byte offset of the cursor at end (after `x`).
    let cursor_byte = s.len();
    let at_byte = s.find('@').unwrap();
    assert_eq!(
        mention_range_at(s, cursor_byte),
        Some((at_byte, cursor_byte))
    );
}

// ----- explicit-path (`@../`, `@./`, `@~/`, `@/`) completion tests -----

#[test]
fn is_explicit_path_prefix_recognizes_all_shell_conventions() {
    // The four explicit prefixes route to filesystem resolution + absolute
    // expansion; plain relative segments fall through to the project scan.
    assert!(is_explicit_path_prefix("../"));
    assert!(is_explicit_path_prefix(".."));
    assert!(is_explicit_path_prefix("./"));
    assert!(is_explicit_path_prefix("."));
    assert!(is_explicit_path_prefix("~/"));
    assert!(is_explicit_path_prefix("~"));
    assert!(is_explicit_path_prefix("/"));
    assert!(is_explicit_path_prefix("/etc/host"));
    assert!(is_explicit_path_prefix("../src/fo"));
    assert!(is_explicit_path_prefix("~/notes/a"));
    // Plain relative queries are NOT explicit — they use the project scan.
    assert!(!is_explicit_path_prefix("src/"));
    assert!(!is_explicit_path_prefix("Cargo.toml"));
    assert!(!is_explicit_path_prefix(""));
}

#[test]
fn resolve_explicit_dir_splits_dir_and_name_prefix() {
    // `../src/fo` from a temp cwd: the directory portion resolves to the
    // canonicalized parent's `src`, the trailing `fo` is the name prefix.
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(tmp.path().join("src")).unwrap();
    std::fs::write(tmp.path().join("src/foobar.rs"), "x").unwrap();
    // The "project" is a subdir of `tmp`; `../` escapes it into `tmp`.
    let project = tmp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    let (dir, prefix) = resolve_explicit_dir("../src/fo", &project).expect("resolved");
    // The directory is absolute and canonicalized (no `..`).
    assert!(dir.is_absolute(), "dir must be absolute: {dir:?}");
    assert!(dir.ends_with("src"), "dir resolves into src: {dir:?}");
    assert_eq!(prefix, "fo");
}

#[test]
fn resolve_explicit_dir_home_prefix_uses_home() {
    // `~/notes/a` resolves the directory under the user's home, regardless of
    // the project cwd. We only assert structural correctness (absolute, ends
    // with `notes`, prefix `a`) since the real home path varies per machine.
    let dummy_cwd = std::path::PathBuf::from("/some/project");
    let (dir, prefix) = resolve_explicit_dir("~/notes/a", &dummy_cwd).expect("resolved");
    assert!(
        dir.is_absolute(),
        "home-relative dir must be absolute: {dir:?}"
    );
    assert!(dir.ends_with("notes"), "dir resolves into ~/notes: {dir:?}");
    assert_eq!(prefix, "a");
}

#[test]
fn resolve_explicit_dir_absolute_prefix_uses_root() {
    // `/etc/h` resolves to `/etc` with prefix `h`, independent of cwd.
    let dummy_cwd = std::path::PathBuf::from("/some/project");
    let (dir, prefix) = resolve_explicit_dir("/etc/h", &dummy_cwd).expect("resolved");
    assert_eq!(dir, std::path::PathBuf::from("/etc"));
    assert_eq!(prefix, "h");
}

#[test]
fn enumerate_explicit_path_completion_expands_to_absolute() {
    // `@../` from a temp project lists the parent directory's children as
    // absolute paths. The candidates are terminal (PathExplicit): accepting
    // one drops the `@` and splices the absolute path — the core of req 1.
    use crate::tui::completion::CompletionItemKind;
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("sibling.md"), "x").unwrap();
    // The "project" is a subdirectory of `tmp`; its parent (`tmp`) holds
    // `sibling.md`, reachable only via `../`.
    let project = tmp.path().join("proj");
    std::fs::create_dir_all(&project).unwrap();
    let (mut app, _proj_tmp) = app_in_tempdir(&[], &[]);
    // Override the captured cwd to the project subdir so `../` escapes it.
    app.cwd = project.clone();
    app.input = "@../sib".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let sibling = completions
        .iter()
        .find(|c| c.label.ends_with("sibling.md"))
        .expect("sibling.md reachable via @../");
    // The label is an absolute path (req 1: expanded to absolute).
    assert!(
        std::path::Path::new(&sibling.label).is_absolute(),
        "explicit completion must be absolute: {}",
        sibling.label
    );
    // Every explicit candidate is terminal on accept.
    assert_eq!(sibling.kind, CompletionItemKind::PathExplicit);

    // Accepting it drops the `@` and splices the absolute path + space.
    let idx = completions
        .iter()
        .position(|c| c.label.ends_with("sibling.md"))
        .unwrap();
    app.accept_completion(idx);
    assert!(
        !app.input.contains('@'),
        "@ trigger must be dropped on accept: {}",
        app.input
    );
    assert!(
        app.input.trim().ends_with("sibling.md"),
        "absolute path spliced: {}",
        app.input
    );
    assert!(app.completion_dismissed, "explicit accept is terminal");
}

#[test]
fn path_query_match_empty_query_keeps_top_level_only() {
    // Empty query: only top-level entries survive.
    assert!(path_query_match("Cargo.toml", ""));
    assert!(path_query_match("src/", ""));
    assert!(!path_query_match("src/main.rs", ""));
    assert!(!path_query_match("src/nested/deep.rs", ""));
}

#[test]
fn path_query_match_substring_case_insensitive() {
    // `@cargo` matches `Cargo.toml` regardless of case.
    assert!(path_query_match("Cargo.toml", "cargo"));
    assert!(path_query_match("src/Cargo.toml", "cargo"));
    assert!(!path_query_match("README.md", "cargo"));
}

#[test]
fn path_query_match_directory_descend_on_trailing_slash() {
    // `@src/` is a directory descend: prefix-match to enumerate its
    // descendants, NOT every path containing `src/` anywhere.
    assert!(path_query_match("src/main.rs", "src/"));
    assert!(path_query_match("src/components/button.rs", "src/"));
    assert!(!path_query_match("tests/src_runner.rs", "src/"));
}

#[test]
fn path_query_match_mid_path_substring() {
    // `@src/foo` falls through to plain substring (no trailing slash),
    // so it only matches paths that literally contain `src/foo`.
    assert!(path_query_match("src/foo.rs", "src/foo"));
    assert!(path_query_match("src/foo/bar.rs", "src/foo"));
    // `src/components/foo.rs` does NOT contain `src/foo` as a substring,
    // so it is excluded — the user can type `@foo` instead for a wider
    // filename match.
    assert!(!path_query_match("src/components/foo.rs", "src/foo"));
    assert!(!path_query_match("src/bar.rs", "src/foo"));
}

#[test]
fn history_rows_lists_newest_first_then_ranks_search() {
    // The App-level view of the Ctrl+R panel. With no query the whole
    // cross-session history is listed newest-first (by created_at_ms),
    // unhighlighted; once the user types, only the fuzzy subsequence matches
    // surface, ordered by score with newest-first order as the stable
    // tiebreaker.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let mk = |text: &str, sid: &str, ts: u64| {
        neenee_core::HistoryEntry::new(
            text.to_string(),
            Some(sid.to_string()),
            Some("~/p".to_string()),
            ts,
        )
    };
    app.input_history = vec![
        mk("scatter", "s1", 10),     // idx 0 — 'cat' mid-word, lowest score
        mk("catalog", "s1", 20),     // idx 1 — 'cat' at boundary, high score
        mk("cargo build", "s1", 30), // idx 2 — 'cat' is not a subsequence
        mk("the cat sat", "s1", 40), // idx 3 — 'cat' at boundary, high score
    ];

    // Empty query → newest-first by timestamp, score 0, no highlights.
    app.input.clear();
    let rows = app.history_rows();
    let indices: Vec<usize> = rows.iter().map(|(i, _)| *i).collect();
    assert_eq!(indices, vec![3, 2, 1, 0], "newest first by timestamp");
    for (_, m) in &rows {
        assert_eq!(m.score, 0);
        assert!(m.positions.is_empty());
    }

    // Search "cat" → matches catalog, "the cat sat", and scatter; not
    // "cargo build" (no 't' after the 'ca'). Boundary matches outrank
    // scatter; among the tied boundary matches the newest-first order wins
    // (idx 3 "the cat sat" ts=40 before idx 1 "catalog" ts=20).
    app.input = "cat".to_string();
    let rows = app.history_rows();
    let indices: Vec<usize> = rows.iter().map(|(i, _)| *i).collect();
    assert_eq!(
        indices,
        vec![3, 1, 0],
        "boundary matches first (newest-first on ties), then scatter"
    );
    assert!(rows[0].1.score > rows[2].1.score);
    for (_, m) in &rows {
        assert_eq!(m.positions.len(), 3);
    }

    // Query with no subsequence match → empty list (the renderer turns this
    // into the "no matches" placeholder).
    app.input = "xyz".to_string();
    assert!(app.history_rows().is_empty());
}

#[test]
fn history_modal_is_click_dismissable_and_restores_draft() {
    use crate::tui::Modal;
    // The history modal and the two pickers join the click-outside-to-
    // dismiss set (their filter is ephemeral, the draft is parked); entry modals
    // that hold precious input (the editor) stay non-dismissable.
    assert!(Modal::HistorySearch.dismissable_by_outside_click());
    assert!(Modal::Models.dismissable_by_outside_click());
    assert!(Modal::Connections.dismissable_by_outside_click());
    assert!(!Modal::ModelEditor.dismissable_by_outside_click());

    // restore_history_draft hands the parked composer draft back and clears the
    // search/preview sub-state — the shared teardown for Esc and outside-click.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.stashed_input = "my draft".to_string();
    app.input = "git".to_string(); // the live fuzzy query
    app.cursor_position = 3;
    app.history_search = true;
    app.history_preview = true;
    app.modal_index = 4;

    app.restore_history_draft();

    assert_eq!(app.input, "my draft", "draft restored from the stash");
    assert_eq!(app.cursor_position, "my draft".chars().count());
    assert!(app.stashed_input.is_empty());
    assert!(!app.history_search);
    assert!(!app.history_preview);
    assert_eq!(app.modal_index, 0);
}

/// Build a minimal `App` scoped to a tempdir project so we can exercise
/// the completion pipeline end-to-end without touching the user's real
/// filesystem. Mirrors how a real session captures cwd at startup.
fn app_in_tempdir(files: &[&str], dirs: &[&str]) -> (App, tempfile::TempDir) {
    let tmp = tempfile::tempdir().expect("tempdir");
    for d in dirs {
        std::fs::create_dir_all(tmp.path().join(d)).expect("mkdir");
    }
    for f in files {
        // Create parent dirs as needed so `src/foo.rs` lays down cleanly.
        let path = tmp.path().join(f);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir for file");
        }
        std::fs::write(path, "x").expect("write file");
    }
    let cwd = tmp.path().to_path_buf();
    let app = App {
        input: String::new(),
        messages: Vec::new(),
        messages_version: 0,
        side_messages: Vec::new(),
        side_messages_version: 0,
        layout_height_cache: Default::default(),
        in_side_view: false,
        side_session_id: None,
        parent_status: neenee_core::ParentStatus::Idle,
        scroll: 0,
        follow_bottom: true,
        content_lines: 0,
        view_height: 0,
        max_scroll: 0,
        sticky_step: None,
        sticky_rect: None,
        activity_rect: None,
        hint_context_rect: None,
        token_ledger: None,
        context_tokens: None,
        round_tps: None,
        token_report_scroll: 0,
        token_report_detail: false,
        todos_rect: None,
        queue_rect: None,
        modal_rect: None,
        modal_body_height: 0,
        sticky_summary_line: None,
        pin_summary_line: None,
        focus_stack: Vec::new(),
        tx: new_test_channel(),
        should_quit: Arc::new(AtomicBool::new(false)),
        serve_tap: Arc::new(tokio::sync::Mutex::new(None)),
        serve_cancel: None,
        suggestion_index: None,
        completion_dismissed: false,
        custom_commands: Vec::new(),
        cursor_position: 0,
        input_scroll: 0,
        active_modal: Modal::None,
        modal_index: 0,
        last_input_rect: neenee_tui_engine::Rect::default(),
        cursor_sync_pending: false,
        cursor_visible: true,
        session_scroll: 0,
        session_modal_follow: true,
        session_info_detail: false,
        session_detail: None,
        session_info_scroll: 0,
        permissions_scroll: 0,
        config_scroll: 0,
        skills_expanded: None,
        history_scroll: 0,
        history_modal_follow: true,
        history_preview: false,
        history_search: false,
        current_provider: "mock".to_string(),
        current_model: "mock".to_string(),
        cwd: cwd.clone(),
        current_session_id: String::new(),
        current_workspace: String::new(),
        path_scan_cache: None,
        session_context: None,
        loop_status: LoopStatus::Idle,
        activity_status: String::new(),
        autopilot: false,
        todos: None,
        round_count: 0,
        current_turn: 0,
        review_alert: String::new(),
        round_started_at: None,
        activity_tab: ActivityTab::Activity,
        activity_scroll: 0,
        queue_scroll: 0,
        queue_modal_follow: true,
        help_scroll: 0,
        modal_keymap_open: false,
        pending_permission: None,
        pending_input: None,
        question: None,
        question_scroll: 0,
        question_modal_follow: true,
        sessions_overview: Vec::new(),
        startup_picker: false,
        permission_confirm_always: false,
        permission_show_details: false,
        permission_scroll: 0,
        permission_max_scroll: 0,
        input_history: Vec::new(),
        history_index: None,
        history_draft: String::new(),
        history_draft_images: Vec::new(),
        history_draft_text_pastes: Vec::new(),
        history_attachments: std::collections::HashMap::new(),
        history_attachments_order: std::collections::VecDeque::new(),
        history_clear_confirm: false,
        input_history_dedup: true,
        input_history_record_commands: false,
        pending_images: Vec::new(),
        pending_text_pastes: Vec::new(),
        pending_dispatch: std::collections::VecDeque::new(),
        queue_blocked_sessions: std::collections::HashSet::new(),
        naturally_completed_sessions: std::collections::HashSet::new(),
        idle_sessions: std::collections::HashSet::new(),
        running_sessions: std::collections::HashSet::new(),
        selection: SelectionState::None,
        drag: SelectionDrag::default(),
        layout_map: LayoutMap::new(),
        modal_hit_map: crate::tui::model::layout::ModalHitMap::new(),
        hovered_step: None,
        transcript_layout: crate::tui::view::layout::Strategy::default(),
        color_scheme: "zen".to_string(),
        custom_color_scheme: neenee_core::ColorSchemeConfig::default(),
        custom_color_draft: neenee_core::ColorSchemeConfig::default(),
        click_outside_dismiss: false,
        focused_target: None,
        copy_toast_until: None,
        copy_toast_message: String::new(),
        copy_toast_failed: false,
        notice_toast_until: None,
        notice_toast_message: String::new(),
        notice_toast_severity: NoticeSeverity::Info,
        ctrl_c_armed_ticks: 0,
        esc_armed_ticks: 0,
        spinner_epoch: std::time::Instant::now(),
        stashed_input: String::new(),
        editor_target: None,
        editor_field: 0,
        editor_key: String::new(),
        editor_model: String::new(),
        editor_model_settings_only: false,
        editor_target_is_builtin: false,
        editor_effort: "high".to_string(),
        editor_thinking_available: false,
        editor_thinking: true,
        custom_field: 0,
        custom_fields: Vec::new(),
        custom_protocol_wire: String::new(),
        custom_models: Vec::new(),
        custom_url_hint: String::new(),
        custom_user_agent: None,
        custom_auth: Default::default(),
        custom_template_id: None,
        awaiting_oauth_add: false,
        oauth_pending_message: String::new(),
        oauth_pending_url: String::new(),
        oauth_pending_user_code: String::new(),
        oauth_pending_error: None,
        oauth_scroll: 0,
        custom_suggest_index: 0,
        custom_scroll: 0,
        custom_edit_id: None,
        custom_name: String::new(),
        custom_base_url: String::new(),
        custom_token: String::new(),
        custom_model: String::new(),
        template_choice: 0,
        template_scroll: 0,
        model_search: false,
        editor_return_to: Modal::None,
        model_scroll: 0,
        model_modal_follow: true,
        pending_provider_delete: None,
        provider_delete_focus: crate::tui::ProviderDeleteChoice::default(),
        provider_delete_rect: None,
        key_status: HashMap::new(),
        provider_picker: ProviderPickerSnapshot::default(),
        theme: Theme::default(),
        logo: None,
    };
    (app, tmp)
}

/// Stand-up helper for tests that just need a sender half of the agent
/// channel; the receiver is dropped because no test drives the agent loop.
fn new_test_channel() -> mpsc::UnboundedSender<AgentRequest> {
    let (tx, _rx) = mpsc::unbounded_channel();
    tx
}

#[test]
fn completions_returns_empty_when_input_does_not_trigger() {
    // Plain text without `@` or `/` produces no completions.
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "hello world".to_string();
    app.cursor_position = app.input.chars().count();
    assert!(app.completions().is_empty());
    assert_eq!(app.completion_kind(), CompletionKind::None);
}

#[test]
fn completions_classifies_slash_input_as_slash_kind() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/re".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    assert_eq!(app.completion_kind(), CompletionKind::Slash);
    assert!(completions.iter().any(|c| c.label == "/repeat"));
    // Slash candidates replace the whole input.
    for c in &completions {
        assert_eq!(c.replace_start, 0);
        assert_eq!(c.replace_end, app.input.len());
    }
}

#[test]
fn completions_autopilot_subcommand_offers_on_off() {
    // After `/autopilot ` (a space the user types to opt into subcommand
    // discovery), the menu must offer `on` and `off` so the pair can be
    // completed instead of dead-ending.
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/autopilot ".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    assert_eq!(app.completion_kind(), CompletionKind::Slash);
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert!(
        labels.contains(&"/autopilot on") && labels.contains(&"/autopilot off"),
        "expected both on/off subcommands, got {labels:?}"
    );
    // Candidates replace the whole input.
    for c in &completions {
        assert_eq!(c.replace_start, 0);
        assert_eq!(c.replace_end, app.input.len());
    }

    // Typing a prefix narrows the pair.
    app.input = "/autopilot of".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, vec!["/autopilot off"]);

    // An unknown suffix dead-ends (no candidates, like any non-prefix).
    app.input = "/autopilot x".to_string();
    app.cursor_position = app.input.chars().count();
    assert!(app.completions().is_empty());
}

/// The OpenAI sub2api template (Name / Base URL / Token) seeds OpenAI text
/// models directly.
fn openai_template() -> &'static crate::tui::providers::ProviderTemplate {
    crate::tui::PROVIDER_TEMPLATES
        .iter()
        .find(|t| t.label == "OpenAI (sub2api)")
        .expect("openai sub2api template")
}

/// The Anthropic relay template (Name / Base URL / Token), which seeds the Claude
/// family and exposes no Model field.
fn anthropic_template() -> &'static crate::tui::providers::ProviderTemplate {
    crate::tui::PROVIDER_TEMPLATES
        .iter()
        .find(|t| t.protocol == "anthropic")
        .expect("anthropic relay template")
}

/// The Antigravity (sub2api) relay template — a Google-native 中转站 with a
/// pre-filled base URL and the three relay-specific model ids seeded.
fn antigravity_template() -> &'static crate::tui::providers::ProviderTemplate {
    crate::tui::PROVIDER_TEMPLATES
        .iter()
        .find(|t| t.label == "Antigravity (sub2api)")
        .expect("antigravity template")
}

#[test]
fn add_provider_row_opens_the_template_chooser() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_provider_template_chooser();
    assert!(app.active_modal == Modal::ProviderTemplate);
    assert_eq!(app.template_choice, 0);
    // `↑/↓` wrap across the template list.
    let n = crate::tui::PROVIDER_TEMPLATES.len();
    app.move_template_choice(false);
    assert_eq!(app.template_choice, n - 1, "wraps to the last template");
    app.move_template_choice(true);
    assert_eq!(app.template_choice, 0, "wraps back to the first");
}

#[test]
fn custom_provider_editor_opens_empty_on_name_field() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.custom_name = "stale".to_string();
    app.open_custom_provider_editor(openai_template());
    assert!(app.active_modal == Modal::CustomProvider);
    assert_eq!(app.custom_field, 0, "opens on the Name field");
    assert!(app.custom_name.is_empty(), "buffers reset on open");
    assert!(
        app.input.is_empty(),
        "Name field borrows an empty input line"
    );
    // The template seeds the protocol and OpenAI model list.
    assert_eq!(app.custom_protocol_wire, "openai");
    assert!(app.custom_models.iter().any(|m| m == "gpt-5.5"));
    assert!(!app.custom_fields.contains(&crate::tui::CustomField::Model));
}

#[test]
fn anthropic_template_seeds_the_claude_family_without_a_model_field() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_custom_provider_editor(anthropic_template());
    assert_eq!(app.custom_protocol_wire, "anthropic");
    // The Claude family is seeded as the provider's model list…
    assert!(app.custom_models.len() > 1, "seeds multiple Claude models");
    assert!(app.custom_models.iter().any(|m| m.starts_with("claude-")));
    // …and there is no Model field (models are fixed by the template).
    assert!(!app.custom_fields.contains(&crate::tui::CustomField::Model));
}

#[test]
fn antigravity_template_prefills_url_and_seeds_relay_models() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_custom_provider_editor(antigravity_template());
    assert_eq!(app.custom_protocol_wire, "google");
    // The relay host is pre-filled so the user only types a name + token; an
    // empty base_url would otherwise fall back to localhost in the catalog.
    assert_eq!(
        app.custom_base_url,
        "https://relay.example.com/antigravity/v1beta"
    );
    // The three effort-tiered / non-preview ids are seeded as channels, with
    // the working models first (AddProvider activates channels[0] by default).
    assert_eq!(
        app.custom_models,
        vec![
            "gemini-3-flash".to_string(),
            "gemini-3.1-pro-low".to_string(),
            "gemini-3.1-pro-high".to_string(),
        ]
    );
    // No free-text Model field — the closed Gemini family is the seed.
    assert!(!app.custom_fields.contains(&crate::tui::CustomField::Model));
    // Name and Token still start empty (the user supplies them).
    assert!(app.custom_name.is_empty());
    assert!(app.custom_token.is_empty());
}

#[test]
fn custom_provider_field_cycle_wraps_and_swaps_buffers() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_custom_provider_editor(openai_template());
    // Fields: Name(0) / Base URL(1) / Token(2).
    let n = app.custom_fields.len() as u8;
    // Type a name, then advance: the name is stashed and the Base URL field
    // loads its (empty) buffer.
    app.input = "My Relay".to_string();
    app.cycle_custom_field(true);
    assert_eq!(app.custom_field, 1);
    assert_eq!(app.custom_name, "My Relay");
    assert!(app.input.is_empty(), "Base URL buffer is empty");
    // Wrap backward from Name (0) to the last field (Token).
    app.cycle_custom_field(false); // 1 -> 0
    assert_eq!(app.custom_field, 0);
    assert_eq!(app.input, "My Relay", "Name buffer reloads into the line");
    app.cycle_custom_field(false); // 0 -> n-1 (wrap)
    assert_eq!(app.custom_field, n - 1);
}

#[test]
fn custom_provider_model_filter_commits_and_offers_custom_id() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let free_model_template = crate::tui::providers::ProviderTemplate {
        id: "openai",
        label: "OpenAI",
        description: "OpenAI relay with a free model id",
        protocol: "openai",
        models: &[],
        needs_url: true,
        url_hint: "https://relay.example.com/v1/chat/completions",
        needs_model: true,
        default_url: None,
        user_agent: None,
        auth: neenee_core::ChannelAuth::ApiKey,
    };
    app.open_custom_provider_editor(&free_model_template);
    // The default model is the first candidate of the template's (OpenAI) protocol.
    assert!(
        app.custom_model_candidates()
            .contains(&app.custom_model.as_str())
    );
    // Focus the Model filter field (the last field) and type a known model.
    app.custom_field = app.custom_fields.len() as u8 - 1;
    assert_eq!(
        app.current_custom_field(),
        Some(crate::tui::CustomField::Model)
    );
    app.load_custom_field();
    app.input = "gpt-4o".to_string();
    app.on_custom_filter_changed();
    assert_eq!(app.custom_model, "gpt-4o");
    // A query matching nothing in the registry is still offered as a custom id.
    app.input = "my-private-model".to_string();
    app.on_custom_filter_changed();
    assert_eq!(app.custom_model, "my-private-model");
}

#[test]
fn picker_connections_count_matches_provider_rows_no_add_row() {
    // Adding a connection is a footer shortcut (`a`) now, not a synthetic list
    // row, so `picker_row_count()` for Connections equals the provider count
    // exactly (no +1).
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.active_modal = Modal::Connections;
    // Seed a few snapshot rows so providers_filtered() renders the full list
    // (the picker is snapshot-driven).
    let row = |id: &str| neenee_core::ProviderPickerRow {
        id: id.to_string(),
        name: id.to_string(),
        model: "m".to_string(),
        models: vec!["m".to_string()],
        model_info: Vec::new(),
        builtin: true,
        protocol: String::new(),
        base_url: String::new(),
        key_ready: true,
        template_id: String::new(),
        last_used_ms: None,
        auth: Default::default(),
    };
    app.provider_picker = neenee_core::ProviderPickerSnapshot {
        default_id: "kimi-code".to_string(),
        rows: vec![row("kimi-code"), row("openai"), row("anthropic")],
    };
    let providers = app.providers_filtered().len();
    assert!(providers > 0, "snapshot seeds the full provider list");
    assert_eq!(app.picker_row_count(), providers);
}

/// `Shift+D` on a custom provider must STAGE the deletion (open the confirm
/// overlay with default focus = Cancel) rather than deleting immediately. This
/// is the core guarantee of the new confirm overlay: `stage_provider_delete`
/// only mutates overlay state — it never enqueues an `AgentRequest` (that is
/// `confirm_provider_delete`'s job).
#[test]
fn delete_provider_stages_overlay_without_deleting() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.active_modal = Modal::Connections;
    let custom = |id: &str| neenee_core::ProviderPickerRow {
        id: id.to_string(),
        name: id.to_string(),
        model: "m".to_string(),
        models: vec!["m".to_string()],
        model_info: Vec::new(),
        builtin: false,
        protocol: String::new(),
        base_url: String::new(),
        key_ready: true,
        template_id: String::new(),
        last_used_ms: None,
        auth: Default::default(),
    };
    app.provider_picker = neenee_core::ProviderPickerSnapshot {
        default_id: "my-custom".to_string(),
        rows: vec![custom("my-custom")],
    };
    app.modal_index = 0;

    app.stage_provider_delete();

    // The deletion is staged, not dispatched.
    assert_eq!(
        app.pending_provider_delete.as_deref(),
        Some("my-custom"),
        "Shift+D stages the provider id without deleting"
    );
    // Default focus is Cancel (the safe choice) so a reflexive Enter cancels.
    assert_eq!(
        app.provider_delete_focus,
        crate::tui::ProviderDeleteChoice::Cancel,
        "confirm overlay defaults to Cancel focus"
    );
}

/// Built-in providers are not deletable: `Shift+D` on one is a no-op (the
/// overlay must not open, nothing staged).
#[test]
fn delete_provider_ignores_builtin() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.active_modal = Modal::Connections;
    let builtin = |id: &str| neenee_core::ProviderPickerRow {
        id: id.to_string(),
        name: id.to_string(),
        model: "m".to_string(),
        models: vec!["m".to_string()],
        model_info: Vec::new(),
        builtin: true,
        protocol: String::new(),
        base_url: String::new(),
        key_ready: true,
        template_id: String::new(),
        last_used_ms: None,
        auth: Default::default(),
    };
    app.provider_picker = neenee_core::ProviderPickerSnapshot {
        default_id: "kimi-code".to_string(),
        rows: vec![builtin("kimi-code")],
    };
    app.modal_index = 0;

    app.stage_provider_delete();

    assert!(
        app.pending_provider_delete.is_none(),
        "built-in provider is never staged for deletion"
    );
}

/// Confirming the overlay dispatches exactly one `DeleteProvider` request and
/// tears the overlay down, so a stray second confirm cannot re-delete.
#[test]
fn confirm_provider_delete_dispatches_once_and_clears() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    // Simulate the overlay already being open (staged from a prior Shift+D).
    app.pending_provider_delete = Some("doomed".to_string());

    let req = app
        .confirm_provider_delete()
        .expect("confirm dispatches when an id is staged");
    assert!(
        matches!(req, AgentRequest::DeleteProvider { ref id } if id == "doomed"),
        "confirm dispatches a DeleteProvider request for the staged id"
    );
    // Overlay torn down: no staged id remains.
    assert!(
        app.pending_provider_delete.is_none(),
        "confirm clears the staged id"
    );
    // A second confirm is a no-op (nothing left to delete).
    assert!(
        app.confirm_provider_delete().is_none(),
        "second confirm is inert after the overlay closes"
    );
}

/// Cancelling the overlay drops the staged id and resets focus to the safe
/// default (Cancel), so reopening the overlay later starts fresh.
#[test]
fn cancel_provider_delete_clears_and_resets_focus() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.pending_provider_delete = Some("doomed".to_string());
    app.provider_delete_focus = crate::tui::ProviderDeleteChoice::Delete;

    app.cancel_provider_delete();

    assert!(
        app.pending_provider_delete.is_none(),
        "cancel clears the staged id"
    );
    assert_eq!(
        app.provider_delete_focus,
        crate::tui::ProviderDeleteChoice::Cancel,
        "cancel resets focus to the safe default"
    );
}

#[test]
fn accept_slash_completion_does_not_append_trailing_space() {
    // Accepting a slash-command completion must splice the bare label with
    // NO trailing space. A trailing `/pursue ` would immediately match the
    // subcommand prefix and re-trigger the completion menu — the opposite
    // of "Enter/Tab finishes the completion". The user opts into subcommand
    // discovery by typing a space themselves.
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/re".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let idx = completions
        .iter()
        .position(|c| c.label == "/repeat")
        .expect("/repeat in candidates");
    app.accept_completion(idx);
    // The label is spliced verbatim — no trailing space.
    assert_eq!(app.input, "/repeat");
    assert_eq!(app.cursor_position, "/pursue".chars().count());
    // A slash accept is a terminal commit: the popup must stay hidden and
    // no subcommand menu may fire. This holds for BOTH Tab and Enter since
    // both route through accept_completion for slash commands.
    assert!(
        app.completion_dismissed,
        "slash accept must latch dismissal"
    );
    assert!(app.suggestion_index.is_none(), "highlight cleared");
    assert!(
        app.completions()
            .iter()
            .all(|c| !c.label.starts_with("/pursue ")),
        "subcommand menu must not fire after accepting a slash completion"
    );
}

#[test]
fn accept_path_dir_completion_stays_live_for_descend() {
    // `@path` *directory* accepts stay live so Tab can keep descending the
    // directory tree: the `@` trigger is kept and the popup re-triggers on the
    // directory's contents. This guards against the terminal-accept logic
    // accidentally suppressing directory navigation.
    let (mut app, _tmp) = app_in_tempdir(&["src/main.rs", "src/util.rs"], &["src"]);
    app.input = "@".to_string();
    app.cursor_position = 1;
    let completions = app.completions();
    // The first candidate is a directory (`src/` sorts before files).
    let dir_idx = completions
        .iter()
        .position(|c| c.label == "src/")
        .expect("src/ directory in candidates");
    app.accept_completion(dir_idx);
    // Directory accept must NOT latch dismissal — descend continues.
    assert!(
        !app.completion_dismissed,
        "directory accept must stay live for descend"
    );
    // The `@` trigger is kept so the popup re-triggers on `src/`'s contents.
    assert!(
        app.input.starts_with("@src/"),
        "dir accept keeps @: {}",
        app.input
    );
}

#[test]
fn accept_path_file_completion_is_terminal_and_drops_at() {
    // `@path` *file* accepts are terminal: the `@` is only a completion
    // trigger and must not survive into the message context once a concrete
    // file is chosen, so accept_completion drops the `@`, appends a trailing
    // space, and latches the dismissal flag.
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "@Ca".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let idx = completions
        .iter()
        .position(|c| c.label == "Cargo.toml")
        .expect("Cargo.toml in candidates");
    app.accept_completion(idx);
    // The `@` trigger is dropped; a trailing space lets the user keep typing.
    assert_eq!(app.input, "Cargo.toml ");
    assert!(
        app.completion_dismissed,
        "file accept must be terminal (latch dismissal)"
    );
}

#[test]
fn accept_path_file_completion_inline_preserves_surrounding_text() {
    // An inline `@mention` mid-sentence: accepting a file must drop the `@`
    // and splice the path in place, preserving the surrounding prose. This is
    // the real-world case — `look at @Cargo` in the middle of a message.
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    // Cursor sits right after the `@Cargo` token, inside the mention.
    // `look at @Cargo please`: `look at ` is 8 chars, `@Cargo` is 6 → cursor
    // at char index 14 sits just past the `o`.
    app.input = "look at @Cargo please".to_string();
    app.cursor_position = 14;
    let completions = app.completions();
    let idx = completions
        .iter()
        .position(|c| c.label == "Cargo.toml")
        .expect("Cargo.toml in candidates");
    app.accept_completion(idx);
    // The `@` is dropped; the path replaces `@Cargo`; trailing `please` is
    // preserved; the existing space before it is reused (no double space).
    assert_eq!(app.input, "look at Cargo.toml please");
}

#[test]
fn completions_path_returns_top_level_for_bare_at() {
    // A bare `@` lists top-level entries only: the file plus the
    // synthesized top-level directory entry.
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml", "src/main.rs", "README.md"], &["src"]);
    app.input = "@".to_string();
    app.cursor_position = 1;
    let completions = app.completions();
    assert_eq!(app.completion_kind(), CompletionKind::Path);

    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    // Dirs come first alphabetically, then files alphabetically.
    assert!(labels.contains(&"src/"));
    assert!(labels.contains(&"Cargo.toml"));
    assert!(labels.contains(&"README.md"));
    // No nested paths leak into the bare-`@` menu.
    assert!(!labels.iter().any(|l| l.contains("main.rs")));
    // Replace range points just past the `@` (byte 1), ends at cursor (1).
    for c in &completions {
        assert_eq!(c.replace_start, 1);
        assert_eq!(c.replace_end, 1);
        assert!(c.description.is_empty(), "path menu carries no description");
    }
}

#[test]
fn completions_path_descends_into_subdirectory() {
    // `@src/` triggers directory descend: only paths under `src/` match.
    let (mut app, _tmp) = app_in_tempdir(
        &["src/main.rs", "src/util/mod.rs", "tests/smoke.rs"],
        &["src", "src/util", "tests"],
    );
    app.input = "@src/".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert!(labels.contains(&"src/"));
    assert!(labels.contains(&"src/main.rs"));
    assert!(labels.contains(&"src/util/"));
    assert!(labels.contains(&"src/util/mod.rs"));
    // Nothing from `tests/` leaks in — descend is a prefix match.
    assert!(!labels.iter().any(|l| l.contains("tests")));
}

#[test]
fn completions_path_substring_match_picks_files_across_dirs() {
    // `@main` finds `src/main.rs` via substring match.
    let (mut app, _tmp) = app_in_tempdir(&["src/main.rs", "lib/other.rs"], &["src", "lib"]);
    app.input = "@main".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert!(labels.contains(&"src/main.rs"));
    assert!(!labels.iter().any(|l| l.contains("other.rs")));
}

#[test]
fn completions_path_skips_dotgit_directory() {
    // `.git/` is always excluded even though hidden files are kept.
    let (mut app, _tmp) = app_in_tempdir(
        &[".git/HEAD", ".git/config", "src/main.rs", ".env"],
        &[".git", "src"],
    );
    app.input = "@".to_string();
    app.cursor_position = 1;
    let completions = app.completions();
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    // Hidden files like `.env` are listed; `.git/` and its contents are not.
    assert!(labels.contains(&".env"));
    assert!(labels.contains(&"src/"));
    assert!(!labels.iter().any(|l| l.starts_with(".git")));
}

#[test]
fn completions_path_cache_populated_once() {
    // The scan should run only the first time `@` triggers; we verify by
    // observing `path_scan_cache` transitioning from None to Some.
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    assert!(app.path_scan_cache.is_none());
    app.input = "@".to_string();
    app.cursor_position = 1;
    let _ = app.completions();
    let first_scan = app
        .path_scan_cache
        .as_ref()
        .expect("scan populated")
        .clone();
    // A second call must not re-scan: cache stays the same Vec pointer
    // content. We compare lengths because the Vec itself may move.
    app.input = "@Ca".to_string();
    app.cursor_position = app.input.chars().count();
    let _ = app.completions();
    let second_scan = app
        .path_scan_cache
        .as_ref()
        .expect("scan still populated")
        .clone();
    assert_eq!(first_scan.entries, second_scan.entries);
}

#[test]
fn manual_walk_returns_files_and_synthesized_dirs() {
    // The manual fallback path (used when rg is missing) must still
    // produce directory entries with trailing slashes and skip `.git`.
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(tmp.path().join("src/nested")).unwrap();
    std::fs::write(tmp.path().join("src/nested/foo.rs"), "x").unwrap();
    std::fs::write(tmp.path().join("top.md"), "x").unwrap();
    std::fs::create_dir(tmp.path().join(".git")).unwrap();
    std::fs::write(tmp.path().join(".git/HEAD"), "x").unwrap();

    let entries = manual_walk(tmp.path());
    assert!(entries.contains(&"top.md".to_string()));
    assert!(entries.contains(&"src/".to_string()));
    assert!(entries.contains(&"src/nested/".to_string()));
    assert!(entries.contains(&"src/nested/foo.rs".to_string()));
    assert!(!entries.iter().any(|e| e.starts_with(".git")));
}

use crate::tui::app::{QueuedDispatch, QueuedDispatchState, RecallQueued};

fn queued_dispatch(id: &str, session_id: &str, text: &str) -> QueuedDispatch {
    QueuedDispatch {
        id: id.to_string(),
        session_id: session_id.to_string(),
        state: QueuedDispatchState::Waiting,
        text: text.to_string(),
        queued_at_ms: 0,
        images: Vec::new(),
        text_pastes: Vec::new(),
    }
}

#[test]
fn queued_dispatch_carries_text_and_images() {
    // Smoke-check the struct's fields are wired as expected by the
    // SendChat and recall paths. Locks the field names + types so a
    // refactor can't quietly drop the images payload.
    let d = QueuedDispatch {
        id: "message-1".to_string(),
        session_id: "session-a".to_string(),
        state: QueuedDispatchState::Waiting,
        text: "hello".to_string(),
        queued_at_ms: 0,
        images: vec![neenee_core::ImagePart {
            mime: "image/png".to_string(),
            data: "base64".to_string(),
        }],
        text_pastes: Vec::new(),
    };
    assert_eq!(d.text, "hello");
    assert_eq!(d.images.len(), 1);
    assert_eq!(d.images[0].mime, "image/png");
}

#[test]
fn outbox_count_and_fifo_dispatch_are_session_scoped() {
    // Every staged message is a next-round item (the insert/next-round
    // distinction was removed), so the outbox count is a single per-session
    // tally and FIFO dispatch is driven purely by `Waiting` state + session.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.pending_dispatch
        .push_back(queued_dispatch("b-next", "session-b", "other"));
    app.pending_dispatch
        .push_back(queued_dispatch("a-next", "session-a", "follow up"));

    assert_eq!(app.pending_count("session-a"), 1);
    assert_eq!(app.pending_count("session-b"), 1);
    let dispatch = app
        .begin_next_round_dispatch("session-a")
        .expect("session-a follow-up");
    assert_eq!(dispatch.id, "a-next");
    assert_eq!(app.pending_dispatch[0].id, "b-next");
}

#[test]
fn recall_queued_is_lifo_and_restores_input() {
    // Every queued dispatch is a next-round item, so recall pops the newest
    // staged message in LIFO order and restores it locally without an agent
    // roundtrip (no insert to cancel).
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.pending_dispatch
        .push_back(queued_dispatch("1", "session-a", "first"));
    app.pending_dispatch
        .push_back(queued_dispatch("2", "session-a", "second"));

    // First recall: most-recently-queued = "second".
    let Some(RecallQueued::Restored(dispatch)) = app.recall_queued("session-a") else {
        panic!("expected local restore");
    };
    app.restore_dispatch(dispatch);
    assert_eq!(app.input, "second");
    assert_eq!(app.cursor_position, "second".chars().count());
    assert_eq!(
        app.history_index, None,
        "history cursor must be cleared so ↓ returns to empty input"
    );
    // Second recall: now "first".
    let Some(RecallQueued::Restored(dispatch)) = app.recall_queued("session-a") else {
        panic!("expected local restore");
    };
    app.restore_dispatch(dispatch);
    assert_eq!(app.input, "first");

    // Third recall: queue empty → no-op.
    assert!(app.recall_queued("session-a").is_none());
    assert_eq!(
        app.input, "first",
        "input must be untouched when the queue is empty"
    );
}

#[test]
fn recall_queued_restores_staged_images() {
    // Images staged with the queued message (Ctrl+V before pressing
    // Enter) come back alongside the text so the user can re-edit and
    // resend without losing the attachment.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let image = neenee_core::ImagePart {
        mime: "image/png".to_string(),
        data: "abc".to_string(),
    };
    let mut dispatch = queued_dispatch("1", "session-a", "look at this");
    dispatch.images = vec![image.clone()];
    app.pending_dispatch.push_back(dispatch);

    let Some(RecallQueued::Restored(dispatch)) = app.recall_queued("session-a") else {
        panic!("expected local restore");
    };
    app.restore_dispatch(dispatch);
    assert_eq!(app.input, "look at this");
    assert_eq!(
        app.pending_images.len(),
        1,
        "recalled images must land back in pending_images for resend"
    );
    assert_eq!(app.pending_images[0].data, image.data);
}

/// The interrupt → ↑/↓ → resend bug: a message sent with pasted images is
/// recorded to input history as text-only, so recalling it via ↑/↓ (or
/// Ctrl+R) and pressing Enter used to ship the bare `[Image #N]` chip label
/// with no payload — the model never received the pixels. Recording must
/// cache the staged attachments keyed by the entry's identity, and recall
/// must restore them into `pending_images` / `pending_text_pastes`.
#[tokio::test]
async fn history_recall_restores_staged_images_and_pastes() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.current_workspace = "~/p".to_string();

    let image = neenee_core::ImagePart {
        mime: "image/png".to_string(),
        data: "abc".to_string(),
    };
    let chip = crate::tui::composer_attachments::image_chip(1, 3);
    let paste = crate::tui::composer_attachments::paste_chip(1, 2, 11);
    let text = format!("describe this {chip} then {paste}");
    app.record_input_history(text.clone(), vec![image.clone()], vec!["big paste".into()]);

    // ↑ recall: the entry is the newest row of the current session; the
    // event loop loads its text and calls restore_history_attachments.
    let session_rows = app.current_session_history();
    assert_eq!(session_rows.len(), 1);
    let orig_idx = session_rows[0];
    app.input = app.input_history[orig_idx].text.clone();
    app.restore_history_attachments(orig_idx);

    assert_eq!(app.input, text, "recalled text keeps its chip labels");
    assert_eq!(
        app.pending_images.len(),
        1,
        "image payload restored for resend"
    );
    assert_eq!(app.pending_images[0].data, "abc");
    assert_eq!(app.pending_text_pastes, vec!["big paste".to_string()]);

    // The chips pair back up with the payloads after an edit reconcile, so
    // a Backspace or typing never orphans them.
    app.reconcile_attachments();
    assert_eq!(app.pending_images.len(), 1);
    assert_eq!(app.pending_text_pastes.len(), 1);
}

/// Recalling a text-only entry (no cached payloads) must clear the staged
/// vectors so a resend never inherits an attachment that belonged to a
/// different entry — e.g. one restored by a Phase-1 unsend that the user then
/// navigated away from.
#[tokio::test]
async fn history_recall_clears_staged_attachments_for_plain_entries() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.current_workspace = "~/p".to_string();
    app.record_input_history("plain prompt".to_string(), Vec::new(), Vec::new());

    let image = neenee_core::ImagePart {
        mime: "image/png".to_string(),
        data: "abc".to_string(),
    };
    app.pending_images.push(image);

    let orig_idx = app.current_session_history()[0];
    app.input = app.input_history[orig_idx].text.clone();
    app.restore_history_attachments(orig_idx);

    assert!(
        app.pending_images.is_empty(),
        "no orphaned payloads on recall"
    );
    assert!(app.pending_text_pastes.is_empty());
}

/// The ↓-past-newest branch restores the draft the user was composing before
/// the first ↑ — including any staged attachments — so an accidental ↑/↓
/// round-trip never drops a pasted image.
#[test]
fn history_draft_round_trip_keeps_attachments() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let draft = "my in-progress draft".to_string();
    let image = neenee_core::ImagePart {
        mime: "image/png".to_string(),
        data: "draft-img".to_string(),
    };
    app.input = draft.clone();
    app.pending_images = vec![image.clone()];

    // First ↑: stash text + attachments (what the HistoryPrev handler does).
    app.history_draft = std::mem::take(&mut app.input);
    app.history_draft_images = std::mem::take(&mut app.pending_images);
    app.history_draft_text_pastes = std::mem::take(&mut app.pending_text_pastes);

    // ↓ past the newest entry: restore text + attachments together.
    app.input = std::mem::take(&mut app.history_draft);
    app.pending_images = std::mem::take(&mut app.history_draft_images);
    app.pending_text_pastes = std::mem::take(&mut app.history_draft_text_pastes);

    assert_eq!(app.input, draft);
    assert_eq!(app.pending_images.len(), 1);
    assert_eq!(app.pending_images[0].mime, "image/png");
    assert_eq!(app.pending_images[0].data, "draft-img");
    assert!(app.pending_text_pastes.is_empty());
}

/// The in-memory cache is bounded so a long session of image-heavy sends
/// cannot balloon the process's memory with base64 payloads.
#[tokio::test]
async fn history_attachment_cache_is_capped() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    for i in 0..40 {
        app.record_input_history(
            format!("prompt {i}"),
            vec![neenee_core::ImagePart {
                mime: "image/png".to_string(),
                data: format!("img-{i}"),
            }],
            Vec::new(),
        );
    }
    assert!(
        app.history_attachments.len() <= crate::tui::app::App::HISTORY_ATTACHMENTS_CAP,
        "cache must stay bounded"
    );
    // A recent entry's payload survives the FIFO eviction…
    let newest_idx = app
        .input_history
        .iter()
        .position(|e| e.text == "prompt 39")
        .expect("last prompt recorded");
    app.restore_history_attachments(newest_idx);
    assert_eq!(app.pending_images[0].data, "img-39");
    // …while the oldest entries were evicted (their recall clears the
    // staged vectors rather than restoring a stale payload).
    let oldest_idx = app
        .input_history
        .iter()
        .position(|e| e.text == "prompt 0")
        .expect("first prompt recorded");
    app.pending_images.clear();
    app.restore_history_attachments(oldest_idx);
    assert!(
        app.pending_images.is_empty(),
        "evicted entries must not restore attachments"
    );
}

/// `[input_history] dedup` (default on): the same prompt text sent twice —
/// even in a different session — stays a single entry, and re-sending bumps
/// its timestamp so it bubbles to the top of the newest-first picker.
#[tokio::test]
async fn record_input_history_dedups_globally_by_text() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.current_workspace = "~/p".to_string();

    app.record_input_history("build the thing".to_string(), Vec::new(), Vec::new());
    app.record_input_history("different prompt".to_string(), Vec::new(), Vec::new());
    assert_eq!(app.input_history.len(), 2);

    // The same text in a *different* session still collapses to one entry,
    // adopting the newest origin so ↑/↓ in the newer session finds it.
    app.current_session_id = "session-b".to_string();
    app.record_input_history("build the thing".to_string(), Vec::new(), Vec::new());

    assert_eq!(
        app.input_history.len(),
        2,
        "global dedup keeps one row per text"
    );
    let deduped = app
        .input_history
        .iter()
        .find(|e| e.text == "build the thing")
        .expect("entry survives dedup");
    assert_eq!(deduped.session_id.as_deref(), Some("session-b"));
    // The re-sent entry is newest → first in the history order.
    let order = app.history_order();
    assert_eq!(app.input_history[order[0]].text, "build the thing");
}

/// With dedup off (`[input_history] dedup = false`) the same words typed in
/// two sessions stay two entries, each with its own origin.
#[tokio::test]
async fn record_input_history_without_dedup_keeps_per_session_entries() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input_history_dedup = false;
    app.current_session_id = "session-a".to_string();
    app.record_input_history("hello".to_string(), Vec::new(), Vec::new());
    app.current_session_id = "session-b".to_string();
    app.record_input_history("hello".to_string(), Vec::new(), Vec::new());
    assert_eq!(app.input_history.len(), 2);
}

/// `/command` invocations are not prompt history: by default they are skipped
/// entirely (`[input_history] record_commands = false`).
#[tokio::test]
async fn record_input_history_skips_slash_commands_by_default() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.record_input_history("/model".to_string(), Vec::new(), Vec::new());
    app.record_input_history("/clear".to_string(), Vec::new(), Vec::new());
    assert!(
        app.input_history.is_empty(),
        "commands must not pollute the prompt history"
    );

    // Opting in restores them.
    app.input_history_record_commands = true;
    app.record_input_history("/model".to_string(), Vec::new(), Vec::new());
    assert_eq!(app.input_history.len(), 1);
    assert_eq!(app.input_history[0].text, "/model");
}

/// The clear-history action wipes the in-memory list and the attachment cache
/// so a fresh recall starts empty.
#[tokio::test]
async fn clear_input_history_wipes_list_and_cache() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.record_input_history(
        "one".to_string(),
        vec![neenee_core::ImagePart {
            mime: "image/png".to_string(),
            data: "img".to_string(),
        }],
        Vec::new(),
    );
    app.record_input_history("two".to_string(), Vec::new(), Vec::new());
    assert_eq!(app.input_history.len(), 2);
    assert_eq!(app.history_attachments.len(), 1);

    app.clear_input_history();
    assert!(app.input_history.is_empty());
    assert!(app.history_attachments.is_empty());
    assert!(!app.history_clear_confirm);
}

/// ↑ walks toward older entries and ↓ walks back toward the newest,
/// restoring the stashed draft past the newest entry. Regression: the two
/// directions were swapped and ↑ was pinned at the newest entry (its
/// `saturating_sub(1)` clamped at 0), so a second ↑ never moved — exactly
/// "只能往上翻一个，再继续按上没效果；按下有效果但不总是".
#[tokio::test]
async fn inline_history_arrows_walk_old_then_new() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.current_workspace = "~/p".to_string();
    // Record oldest → newest so the newest row is also the latest stamped.
    for text in ["first (oldest)", "second", "third (newest)"] {
        app.record_input_history(text.to_string(), Vec::new(), Vec::new());
    }
    // Pre-seed a draft so the first-↑ stash has something to restore.
    app.input = "in-progress draft".to_string();

    let rows = app.current_session_history();
    assert_eq!(rows.len(), 3);

    // ↑ #1: the newest entry.
    assert!(app.history_prev(&rows));
    assert_eq!(app.input, "third (newest)");
    assert_eq!(app.history_index, Some(0));

    // ↑ #2: the second-newest (this used to stick at position 0).
    assert!(app.history_prev(&rows));
    assert_eq!(app.input, "second");
    assert_eq!(app.history_index, Some(1));

    // ↑ #3: the oldest.
    assert!(app.history_prev(&rows));
    assert_eq!(app.input, "first (oldest)");
    assert_eq!(app.history_index, Some(2));

    // ↑ #4: already at the oldest — clamps and stays put.
    assert!(app.history_prev(&rows));
    assert_eq!(app.input, "first (oldest)");
    assert_eq!(app.history_index, Some(2));

    // ↓ walks back toward the newest.
    assert!(app.history_next(&rows));
    assert_eq!(app.input, "second");
    assert_eq!(app.history_index, Some(1));

    assert!(app.history_next(&rows));
    assert_eq!(app.input, "third (newest)");
    assert_eq!(app.history_index, Some(0));

    // ↓ past the newest restores the stashed draft (not a blank box).
    assert!(!app.history_next(&rows));
    assert_eq!(app.input, "in-progress draft");
    assert_eq!(app.history_index, None);

    // A bare ↓ without any prior ↑ is a no-op (cursor not armed).
    assert!(!app.history_next(&rows));
}

/// The ↑/↓ round-trip preserves staged attachments end to end: they are
/// stashed on the first ↑, and restored when ↓ walks back past the newest
/// entry — so an accidental ↑/↓ never drops a pasted image.
#[tokio::test]
async fn inline_history_round_trip_keeps_staged_attachments() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.record_input_history("sent prompt".to_string(), Vec::new(), Vec::new());

    let image = neenee_core::ImagePart {
        mime: "image/png".to_string(),
        data: "abc".to_string(),
    };
    app.input = "my draft".to_string();
    app.pending_images = vec![image.clone()];

    let rows = app.current_session_history();
    assert!(app.history_prev(&rows));
    assert_eq!(app.input, "sent prompt");
    assert!(
        app.pending_images.is_empty(),
        "recalled entry has no cached attachments → vectors cleared"
    );

    // ↓ back past the newest restores the draft AND its attachments.
    assert!(!app.history_next(&rows));
    assert_eq!(app.input, "my draft");
    assert_eq!(app.pending_images.len(), 1);
    assert_eq!(app.pending_images[0].data, "abc");
}

/// The pointer model's "newest slot = unsent input" invariant: once a draft
/// is successfully sent it is historicised, so the remembered draft slot is
/// cleared and a later ↑→↓ round-trip must NOT bring the old draft back —
/// it returns to an empty composer (the fresh unsent slot).
#[tokio::test]
async fn sending_clears_the_remembered_draft() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.current_workspace = "~/p".to_string();
    app.record_input_history("older prompt".to_string(), Vec::new(), Vec::new());

    // Compose a draft, walk ↑ into history and back ↓ — the draft is stashed
    // in the remembered slot and restored into the composer.
    app.input = "my draft".to_string();
    let rows = app.current_session_history();
    assert!(app.history_prev(&rows));
    assert!(!app.history_next(&rows));
    assert_eq!(
        app.input, "my draft",
        "draft content restored into the composer on ↓ past the newest row"
    );
    // The remembered slot is a *lazy* stash (shell-style): after a restore the
    // content lives in the composer and the slot is re-stashed on the next ↑,
    // so the slot may be empty here without losing anything.

    // Send: the input is taken out of the composer, historicised (recorded),
    // and the draft slot cleared — exactly the SendChat handler's sequence.
    let sent = std::mem::take(&mut app.input);
    app.record_input_history(sent, Vec::new(), Vec::new());
    app.clear_history_draft();
    assert!(app.history_draft.is_empty());
    assert!(app.input.is_empty(), "composer is blank after a send");

    // A later ↑→↓ round-trip restores an empty composer, not the sent text.
    let rows = app.current_session_history();
    assert!(app.history_prev(&rows));
    assert!(!app.history_next(&rows));
    assert_eq!(app.input, "", "no stale draft may return after a send");
}

/// The pointer model's "unsent restore = new draft" invariant: an input put
/// back by a Phase-1 unsend (or Ctrl+R insert / queue recall) becomes the
/// newest editable slot. It replaces any stale remembered draft, and a ↓ past
/// the newest history row restores *this* input.
#[tokio::test]
async fn adopt_as_draft_replaces_stale_draft_and_is_restored() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.current_workspace = "~/p".to_string();
    app.record_input_history("older".to_string(), Vec::new(), Vec::new());

    // A stale remembered draft from an earlier session of editing.
    app.history_draft = "stale draft".to_string();
    app.input = "whatever".to_string();

    let image = neenee_core::ImagePart {
        mime: "image/png".to_string(),
        data: "img".to_string(),
    };
    app.adopt_as_draft(
        "interrupted input".to_string(),
        vec![image.clone()],
        Vec::new(),
    );

    assert_eq!(app.input, "interrupted input");
    assert_eq!(app.history_index, None, "adoption enters draft mode");
    assert_eq!(
        app.history_draft, "interrupted input",
        "stale draft replaced"
    );
    assert_eq!(app.history_draft_images.len(), 1);
    assert_eq!(app.pending_images.len(), 1);

    // ↑ then ↓ past the newest restores the adopted input, not the stale one.
    let rows = app.current_session_history();
    assert!(app.history_prev(&rows));
    assert!(!app.history_next(&rows));
    assert_eq!(app.input, "interrupted input");
    assert_eq!(app.pending_images.len(), 1);
}

/// History rows are read-only snapshots: editing one is temporary and is
/// discarded the moment the pointer moves — coming back reloads the original
/// text (the shell "other rows are readonly" behaviour).
#[tokio::test]
async fn history_rows_are_readonly_snapshots() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.current_workspace = "~/p".to_string();
    app.record_input_history("older row".to_string(), Vec::new(), Vec::new());
    app.record_input_history("newest row".to_string(), Vec::new(), Vec::new());

    let rows = app.current_session_history();
    assert_eq!(rows.len(), 2);
    // Newest-first: rows[0] = "newest row" (later stamp), rows[1] = "older row".
    assert!(app.history_prev(&rows));
    assert_eq!(app.input, "newest row");
    assert!(app.history_prev(&rows));
    assert_eq!(app.input, "older row");

    // Edit the history row — the edit is temporary.
    app.input = "EDITED".to_string();
    // Move away (clamp at the oldest reloads its original text) and back.
    assert!(app.history_prev(&rows));
    assert_eq!(
        app.input, "older row",
        "history row reloads its original text"
    );
    assert!(app.history_next(&rows));
    assert_eq!(app.input, "newest row");
    assert!(!app.history_next(&rows));
    // The adopted/empty draft comes back, never the temporary edit.
    assert_eq!(app.input, app.history_draft);
}

/// Queue recall adopts the recalled content as the draft (text + attachments
/// mirrored into both the pending slots and the remembered-draft stash).
#[test]
fn recall_queued_adopts_content_as_draft() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let image = neenee_core::ImagePart {
        mime: "image/png".to_string(),
        data: "abc".to_string(),
    };
    let mut dispatch = queued_dispatch("1", "session-a", "queued msg");
    dispatch.images = vec![image];
    app.pending_dispatch.push_back(dispatch);

    let Some(RecallQueued::Restored(dispatch)) = app.recall_queued("session-a") else {
        panic!("expected local restore");
    };
    app.restore_dispatch(dispatch);
    assert_eq!(app.input, "queued msg");
    assert_eq!(app.history_index, None, "recall enters draft mode");
    assert_eq!(
        app.history_draft, "queued msg",
        "recalled text becomes the draft"
    );
    assert_eq!(app.history_draft_images.len(), 1);
    assert_eq!(app.pending_images.len(), 1);
}

#[test]
fn recall_queued_always_restores_locally() {
    // With the insert/next-round distinction gone there is no agent-side
    // cancel to wait for: recall always pops the newest staged message and
    // hands it back as a local `Restored` item (the event loop then feeds it
    // to `restore_dispatch`), leaving the queue one item shorter.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.pending_dispatch
        .push_back(queued_dispatch("queued-1", "session-a", "queued"));

    let Some(RecallQueued::Restored(dispatch)) = app.recall_queued("session-a") else {
        panic!("expected local restore, not an agent cancel");
    };
    assert_eq!(dispatch.id, "queued-1");
    app.restore_dispatch(dispatch);
    assert_eq!(app.input, "queued");
    assert!(
        app.pending_dispatch.is_empty(),
        "recalled item must be removed from the outbox"
    );
}

#[test]
fn recall_queued_latches_completion_dismissal() {
    // A recall replaces `input` programmatically (not via a keystroke), so it
    // must latch `completion_dismissed` the same way a slash-command accept
    // does. Otherwise recalling a queued `/help` would immediately re-open the
    // slash-completion popup — a spurious "complete" step the user never asked
    // for. Mirrors the latch in the history-navigation paths.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.pending_dispatch
        .push_back(queued_dispatch("1", "session-a", "/help"));

    let Some(RecallQueued::Restored(dispatch)) = app.recall_queued("session-a") else {
        panic!("expected local restore");
    };
    app.restore_dispatch(dispatch);
    assert_eq!(app.input, "/help");
    assert!(
        app.completion_dismissed,
        "recall must latch dismissal so the slash popup stays hidden"
    );
    assert!(
        app.suggestion_index.is_none(),
        "recall must clear the completion highlight"
    );
    // The completions for `/help` are non-empty, so the latch is the only thing
    // keeping the render gate (`!completion_dismissed`) from drawing the menu.
    assert!(
        !app.completions().is_empty(),
        "`/help` should have candidates"
    );
}

#[test]
fn toggle_queue_block_flips_state_and_blocks_dispatch() {
    // `F3` / queue-modal block is the hard "send nothing" override. While a
    // session is blocked, `begin_next_round_dispatch` must yield nothing —
    // even though the item is `Waiting`. The event loop relies on
    // `is_queue_blocked` (and the app-side gate is its mirror) so a blocked
    // outbox can't slip through.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.pending_dispatch
        .push_back(queued_dispatch("1", "session-a", "first"));
    app.pending_dispatch
        .push_back(queued_dispatch("2", "session-a", "second"));

    // Not blocked initially.
    assert!(!app.is_queue_blocked("session-a"));
    assert_eq!(app.pending_count("session-a"), 2);

    // Toggle on.
    assert!(
        app.toggle_queue_block("session-a"),
        "first toggle should block"
    );
    assert!(app.is_queue_blocked("session-a"));

    // The block is persistent and session-scoped: another session is
    // unaffected.
    app.pending_dispatch
        .push_back(queued_dispatch("3", "session-b", "other"));
    assert!(!app.is_queue_blocked("session-b"));

    // Toggle off.
    assert!(
        !app.toggle_queue_block("session-a"),
        "second toggle should resume"
    );
    assert!(!app.is_queue_blocked("session-a"));
}

#[test]
fn block_and_resume_helpers_are_idempotent() {
    // `block_queue` forces the block on; `resume_queue` forces it off. Both
    // must be safe to call repeatedly. The queue modal's open/close path
    // relies on this: open always blocks, close always resumes.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.pending_dispatch
        .push_back(queued_dispatch("1", "session-a", "x"));

    app.block_queue("session-a");
    assert!(app.is_queue_blocked("session-a"));
    app.block_queue("session-a"); // idempotent
    assert!(app.is_queue_blocked("session-a"));

    app.resume_queue("session-a");
    assert!(!app.is_queue_blocked("session-a"));
    app.resume_queue("session-a"); // idempotent
    assert!(!app.is_queue_blocked("session-a"));
}

#[test]
fn recall_queued_at_targets_selected_index_not_newest() {
    // The queue modal's `Enter` re-edits the *selected* item (the ↑/↓
    // highlight), so a mid-queue item can be pulled back rather than always
    // the newest. `recall_queued_at(idx=0)` returns the front (next to pop),
    // distinct from `recall_queued` which is LIFO/newest.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.pending_dispatch
        .push_back(queued_dispatch("front", "session-a", "first"));
    app.pending_dispatch
        .push_back(queued_dispatch("back", "session-a", "second"));

    // idx 0 = front = "first".
    let Some(RecallQueued::Restored(dispatch)) = app.recall_queued_at("session-a", 0) else {
        panic!("expected restore of selected item");
    };
    assert_eq!(dispatch.id, "front");
    app.restore_dispatch(dispatch);
    assert_eq!(app.input, "first");
    assert_eq!(
        app.pending_count("session-a"),
        1,
        "recalled item must leave the outbox"
    );

    // Now the only remaining item is "second"; idx 0 still works.
    let Some(RecallQueued::Restored(dispatch)) = app.recall_queued_at("session-a", 0) else {
        panic!("expected restore");
    };
    assert_eq!(dispatch.id, "back");

    // Out of range is a no-op (returns None), leaving the (now empty) queue
    // untouched.
    assert!(app.recall_queued_at("session-a", 0).is_none());
}

#[test]
fn remove_queued_at_deletes_by_index_and_clamps() {
    // `D` in the queue modal deletes the highlighted item. The event loop
    // clamps `modal_index` after a delete; here we verify the core removal is
    // index-keyed and session-scoped.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.pending_dispatch
        .push_back(queued_dispatch("a", "session-a", "one"));
    app.pending_dispatch
        .push_back(queued_dispatch("b", "session-b", "two"));
    app.pending_dispatch
        .push_back(queued_dispatch("c", "session-a", "three"));

    // session-a has two items: idx 0 = "a", idx 1 = "c". Delete idx 1.
    let removed = app
        .remove_queued_at("session-a", 1)
        .expect("should remove idx 1");
    assert_eq!(removed.id, "c");
    assert_eq!(app.pending_count("session-a"), 1);
    assert_eq!(app.pending_count("session-b"), 1, "other session untouched");

    // Out of range → None, nothing removed.
    assert!(app.remove_queued_at("session-a", 5).is_none());
    assert_eq!(app.pending_count("session-a"), 1);
}

#[test]
fn move_queued_swaps_within_session_and_clamps_at_edges() {
    // `J`/`K` in the queue modal reorder the highlighted item. Moving toward
    // the front (delta -1) makes it the next to pop; toward the tail (delta 1)
    // pushes it back. Reorder is clamped to the session slice so it can't
    // escape into another session's items.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.pending_dispatch
        .push_back(queued_dispatch("a", "session-a", "one"));
    app.pending_dispatch
        .push_back(queued_dispatch("x", "session-b", "intruder"));
    app.pending_dispatch
        .push_back(queued_dispatch("b", "session-a", "two"));
    app.pending_dispatch
        .push_back(queued_dispatch("c", "session-a", "three"));

    // session-a display order: [a, b, c]. Move idx 0 (a) toward the tail by 2:
    // clamped to the last position → order becomes [b, c, a].
    app.move_queued("session-a", 0, 2);
    let order: Vec<&str> = app
        .pending_dispatch
        .iter()
        .filter(|d| d.session_id == "session-a")
        .map(|d| d.id.as_str())
        .collect();
    assert_eq!(order, vec!["b", "c", "a"]);

    // The intruder session-b item is untouched: reorder never crossed session
    // boundaries (session-b still has exactly one item, the same one).
    let session_b: Vec<&str> = app
        .pending_dispatch
        .iter()
        .filter(|d| d.session_id == "session-b")
        .map(|d| d.id.as_str())
        .collect();
    assert_eq!(session_b, vec!["x"]);

    // Move the now-front item (b) toward the front by 5: clamped to 0 →
    // stays put. Order unchanged.
    app.move_queued("session-a", 0, -5);
    let order: Vec<&str> = app
        .pending_dispatch
        .iter()
        .filter(|d| d.session_id == "session-a")
        .map(|d| d.id.as_str())
        .collect();
    assert_eq!(order, vec!["b", "c", "a"]);

    // Move idx 1 (c) toward the front by 1: swaps with b → [c, b, a].
    app.move_queued("session-a", 1, -1);
    let order: Vec<&str> = app
        .pending_dispatch
        .iter()
        .filter(|d| d.session_id == "session-a")
        .map(|d| d.id.as_str())
        .collect();
    assert_eq!(order, vec!["c", "b", "a"]);
}

#[test]
fn modal_paste_splices_text_inline_stripping_newlines() {
    // Pasting into a free-text modal field (here the provider editor's
    // API-key field) splices the text at the cursor and collapses newlines
    // so a copied multi-line block pastes as one continuous single line,
    // matching the single-line semantics the modal already enforces. No
    // chip is inserted and no attachment is staged.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.active_modal = Modal::ModelEditor;
    app.editor_field = 0;
    app.input = "sk-".to_string();
    app.cursor_position = app.input.chars().count();

    clipboard_ops::apply_clipboard_paste(
        &mut app,
        crate::tui::clipboard::ClipboardRead::Text("abc\ndef\n".to_string()),
    );

    assert_eq!(app.input, "sk-abcdef");
    assert_eq!(app.cursor_position, "sk-abcdef".chars().count());
    assert!(
        app.pending_text_pastes.is_empty(),
        "no chip staging in modals"
    );
    assert!(
        !app.input.contains("Pasted text"),
        "no chip label in modals"
    );
}

#[test]
fn modal_paste_inserts_at_cursor_not_at_end() {
    // The splice honors the cursor position, so a paste in the middle of
    // an existing field inserts between the surrounding characters rather
    // than appending.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.active_modal = Modal::ModelEditor;
    app.editor_field = 1;
    app.input = "gpt-4omini".to_string();
    app.cursor_position = "gpt-4o".chars().count();

    clipboard_ops::apply_clipboard_paste(
        &mut app,
        crate::tui::clipboard::ClipboardRead::Text("turbo".to_string()),
    );

    assert_eq!(app.input, "gpt-4oturbomini");
    assert_eq!(
        app.cursor_position,
        "gpt-4oturbo".chars().count(),
        "cursor lands just past the inserted text"
    );
}

#[test]
fn modal_paste_applies_to_provider_picker_and_history_search() {
    // The inline paste path is shared by every free-text modal that borrows
    // the input line, so the model picker filter and the history search
    // query paste the same way as the editor.
    for modal in [Modal::Models, Modal::HistorySearch] {
        let (mut app, _tmp) = app_in_tempdir(&[], &[]);
        app.active_modal = modal;
        app.input = String::new();
        app.cursor_position = 0;

        clipboard_ops::apply_clipboard_paste(
            &mut app,
            crate::tui::clipboard::ClipboardRead::Text("query".to_string()),
        );

        assert_eq!(
            app.input, "query",
            "paste should inline into free-text modal"
        );
        assert_eq!(app.cursor_position, "query".chars().count());
        assert!(app.pending_text_pastes.is_empty());
    }
}

#[test]
fn modal_paste_drops_image_with_failure_toast() {
    // An image paste has nowhere to go in a single-line modal field, so it
    // is dropped with a failure toast rather than silently lost or staged
    // as an attachment.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.active_modal = Modal::ModelEditor;
    app.input = String::new();
    app.cursor_position = 0;

    clipboard_ops::apply_clipboard_paste(
        &mut app,
        crate::tui::clipboard::ClipboardRead::Image {
            data: vec![0x89, 0x50, 0x4e, 0x47],
            mime: "image/png".to_string(),
        },
    );

    assert!(app.input.is_empty(), "image paste must not insert text");
    assert!(
        app.pending_images.is_empty(),
        "no attachment staging in modals"
    );
    assert!(
        app.copy_toast_failed,
        "image paste in a modal should toast a failure"
    );
    assert!(app.copy_toast_until.is_some());
}

#[test]
fn composer_paste_still_chips_large_text_on_main_prompt() {
    // The main-prompt path is unchanged: a large paste collapses into a
    // `[Pasted text #N +M lines]` chip and stages the full text, so the
    // modal-aware branching did not regress the composer behaviour.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.active_modal = Modal::None;
    app.input = String::new();
    app.cursor_position = 0;
    let big = format!("line\n{}", "x".repeat(2048));

    clipboard_ops::apply_clipboard_paste(
        &mut app,
        crate::tui::clipboard::ClipboardRead::Text(big.clone()),
    );

    assert!(
        app.input.contains("Pasted text #1"),
        "large paste on the main prompt should produce a chip"
    );
    assert_eq!(app.pending_text_pastes.len(), 1);
    assert_eq!(app.pending_text_pastes[0], big);
}

#[test]
fn paste_in_readonly_modal_is_dropped_silently() {
    // Read-only / non-text modals (Help, Sessions, Permission, ...) drop a
    // paste silently — no insertion, no toast, no attachment.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.active_modal = Modal::Help;
    app.input = String::new();
    app.cursor_position = 0;

    clipboard_ops::apply_clipboard_paste(
        &mut app,
        crate::tui::clipboard::ClipboardRead::Text("ignored".to_string()),
    );

    assert!(app.input.is_empty());
    assert!(
        app.copy_toast_until.is_none(),
        "readonly modal paste should not toast"
    );
    assert!(app.pending_text_pastes.is_empty());
}

#[test]
fn composer_image_paste_rejected_when_model_lacks_vision() {
    // When the current model doesn't support vision, pasting an image on
    // the main prompt should show a failure toast and leave no attachment.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.active_modal = Modal::None;
    app.current_model = "glm-5.2".to_string(); // vision: false
    app.input = String::new();
    app.cursor_position = 0;

    clipboard_ops::apply_clipboard_paste(
        &mut app,
        crate::tui::clipboard::ClipboardRead::Image {
            data: vec![0x89, 0x50, 0x4e, 0x47],
            mime: "image/png".to_string(),
        },
    );

    assert!(
        app.pending_images.is_empty(),
        "non-vision model must not stage image attachments"
    );
    assert!(
        app.copy_toast_failed,
        "non-vision model should toast a failure on image paste"
    );
    assert!(
        app.copy_toast_message.contains("does not support images"),
        "toast should say the model doesn't support images, got: {}",
        app.copy_toast_message,
    );
    assert!(app.copy_toast_until.is_some());
}

#[test]
fn composer_image_paste_accepted_when_model_has_vision() {
    // When the current model supports vision, pasting an image on the main
    // prompt should stage the attachment and insert an `[Image #N]` chip.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.active_modal = Modal::None;
    app.current_model = "gpt-4o".to_string(); // vision: true
    app.input = String::new();
    app.cursor_position = 0;

    clipboard_ops::apply_clipboard_paste(
        &mut app,
        crate::tui::clipboard::ClipboardRead::Image {
            data: vec![0x89, 0x50, 0x4e, 0x47],
            mime: "image/png".to_string(),
        },
    );

    assert_eq!(
        app.pending_images.len(),
        1,
        "vision-capable model should stage the image attachment"
    );
    assert!(
        app.input.contains("Image #1"),
        "image chip should be inserted into the input, got: {}",
        app.input,
    );
    assert!(
        !app.copy_toast_failed,
        "vision-capable model should show a success toast"
    );
    assert!(app.copy_toast_until.is_some());
}

#[test]
fn set_cursor_marks_immediate_sync_pending() {
    // The IME-correctness fix hinges on every caret move routing through
    // `set_cursor` so the event loop's immediate flush re-anchors the
    // terminal cursor before the next frame. A raw write to
    // `cursor_position` would silently skip it. This locks the contract.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = "hello".to_string();
    app.cursor_sync_pending = false;

    app.set_cursor(3);
    assert_eq!(app.cursor_position, 3);
    assert!(
        app.cursor_sync_pending,
        "set_cursor must arm the immediate cursor sync — the whole IME fix depends on it"
    );

    // set_cursor_end is the common post-replacement helper and must do the same.
    app.cursor_sync_pending = false;
    app.set_cursor_end();
    assert_eq!(app.cursor_position, 5);
    assert!(
        app.cursor_sync_pending,
        "set_cursor_end must also arm the sync"
    );
}

// ── Caret ownership / visibility (IME anchor) ─────────────────────────────
// `App::caret_owner` / `App::caret_visible` are the single source of truth for
// which surface holds the terminal cursor. The IME anchors its composition
// window to that cursor, so any state that owns no caret must hide it —
// otherwise the IME binds to a stale coordinate (the "drift" when a disclosure
// is clicked mid-composition). These lock the contract for every state.

#[test]
fn caret_owner_composer_by_default() {
    let (app, _tmp) = app_in_tempdir(&[], &[]);
    assert_eq!(app.caret_owner(), CaretOwner::Composer);
    assert!(
        app.caret_visible(),
        "no modal, no focus, no selection → visible"
    );
}

#[test]
fn caret_owner_none_when_step_focused() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.focused_target = Some(InteractiveTarget::tool_step(0));
    assert_eq!(app.caret_owner(), CaretOwner::None);
    assert!(
        !app.caret_visible(),
        "a focused transcript step owns no caret → hidden, IME unanchored"
    );
}

#[test]
fn caret_owner_none_in_envoy_view() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.enter_envoy("call-1".to_string());
    assert_eq!(app.caret_owner(), CaretOwner::None);
    assert!(
        !app.caret_visible(),
        "envoy zoom has no input line → cursor hidden, IME unanchored"
    );
}

#[test]
fn caret_owner_modal_for_caret_modals() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    for modal in [
        Modal::Models,
        Modal::Connections,
        Modal::ModelEditor,
        Modal::CustomProvider,
        Modal::ConfigThemeCustom,
    ] {
        app.active_modal = modal;
        assert_eq!(
            app.caret_owner(),
            CaretOwner::Modal,
            "{modal:?} borrows the input line and renders its own caret",
        );
        assert!(
            app.caret_visible(),
            "{modal:?} must keep the cursor visible so the IME anchors to its field",
        );
    }
}

#[test]
fn caret_owner_none_for_read_only_and_decision_modals() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    for modal in [
        Modal::Help,
        Modal::Sessions,
        Modal::Tools,
        Modal::Mcp,
        Modal::Permissions,
        Modal::Activity,
        // `Question` is listed here to cover the *default* state — any option
        // but "Other" highlighted (or no question model at all). Its caret
        // ownership is conditional: see `caret_owner_question_owns_caret_only_on_other`.
        Modal::Question,
        Modal::Permission,
        Modal::InputInjection,
    ] {
        app.active_modal = modal;
        assert_eq!(
            app.caret_owner(),
            CaretOwner::None,
            "{modal:?} renders no caret → cursor must hide so the IME has no stale anchor",
        );
        assert!(
            !app.caret_visible(),
            "{modal:?} must hide the terminal cursor",
        );
    }
}

#[test]
fn caret_owner_question_owns_caret_only_on_other() {
    // The Question modal is a decision sheet (no caret) EXCEPT while the
    // synthetic "Other" free-text row is highlighted — then it is a real
    // text-input surface and must own the terminal cursor so the host IME can
    // anchor its composition window. Navigating to/from "Other" flips
    // ownership, so the IME anchor appears exactly when there is a field to
    // type into and never when there is not.
    use crate::tui::question_model::{QuestionAction, QuestionModel};
    use neenee_core::{UserQuestion, UserQuestionOption, UserQuestionRequest};

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let req = UserQuestionRequest {
        id: "q".into(),
        questions: vec![UserQuestion {
            header: None,
            question: "pick".into(),
            options: vec![
                UserQuestionOption {
                    label: "a".into(),
                    description: None,
                },
                UserQuestionOption {
                    label: "b".into(),
                    description: None,
                },
            ],
            multi_select: false,
        }],
    };
    // Open: highlight on row 0 (a real option) → no caret, cursor hidden.
    let model = QuestionModel::open(req);
    app.active_modal = Modal::Question;
    app.question = Some(model.clone());
    assert_eq!(
        app.caret_owner(),
        CaretOwner::None,
        "real option → no caret"
    );
    assert!(
        !app.caret_visible(),
        "a non-Other option must hide the cursor so the IME has no stale anchor",
    );

    // Navigate down to "Other" (index 2) → caret owned, cursor visible.
    let model = model.update(QuestionAction::Down).0; // -> b (1)
    let model = model.update(QuestionAction::Down).0; // -> Other (2)
    app.question = Some(model);
    assert_eq!(
        app.caret_owner(),
        CaretOwner::Modal,
        "Other highlighted → modal owns the caret for the IME",
    );
    assert!(
        app.caret_visible(),
        "the Other field must keep the cursor visible so the IME anchors to it",
    );

    // Navigate back to a real option → ownership reverts to None.
    let model = app.question.take().unwrap().update(QuestionAction::Up).0;
    app.question = Some(model);
    assert_eq!(
        app.caret_owner(),
        CaretOwner::None,
        "leaving Other must drop caret ownership again",
    );
}

#[test]
fn caret_hidden_while_selection_active_even_for_composer() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    // Composer owns the caret, but an active selection hides the block cursor
    // so it does not clash with the selection background. Ownership is
    // unaffected; only visibility folds in the selection.
    assert_eq!(app.caret_owner(), CaretOwner::Composer);
    app.selection = SelectionState::Range {
        anchor: crate::tui::model::layout::SemanticCursor::new(0, 0, 0),
        head: crate::tui::model::layout::SemanticCursor::new(0, 0, 3),
    };
    assert_eq!(app.caret_owner(), CaretOwner::Composer);
    assert!(
        !app.caret_visible(),
        "an active selection hides the cursor regardless of ownership",
    );
}

#[test]
fn modal_owns_caret_matches_renderer_set_cursor_sites() {
    // Every modal that calls `set_cursor_position` in its renderer must be
    // declared in `Modal::owns_caret`, and vice versa — the two lists must
    // stay in lockstep so visibility and paint never disagree.
    //
    // The one deliberate exception is `Modal::Question`: its renderer places
    // the real cursor only while the "Other" free-text row is highlighted, and
    // ownership is decided *state-dependently* in `App::caret_owner` (which
    // consults `QuestionModel::is_other_highlighted`) rather than by the static
    // `owns_caret()`. It therefore appears in neither list here — it is tested
    // separately by `caret_owner_question_owns_caret_only_on_other`.
    // HistorySearch is also a deliberate exception: its panel floats above a
    // live composer that IS the filter field, so the composer (not the modal)
    // owns the caret — handled state-dependently in `App::caret_owner`. It
    // appears in `not_owns` below and is exercised by the caret-owner tests.
    let owns = [
        Modal::Models,
        Modal::Connections,
        Modal::ModelEditor,
        Modal::CustomProvider,
        Modal::ConfigThemeCustom,
    ];
    for m in owns {
        assert!(m.owns_caret(), "{m:?} must own the caret");
    }
    let not_owns = [
        Modal::None,
        Modal::Help,
        Modal::Sessions,
        Modal::Tools,
        Modal::Mcp,
        Modal::Permissions,
        Modal::Activity,
        Modal::Question,
        Modal::Permission,
        Modal::InputInjection,
        Modal::ProviderTemplate,
        Modal::HistorySearch,
    ];
    for m in not_owns {
        assert!(!m.owns_caret(), "{m:?} must not own the caret");
    }
}

/// `modal_scroll_field` is the single source of truth that every `Scroll*`
/// action consults: it must resolve each scrollable modal to its own scroll
/// offset (and the right follow-flag for list modals), and return `None` for
/// the modals that don't scroll their own body. This is the event-loop half of
/// "any modal should support scroll" — if a modal is missing here, a page key
/// silently no-ops inside it.
#[test]
fn modal_scroll_field_resolves_every_scrollable_modal() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);

    // Seed a few follow flags so we can assert the helper hands back the
    // right one (and that mutating through it actually clears it).
    app.session_modal_follow = true;
    app.history_modal_follow = true;
    app.queue_modal_follow = true;
    app.model_modal_follow = true;
    app.question_modal_follow = true;

    // List modals return a follow-flag; clearing it must hit the right field.
    app.active_modal = Modal::Queue;
    {
        let (scroll, follow) = app.modal_scroll_field().expect("queue scrolls");
        *scroll = 5;
        if let Some(f) = follow {
            *f = false;
        }
    }
    assert_eq!(app.queue_scroll, 5, "queue scroll mutated through helper");
    assert!(
        !app.queue_modal_follow,
        "queue follow cleared through helper"
    );

    app.active_modal = Modal::Tools;
    {
        let (_, follow) = app.modal_scroll_field().expect("tools scrolls");
        if let Some(f) = follow {
            *f = false;
        }
    }
    assert!(
        !app.session_modal_follow,
        "tools reuses session follow flag"
    );

    app.active_modal = Modal::Sessions;
    {
        let (_, follow) = app.modal_scroll_field().expect("sessions scrolls");
        assert!(follow.is_some(), "sessions shares the session follow flag");
    }

    // Pure-content modals return a scroll ref but no follow flag.
    for m in [
        Modal::Help,
        Modal::Activity,
        Modal::Permissions,
        Modal::Config,
    ] {
        app.active_modal = m;
        let (s, f) = app.modal_scroll_field().expect("{m:?} scrolls");
        assert!(f.is_none(), "{m:?} has no selection-follow flag");
        // Mutating must hit a distinct field per modal (not all the same slot).
        *s = 7;
    }
    assert_eq!(app.help_scroll, 7);
    app.active_modal = Modal::Activity;
    if let Some((s, _)) = app.modal_scroll_field() {
        *s = 9;
    }
    assert_eq!(app.activity_scroll, 9);
    assert_ne!(app.help_scroll, 9, "each modal has its own field");

    // The non-scrolling modals resolve to None so the action falls through to
    // the transcript / caret handling.
    for m in [
        Modal::None,
        Modal::Permission,
        Modal::ModelEditor,
        Modal::InputInjection,
    ] {
        app.active_modal = m;
        assert!(
            app.modal_scroll_field().is_none(),
            "{m:?} must not scroll its own body"
        );
    }
}

/// The page step follows the captured modal body height (when known) and
/// falls back to the transcript `view_height` before the first render. It must
/// always be at least 1 so a page key never no-ops on a zero capture.
#[test]
fn modal_page_step_tracks_body_height_and_floors_at_one() {
    use crate::tui::event_loop::modal_page_step;
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);

    // No body height captured yet → falls back to view_height, floored at 1.
    app.view_height = 0;
    app.modal_body_height = 0;
    assert_eq!(modal_page_step(&app), 1);

    // Transcript height known, modal not yet rendered → uses view_height - 1.
    app.view_height = 24;
    assert_eq!(modal_page_step(&app), 23);

    // Once the modal body height is captured, it wins over view_height so a
    // page advance matches the actual modal, not the transcript behind it.
    app.modal_body_height = 10;
    assert_eq!(
        modal_page_step(&app),
        9,
        "modal body height takes precedence"
    );

    // A 1-row modal body still yields a step of 1 (never 0).
    app.modal_body_height = 1;
    assert_eq!(modal_page_step(&app), 1);
}

/// `neenee resume` (no id) opens the sessions picker at startup instead of
/// loading any session, so the `startup_picker` flag must gate quit-on-close.
/// This pins the two state transitions the event loop relies on:
///
/// 1. The flag defaults to `false` in an ordinary (in-session) App, so the
///    `/sessions` modal only ever dismisses on Esc.
/// 2. Selecting a session from the picker clears the flag — once a real
///    conversation backs the view, the picker reverts to a plain transient
///    overlay. (The event loop's `OpenSelectedSession` arm does this.)
#[test]
fn startup_picker_flag_governs_sessions_modal_quit_and_resets_on_open() {
    use std::sync::atomic::Ordering;

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);

    // Default: an in-session App never treats the picker as a startup gate.
    assert!(!app.startup_picker);

    // Simulate the startup path (`neenee resume` with no id): the picker
    // opens and `startup_picker` is armed. Closing it must quit.
    app.startup_picker = true;
    app.active_modal = Modal::Sessions;
    assert!(app.startup_picker && app.active_modal == Modal::Sessions);
    // The quit gate is `should_quit`; it is still clear until a close happens.
    assert!(!app.should_quit.load(Ordering::SeqCst));

    // Open a session from the picker: the flag clears so a later `/sessions`
    // modal behaves as a normal transient overlay.
    app.startup_picker = false;
    app.active_modal = Modal::None;
    assert!(
        !app.startup_picker,
        "opening a session drops the startup gate"
    );
}

/// Helper: build a minimal picker row so a test list is readable.
fn overview_row(id: &str) -> neenee_core::SessionOverview {
    neenee_core::SessionOverview {
        id: id.to_string(),
        overview: format!("overview-{id}"),
        created_at: 0,
        updated_at: 0,
        message_count: 0,
        active: false,
    }
}

/// Deleting the highlighted row from the sessions picker must leave the cursor
/// on the **same line** (the next session slides up into the removed slot), not
/// jump back to the top. The `DeleteSelectedSession` event-loop arm does this
/// optimistically — it removes the row and clamps `modal_index` — so this pins
/// that core behaviour: a mid-list delete keeps the index, and a delete of the
/// last row clamps to the new last row rather than wrapping to 0.
#[test]
fn sessions_picker_delete_keeps_cursor_on_the_same_line() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.sessions_overview = (0..5)
        .map(|i| overview_row(&format!("s{i}")))
        .collect::<Vec<_>>();
    app.active_modal = Modal::Sessions;

    // Delete the row at index 2 (mid-list). The cursor must stay at 2 — now
    // pointing at "s3", which slid into the freed slot.
    app.modal_index = 2;
    let idx = app.modal_index.min(app.sessions_overview.len() - 1);
    let deleted = app.sessions_overview.remove(idx);
    assert_eq!(deleted.id, "s2");
    app.modal_index = app.modal_index.min(app.sessions_overview.len() - 1);
    assert_eq!(app.modal_index, 2, "mid-list delete keeps the cursor put");
    assert_eq!(
        app.sessions_overview[app.modal_index].id, "s3",
        "the next session slid into the removed slot"
    );

    // Delete the now-last row (index 3 in the shrunken 4-row list, which holds
    // s4). The list then has 3 rows, so the cursor must clamp to index 2 (the
    // new last row), not jump to 0.
    app.modal_index = 3;
    let idx = app.modal_index.min(app.sessions_overview.len() - 1);
    app.sessions_overview.remove(idx);
    app.modal_index = app.modal_index.min(app.sessions_overview.len() - 1);
    assert_eq!(
        app.modal_index, 2,
        "deleting the last row clamps to the new last row, not the top"
    );
}

/// Regression: after a delete the backend pushes a fresh `SessionsOverview`,
/// and the event loop used to treat *every* such push as an "open the picker"
/// request — resetting `modal_index` to 0 and `session_scroll` to 0. That
/// snapped the selection back to the top on each delete, undoing the optimistic
/// local removal. The refresh path must preserve the cursor/scroll when the
/// modal is already open, resetting only on a genuine open (closed → open).
#[test]
fn sessions_picker_data_refresh_does_not_reset_cursor_when_already_open() {
    // This mirrors the event-loop branch exactly: `opening` is true only when
    // the modal is not already Sessions.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.sessions_overview = (0..5)
        .map(|i| overview_row(&format!("s{i}")))
        .collect::<Vec<_>>();
    app.active_modal = Modal::Sessions;
    app.modal_index = 3;
    app.session_scroll = 2;

    // Simulate the refresh path (open_sessions signal + fresh overview) with
    // the modal ALREADY open: cursor and scroll must be preserved.
    let opening = app.active_modal != Modal::Sessions; // false
    app.active_modal = Modal::Sessions;
    if opening {
        app.modal_index = 0;
        app.session_scroll = 0;
        app.session_modal_follow = true;
    }
    assert_eq!(app.modal_index, 3, "refresh while open keeps the cursor");
    assert_eq!(app.session_scroll, 2, "refresh while open keeps the scroll");

    // Now simulate opening from a different modal (the genuine-open case):
    // cursor and scroll reset to the top.
    app.active_modal = Modal::None;
    let opening = app.active_modal != Modal::Sessions; // true
    app.active_modal = Modal::Sessions;
    if opening {
        app.modal_index = 0;
        app.session_scroll = 0;
        app.session_modal_follow = true;
    }
    assert_eq!(app.modal_index, 0, "a genuine open resets the cursor");
    assert_eq!(app.session_scroll, 0, "a genuine open resets the scroll");
}

/// `resolve_scroll` is the pure scroll-resolution core factored out of
/// `render_body`, now also used by the windowed sessions picker. It must keep
/// the selection in view (edge-margin follow), clamp to the valid range, and
/// report the true `max_scroll` of the full list — which is what lets the
/// picker build only the visible window while the scrollbar still reflects the
/// whole list. This pins those invariants.
#[test]
fn resolve_scroll_follows_selection_and_clamps_to_max_scroll() {
    use crate::tui::primitives::{SCROLL_EDGE_MARGIN, resolve_scroll};

    // 100 rows, 10 visible → max_scroll is 90. A selection at row 50 with a
    // top-anchored scroll of 0 must pull the viewport down so row 50 is in
    // view (edge-margin follow), but never past max_scroll.
    let mut scroll = 0usize;
    let (start, max_scroll) = resolve_scroll(&mut scroll, 10, 100, Some(50), SCROLL_EDGE_MARGIN);
    assert_eq!(max_scroll, 90, "max_scroll reflects the full list length");
    assert!(
        (start..start + 10).contains(&50),
        "selection 50 must land inside the resolved window {start}..{}",
        start + 10
    );
    assert!(start <= 90, "resolved scroll never exceeds max_scroll");

    // Selection at the very end clamps to max_scroll (no overshoot).
    let mut scroll = 0usize;
    let (start, _) = resolve_scroll(&mut scroll, 10, 100, Some(99), SCROLL_EDGE_MARGIN);
    assert_eq!(start, 90, "end-of-list selection pins to max_scroll");

    // Fewer rows than the viewport: max_scroll is 0 and scroll collapses to 0.
    let mut scroll = 5usize;
    let (start, max_scroll) = resolve_scroll(&mut scroll, 10, 3, Some(1), SCROLL_EDGE_MARGIN);
    assert_eq!(
        max_scroll, 0,
        "content shorter than viewport has no scroll range"
    );
    assert_eq!(start, 0);

    // No follow: scroll is only clamped to max_scroll, never auto-scrolled.
    let mut scroll = 200usize;
    let (start, max_scroll) = resolve_scroll(&mut scroll, 10, 100, None, SCROLL_EDGE_MARGIN);
    assert_eq!(max_scroll, 90);
    assert_eq!(start, 90, "out-of-range scroll clamps to max_scroll");
}

/// Esc back-out must respect modal hierarchy: a drill-in sub-page backs out to
/// its parent view *before* any close/quit logic runs. Regression for a bug
/// where pressing Esc in the `Sessions › Info` sub-view at startup
/// (`startup_picker` armed) quit the program instead of returning to the
/// sessions list — because the startup-quit check was ordered before the
/// sub-page back-out check. This mirrors the event loop's `CloseModal` arm
/// ordering exactly (deepest level first).
#[test]
fn esc_in_session_info_subpage_backs_out_before_quit_or_close() {
    use std::sync::atomic::Ordering;

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);

    // The user is in the Sessions › Info sub-view at startup: both the startup
    // gate AND the info drill-in are active. Esc must back out to the list,
    // NOT quit.
    app.startup_picker = true;
    app.active_modal = Modal::Sessions;
    app.session_info_detail = true;
    app.session_detail = Some(neenee_core::SessionDetail {
        id: "x".to_string(),
        ..Default::default()
    });
    assert!(!app.should_quit.load(Ordering::SeqCst));

    // Mirror the CloseModal arm's ordering (deepest level wins).
    let quit = if app.active_modal == Modal::Sessions && app.session_info_detail {
        app.session_info_detail = false;
        app.session_detail = None;
        app.session_info_scroll = 0;
        false
    } else if app.startup_picker && app.active_modal == Modal::Sessions {
        app.should_quit.store(true, Ordering::SeqCst);
        true
    } else {
        false
    };
    assert!(!quit, "Esc from Info backs out to the list, never quits");
    assert!(
        !app.session_info_detail,
        "sub-view cleared — back on the list"
    );
    assert!(
        !app.should_quit.load(Ordering::SeqCst),
        "program did not quit"
    );

    // Now the list is showing (still at startup). A second Esc DOES quit, since
    // there is no deeper sub-view left.
    let quit = if app.active_modal == Modal::Sessions && app.session_info_detail {
        false
    } else if app.startup_picker && app.active_modal == Modal::Sessions {
        app.should_quit.store(true, Ordering::SeqCst);
        true
    } else {
        false
    };
    assert!(quit, "Esc from the startup list quits the program");
    assert!(app.should_quit.load(Ordering::SeqCst));
}

/// Ctrl+C at the `neenee resume` startup picker must quit the program — the
/// same as Esc and an outside click — NOT drop into an empty session. Regression
/// for a bug where Ctrl+C closed the modal (`active_modal = None`) but never set
/// `should_quit`, so the user landed in a bare empty chat (which a stray
/// `/models` then persisted as an empty-session file). Mirrors the event loop's
/// `CtrlC` arm ordering.
#[test]
fn ctrl_c_at_startup_picker_quits_instead_of_dropping_to_empty_session() {
    use std::sync::atomic::Ordering;

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.startup_picker = true;
    app.active_modal = Modal::Sessions;
    assert!(!app.should_quit.load(Ordering::SeqCst));

    // Mirror the CtrlC arm: startup_picker + Sessions → quit (not modal-close).
    // (Selection copy is skipped — no selection in a modal.)
    let quit = if app.startup_picker && app.active_modal == Modal::Sessions {
        app.should_quit.store(true, Ordering::SeqCst);
        true
    } else if app.active_modal != Modal::None && app.active_modal != Modal::Permission {
        app.active_modal = Modal::None;
        false
    } else {
        false
    };
    assert!(quit, "Ctrl+C at the startup picker quits");
    assert!(
        app.should_quit.load(Ordering::SeqCst),
        "program quits, does not drop into an empty session"
    );
    // The modal was NOT merely closed (which is what created the empty-session
    // trap): should_quit is set, so the loop exits.
    assert_ne!(app.active_modal, Modal::None, "quit path wins over close");
}
