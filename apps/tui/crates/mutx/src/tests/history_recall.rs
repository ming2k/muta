//! Input history, recall, session resume backfill, and on-disk history isolation tests.

use super::*;

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
        muta_contracts::InjectionOrigin::new(muta_contracts::InjectionKind::UserSteer),
    );
    let second = Message::new(Role::Assistant, "revised answer");

    let restored =
        transcript_messages_from_core(vec![first, inserted, second], &config::TuiConfig::default());
    assert_eq!(restored[1].origin, UserMessageOrigin::Steer);
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

/// ADR-0108: a restored slash/shell echo folds into the command ledger
/// projection — the invocation renders once, on the `⌘` command component,
/// never twice (a `▌ cmd` user bubble *and* a command row). The echo is
/// dropped from the dialogue before merge; the ledger row keeps the record.
#[test]
fn restored_slash_echoes_fold_into_command_components() {
    use crate::model::document::UserMessageOrigin;
    use muta_contracts::Role;

    let restored = transcript_messages_from_core(
        vec![
            Message::new(Role::User, "hello"), // a real prompt: survives
            Message::new(Role::Assistant, "hi"),
            // A durable command echo (the legacy live path persisted these).
            Message::command_echo("/pursue ship it"),
            // A display-content slash shape (legacy sessions pre-ADR-0050).
            {
                let mut m = Message::new(Role::User, "expanded prompt text");
                m.display_content = Some("/delegate on".to_string());
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
        runner_meta: None,
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
        runner_meta: None,
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
            runner_meta: None,
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
fn history_rows_lists_newest_first_then_ranks_search() {
    // The App-level view of the Ctrl+R panel. With no query the whole
    // cross-session history is listed newest-first (by created_at_ms),
    // unhighlighted; once the user types, only the fuzzy subsequence matches
    // surface, ordered by score with newest-first order as the stable
    // tiebreaker.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let mk = |text: &str, sid: &str, ts: u64| {
        muta_contracts::HistoryEntry::new(
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

    // Phase 3 (ADR-0133): the per-view draft contract. Parking the draft on
    // the HistorySearch view's own slot, then dismissing the view, hands it
    // back to the composer — the same Esc/outside-click teardown.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_panel(crate::surfaces::PanelId::HistorySearch);
    // Simulate the parked draft (open_panel parked the live composer, which
    // started empty) and the live filter state.
    if let Some(st) = app
        .panels
        .states_mut(&crate::surfaces::PanelId::HistorySearch)
    {
        st.draft = Some("my draft".to_string());
    }
    app.input = "git".to_string(); // the live fuzzy query
    app.cursor_position = 3;
    app.history_search = true;
    app.history_preview = true;
    app.modal_index = 4;

    assert!(app.dismiss_surface());

    assert_eq!(app.input, "my draft", "draft restored from the view's slot");
    assert_eq!(app.cursor_position, "my draft".chars().count());
    assert!(
        app.panels
            .states(&crate::surfaces::PanelId::HistorySearch)
            .is_none_or(|st| st.draft.is_none()),
        "slot emptied"
    );
    assert!(!app.history_search);
    assert!(!app.history_preview);
    assert_eq!(app.active_modal(), crate::Modal::None);
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
    let image = muta_contracts::ImagePart {
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

    let image = muta_contracts::ImagePart {
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

    let image = muta_contracts::ImagePart {
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
    let image = muta_contracts::ImagePart {
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
            vec![muta_contracts::ImagePart {
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
        vec![muta_contracts::ImagePart {
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
    let path = crate::paths::get().history_file();
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
    let image = muta_contracts::ImagePart {
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
