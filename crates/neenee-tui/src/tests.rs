use super::*;
use neenee_contracts::{AgentResponse, LoopStatus, Message, Role, RoundEvent, ToolCall};

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use tokio::sync::mpsc;

use crate::app::{App, CaretOwner};
use crate::completion::CompletionKind;
use crate::completion::{
    completion_anchor_x, is_explicit_path_prefix, manual_walk, mention_range_at, path_query_match,
    resolve_explicit_dir, resolved_slash_command_len,
};
use crate::config;
use crate::event_loop::{display_status, focused_messages_mut};
use crate::model::layout::{InteractiveTarget, LayoutMap};
use crate::model::selection::{SelectionDrag, SelectionState};
use crate::transcript::{
    finalize_streaming_reasoning, transcript_message_from_core, transcript_messages_from_core,
};
use crate::versioned::{TranscriptPatch, TranscriptUpdate};
use crate::view::Theme;
use crate::{ActivityTab, Modal};
use neenee_contracts::{AgentRequest, ProviderPickerSnapshot};

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
            stream: neenee_contracts::ToolStream::Stdout("line\n".to_string()),
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

    assert!(crate::event_loop::apply_transcript_patch(
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
    use crate::model::document::UserMessageOrigin;
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
    use crate::model::document::UserMessageOrigin;
    let first = Message::new(Role::Assistant, "first answer");
    let inserted = Message::new(Role::User, "one more constraint").with_origin(
        neenee_contracts::InjectionOrigin::new(neenee_contracts::InjectionKind::UserSteer),
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
    use crate::model::document::UserMessageOrigin;

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
        neenee_contracts::CommandRecord::new("search", "foo").with_result(
            neenee_contracts::CommandResult::Search {
                query: "foo".to_string(),
                hits: vec![],
            },
        ),
        // A result-less record (legacy fold / shell passthrough): the
        // invocation still restores, with an empty expandable body.
        neenee_contracts::CommandRecord::new("shell", "!ls -la"),
    ];
    let restored = transcript_commands_from_ledger(commands);
    assert_eq!(restored.len(), 2);
    let search = &restored[0];
    assert!(
        search.is_command_result(),
        "command rows carry the CommandResult kind"
    );
    assert_eq!(
        search.command_result_summary().as_deref(),
        Some("/search foo")
    );
    assert_eq!(
        search.command_result_text().as_deref(),
        Some("No relevant history found.")
    );
    assert_eq!(search.round, None, "a command is not a conversation turn");
    assert_ne!(
        search.role,
        neenee_contracts::Role::Assistant,
        "never assistant prose"
    );

    let shell = &restored[1];
    assert!(shell.is_command_result());
    assert_eq!(shell.command_result_summary().as_deref(), Some("!ls -la"));
    assert_eq!(shell.command_result_text(), None);
}

/// ADR-0106 §2: on resume, command rows merge at their turn seams — a
/// command issued between two prompts renders between those rounds, exactly
/// where it appeared live — instead of all appending to the dialogue's tail.
#[test]
fn command_rows_merge_at_their_turn_seams_on_restore() {
    use crate::model::document::TranscriptMessage;
    use crate::transcript::merge_command_rows;
    use neenee_contracts::Role;

    // Dialogue: two rounds, timestamps 1000 and 3000.
    let dialogue = vec![
        TranscriptMessage::new(Role::User, "first prompt")
            .with_sent_at_ms(1000)
            .with_origin(crate::model::document::UserMessageOrigin::Chat),
        TranscriptMessage::new(Role::Assistant, "first reply"),
        TranscriptMessage::new(Role::User, "second prompt")
            .with_sent_at_ms(3000)
            .with_origin(crate::model::document::UserMessageOrigin::Chat),
        TranscriptMessage::new(Role::Assistant, "second reply"),
    ];
    // Ledger: one command run between the rounds (2000), one after the last
    // (4000). Rebuild stamps each row with the record's timestamp.
    let commands = crate::transcript::transcript_commands_from_ledger(vec![
        neenee_contracts::CommandRecord::new("compact", "")
            .with_result(neenee_contracts::CommandResult::Ack {
                title: "Compacted".to_string(),
            })
            .with_timestamp(2000),
        neenee_contracts::CommandRecord::new("new", "")
            .with_result(neenee_contracts::CommandResult::Text(
                "Started new session: c3".to_string(),
            ))
            .with_timestamp(4000),
    ]);

    let merged = merge_command_rows(dialogue, commands);
    let texts: Vec<&str> = merged.iter().map(|m| m.raw.as_str()).collect();
    assert_eq!(
        texts,
        vec![
            "first prompt",
            "first reply",
            "/compact",
            "second prompt",
            "second reply",
            "/new",
        ],
        "the mid-round command lands at its seam; the post-dialogue one at the tail"
    );
}

/// ADR-0108: a restored slash/shell echo folds into the command ledger
/// projection — the invocation renders once, on the `⌘` command component,
/// never twice (a `▌ cmd` user bubble *and* a command row). The echo is
/// dropped from the dialogue before merge; the ledger row keeps the record.
#[test]
fn restored_slash_echoes_fold_into_command_components() {
    use crate::model::document::UserMessageOrigin;
    use neenee_contracts::Role;

    let restored = transcript_messages_from_core(
        vec![
            Message::new(Role::User, "hello"), // a real prompt: survives
            Message::new(Role::Assistant, "hi"),
            // A durable command echo (the legacy live path persisted these).
            Message::command_echo("/pursue ship it"),
            // A display-content slash shape (legacy sessions pre-ADR-0050).
            {
                let mut m = Message::new(Role::User, "expanded prompt text");
                m.display_content = Some("/autopilot on".to_string());
                m
            },
            Message::new(Role::User, "and me too"), // another real prompt
        ],
        &crate::config::TuiConfig::default(),
    );

    let raws: Vec<&str> = restored.iter().map(|m| m.raw.as_str()).collect();
    assert_eq!(
        raws,
        vec!["hello", "hi", "and me too"],
        "command echoes must not render as user bubbles (ADR-0108); the ledger row owns the invocation"
    );
    // The projection builder still classifies them correctly when asked
    // directly (used by Activity-modal prompt gating) — the fold happens at
    // the list level, not by breaking classification.
    let echo = transcript_message_from_core(Message::command_echo("/pursue ship it")).unwrap();
    assert_eq!(echo.origin, UserMessageOrigin::Slash);
}

/// ADR-0106 §2: dialogue with no user timestamps is never reordered — the
/// command rows keep ledger order at the tail rather than guessing.
#[test]
fn command_rows_tail_when_dialogue_has_no_timestamps() {
    use crate::model::document::TranscriptMessage;
    use crate::transcript::merge_command_rows;
    use neenee_contracts::Role;

    let dialogue = vec![
        TranscriptMessage::new(Role::User, "prompt"),
        TranscriptMessage::new(Role::Assistant, "reply"),
    ];
    let commands = crate::transcript::transcript_commands_from_ledger(vec![
        neenee_contracts::CommandRecord::new("compact", "").with_timestamp(123),
    ]);

    let merged = merge_command_rows(dialogue, commands);
    let texts: Vec<&str> = merged.iter().map(|m| m.raw.as_str()).collect();
    assert_eq!(texts, vec!["prompt", "reply", "/compact"]);
}

#[test]
fn command_result_message_expands_and_round_trips_display() {
    // The command block's collapsed header is the invocation; expansion
    // reveals the typed result body. Pinning is respected (user toggle wins).
    use crate::model::document::TranscriptMessage;
    let mut message = TranscriptMessage::command_result(
        "permissions",
        "",
        Some(neenee_contracts::CommandResult::PermissionList {
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

/// ADR-0106: the command row's layout follows the shape of the reply — a
/// short single line joins inline (` · `), anything longer discloses, and a
/// missing result renders plain. The classifier is width-aware so an inline
/// reply is never a truncated fragment.
#[test]
fn command_row_layout_classifies_by_result_shape() {
    use crate::model::document::{CommandRowLayout, TranscriptMessage};

    // No result (shell passthrough / legacy fold) → Plain.
    let shell = TranscriptMessage::command_result("shell", "!ls -la", None);
    assert_eq!(
        shell.command_row_layout(80),
        Some(CommandRowLayout::Plain),
        "a result-less record has nothing to disclose"
    );

    // Short single-line reply fits beside the invocation → Inline.
    let fresh = TranscriptMessage::command_result(
        "new",
        "",
        Some(neenee_contracts::CommandResult::Text(
            "Started new session: a1b2c3".to_string(),
        )),
    );
    assert_eq!(fresh.command_row_layout(80), Some(CommandRowLayout::Inline));

    // The same reply in a narrow band must NOT inline — it would truncate.
    assert_eq!(
        fresh.command_row_layout(12),
        Some(CommandRowLayout::Disclose),
        "an inline reply that would truncate discloses instead"
    );

    // Multi-line replies always disclose, at any width.
    let permissions = TranscriptMessage::command_result(
        "permissions",
        "",
        Some(neenee_contracts::CommandResult::PermissionList {
            allowed: vec!["bash".to_string(), "edit_file".to_string()],
        }),
    );
    assert_eq!(
        permissions.command_row_layout(200),
        Some(CommandRowLayout::Disclose),
        "a multi-line result always earns the disclosure affordance"
    );
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
    // No persisted effort → no depth chip on restore.
    assert_eq!(restored.effort, None);

    // The persisted reasoning depth round-trips with the attribution.
    let mut message = Message::new(Role::Assistant, "deep thought");
    message.effort = Some("high".to_string());
    let restored = transcript_message_from_core(message).unwrap();
    assert_eq!(restored.effort.as_deref(), Some("high"));
    assert_eq!(restored.attribution_label().map(|(_, m)| m), None::<String>);

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
        effort: None,
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
        effort: None,
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
            effort: None,
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
fn provider_retry_state_formats_summary_and_timing() {
    let now = std::time::Instant::now();
    let state = ProviderRetryState {
        attempt: 2,
        max_attempts: 16,
        retry_at: now + std::time::Duration::from_millis(6_600),
        failure: "HTTP 429: rate limited".to_string(),
    };
    let summary = state.summary(now);
    assert_eq!(summary, "retry 1/15 · next in 6.6s");

    let running_state = ProviderRetryState {
        attempt: 4,
        max_attempts: 16,
        retry_at: now - std::time::Duration::from_millis(1_200),
        failure: "HTTP 503: overloaded".to_string(),
    };
    let running_summary = running_state.summary(now);
    assert_eq!(running_summary, "retry 3/15 · running · 1.2s");
}

#[test]
fn activity_modal_renders_provider_retry_failure() {
    let now = std::time::Instant::now();
    let retry = ProviderRetryState {
        attempt: 3,
        max_attempts: 30,
        retry_at: now + std::time::Duration::from_millis(4_000),
        failure: "HTTP 429: rate limit exceeded".to_string(),
    };
    let mut grid = neenee_tui_engine::Grid::new(80, 24);
    let mut frame = neenee_tui_engine::Frame::new(&mut grid);
    let mut scroll = 0;
    let theme = Theme::default();
    let rect = crate::overlays::draw_activity_modal(
        &mut frame,
        crate::overlays::ActivityModalView {
            active_tab: crate::modal::ActivityTab::Activity,
            todos: None,
            user_prompt: Some("Fix issue in parser"),
            round_count: 1,
            current_turn: 1,
            current_model: "claude-sonnet",
            round_started_at: Some(now),
            activity: "waiting to retry",
            provider_retry: Some(&retry),
        },
        &mut scroll,
        &theme,
        &crate::model::selection::SelectionState::None,
        &mut crate::model::layout::LayoutMap::new(),
    );
    assert!(rect.width > 0 && rect.height > 0);
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
    let focus: Vec<crate::app::ZoomFrame> = Vec::new();
    let resolved = event_loop::resolve_focused_mut(&mut messages, &focus, 2);
    assert_eq!(resolved.map(|m| m.raw.clone()).as_deref(), Some("ok"));
}

#[test]
fn resolve_focused_mut_indexes_children_when_focused() {
    let mut messages = conversation_with_envoys();
    let focus = vec![crate::app::ZoomFrame {
        call_id: "task_b".to_string(),
        saved_scroll: crate::app::ScrollSnapshot::default(),
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
    let focus = vec![crate::app::ZoomFrame {
        call_id: "task_a".to_string(),
        saved_scroll: crate::app::ScrollSnapshot::default(),
    }];
    let total = focused_messages_mut(&mut messages, &focus).count();
    assert_eq!(total, 1);
    let tool_steps = focused_messages_mut(&mut messages, &focus)
        .filter(|m| m.is_tool_step())
        .count();
    assert_eq!(tool_steps, 0);

    // Root view: 4 messages total, 2 of which are tool steps.
    let focus: Vec<crate::app::ZoomFrame> = Vec::new();
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
    assert_eq!(resolved_slash_command_len("/models", &[]), Some(7));
}

#[test]
fn resolved_slash_len_covers_only_the_command_token_not_args() {
    // `/sessions abc` — only `/sessions` (9 bytes) is the resolved command;
    // the argument tail is excluded so the accent stops at the token.
    assert_eq!(resolved_slash_command_len("/sessions abc", &[]), Some(9));
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
    // Trigger words steer to a command but are NOT commands themselves, so
    // they never earn the resolved-command accent.
    assert_eq!(resolved_slash_command_len("/clear", &[]), None);
    assert_eq!(resolved_slash_command_len("/reset", &[]), None);
    assert_eq!(resolved_slash_command_len("/continue", &[]), None);
    // Plain prose and `@` mentions never highlight.
    assert_eq!(resolved_slash_command_len("hello", &[]), None);
    assert_eq!(resolved_slash_command_len("@src/main.rs", &[]), None);
}

#[test]
fn completions_trigger_word_pins_suggestion_on_top() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/clear".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    assert_eq!(app.completion_kind(), CompletionKind::Slash);
    let first = completions.first().expect("a suggestion row is present");
    assert_eq!(first.label, "/new");
    assert!(
        !first.description.is_empty(),
        "the suggestion must explain why the user is being steered"
    );
    // Accepting rewrites the whole input to the target command.
    assert_eq!(first.replace_start, 0);
    assert_eq!(first.replace_end, app.input.len());
    // No built-in starts with `/clear`, so the suggestion is the only row.
    assert_eq!(completions.len(), 1);
}

/// The anchor pass is what makes "popup visible ⇒ first row selected" true:
/// with no prior highlight it seeds `Some(0)`, so the band, the details
/// flyout, and a plain Enter/Tab all land on the first candidate without
/// any prior ↓.
#[test]
fn anchor_seeds_the_first_candidate_when_the_menu_opens() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/se".to_string();
    app.cursor_position = app.input.chars().count();
    assert!(app.suggestion_index.is_none());
    let completions = app.completions();
    assert!(!completions.is_empty(), "`/se` should have candidates");
    app.anchor_completion_selection(&completions);
    assert_eq!(
        app.suggestion_index,
        Some(0),
        "a freshly opened menu must start highlighted on its first row"
    );
}

/// A visible menu keeps exactly one highlighted row even when the candidate
/// list shrinks under a stale index: the highlight clamps into range rather
/// than pointing past the list (which would render no band and no flyout).
#[test]
fn anchor_clamps_a_stale_highlight_into_range() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/se".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let count = completions.len();
    // Simulate a stale index from a wider list (e.g. a refine filtered
    // candidates away between keystrokes).
    app.suggestion_index = Some(count + 5);
    app.anchor_completion_selection(&completions);
    assert_eq!(
        app.suggestion_index,
        Some(count - 1),
        "an out-of-range highlight must clamp to the last candidate"
    );
}

/// A resolved composer (the text exactly equals a candidate) renders no
/// menu, so the anchor must clear the highlight — otherwise a lingering
/// index would keep Enter/Tab committing a command the user cannot see.
#[test]
fn anchor_clears_the_highlight_when_no_menu_is_rendered() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/sessions".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    // `/sessions` is a real command: its exact match is the composer text.
    assert!(
        completions
            .iter()
            .any(|c| c.label == app.input && c.replace_end == app.input.len()),
        "`/sessions` should be among its own candidates"
    );
    app.suggestion_index = Some(0);
    app.anchor_completion_selection(&completions);
    assert_eq!(
        app.suggestion_index, None,
        "no rendered menu must mean no highlight"
    );
}

/// Tab's re-open gesture keys off trigger text that survived Esc: a partial
/// slash command qualifies, a resolved exact command does not (its popup is
/// hidden on purpose), and plain prose never does.
#[test]
fn completion_trigger_text_present_matches_the_composer_state() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/se".to_string();
    app.cursor_position = app.input.chars().count();
    assert!(app.completion_trigger_text_present());
    app.input = "/sessions".to_string();
    app.cursor_position = app.input.chars().count();
    assert!(
        !app.completion_trigger_text_present(),
        "a resolved exact command must not offer a re-open"
    );
    app.input = "plain prose".to_string();
    app.cursor_position = app.input.chars().count();
    assert!(!app.completion_trigger_text_present());
}

/// Regression for the wiring itself: the event loop feeds the input layer the
/// **unsuppressed** `completion_kind` (the dismissal latch travels as its own
/// `completion_dismissed` flag). Suppressing the kind while the latch is set
/// would make Tab's re-open branch unreachable — `completion_kind` would be
/// `None` exactly when the user pressed Tab after Esc — so this pins the
/// contract end to end through the real mapper.
#[test]
fn tab_after_esc_reopens_through_the_event_loop_context_shape() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/se".to_string();
    app.cursor_position = app.input.chars().count();

    // Esc's arm: latch the dismissal, drop the highlight.
    app.suggestion_index = None;
    app.completion_dismissed = true;

    // Build the context exactly as `run_app_loop` does: the candidate list
    // is suppressed (empty) while the latch is set, but the classification
    // is NOT — that distinction is what makes the re-open gesture visible
    // to the input layer.
    let suppress_completions = app.completion_dismissed;
    let completions = if suppress_completions {
        Vec::new()
    } else {
        app.completions()
    };
    let completion_kind = app.completion_kind();
    let has_trigger_text = app.completion_trigger_text_present();
    let mut input = app.input.clone();
    let mut cursor = app.cursor_position;
    let mut drag = crate::model::selection::SelectionDrag::default();
    let action = crate::input::process_event(
        crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Tab,
            crossterm::event::KeyModifiers::NONE,
        )),
        &mut input,
        &mut cursor,
        crate::input::InputContext {
            active_modal: crate::Modal::None,
            completion_kind,
            suggestion_count: completions.len(),
            has_exact_suggestion: false,
            suggestion_index: app.suggestion_index,
            completion_dismissed: app.completion_dismissed,
            has_trigger_text,
            ..Default::default()
        },
        &mut drag,
    );
    assert_eq!(
        action,
        crate::input::InputAction::ReopenCompletion,
        "Tab after Esc must re-open the dismissed slash menu"
    );
    // And the ReopenCompletion arm's state change restores a selected menu
    // once the loop's post-dispatch anchor runs.
    app.completion_dismissed = false;
    let completions = app.completions();
    app.anchor_completion_selection(&completions);
    assert_eq!(app.suggestion_index, Some(0));
    assert!(!app.completion_dismissed);
}

/// The Esc → Tab round trip, driven through the same `App` state the action
/// arms mutate (`CloseCompletion` latches the dismissal + clears the
/// highlight; `ReopenCompletion` drops the latch; the loop's anchor pass
/// re-seeds the highlight). After the round trip the menu must be visible
/// **and** carry a highlighted row again — the state the renderer needs to
/// paint the band and the details flyout.
#[test]
fn esc_then_tab_round_trip_restores_a_highlighted_menu() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/se".to_string();
    app.cursor_position = app.input.chars().count();

    // Frame 1: menu opens, anchor seeds the first candidate.
    let completions = app.completions();
    app.anchor_completion_selection(&completions);
    assert_eq!(app.suggestion_index, Some(0));
    assert!(!app.completion_dismissed);

    // Esc (CloseCompletion arm): popup hidden, highlight dropped.
    app.suggestion_index = None;
    app.completion_dismissed = true;
    assert!(app.completion_trigger_text_present());

    // Tab (ReopenCompletion arm): latch dropped, then the loop's post-
    // dispatch anchor re-derives candidates and re-seeds the highlight.
    app.completion_dismissed = false;
    let completions = app.completions();
    app.anchor_completion_selection(&completions);
    assert_eq!(
        app.suggestion_index,
        Some(0),
        "the reopened menu must land already selected"
    );
}

#[test]
fn completions_trigger_word_suggestion_precedes_prefix_matches() {
    // A trigger that also prefixes a real command must still pin its
    // suggestion first. `/re` is a shared prefix, not a trigger: normal
    // prefix completion with no suggestion. `/reset` is the full trigger.
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/re".to_string();
    app.cursor_position = app.input.chars().count();
    assert!(
        !app.completions().iter().any(|c| c.label == "/new"),
        "a partial trigger is prose-in-progress, not a suggestion"
    );

    app.input = "/reset".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    assert_eq!(
        completions.first().map(|c| c.label.as_str()),
        Some("/new"),
        "the suggestion pins on top even if a real command shares the prefix"
    );
}

#[test]
fn completions_continue_trigger_suggests_sessions() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/continue".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    assert_eq!(
        completions.first().map(|c| c.label.as_str()),
        Some("/sessions")
    );
}

#[test]
fn completions_settings_triggers_and_subcommands() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);

    // Typing /preferences steers to /settings
    app.input = "/preferences".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    assert_eq!(
        completions.first().map(|c| c.label.as_str()),
        Some("/settings")
    );

    // Typing /theme steers to /settings
    app.input = "/theme".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    assert_eq!(
        completions.first().map(|c| c.label.as_str()),
        Some("/settings")
    );

    // Typing /settings suggests /settings reload
    app.input = "/settings ".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, vec!["/settings reload"]);

    // Legacy /config <space> also suggests /settings reload
    app.input = "/config ".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, vec!["/settings reload"]);
}

#[test]
fn completions_subcommand_argument_never_triggers_suggestion() {
    // `clear` is a trigger word at the top level, but as a `/permissions`
    // argument it is a real subcommand and must not be steered away.
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/permissions clear".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert_eq!(labels, vec!["/permissions clear"]);
}

#[test]
fn completions_intent_keywords_suggest_canonical_command() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/timer".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    assert_eq!(app.completion_kind(), CompletionKind::Slash);
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert!(
        labels.contains(&"/schedule"),
        "typing /timer should suggest /schedule"
    );

    // Check intent suggestion kind and doc
    let schedule_cand = completions.iter().find(|c| c.label == "/schedule").unwrap();
    assert!(matches!(
        schedule_cand.kind,
        crate::completion::CompletionItemKind::IntentSuggestion { .. }
    ));
    assert!(schedule_cand.doc.is_some());
    let doc = schedule_cand.doc.as_ref().unwrap();
    assert_eq!(doc.name, "/schedule");
    assert_eq!(doc.category.as_deref(), Some("Automation"));

    // /switch suggests /models and /principal
    app.input = "/switch".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let labels: Vec<&str> = completions.iter().map(|c| c.label.as_str()).collect();
    assert!(labels.contains(&"/models"));
    assert!(labels.contains(&"/principal"));
}

#[test]
fn completions_candidates_carry_rich_doc_for_inspector() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/models".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let models_cand = completions
        .iter()
        .find(|c| c.label == "/models")
        .expect("find /models");
    assert!(models_cand.doc.is_some());
    let doc = models_cand.doc.as_ref().unwrap();
    assert_eq!(doc.name, "/models");
    assert!(!doc.description.is_empty());
    assert!(!doc.usage.is_empty());
    assert!(!doc.examples.is_empty());
    assert_eq!(doc.category.as_deref(), Some("Model"));
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
    #[cfg(windows)]
    assert!(is_explicit_path_prefix(r"C:\Users\runner\notes"));
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
    // A native absolute query resolves to its parent with the final component
    // as prefix, independent of cwd (drive/root semantics vary by platform).
    let tmp = tempfile::tempdir().expect("tempdir");
    let expected = tmp.path().canonicalize().unwrap();
    let query = tmp.path().join("h").display().to_string();
    let (dir, prefix) = resolve_explicit_dir(&query, tmp.path()).expect("resolved");
    assert_eq!(dir, expected);
    assert_eq!(prefix, "h");
}

#[cfg(windows)]
#[test]
fn resolve_explicit_dir_accepts_native_windows_separators() {
    let cwd = std::path::Path::new(r"D:\workspace\neenee");
    let (dir, prefix) =
        resolve_explicit_dir(r"C:\Users\runner\notes\draft", cwd).expect("resolved");
    assert_eq!(dir, std::path::PathBuf::from(r"C:\Users\runner\notes"));
    assert_eq!(prefix, "draft");
}

#[test]
fn enumerate_explicit_path_completion_expands_to_absolute() {
    // `@../` from a temp project lists the parent directory's children as
    // absolute paths. The candidates are terminal (PathExplicit): accepting
    // one drops the `@` and splices the absolute path — the core of req 1.
    use crate::completion::CompletionItemKind;
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
        neenee_contracts::HistoryEntry::new(
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
    use crate::Modal;
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
/// Test constructor for cross-module relay tests (the event loop's
/// input-selection tests): a default `App` in a temp dir, with no files.
/// The returned temp dir must be kept alive by the caller for the app's
/// lifetime.
#[cfg(test)]
pub(crate) fn new_app_for_relay_tests() -> App {
    let (app, _tmp) = app_in_tempdir(&[], &[]);
    // Leak the temp dir intentionally: these tests only touch in-memory
    // state, and returning `(App, TempDir)` would force every caller to
    // juggle the guard. The OS reclaims the empty dir at process exit.
    std::mem::forget(_tmp);
    app
}

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
        parent_status: neenee_contracts::ParentStatus::Idle,
        btw_list: Vec::new(),
        session_chrome: std::collections::HashMap::new(),
        saved_primary_chrome: None,
        btw_scroll: 0,
        btw_modal_follow: true,
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
        token_report: None,
        context_tokens: None,
        token_report_scroll: 0,
        token_report_detail: false,
        usage_stats: None,
        usage_stats_scroll: 0,
        todos_rect: None,
        queue_rect: None,
        modal_rect: None,
        modal_body_height: 0,
        sticky_summary_line: None,
        pin_summary_line: None,
        scroll_settle_pending: false,
        focus_stack: Vec::new(),
        tx: new_test_channel(),
        should_quit: Arc::new(AtomicBool::new(false)),
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
        config_focus: crate::overlays::ConfigFocus::Categories,
        config_category: 0,
        config_detail_index: 0,
        config_detail_scroll: 0,
        config_custom_editing: false,
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
        path_scan_slot: None,
        path_scan_wake: None,
        session_context: None,
        loop_status: LoopStatus::Idle,
        harness_retry_pending: false,
        activity_status: String::new(),
        provider_retry: None,
        autopilot: false,
        todos: None,
        round_count: 0,
        current_turn: 0,
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
        host_sessions: Vec::new(),
        host_scroll: 0,
        host_modal_follow: true,
        host_focus: crate::overlays::DashboardFocus::Detail,
        host_detail_scroll: 0,
        host_preview: None,
        host_preview_scroll: 0,
        host_prompting: false,
        host_prompt_new: false,
        switch_to_target: None,
        startup_overlay: crate::StartupOverlay::None,
        permission_confirm_always: false,
        permission_show_details: false,
        permission_scroll: 0,
        permission_max_scroll: 0,
        input_history: Vec::new(),
        history_index: None,
        history_draft: String::new(),
        history_draft_images: Vec::new(),
        history_draft_text_pastes: Vec::new(),
        queue_pointer: None,
        queue_pointer_draft: String::new(),
        queue_pointer_draft_images: Vec::new(),
        queue_pointer_draft_text_pastes: Vec::new(),
        history_attachments: std::collections::HashMap::new(),
        history_attachments_order: std::collections::VecDeque::new(),
        session_history_backfill: Vec::new(),
        session_history_backfill_cursor: 0,
        history_clear_confirm: false,
        input_history_dedup: true,
        input_history_record_commands: false,
        // Tests must not touch the developer's real `history.json`: with the
        // guard off, `record_input_history` writes (and the clear action
        // truncates) `$XDG_STATE_HOME/neenee/history.json` — a leak that
        // polluted the file with synthetic `prompt N` rows.
        input_history_persist: false,
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
        modal_hit_map: crate::model::layout::ModalHitMap::new(),
        hovered_step: None,
        transcript_layout: crate::view::layout::Strategy::default(),
        color_scheme: "zen".to_string(),
        custom_color_scheme: neenee_contracts::ColorSchemeConfig::default(),
        custom_color_draft: neenee_contracts::ColorSchemeConfig::default(),
        click_outside_dismiss: false,
        expand_auto_scroll: false,
        focused_target: None,
        copy_toast_until: None,
        copy_toast_message: String::new(),
        copy_toast_failed: false,
        notice_toast_until: None,
        notice_toast_message: String::new(),
        notice_toast_severity: NoticeSeverity::Info,
        ctrl_c_armed_until: None,
        esc_armed_ticks: 0,
        spinner_epoch: std::time::Instant::now(),
        carousel_epoch: std::time::Instant::now(),
        effort_ignition_epoch: None,
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
        oauth_selected_item: 0,
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
        provider_delete_focus: crate::ProviderDeleteChoice::default(),
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

/// The official OpenAI template (Name / Token) seeds OpenAI text models directly.
fn openai_template() -> &'static crate::providers::ProviderTemplate {
    crate::PROVIDER_TEMPLATES
        .iter()
        .find(|t| t.id == "openai")
        .expect("openai template")
}

/// The Anthropic template (Name / Base URL / Token), which seeds the Claude
/// family and exposes no Model field.
fn anthropic_template() -> &'static crate::providers::ProviderTemplate {
    crate::PROVIDER_TEMPLATES
        .iter()
        .find(|t| t.id == "anthropic")
        .expect("anthropic template")
}

/// The Google Antigravity template — a Google-native subscription with seeded models.
fn antigravity_template() -> &'static crate::providers::ProviderTemplate {
    crate::PROVIDER_TEMPLATES
        .iter()
        .find(|t| t.id == "antigravity-oauth")
        .expect("antigravity template")
}

#[test]
fn add_provider_row_opens_the_template_chooser() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_provider_template_chooser();
    assert!(app.active_modal == Modal::ProviderTemplate);
    assert_eq!(app.template_choice, 0);
    // `↑/↓` wrap across the template list.
    let n = crate::PROVIDER_TEMPLATES.len();
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
    assert!(!app.custom_fields.contains(&crate::CustomField::Model));
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
    assert!(!app.custom_fields.contains(&crate::CustomField::Model));
}

#[test]
fn antigravity_template_prefills_url_and_seeds_relay_models() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_custom_provider_editor(antigravity_template());
    assert_eq!(app.custom_protocol_wire, "google");
    assert_eq!(
        app.custom_base_url,
        "https://daily-cloudcode-pa.googleapis.com"
    );
    assert_eq!(
        app.custom_models,
        neenee_providers::ANTIGRAVITY_OAUTH_MODELS
    );
    // No free-text Model field — the closed Gemini family is the seed.
    assert!(!app.custom_fields.contains(&crate::CustomField::Model));
    // Name and Token still start empty (the user supplies them).
    assert!(app.custom_name.is_empty());
    assert!(app.custom_token.is_empty());
}

#[test]
fn custom_provider_field_cycle_wraps_and_swaps_buffers() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let custom_template = crate::PROVIDER_TEMPLATES
        .iter()
        .find(|t| t.id == "custom-openai")
        .expect("custom-openai template");
    app.open_custom_provider_editor(custom_template);
    // Fields: Name(0) / Base URL(1) / Token(2) / Model(3).
    let n = app.custom_fields.len() as u8;
    // Type a name, then advance: the name is stashed and the Base URL field
    // loads its (empty) buffer.
    app.input = "My Relay".to_string();
    app.cycle_custom_field(true);
    assert_eq!(app.custom_field, 1);
    assert_eq!(app.custom_name, "My Relay");
    assert!(app.input.is_empty(), "Base URL buffer is empty");
    // Wrap backward from Name (0) to the last field (Model).
    app.cycle_custom_field(false); // 1 -> 0
    assert_eq!(app.custom_field, 0);
    assert_eq!(app.input, "My Relay", "Name buffer reloads into the line");
    app.cycle_custom_field(false); // 0 -> n-1 (wrap)
    assert_eq!(app.custom_field, n - 1);
}

#[test]
fn custom_provider_model_filter_commits_and_offers_custom_id() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    // The real generic template: it exposes the Model field and seeds no
    // models, so the flow under test is exactly what ships.
    let free_model_template = crate::providers::PROVIDER_TEMPLATES
        .iter()
        .find(|t| t.id == "custom-openai")
        .expect("custom-openai template");
    app.open_custom_provider_editor(free_model_template);
    // The default model is the first candidate of the template's (OpenAI) protocol.
    assert!(
        app.custom_model_candidates()
            .contains(&app.custom_model.as_str())
    );
    // Focus the Model filter field (the last field) and type a known model.
    app.custom_field = app.custom_fields.len() as u8 - 1;
    assert_eq!(app.current_custom_field(), Some(crate::CustomField::Model));
    app.load_custom_field();
    app.input = "gpt-4o".to_string();
    app.on_custom_filter_changed();
    assert_eq!(app.custom_model, "gpt-4o");
    // A query matching nothing in the registry is still offered as a custom id.
    app.input = "my-private-model".to_string();
    app.on_custom_filter_changed();
    assert_eq!(app.custom_model, "my-private-model");
    // A query with spaces is automatically sanitized to use hyphens.
    app.input = "my custom private model".to_string();
    app.on_custom_filter_changed();
    assert_eq!(app.custom_model, "my-custom-private-model");
}

#[test]
fn custom_openai_template_submits_with_the_typed_model_and_url() {
    // End-to-end create flow for the generic template: fields Name/Base
    // URL/Token/Model, and the submitted `AddProvider` carries the typed
    // model id (not a seeded list) plus the relay endpoint.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let template = crate::providers::PROVIDER_TEMPLATES
        .iter()
        .find(|t| t.id == "custom-openai")
        .expect("custom-openai template");
    // The editor's visible fields include the Model filter field.
    assert_eq!(
        template.fields(),
        vec![
            crate::CustomField::Name,
            crate::CustomField::BaseUrl,
            crate::CustomField::Token,
            crate::CustomField::Model,
        ]
    );
    app.open_custom_provider_editor(template);
    app.custom_name = "WeChat".to_string();
    app.custom_base_url = "https://chatapi.weixin.qq.com/openai/v1/chat/completions".to_string();
    app.custom_token = "tok".to_string();
    // Focus the Model field, type the cased id, and commit it via the
    // suggestion commit (a cased id is offered as a custom value).
    app.custom_field = 3;
    app.load_custom_field();
    app.input = "GLM-5.2".to_string();
    app.on_custom_filter_changed();
    assert_eq!(app.custom_model, "GLM-5.2");

    // Submit: the request must carry the single typed model as the seeded
    // list, the template id, and the endpoint — a case-sensitive id travels
    // verbatim (the WeChat endpoint 400s on the lowercase spelling).
    app.stash_custom_field();
    let payload = serde_json::json!({
        "name": app.custom_name,
        "protocol": app.custom_protocol_wire,
        "base_url": app.custom_base_url,
        "models": [app.custom_model],
        "template_id": template.id,
    });
    assert_eq!(payload["models"][0], "GLM-5.2");
    assert_eq!(payload["template_id"], "custom-openai");
    assert_eq!(payload["protocol"], "openai");
    assert_eq!(
        payload["base_url"],
        "https://chatapi.weixin.qq.com/openai/v1/chat/completions"
    );
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
    let row = |id: &str| neenee_contracts::ProviderPickerRow {
        id: id.to_string(),
        name: id.to_string(),
        model: "m".to_string(),
        models: vec!["m".to_string()],
        model_info: Vec::new(),
        builtin: true,
        protocol: String::new(),
        base_url: String::new(),
        key_ready: true,
        preset_id: String::new(),
        client_identity: Default::default(),
        last_used_ms: None,
        auth: Default::default(),
    };
    app.provider_picker = neenee_contracts::ProviderPickerSnapshot {
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
    let custom = |id: &str| neenee_contracts::ProviderPickerRow {
        id: id.to_string(),
        name: id.to_string(),
        model: "m".to_string(),
        models: vec!["m".to_string()],
        model_info: Vec::new(),
        builtin: false,
        protocol: String::new(),
        base_url: String::new(),
        key_ready: true,
        preset_id: String::new(),
        client_identity: Default::default(),
        last_used_ms: None,
        auth: Default::default(),
    };
    app.provider_picker = neenee_contracts::ProviderPickerSnapshot {
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
        crate::ProviderDeleteChoice::Cancel,
        "confirm overlay defaults to Cancel focus"
    );
}

/// Built-in providers are not deletable: `Shift+D` on one is a no-op (the
/// overlay must not open, nothing staged).
#[test]
fn delete_provider_ignores_builtin() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.active_modal = Modal::Connections;
    let builtin = |id: &str| neenee_contracts::ProviderPickerRow {
        id: id.to_string(),
        name: id.to_string(),
        model: "m".to_string(),
        models: vec!["m".to_string()],
        model_info: Vec::new(),
        builtin: true,
        protocol: String::new(),
        base_url: String::new(),
        key_ready: true,
        preset_id: String::new(),
        client_identity: Default::default(),
        last_used_ms: None,
        auth: Default::default(),
    };
    app.provider_picker = neenee_contracts::ProviderPickerSnapshot {
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
    app.provider_delete_focus = crate::ProviderDeleteChoice::Delete;

    app.cancel_provider_delete();

    assert!(
        app.pending_provider_delete.is_none(),
        "cancel clears the staged id"
    );
    assert_eq!(
        app.provider_delete_focus,
        crate::ProviderDeleteChoice::Cancel,
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
    // Off-runtime (no tokio handle) the scan runs inline and lands in the
    // cache on the first `@`; a second `@` reuses it and requests nothing.
    // The async production path is the same state machine with a slot in
    // between (see path_scan) — its landing is covered by the harvest
    // assertions below on the slot itself.
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    assert!(app.path_scan_cache.is_none());
    app.input = "@".to_string();
    app.cursor_position = 1;
    let _ = app.completions();
    let first_len = app
        .path_scan_cache
        .as_ref()
        .expect("scan populated")
        .as_ref()
        .expect("some")
        .entries
        .len();
    assert!(first_len > 0, "the temp dir had at least Cargo.toml");
    // A second call must not re-request: cache intact, no slot installed.
    app.input = "@Ca".to_string();
    app.cursor_position = app.input.chars().count();
    let _ = app.completions();
    assert!(app.path_scan_slot.is_none(), "no second scan request");
    let second_len = app
        .path_scan_cache
        .as_ref()
        .expect("scan still populated")
        .as_ref()
        .expect("some")
        .entries
        .len();
    assert_eq!(first_len, second_len);
}

#[test]
fn path_scan_slot_round_trip() {
    // The async hand-off itself: store then harvest yields the scan exactly
    // once, and a second harvest is empty (no duplication into the cache).
    let slot = crate::completion::PathScanSlot::default();
    assert!(slot.harvest().is_none());
    let scan = crate::completion::PathScan {
        entries: vec!["a.txt".to_string()],
    };
    slot.store(scan);
    let got = slot.harvest().expect("harvest yields the stored scan");
    assert_eq!(got.entries, vec!["a.txt".to_string()]);
    assert!(slot.harvest().is_none(), "harvest is take-once");
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

use crate::app::{QueuedDispatch, QueuedDispatchState, RecallQueued};

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
        images: vec![neenee_contracts::ImagePart {
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

// ── Queue pointer navigation (ADR-0126) ─────────────────────────────────────

#[test]
fn queue_pointer_walks_without_removing_items() {
    // The pointer is non-destructive: three ↑ presses walk c → b → a while
    // the queue keeps all three items in order.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.pending_dispatch
        .push_back(queued_dispatch("a", "session-a", "msg a"));
    app.pending_dispatch
        .push_back(queued_dispatch("b", "session-b", "msg b other session"));
    app.pending_dispatch
        .push_back(queued_dispatch("c", "session-a", "msg c"));

    // First ↑ arms at the newest session-a item ("c") and stashes the draft.
    app.input = "my draft".to_string();
    assert!(app.queue_pointer_prev("session-a"));
    assert_eq!(app.input, "msg c");
    assert_eq!(app.queue_pointer.as_deref(), Some("c"));
    assert_eq!(app.pending_count("session-a"), 2, "nothing left the queue");

    // Second ↑ → "a" (the only older item); third ↑ clamps there.
    assert!(app.queue_pointer_prev("session-a"));
    assert_eq!(app.input, "msg a");
    assert!(app.queue_pointer_prev("session-a"));
    assert_eq!(app.input, "msg a", "clamped at the oldest item");
    assert_eq!(app.pending_count("session-a"), 2);
}

#[test]
fn queue_pointer_down_restores_the_draft() {
    // ↓ walks back toward newer items and, past the newest, dissolves the
    // pointer and restores the stashed draft — the same exit as the history
    // pointer.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.pending_dispatch
        .push_back(queued_dispatch("a", "session-a", "msg a"));
    app.pending_dispatch
        .push_back(queued_dispatch("c", "session-a", "msg c"));

    app.input = "my draft".to_string();
    assert!(app.queue_pointer_prev("session-a")); // → c
    assert!(app.queue_pointer_prev("session-a")); // → a
    assert!(app.queue_pointer_next("session-a")); // → c
    assert_eq!(app.input, "msg c");
    // Past the newest: the press dissolves the pointer (consumed) and
    // restores the stashed draft.
    assert!(
        app.queue_pointer_next("session-a"),
        "dissolve consumes the key"
    );
    assert!(app.queue_pointer.is_none());
    assert_eq!(app.input, "my draft");
    // An unarmed ↓ is inert (the caller falls through to history).
    assert!(!app.queue_pointer_next("session-a"));
}

#[test]
fn queue_pointer_commit_edits_in_place() {
    // Enter writes the edited content back into the pointed-at item — in
    // place. Queue a=α, b=β, c=γ; walk to a; edit to δ; Enter → the queue is
    // δ, β, γ (same length, same order, same slot) — never β, γ, δ and never
    // a duplicate.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.pending_dispatch
        .push_back(queued_dispatch("a", "session-a", "alpha"));
    app.pending_dispatch
        .push_back(queued_dispatch("b", "session-a", "beta"));
    app.pending_dispatch
        .push_back(queued_dispatch("c", "session-a", "gamma"));

    assert!(app.queue_pointer_prev("session-a")); // → c
    assert!(app.queue_pointer_prev("session-a")); // → b
    assert!(app.queue_pointer_prev("session-a")); // → a
    app.input = "delta".to_string();
    assert!(app.commit_queue_pointer("session-a").is_some());

    let texts: Vec<&str> = app
        .pending_dispatch
        .iter()
        .filter(|d| d.session_id == "session-a")
        .map(|d| d.text.as_str())
        .collect();
    assert_eq!(texts, vec!["delta", "beta", "gamma"], "edit lands in place");
    assert!(app.queue_pointer.is_none(), "commit dissolves the pointer");
    assert_eq!(app.pending_count("session-a"), 3, "queue length unchanged");
}

#[test]
fn queue_pointer_vanished_target_sends_as_new_message() {
    // If the pointed-at item shipped while the user was editing, the pointer
    // is empty: the composer keeps the edit and Enter falls through to an
    // ordinary send (the caller's fresh-message path).
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.pending_dispatch
        .push_back(queued_dispatch("a", "session-a", "alpha"));
    assert!(app.queue_pointer_prev("session-a"));
    app.input = "edited".to_string();

    // The item leaves the queue (shipped) behind the user's back…
    app.remove_dispatch("session-a", "a");
    // …so the commit dissolves the pointer but preserves the edit.
    assert!(app.commit_queue_pointer("session-a").is_none());
    assert!(app.queue_pointer.is_none());
    assert_eq!(app.input, "edited", "the edit must survive the race");
}

/// ADR-0126 behavior lock: `Ctrl+O` stages the insert as a **transcript
/// entry** the moment it is sent — queued delivery state, `↳ insert` origin,
/// a correlation `insert_id` — and the outbox stays untouched. The staged
/// images ship with the request (they were silently dropped before).
#[tokio::test]
async fn insert_into_round_stages_a_transcript_entry_and_ships_images() {
    use crate::model::document::{DeliveryStatus, UserMessageOrigin};

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();
    app.running_sessions.insert("session-a".to_string());
    app.input = "steer this".to_string();
    let image = neenee_contracts::ImagePart {
        mime: "image/png".to_string(),
        data: "abc".to_string(),
    };
    app.pending_images = vec![image];

    // Replace the sink channel with a live receiver so the request the
    // handler sends can be inspected.
    let (tx, mut requests) = mpsc::unbounded_channel();
    app.tx = tx;
    super::event_loop::handle_insert_into_round(&mut app, &runtime, "session-a").await;

    // The transcript owns the insert immediately, in the pending state.
    let messages = runtime.messages.read().await.clone();
    let entry = messages.last().expect("the insert entry was pushed");
    assert_eq!(entry.role, neenee_contracts::Role::User);
    assert_eq!(entry.raw, "steer this");
    assert_eq!(entry.delivery, DeliveryStatus::Queued);
    assert_eq!(entry.origin, UserMessageOrigin::Insert);
    assert!(
        entry.insert_id.is_some(),
        "the entry carries its correlation id"
    );

    // The outbox is not involved.
    assert_eq!(
        app.pending_count("session-a"),
        0,
        "an insert must never become an outbox item"
    );

    // The request ships the staged images (regression: the old path sent
    // `Vec::new()` and dropped them) and carries the same correlation id.
    let sent = requests.try_recv().expect("the insert request was sent");
    match sent {
        neenee_contracts::AgentRequest::InsertUserInput { input, .. } => {
            assert_eq!(input.images.len(), 1, "staged images must ship");
            assert_eq!(input.images[0].data, "abc");
            assert_eq!(
                entry.insert_id.as_deref(),
                Some(input.id.as_str()),
                "the entry and the request share the correlation id"
            );
        }
        other => panic!("expected InsertUserInput, got {other:?}"),
    }
}

/// ADR-0126 behavior lock: an idle `Ctrl+O` restores the composer verbatim —
/// a stray chord never eats the draft.
#[tokio::test]
async fn insert_into_round_while_idle_restores_the_draft() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();
    app.input = "my draft".to_string();
    app.pending_text_pastes = vec!["big paste".to_string()];

    super::event_loop::handle_insert_into_round(&mut app, &runtime, "session-a").await;

    assert_eq!(app.input, "my draft");
    assert_eq!(app.pending_text_pastes.len(), 1);
    assert_eq!(
        runtime.messages.read().await.len(),
        0,
        "nothing staged while idle"
    );
}

#[test]
fn recall_queued_restores_staged_images() {
    // Images staged with the queued message (Ctrl+V before pressing
    // Enter) come back alongside the text so the user can re-edit and
    // resend without losing the attachment.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let image = neenee_contracts::ImagePart {
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

    let image = neenee_contracts::ImagePart {
        mime: "image/png".to_string(),
        data: "abc".to_string(),
    };
    let chip = crate::composer_attachments::image_chip(1, 3);
    let paste = crate::composer_attachments::paste_chip(1, 2, 11);
    let text = format!("describe this {chip} then {paste}");
    app.record_input_history(text.clone(), vec![image.clone()], vec!["big paste".into()]);

    // ↑ recall: the entry is the newest row of the current session; the
    // event loop loads its text and calls restore_history_attachments.
    let session_rows = app.current_session_history();
    assert_eq!(session_rows.len(), 1);
    let orig_idx = session_rows[0];
    app.input = app.history_entry(orig_idx).expect("row").text.clone();
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

    let image = neenee_contracts::ImagePart {
        mime: "image/png".to_string(),
        data: "abc".to_string(),
    };
    app.pending_images.push(image);

    let orig_idx = app.current_session_history()[0];
    app.input = app.history_entry(orig_idx).expect("row").text.clone();
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
    let image = neenee_contracts::ImagePart {
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
            vec![neenee_contracts::ImagePart {
                mime: "image/png".to_string(),
                data: format!("img-{i}"),
            }],
            Vec::new(),
        );
    }
    assert!(
        app.history_attachments.len() <= crate::app::App::HISTORY_ATTACHMENTS_CAP,
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

/// The session-bound ↑/↓ rows come from the union of the tagged persisted
/// history **and** the transcript-derived backfill. A resumed session whose
/// prompts were typed in another client (so `history.json` never saw them)
/// still recalls them: `backfill_session_history` seeds derived rows and
/// `current_session_history` walks both stores.
///
/// Mirrors the event loop's `user_prompt_tail` extraction (kept local because
/// that one is private to the loop module).
fn prompt_tail(messages: &[TranscriptMessage]) -> Vec<(String, bool, u64)> {
    messages
        .iter()
        .filter(|m| m.role == Role::User)
        .map(|m| {
            (
                m.raw.clone(),
                m.origin == UserMessageOrigin::Chat,
                m.sent_at_ms.unwrap_or(0),
            )
        })
        .collect()
}

#[tokio::test]
async fn resumed_session_backfills_prompt_rows_from_transcript() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.current_workspace = "~/p".to_string();
    // One prompt this client genuinely recorded (simulating the live tail of
    // the resumed session typed through this TUI), and a stale prompt that
    // belongs to a different session entirely.
    app.record_input_history("live prompt".to_string(), Vec::new(), Vec::new());
    app.record_input_history("other session's prompt".to_string(), Vec::new(), Vec::new());
    app.input_history.last_mut().unwrap().session_id = Some("session-b".to_string());

    // The resumed transcript: genuine chat prompts (oldest-first, as the
    // listener rebuilds it) plus a slash command and a shell passthrough —
    // neither of which may become a recall row.
    let transcript = vec![
        TranscriptMessage::new(Role::User, "first turn").with_sent_at_ms(100),
        TranscriptMessage::new(Role::Assistant, "ok"),
        TranscriptMessage::new(Role::User, "live prompt").with_sent_at_ms(200),
        TranscriptMessage::new(Role::User, "/model").with_origin(UserMessageOrigin::Slash),
        TranscriptMessage::new(Role::User, "!ls -la").with_origin(UserMessageOrigin::Shell),
    ];
    app.backfill_session_history(&prompt_tail(&transcript), 1000);

    // Only the unseen prompt is backfilled; the already-recorded one is not
    // duplicated, and the derived rows never touch the persisted store.
    assert_eq!(
        app.session_history_backfill.len(),
        1,
        "only the unrecorded prompt is backfilled"
    );
    assert_eq!(app.session_history_backfill[0].text, "first turn");
    assert_eq!(app.input_history.len(), 2, "persisted history untouched");

    // ↑ walks the union newest-first: the live prompt (ts stamped by the
    // send), then the backfilled row.
    let rows = app.current_session_history();
    assert_eq!(rows.len(), 2, "other session's prompt is filtered out");
    assert_eq!(app.history_entry(rows[0]).unwrap().text, "live prompt");
    assert_eq!(app.history_entry(rows[1]).unwrap().text, "first turn");
    assert!(app.history_prev(&rows));
    assert_eq!(app.input, "live prompt");
    assert!(app.history_prev(&rows));
    assert_eq!(app.input, "first turn");

    // The backfill is incremental: re-running with the same transcript adds
    // nothing; appending a new turn adds exactly that row.
    app.backfill_session_history(&prompt_tail(&transcript), 1000);
    assert_eq!(app.session_history_backfill.len(), 1);
    let mut grown = transcript.clone();
    grown.push(TranscriptMessage::new(Role::User, "third turn").with_sent_at_ms(300));
    app.backfill_session_history(&prompt_tail(&grown), 1000);
    assert_eq!(app.session_history_backfill.len(), 2);
}

/// Switching the viewed session must not carry composer state across the
/// boundary: the ↑/↓ cursor, the stashed draft, staged attachments, and the
/// backfill all belong to the conversation being left.
#[tokio::test]
async fn switching_sessions_resets_navigation_and_backfill() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.record_input_history("prompt in a".to_string(), Vec::new(), Vec::new());
    // Simulate a walk into history (cursor armed) with a stashed draft and a
    // staged image, plus a backfilled row.
    app.input = "walked row".to_string();
    app.history_index = Some(0);
    app.history_draft = "half-typed draft in a".to_string();
    app.pending_images = vec![neenee_contracts::ImagePart {
        mime: "image/png".to_string(),
        data: "abc".to_string(),
    }];
    app.backfill_session_history(
        &prompt_tail(&[TranscriptMessage::new(Role::User, "resumed turn").with_sent_at_ms(1)]),
        1,
    );
    assert_eq!(app.session_history_backfill.len(), 1);

    // The event loop's per-frame transition: the id moves, the state resets.
    app.current_session_id = "session-b".to_string();
    app.on_viewed_session_changed();

    assert_eq!(
        app.history_index, None,
        "cursor does not cross the boundary"
    );
    assert!(app.history_draft.is_empty(), "draft does not leak");
    assert!(app.input.is_empty(), "composer starts clean");
    assert!(app.pending_images.is_empty(), "attachments do not leak");
    assert!(
        app.session_history_backfill.is_empty(),
        "backfill is rebuilt per conversation"
    );
    assert_eq!(app.session_history_backfill_cursor, 0);

    // ↑ in the new session recalls only that session's rows — here none.
    let rows = app.current_session_history();
    assert!(rows.is_empty(), "session-b has no recallable rows yet");
    assert!(!app.history_prev(&rows), "↑ is a no-op with no rows");
}

/// The ↑/↓ rows follow the **live** session id, not the id the client started
/// with: `current_session_id` is what stamps new entries, so a prompt sent
/// after a mid-run `/session open` is tagged with the switched-to session.
/// (The wiring this guards — the listener updating `UiRuntime::live_session_id`
/// from `ConversationCleared`/`ConversationReplaced` — lives in the event
/// loop; here the contract is that stamping and recall agree on one id.)
#[tokio::test]
async fn history_rows_are_scoped_by_the_live_session_id() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    // The client started attached to session-old…
    app.current_session_id = "session-old".to_string();
    app.record_input_history(
        "typed before the switch".to_string(),
        Vec::new(),
        Vec::new(),
    );
    // …then `/session open` repointed the harness and the listener tracked it.
    app.current_session_id = "session-new".to_string();
    app.on_viewed_session_changed();
    app.record_input_history("typed after the switch".to_string(), Vec::new(), Vec::new());

    let texts: Vec<&str> = app
        .current_session_history()
        .into_iter()
        .filter_map(|i| app.history_entry(i).map(|e| e.text.as_str()))
        .collect();
    assert_eq!(texts, vec!["typed after the switch"]);
}

/// `/command` invocations are not prompt history: by default they are skipped
/// entirely (`[input_history] record_commands = false`).
#[tokio::test]
async fn record_input_history_skips_slash_commands_by_default() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.record_input_history("/model".to_string(), Vec::new(), Vec::new());
    app.record_input_history("/new".to_string(), Vec::new(), Vec::new());
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
        vec![neenee_contracts::ImagePart {
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

/// `App`'s test constructor keeps disk persistence off, so exercising the
/// record/clear paths must never touch the *real* `history.json` under
/// `$XDG_STATE_HOME` (regression: `record_input_history` used to merge
/// synthetic `prompt N` rows straight into the user's file, and the clear
/// action truncated it outright). The write/clear happens on a
/// `spawn_blocking` thread, so poll briefly for a stray write to land.
#[tokio::test]
async fn test_app_does_not_touch_disk_history() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    assert!(
        !app.input_history_persist,
        "test-constructed App must default to no disk persistence"
    );
    let path = neenee_persistence::config::Config::history_file_path();
    let before = std::fs::read(&path).ok();

    app.current_session_id = "session-a".to_string();
    for i in 0..5 {
        app.record_input_history(format!("prompt {i}"), Vec::new(), Vec::new());
    }
    app.clear_input_history();

    // Give any (buggy) spawned writer a moment, then assert the real history
    // file is byte-for-byte unchanged (and not newly created).
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let after = std::fs::read(&path).ok();
    assert_eq!(
        before, after,
        "the real history file at {path:?} changed while running with persistence disabled"
    );
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

    let image = neenee_contracts::ImagePart {
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

    let image = neenee_contracts::ImagePart {
        mime: "image/png".to_string(),
        data: "img".to_string(),
    };
    app.adopt_as_draft(
        "interrupted input".to_string(),
        vec![image.clone()],
        Vec::new(),
        crate::app::DraftAdoption::Replace,
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

/// The Phase-1 unsend restore is asynchronous, not a user gesture: it must
/// never clobber a draft the user is composing while the round ran. The
/// interrupted prompt stays recoverable via the recorded history instead.
#[tokio::test]
async fn unsent_restore_preserves_in_progress_draft() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.current_session_id = "session-a".to_string();
    app.current_workspace = "~/p".to_string();

    // The user was mid-composition when the interrupt landed.
    app.input = "half-typed next message".to_string();
    app.adopt_as_draft(
        "interrupted input".to_string(),
        Vec::new(),
        Vec::new(),
        crate::app::DraftAdoption::OnlyIfIdle,
    );

    assert_eq!(
        app.input, "half-typed next message",
        "the in-progress draft wins over the async restore"
    );
    assert_eq!(app.history_index, None);

    // An idle composer still adopts as before.
    app.input.clear();
    app.adopt_as_draft(
        "interrupted input".to_string(),
        Vec::new(),
        Vec::new(),
        crate::app::DraftAdoption::OnlyIfIdle,
    );
    assert_eq!(app.input, "interrupted input");

    // Explicit gestures (queue recall) keep replacing even a busy composer.
    app.input = "another half-typed".to_string();
    app.adopt_as_draft(
        "recalled from queue".to_string(),
        Vec::new(),
        Vec::new(),
        crate::app::DraftAdoption::Replace,
    );
    assert_eq!(app.input, "recalled from queue");
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
    let image = neenee_contracts::ImagePart {
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

/// ADR-0110: dispatching a slash command must not arm the activity bar's
/// liveness surface. A command is a synchronous control-plane operation
/// outside the round state machine — no `is_responding`, no optimistic
/// `"queued"` label (which would also fabricate an `Esc Esc interrupt`
/// affordance over a dispatch that cannot be interrupted), and no running-
/// session bookkeeping. The pending command row is the command's in-flight
/// feedback (ADR-0108); this locks the bar against ever lighting for it.
#[tokio::test]
async fn slash_dispatch_never_arms_activity_state() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();
    let session = crate::SessionSource::Remote {
        session_id: "session-a".to_string(),
    };

    super::event_loop::handle_send_slash(&mut app, &runtime, &session, "/autopilot on".to_string())
        .await;

    assert!(
        !runtime
            .is_responding
            .load(std::sync::atomic::Ordering::SeqCst),
        "a command must not arm is_responding"
    );
    assert!(
        runtime.activity_status.lock().await.is_empty(),
        "a command must not paint an optimistic activity label"
    );
    assert!(
        !app.running_sessions.contains("session-a"),
        "a command must not mark the session as running"
    );
    // The in-flight feedback is the pending command row, not the bar.
    let messages = runtime.messages.read().await.clone();
    assert!(
        messages
            .last()
            .is_some_and(|message| message.is_command_result()
                && message.command_result_phase()
                    == Some(crate::model::document::CommandPhase::Pending)),
        "dispatch must push the pending command row (ADR-0108)"
    );
}

/// ADR-0110, `/serve` branch: the frontend-only interception also performs no
/// activity-state retirement, because nothing was ever armed — and the row it
/// settles completes inline rather than lingering as a promise.
#[tokio::test]
async fn serve_interception_completes_row_without_activity_retirement() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();
    let session = crate::SessionSource::Remote {
        session_id: "session-a".to_string(),
    };

    super::event_loop::handle_send_slash(&mut app, &runtime, &session, "/serve".to_string()).await;

    assert!(
        !runtime
            .is_responding
            .load(std::sync::atomic::Ordering::SeqCst)
    );
    assert!(runtime.activity_status.lock().await.is_empty());
    let messages = runtime.messages.read().await.clone();
    assert!(
        messages
            .last()
            .is_some_and(|message| message.is_command_result()
                && message.command_result_phase()
                    == Some(crate::model::document::CommandPhase::Completed)),
        "`/serve` settles its row inline with the reply"
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
fn inserts_are_transcript_owned_not_outbox_items() {
    // ADR-0126: a mid-round insert (`Ctrl+O`) never enters the outbox. It
    // becomes a transcript entry (`DeliveryStatus::Queued`) the moment it is
    // sent, so the outbox cannot dispatch, recall, delete, or reorder it —
    // and `UserInputUnavailable` hands it back by *staging a new outbox item*
    // (same id), at which point it becomes an ordinary manageable entry.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.pending_dispatch
        .push_back(queued_dispatch("w1", "session-a", "waiting one"));

    // Race loss: the harness hands the insert back. There is no outbox item
    // to flip, so the content is staged as a fresh `Waiting` item under the
    // same id.
    app.requeue_dispatch(
        "session-a",
        "steer-1",
        Some(("steer this".to_string(), Vec::new(), Vec::new())),
    );
    assert_eq!(app.pending_count("session-a"), 2);
    let order: Vec<&str> = app
        .pending_dispatch
        .iter()
        .filter(|d| d.session_id == "session-a")
        .map(|d| d.id.as_str())
        .collect();
    assert_eq!(
        order,
        vec!["w1", "steer-1"],
        "handed-back insert joins the FIFO tail"
    );

    // The handed-back item is an ordinary queue item now: FIFO dispatch pops
    // the front (w1)…
    let popped = app
        .begin_next_round_dispatch("session-a")
        .expect("a Waiting item pops first");
    assert_eq!(popped.id, "w1");
    // …and the modal can recall it like any other entry.
    assert!(
        app.recall_queued_at("session-a", 0).is_some(),
        "the handed-back insert is modal-addressable"
    );

    // Only the Dispatching leftover (w1) remains; a live insert never
    // touched the outbox at any point in its lifecycle.
    assert_eq!(app.pending_count("session-a"), 1);
    assert!(
        app.pending_dispatch
            .iter()
            .all(|d| d.state == QueuedDispatchState::Dispatching)
    );
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
        crate::clipboard::ClipboardRead::Text("abc\ndef\n".to_string()),
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
        crate::clipboard::ClipboardRead::Text("turbo".to_string()),
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
            crate::clipboard::ClipboardRead::Text("query".to_string()),
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
        crate::clipboard::ClipboardRead::Image {
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
        crate::clipboard::ClipboardRead::Text(big.clone()),
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
        crate::clipboard::ClipboardRead::Text("ignored".to_string()),
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
        crate::clipboard::ClipboardRead::Image {
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
        crate::clipboard::ClipboardRead::Image {
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
        Modal::InputInjection,
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
        Modal::Config,
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
    use crate::question_model::{QuestionAction, QuestionModel};
    use neenee_contracts::{UserQuestion, UserQuestionOption, UserQuestionRequest};

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
        anchor: crate::model::layout::SemanticCursor::new(0, 0, 0),
        head: crate::model::layout::SemanticCursor::new(0, 0, 3),
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
        Modal::InputInjection,
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
        Modal::Config,
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
    use crate::event_loop::modal_page_step;
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
/// loading any session, so the `startup_overlay` state must gate quit-on-close.
/// This pins the two state transitions the event loop relies on:
///
/// 1. The overlay defaults to `None` in an ordinary (in-session) App, so the
///    `/sessions` modal only ever dismisses on Esc.
/// 2. Selecting a session from the picker clears the overlay — once a real
///    conversation backs the view, the picker reverts to a plain transient
///    overlay. (The event loop's `OpenSelectedSession` arm does this.)
#[test]
fn startup_picker_flag_governs_sessions_modal_quit_and_resets_on_open() {
    use std::sync::atomic::Ordering;

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);

    // Default: an in-session App never treats the picker as a startup gate.
    assert_eq!(app.startup_overlay, crate::StartupOverlay::None);

    // Simulate the startup path (`neenee resume` with no id): the picker
    // opens and `startup_overlay` is armed. Closing it must quit.
    app.startup_overlay = crate::StartupOverlay::SessionsPicker;
    app.active_modal = Modal::Sessions;
    assert!(
        app.startup_overlay == crate::StartupOverlay::SessionsPicker
            && app.active_modal == Modal::Sessions
    );
    // The quit gate is `should_quit`; it is still clear until a close happens.
    assert!(!app.should_quit.load(Ordering::SeqCst));

    // Open a session from the picker: the overlay clears so a later `/sessions`
    // modal behaves as a normal transient overlay.
    app.startup_overlay = crate::StartupOverlay::None;
    app.active_modal = Modal::None;
    assert_eq!(
        app.startup_overlay,
        crate::StartupOverlay::None,
        "opening a session drops the startup gate"
    );
}

/// Helper: build a minimal picker row so a test list is readable.
fn overview_row(id: &str) -> neenee_contracts::SessionOverview {
    neenee_contracts::SessionOverview {
        parent_id: None,
        fork_kind: neenee_contracts::SessionForkKind::Trunk,
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
    use crate::primitives::{SCROLL_EDGE_MARGIN, resolve_scroll};

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
/// (`startup_overlay` armed) quit the program instead of returning to the
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
    app.startup_overlay = crate::StartupOverlay::SessionsPicker;
    app.active_modal = Modal::Sessions;
    app.session_info_detail = true;
    app.session_detail = Some(neenee_contracts::SessionDetail {
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
    } else if app.startup_overlay == crate::StartupOverlay::SessionsPicker
        && app.active_modal == Modal::Sessions
    {
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
    } else if app.startup_overlay == crate::StartupOverlay::SessionsPicker
        && app.active_modal == Modal::Sessions
    {
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
    app.startup_overlay = crate::StartupOverlay::SessionsPicker;
    app.active_modal = Modal::Sessions;
    assert!(!app.should_quit.load(Ordering::SeqCst));

    // Mirror the CtrlC arm: startup_overlay + Sessions → quit (not modal-close).
    // (Selection copy is skipped — no selection in a modal.)
    let quit = if app.startup_overlay == crate::StartupOverlay::SessionsPicker
        && app.active_modal == Modal::Sessions
    {
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

/// The disclosure-toggle scroll settle: expanding a step must latch
/// `scroll_settle_pending` so the event loop stages its next frame (measure
/// the new height) before painting the toggle's target scroll offset. That
/// staging is what keeps the un-clamped intermediate viewport off the
/// terminal — the expand/collapse flicker.
#[test]
fn disclosure_toggle_latches_scroll_settle() {
    use crate::model::document::TranscriptMessage;

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    // The settle path only runs when the auto-scroll behavior is enabled
    // (`[tui] expand_auto_scroll`, default off).
    app.expand_auto_scroll = true;
    let mut messages = vec![
        TranscriptMessage::new(Role::User, "hi"),
        TranscriptMessage::tool_step("call_1", "read_text", r#"{"path":"README.md"}"#),
    ];

    // No toggle happened yet: nothing to settle.
    assert!(!app.scroll_settle_pending);

    // Expanding (collapsed by default) both flips the pin and latches the
    // settle request — the loop must not paint the expand's scroll target
    // against the pre-expand layout.
    assert!(app.toggle_step_pinned(&mut messages, 1));
    assert!(messages[1].tool_step_expanded() == Some(true));
    assert!(
        app.scroll_settle_pending,
        "expand must latch the settle request"
    );

    // Collapsing latches it too: the shrunk stream re-validates the offset.
    assert!(app.toggle_step_pinned(&mut messages, 1));
    assert!(messages[1].tool_step_expanded() == Some(false));
    assert!(
        app.scroll_settle_pending,
        "collapse must latch the settle request"
    );

    // The settle is one frame deep: once the loop has staged and settled the
    // frame, the latch clears (mirrored here the way the event loop consumes
    // it — a no-op toggle target keeps the latch off).
    app.scroll_settle_pending = false;

    // A toggle that resolves to nothing (index out of range) latches nothing
    // and leaves the messages untouched.
    let before = app.scroll_settle_pending;
    assert!(!app.toggle_step_pinned(&mut messages, 9));
    assert_eq!(app.scroll_settle_pending, before);
}

/// The default configuration (`[tui] expand_auto_scroll = false`, the
/// shipping default): a disclosure toggle is a pure read interaction. The
/// card flips its expansion, but the scroll offset and the follow/pin state
/// are left exactly as the user had them — the view never moves as a side
/// effect of a click, which is also what keeps any toggle from disturbing
/// an in-progress read.
#[test]
fn disclosure_toggle_disabled_by_default_leaves_scroll_untouched() {
    use crate::model::document::TranscriptMessage;

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    assert!(
        !app.expand_auto_scroll,
        "expand_auto_scroll defaults to disabled"
    );
    let mut messages = vec![
        TranscriptMessage::new(Role::User, "hi"),
        TranscriptMessage::tool_step("call_1", "read_text", r#"{"path":"README.md"}"#),
    ];

    app.scroll = 12;
    let scroll_before = app.scroll;

    // Expand: the pin flips, the scroll offset does not move. A settle frame
    // is still requested — not to scroll, but to re-validate the untouched
    // offset against the new height.
    assert!(app.toggle_step_pinned(&mut messages, 1));
    assert!(messages[1].tool_step_expanded() == Some(true));
    assert_eq!(app.scroll, scroll_before, "scroll must not move on expand");

    // Collapsing clears follow-bottom (reading history pauses auto-follow,
    // matching every other transcript interaction) but still leaves the
    // offset where the user had it.
    assert!(app.toggle_step_pinned(&mut messages, 1));
    assert!(messages[1].tool_step_expanded() == Some(false));
    assert!(!app.follow_bottom, "toggle pauses bottom-follow");
    assert_eq!(
        app.scroll, scroll_before,
        "scroll must not move on collapse"
    );
}

/// The view reset that follows a focus change (envoy zoom enter/exit) must
/// drop a pending settle: the staged frame it was computed for belongs to a
/// transcript slice that is no longer displayed.
#[test]
fn view_reset_clears_pending_scroll_settle() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.scroll_settle_pending = true;
    app.reset_view_state();
    assert!(
        !app.scroll_settle_pending,
        "reset_view_state must clear a pending settle"
    );
}

// ─── Input selection caret relay ──────────────────────────────────────────
// A whole-input selection hides the block caret, but its position is defined
// as the selection's head (where the mouse was released). Direction keys must
// relay from that hidden position and break the selection; deletes replace
// the selection. These tests lock the App-side helpers and the event loop's
// relay probe.

fn app_with_input_selection(input: &str) -> App {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = input.to_string();
    // The drag-selection shape the composer actually records: middle-click /
    // whole-block select of the live input.
    app.selection = SelectionState::Block {
        message_idx: crate::view::INPUT_MSG_IDX,
        block_idx: 0,
    };
    // The hidden caret parked where the mouse released (the drag's head).
    app.set_cursor(input.chars().count());
    app
}

#[test]
fn has_input_selection_detects_both_block_and_range() {
    let mut app = app_with_input_selection("hello");
    assert!(app.has_input_selection());

    // A transcript selection never binds the composer.
    app.selection = SelectionState::Block {
        message_idx: 0,
        block_idx: 0,
    };
    assert!(
        !app.has_input_selection(),
        "transcript selections must not trigger the input caret relay"
    );

    // An active Range on INPUT_MSG_IDX is an input selection.
    app.selection = SelectionState::Range {
        anchor: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 0),
        head: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 2),
    };
    assert!(app.has_input_selection());

    // A collapsed Range (anchor == head) is not active and does not count.
    app.selection = SelectionState::Range {
        anchor: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 0),
        head: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 0),
    };
    assert!(!app.has_input_selection());
}

#[test]
fn adopt_caret_head_and_tail_break_selection() {
    let mut app = app_with_input_selection("hello");
    // Park the visible caret somewhere stale — the adopt must override it,
    // proving the relay wins over the stale position.
    app.cursor_position = 1;

    assert!(app.adopt_caret_from_input_selection(SelectionEdge::Head));
    assert_eq!(app.cursor_position, 5, "head edge = buffer end");
    assert_eq!(app.selection, SelectionState::None, "selection must break");

    // Re-arm and adopt the tail edge.
    app.selection = SelectionState::Block {
        message_idx: crate::view::INPUT_MSG_IDX,
        block_idx: 0,
    };
    assert!(app.adopt_caret_from_input_selection(SelectionEdge::Tail));
    assert_eq!(app.cursor_position, 0, "tail edge = buffer start");
    assert_eq!(app.selection, SelectionState::None);

    // Range selection: head is the release point, tail is the anchor point.
    app.selection = SelectionState::Range {
        anchor: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 1),
        head: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 4),
    };
    assert!(app.adopt_caret_from_input_selection(SelectionEdge::Head));
    assert_eq!(app.cursor_position, 4, "head edge adopts head cursor");
    assert_eq!(app.selection, SelectionState::None);

    // Backward drag: anchor is 4, head is 1 (mouse released at 1).
    app.selection = SelectionState::Range {
        anchor: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 4),
        head: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 1),
    };
    assert!(app.adopt_caret_from_input_selection(SelectionEdge::Head));
    assert_eq!(app.cursor_position, 1, "head edge adopts release position");
    assert_eq!(app.selection, SelectionState::None);

    // No selection → no-op, reports false.
    assert!(!app.adopt_caret_from_input_selection(SelectionEdge::Head));
}

#[test]
fn delete_input_selection_clears_buffer_and_selection() {
    let mut app = app_with_input_selection("hello world");
    assert!(app.delete_input_selection());
    assert_eq!(app.input, "");
    assert_eq!(app.cursor_position, 0);
    assert_eq!(app.selection, SelectionState::None);
    // Second call is a no-op.
    assert!(!app.delete_input_selection());

    // Partial range deletion deletes only the selected slice.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = "hello world".to_string();
    app.selection = SelectionState::Range {
        anchor: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 6),
        head: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 10),
    };
    assert!(app.delete_input_selection());
    assert_eq!(app.input, "hello ");
    assert_eq!(app.cursor_position, 6);
    assert_eq!(app.selection, SelectionState::None);
}

#[test]
fn input_selection_relays_arrows_only_when_composer_owns_caret() {
    let mut app = app_with_input_selection("hello");
    assert_eq!(app.caret_owner(), CaretOwner::Composer);
    assert!(app.input_selection_relays_arrows());

    // A transcript step holding focus means the composer no longer owns the
    // caret: arrows mean step navigation, so the relay must stand down even
    // though a selection is technically active.
    app.focused_target = Some(crate::model::layout::InteractiveTarget::tool_step(0));
    assert!(
        !app.input_selection_relays_arrows(),
        "arrows belong to step navigation while a step holds focus"
    );
}

/// Drive the relay probe the way the event loop does: probe the raw crossterm
/// event, and only fall through to `process_event` when the probe misses.
fn relay_probe(
    app: &mut App,
    code: crossterm::event::KeyCode,
) -> Option<crate::input::InputAction> {
    crate::event_loop::probe_input_selection_relay(
        app,
        &crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
            code,
            crossterm::event::KeyModifiers::NONE,
        )),
    )
}

#[test]
fn relay_left_arrow_breaks_selection_at_head_then_steps() {
    let mut app = app_with_input_selection("hello world");
    // Hidden caret at the release point: end of "hello world" (char 11).
    // ← must break the selection there and step one left: 10.
    let action = relay_probe(&mut app, crossterm::event::KeyCode::Left);
    assert!(matches!(action, Some(crate::input::InputAction::None)));
    assert_eq!(
        app.selection,
        SelectionState::None,
        "← must break selection"
    );
    assert_eq!(
        app.cursor_position, 10,
        "first ← lands one past the release point"
    );
    assert!(
        app.cursor_sync_pending,
        "relay must go through set_cursor so the IME flush fires"
    );
}

#[test]
fn relay_right_arrow_clamps_at_buffer_end() {
    let mut app = app_with_input_selection("abc");
    app.cursor_position = 3; // released at the end
    relay_probe(&mut app, crossterm::event::KeyCode::Right);
    assert_eq!(app.cursor_position, 3, "→ past the end clamps");
    assert_eq!(app.selection, SelectionState::None);
}

#[test]
fn relay_up_and_down_restore_hidden_caret() {
    // The hidden caret's position for a whole-input selection is defined as
    // the head edge (the buffer end) — ↑/↓ restore the caret there and
    // consume the press, rather than leaving the stale pre-selection
    // position in place.
    let mut app = app_with_input_selection("hello");
    app.cursor_position = 1; // stale visible caret from before the drag
    relay_probe(&mut app, crossterm::event::KeyCode::Up);
    assert_eq!(
        app.cursor_position, 5,
        "↑ must restore the caret at the head edge, not the stale position"
    );
    assert_eq!(app.selection, SelectionState::None);

    // ↓ behaves identically: adopt the head edge and consume the press. The
    // press itself does not walk lines or history — that resumes from the
    // restored position on the next key.
    let mut app = app_with_input_selection("hello");
    app.cursor_position = 1;
    relay_probe(&mut app, crossterm::event::KeyCode::Down);
    assert_eq!(app.cursor_position, 5);
    assert_eq!(app.selection, SelectionState::None);
}

#[test]
fn relay_backspace_and_delete_replace_selection() {
    for code in [
        crossterm::event::KeyCode::Backspace,
        crossterm::event::KeyCode::Delete,
    ] {
        let mut app = app_with_input_selection("keep this");
        app.cursor_position = 1; // stale visible caret
        let action = relay_probe(&mut app, code);
        assert!(
            matches!(action, Some(crate::input::InputAction::Backspace)),
            "delete-family must return Backspace's post-edit signal"
        );
        assert_eq!(app.input, "", "the whole selection goes in one stroke");
        assert_eq!(app.cursor_position, 0);
        assert_eq!(app.selection, SelectionState::None);
    }
}

#[test]
fn relay_ignores_keys_without_selection_or_outside_family() {
    // No selection: the probe must miss so ordinary input handling runs.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = "hi".to_string();
    assert!(relay_probe(&mut app, crossterm::event::KeyCode::Left).is_none());

    // With a selection, an uninvolved key (e.g. `x`) must NOT be swallowed:
    // typing over a selection is out of scope for the relay (the TUI has no
    // replace-selection-on-type), so the key keeps its normal meaning.
    let mut app = app_with_input_selection("hi");
    assert!(relay_probe(&mut app, crossterm::event::KeyCode::Char('x')).is_none());
    assert!(
        app.has_input_selection(),
        "an uninvolved key must leave the selection intact"
    );
}

#[test]
fn range_selection_left_arrow_breaks_selection_at_release_position() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = "hello world".to_string();
    // Drag forward from 'w' (6) to 'd' (10/11): mouse released at 11.
    app.selection = SelectionState::Range {
        anchor: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 6),
        head: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 11),
    };
    app.cursor_position = 11;

    let action = relay_probe(&mut app, crossterm::event::KeyCode::Left);
    assert!(matches!(action, Some(crate::input::InputAction::None)));
    assert_eq!(
        app.selection,
        SelectionState::None,
        "selection must be cancelled"
    );
    assert_eq!(
        app.cursor_position, 10,
        "caret steps left from release point 11"
    );

    // Backward drag: drag from 'd' (11) to 'w' (6): mouse released at 6.
    app.selection = SelectionState::Range {
        anchor: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 11),
        head: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 6),
    };
    app.cursor_position = 6;

    let action = relay_probe(&mut app, crossterm::event::KeyCode::Left);
    assert!(matches!(action, Some(crate::input::InputAction::None)));
    assert_eq!(
        app.selection,
        SelectionState::None,
        "selection must be cancelled"
    );
    assert_eq!(
        app.cursor_position, 5,
        "caret steps left from release point 6"
    );
}

#[test]
fn range_selection_right_arrow_breaks_selection_at_release_position() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = "hello world".to_string();
    // Backward drag: drag from 'd' (11) to 'w' (6): mouse released at 6.
    app.selection = SelectionState::Range {
        anchor: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 11),
        head: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 6),
    };
    app.cursor_position = 6;

    let action = relay_probe(&mut app, crossterm::event::KeyCode::Right);
    assert!(matches!(action, Some(crate::input::InputAction::None)));
    assert_eq!(
        app.selection,
        SelectionState::None,
        "selection must be cancelled"
    );
    assert_eq!(
        app.cursor_position, 7,
        "caret steps right from release point 6"
    );
}

#[test]
fn range_selection_up_and_down_restore_caret_at_release_position() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = "hello world".to_string();
    app.selection = SelectionState::Range {
        anchor: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 11),
        head: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 6),
    };
    app.cursor_position = 1; // stale

    let action = relay_probe(&mut app, crossterm::event::KeyCode::Up);
    assert!(matches!(action, Some(crate::input::InputAction::None)));
    assert_eq!(app.selection, SelectionState::None);
    assert_eq!(app.cursor_position, 6, "↑ restores caret at release point");
}

#[test]
fn range_selection_home_and_end_jump_to_selection_edges() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = "hello world".to_string();
    app.selection = SelectionState::Range {
        anchor: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 6),
        head: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 10),
    };

    let action = relay_probe(&mut app, crossterm::event::KeyCode::Home);
    assert!(matches!(action, Some(crate::input::InputAction::None)));
    assert_eq!(app.selection, SelectionState::None);
    assert_eq!(app.cursor_position, 6, "Home jumps to start of range");

    app.selection = SelectionState::Range {
        anchor: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 6),
        head: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 10),
    };

    let action = relay_probe(&mut app, crossterm::event::KeyCode::End);
    assert!(matches!(action, Some(crate::input::InputAction::None)));
    assert_eq!(app.selection, SelectionState::None);
    assert_eq!(app.cursor_position, 11, "End jumps to end of range");
}

#[test]
fn range_selection_cjk_left_arrow_snaps_grapheme() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = "你好世界".to_string();
    // Drag backwards from '界' (byte 9..12, char 3..4) to '好' (byte 3..6, char 1..2).
    // Mouse released at byte 3 (char 1).
    app.selection = SelectionState::Range {
        anchor: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 12),
        head: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 3),
    };
    app.cursor_position = 1;

    let action = relay_probe(&mut app, crossterm::event::KeyCode::Left);
    assert!(matches!(action, Some(crate::input::InputAction::None)));
    assert_eq!(app.selection, SelectionState::None);
    assert_eq!(app.cursor_position, 0, "← steps left from char 1 to char 0");
}

// ─────────────────────────────────────────────────────────────────────────────
// View-scoped chrome for `/btw` aside views (ADR-0103 fix): an aside view must
// render its own session's activity bar, never inherit the primary's, and the
// primary's chrome must survive the aside detour untouched.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn aside_view_does_not_inherit_the_primary_activity_bar() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    // The primary is mid-round with live chrome.
    app.activity_status = "responding".to_string();
    app.round_started_at = Some(std::time::Instant::now());
    app.round_count = 7;
    app.current_turn = 3;

    // Open a brand-new aside: no chrome entry exists yet, so the view must
    // show a fresh idle surface — not the primary's streaming bar.
    app.enter_side_view("side-1".to_string());
    assert!(app.in_side_view);
    assert!(
        app.viewed_chrome().activity.is_empty(),
        "a new aside starts idle, not 'responding'"
    );
    assert_eq!(
        app.viewed_chrome().round_count,
        0,
        "a new aside carries no round counter"
    );
    assert!(
        app.viewed_chrome().round_started_at.is_none(),
        "a new aside has no elapsed timer"
    );

    // The primary's chrome is parked, not destroyed.
    let parked = app.saved_primary_chrome.as_ref().expect("primary parked");
    assert_eq!(parked.activity, "responding");
    assert_eq!(parked.round_count, 7);
    assert_eq!(parked.current_turn, 3);
    assert!(parked.round_started_at.is_some());
}

#[test]
fn exiting_an_aside_restores_the_primary_chrome_exactly() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.activity_status = "running tool: bash".to_string();
    let started = std::time::Instant::now();
    app.round_started_at = Some(started);
    app.round_count = 12;
    app.current_turn = 2;

    app.enter_side_view("side-9".to_string());
    // While inside the aside, its own events land in its chrome entry only.
    app.session_chrome.insert(
        "side-9".to_string(),
        crate::app::SessionChrome {
            activity: "thinking".to_string(),
            responding: true,
            round_count: 1,
            current_turn: 1,
            round_started_at: Some(std::time::Instant::now()),
            can_retry: false,
        },
    );
    // Re-entering (focus jump) must swap the aside's own chrome in.
    app.enter_side_view("side-9".to_string());
    assert_eq!(app.viewed_chrome().activity, "thinking");
    assert_eq!(app.viewed_chrome().round_count, 1);

    // Leaving restores the primary's parked chrome bit-for-bit: the primary
    // round that kept streaming in the background shows its own bar again.
    app.exit_side_view();
    assert!(!app.in_side_view);
    let chrome = app.viewed_chrome();
    assert_eq!(chrome.activity, "running tool: bash");
    assert_eq!(chrome.round_count, 12);
    assert_eq!(chrome.current_turn, 2);
    assert!(chrome.round_started_at.is_some());
}

#[test]
fn reentering_a_running_aside_shows_its_own_chrome() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    // The primary is idle.
    app.activity_status.clear();
    app.round_started_at = None;

    // A background aside is streaming (its listener-maintained entry).
    app.session_chrome.insert(
        "side-2".to_string(),
        crate::app::SessionChrome {
            activity: "responding".to_string(),
            responding: true,
            round_count: 2,
            current_turn: 1,
            round_started_at: Some(std::time::Instant::now()),
            can_retry: false,
        },
    );
    app.enter_side_view("side-2".to_string());
    let chrome = app.viewed_chrome();
    assert_eq!(chrome.activity, "responding");
    assert!(chrome.responding);
    assert_eq!(chrome.round_count, 2);
    assert!(
        chrome.round_started_at.is_some(),
        "the aside's elapsed timer is its own"
    );
}
