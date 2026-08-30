//! Overlay/modal/navigation tests: pickers, transient sheets, sub-layer pop, queue management, aside views, chrome restoration.

use super::*;

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
fn picker_connections_count_matches_provider_rows_no_add_row() {
    // Adding a connection is a footer shortcut (`a`) now, not a synthetic list
    // row, so `picker_row_count()` for Connections equals the provider count
    // exactly (no +1).
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.set_active_modal_for_test(Modal::Connections);
    // Seed a few snapshot rows so providers_filtered() renders the full list
    // (the picker is snapshot-driven).
    let row = |id: &str| muta_contracts::ProviderPickerRow {
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
    app.provider_picker = muta_contracts::ProviderPickerSnapshot {
        default_id: "kimi-code".to_string(),
        rows: vec![row("kimi-code"), row("openai"), row("anthropic")],
    };
    let providers = app.providers_filtered().len();
    assert!(providers > 0, "snapshot seeds the full provider list");
    assert_eq!(app.picker_row_count(), providers);
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
    app.pending_images = vec![muta_contracts::ImagePart {
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

    let image = muta_contracts::ImagePart {
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

    let image = muta_contracts::ImagePart {
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

    super::event_loop::handle_send_slash(&mut app, &runtime, &session, "/delegate on".to_string())
        .await;

    assert!(
        !runtime
            .is_responding
            .load(std::sync::atomic::Ordering::SeqCst),
        "a command must not arm is_responding"
    );
    assert!(
        runtime.phase.lock().await.is_none(),
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
    // A live busy-Enter steer never enters the outbox. It
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
    app.set_active_modal_for_test(Modal::ModelEditor);
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
    app.set_active_modal_for_test(Modal::ModelEditor);
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
        app.set_active_modal_for_test(modal);
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
    app.set_active_modal_for_test(Modal::ModelEditor);
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
fn picker_caret_owner_exists_only_in_search_mode() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    for modal in [Modal::Models, Modal::Connections] {
        app.set_active_modal_for_test(modal);
        app.model_search = false;
        assert_eq!(
            app.caret_owner(),
            CaretOwner::None,
            "{modal:?} browse mode has no editable field"
        );
        app.model_search = true;
        assert_eq!(
            app.caret_owner(),
            CaretOwner::Modal,
            "{modal:?} search mode owns the visible query field"
        );
    }
}

#[test]
fn modal_owns_caret_lists_only_unconditional_input_surfaces() {
    // Static ownership is reserved for modals that render a text field in
    // every state. Browse/search pickers are state-dependent and resolved in
    // `App::caret_owner` instead.
    //
    // The one deliberate exception is `Modal::Question`: its renderer places
    // the real cursor only while the "Other" free-text row is highlighted, and
    // ownership is decided *state-dependently* in `App::caret_owner` (which
    // consults `QuestionModel::is_other_highlighted`) rather than by the static
    // `owns_caret()`. It therefore appears in neither list here — it is tested
    // separately by `caret_owner_question_owns_caret_only_on_other`.
    // HistorySearch is also state-dependent: its panel floats above a
    // live composer that IS the filter field, so the composer (not the modal)
    // owns the caret — handled state-dependently in `App::caret_owner`. It
    // appears in `not_owns` below and is exercised by the caret-owner tests.
    let owns = [Modal::CustomProvider, Modal::InputInjection];
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
        Modal::ProviderPreset,
        Modal::HistorySearch,
        Modal::Models,
        Modal::Connections,
        Modal::ModelEditor,
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
    app.set_active_modal_for_test(Modal::Queue);
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

    app.set_active_modal_for_test(Modal::Tools);
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

    app.set_active_modal_for_test(Modal::Sessions);
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
        app.set_active_modal_for_test(m);
        let (s, f) = app.modal_scroll_field().expect("{m:?} scrolls");
        assert!(f.is_none(), "{m:?} has no selection-follow flag");
        // Mutating must hit a distinct field per modal (not all the same slot).
        *s = 7;
    }
    assert_eq!(app.help_scroll, 7);
    app.set_active_modal_for_test(Modal::Activity);
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
        app.set_active_modal_for_test(m);
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

/// `mutx attach` (no id) opens the sessions picker at startup instead of
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

    // Simulate the startup path (`mutx attach` with no id): the picker
    // opens and `startup_overlay` is armed. Closing it must quit.
    app.startup_overlay = crate::StartupOverlay::SessionsPicker;
    app.set_active_modal_for_test(Modal::Sessions);
    assert!(
        app.startup_overlay == crate::StartupOverlay::SessionsPicker
            && app.active_modal() == Modal::Sessions
    );
    // The quit gate is `should_quit`; it is still clear until a close happens.
    assert!(!app.should_quit.load(Ordering::SeqCst));

    // Open a session from the picker: the overlay clears so a later `/sessions`
    // modal behaves as a normal transient overlay.
    app.startup_overlay = crate::StartupOverlay::None;
    app.set_active_modal_for_test(Modal::None);
    assert_eq!(
        app.startup_overlay,
        crate::StartupOverlay::None,
        "opening a session drops the startup gate"
    );
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

/// Leaving the aside view (Ctrl+C detach, `SideViewSignal::Closed`) must
/// drop any armed Esc confirmation: it targets the aside's round, and a
/// carried arm could fire the *primary's* interrupt on the next Esc.
#[test]
fn leaving_side_view_drops_the_armed_esc_confirmation() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.side_session_id = Some("aside-1".to_string());
    app.in_side_view = true;
    app.current_session_id = "aside-1".to_string();
    app.running_sessions.insert("aside-1".to_string());

    assert!(!app.esc_press(), "the first Esc inside the aside arms");
    assert!(app.esc_armed());

    // Detach: exit_side_view itself runs inside on_viewed_session_changed,
    // which owns the disarm.
    app.exit_side_view();
    assert!(
        !app.esc_armed(),
        "detaching drops the aside's armed confirmation"
    );

    // And re-entering a view always starts unarmed.
    app.enter_side_view("aside-1".to_string());
    assert!(!app.esc_armed());
    assert!(!app.esc_press());
    assert!(app.esc_armed(), "a fresh arm works inside the view");
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

// ─────────────────────────────────────────────────────────────────────────────
// View-scoped chrome for `/btw` aside views (ADR-0103 fix): an aside view must
// render its own session's activity bar, never inherit the primary's, and the
// primary's chrome must survive the aside detour untouched.
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn aside_view_does_not_inherit_the_primary_activity_bar() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    // The primary is mid-round with live chrome.
    app.phase = Some(crate::phase::Phase::Answering);
    app.round_started_at = Some(std::time::Instant::now());
    app.round_count = 7;
    app.current_turn = 3;

    // Open a brand-new aside: no chrome entry exists yet, so the view must
    // show a fresh idle surface — not the primary's streaming bar.
    app.enter_side_view("side-1".to_string());
    assert!(app.in_side_view);
    assert!(
        app.viewed_chrome().phase.is_none(),
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
    assert_eq!(parked.phase, Some(crate::phase::Phase::Answering));
    assert_eq!(parked.round_count, 7);
    assert_eq!(parked.current_turn, 3);
    assert!(parked.round_started_at.is_some());
}

#[test]
fn exiting_an_aside_restores_the_primary_chrome_exactly() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.phase = Some(crate::phase::Phase::Tool(crate::phase::ToolVerb::Running));
    let started = std::time::Instant::now();
    app.round_started_at = Some(started);
    app.round_count = 12;
    app.current_turn = 2;

    app.enter_side_view("side-9".to_string());
    // While inside the aside, its own events land in its chrome entry only.
    app.session_chrome.insert(
        "side-9".to_string(),
        crate::app::SessionChrome {
            phase: Some(crate::phase::Phase::Thinking),
            responding: true,
            round_count: 1,
            current_turn: 1,
            round_started_at: Some(std::time::Instant::now()),
            can_retry: false,
            last_turn_performance: None,
        },
    );
    // Re-entering (focus jump) must swap the aside's own chrome in.
    app.enter_side_view("side-9".to_string());
    assert!(matches!(
        app.viewed_chrome().phase,
        Some(crate::phase::Phase::Thinking)
    ));
    assert_eq!(app.viewed_chrome().round_count, 1);

    // Leaving restores the primary's parked chrome bit-for-bit: the primary
    // round that kept streaming in the background shows its own bar again.
    app.exit_side_view();
    assert!(!app.in_side_view);
    let chrome = app.viewed_chrome();
    assert_eq!(
        chrome.phase,
        Some(crate::phase::Phase::Tool(crate::phase::ToolVerb::Running))
    );
    assert_eq!(chrome.round_count, 12);
    assert_eq!(chrome.current_turn, 2);
    assert!(chrome.round_started_at.is_some());
}

#[test]
fn reentering_a_running_aside_shows_its_own_chrome() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    // The primary is idle.
    app.phase = None;
    app.round_started_at = None;

    // A background aside is streaming (its listener-maintained entry).
    app.session_chrome.insert(
        "side-2".to_string(),
        crate::app::SessionChrome {
            phase: Some(crate::phase::Phase::Answering),
            responding: true,
            round_count: 2,
            current_turn: 1,
            round_started_at: Some(std::time::Instant::now()),
            can_retry: false,
            last_turn_performance: None,
        },
    );
    app.enter_side_view("side-2".to_string());
    let chrome = app.viewed_chrome();
    assert_eq!(chrome.phase, Some(crate::phase::Phase::Answering));
    assert!(chrome.responding);
    assert_eq!(chrome.round_count, 2);
    assert!(
        chrome.round_started_at.is_some(),
        "the aside's elapsed timer is its own"
    );
}

#[test]
fn todos_and_activity_are_separate_places() {
    // Two view ids share the Activity modal but keep their own tab —
    // switching between them lands on the section the id names.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_panel(crate::surfaces::PanelId::Todos);
    assert_eq!(app.active_modal(), Modal::Activity);
    assert_eq!(app.activity_tab, ActivityTab::Todos);
    assert!(app.dismiss_surface());

    app.open_panel(crate::surfaces::PanelId::Activity);
    assert_eq!(app.activity_tab, ActivityTab::Activity);
}

#[test]
fn config_view_reopen_keeps_pane_and_category() {
    // Settings is a full-screen view (ADR-0141) whose fields persist
    // natively on `App`: the enter ritual (pane reset + current-scheme
    // positioning) runs on every enter; a reopen keeps the category/pane
    // the user left. Esc's three-step back (editor → detail → categories →
    // hide) ends in the shared dismiss verb.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.show_view_surface(crate::surfaces::View::Settings);
    assert_eq!(app.active_modal(), Modal::Config);

    // The user walks into a category and the Detail pane, then hides.
    app.config_category = 2;
    app.config_focus = crate::overlays::ConfigFocus::Detail;
    assert!(app.dismiss_surface());
    assert_eq!(app.active_modal(), Modal::None);

    // Reopen: the pane/category survived.
    app.show_view_surface(crate::surfaces::View::Settings);
    assert_eq!(app.config_category, 2, "category retained across hide");
    assert_eq!(
        app.config_focus,
        crate::overlays::ConfigFocus::Detail,
        "pane retained across hide"
    );
}

#[test]
fn switching_picker_view_preserves_query_and_chat_draft_separately() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = "unsent chat".to_string();
    app.open_panel(crate::surfaces::PanelId::Models);
    app.model_search = true;
    app.input = "claude".to_string();

    app.open_panel(crate::surfaces::PanelId::Help);
    assert_eq!(
        app.input, "unsent chat",
        "switch restores the chat composer"
    );

    app.open_panel(crate::surfaces::PanelId::Models);
    assert_eq!(
        app.input, "claude",
        "picker query is retained independently"
    );
    assert!(app.model_search, "the search sub-layer is retained too");
    assert!(app.dismiss_surface());
    assert_eq!(app.input, "unsent chat");
}

#[test]
fn transient_sheet_returns_to_exact_todos_view() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_panel(crate::surfaces::PanelId::Todos);
    app.push_transient_surface(Modal::Question);
    assert_eq!(app.pop_transient_surface(), Modal::Activity);
    assert_eq!(app.active_panel(), Some(crate::surfaces::PanelId::Todos));
    assert_eq!(app.activity_tab, ActivityTab::Todos);
}

#[test]
fn backend_navigation_waits_for_transient_and_drill_in_surfaces() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    assert!(app.can_accept_navigation_signal(), "chat is safe");

    app.open_panel(crate::surfaces::PanelId::Models);
    app.push_transient_surface(Modal::ModelEditor);
    assert!(
        !app.can_accept_navigation_signal(),
        "an editor must not be preempted"
    );
    app.pop_transient_surface();
    assert!(app.can_accept_navigation_signal());

    app.show_view_surface(crate::surfaces::View::Settings);
    app.config_focus = crate::overlays::ConfigFocus::Detail;
    assert!(
        !app.can_accept_navigation_signal(),
        "a parent-owned drill-in must finish or pop first"
    );
}

#[test]
fn explicit_view_close_discards_retained_state_and_payload() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_panel(crate::surfaces::PanelId::UsageStats);
    app.usage_stats_scroll = 17;
    app.close_panel(crate::surfaces::PanelId::UsageStats);

    assert!(!app.panels.is_open(crate::surfaces::PanelId::UsageStats));
    assert_eq!(app.active_modal(), Modal::None);
    assert_eq!(app.usage_stats_scroll, 0);
    assert!(app.open_panel(crate::surfaces::PanelId::UsageStats));
}

#[test]
fn switching_away_from_queue_runs_exit_hook() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let sid = "queue-session";
    app.open_panel(crate::surfaces::PanelId::Queue);
    app.block_queue(sid);
    app.queue_exit_session = Some(sid.to_string());

    app.open_panel(crate::surfaces::PanelId::Help);

    assert!(!app.is_queue_blocked(sid));
    assert!(app.queue_exit_session.is_none());
}

#[test]
fn queue_view_hide_releases_the_auto_block() {
    // Phase 4: the open-time auto-block is released by EVERY hide path
    // (the exit hook in hide_active_panel), not just the Esc arm.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_panel(crate::surfaces::PanelId::Queue);
    app.block_queue("sess");
    app.queue_exit_session = Some("sess".to_string());
    assert!(app.is_queue_blocked("sess"));

    assert!(app.dismiss_surface());
    assert!(
        !app.is_queue_blocked("sess"),
        "exit hook resumed the outbox"
    );
}

#[test]
fn pop_sublayer_steps_back_one_level_at_a_time() {
    // The shared one-step-back (phase 4): Esc's deepest-first chain and the
    // outside-click mirror both route through here.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.set_active_modal_for_test(crate::Modal::Telemetry);
    app.telemetry_detail = true;
    assert!(app.pop_sublayer());
    assert!(!app.telemetry_detail, "drill-in closed");
    assert_eq!(
        app.active_modal(),
        crate::Modal::Telemetry,
        "view stays up"
    );
    assert!(!app.pop_sublayer(), "no sub-layer left");

    // Host: preview is the deepest layer (painted over the prompting
    // state), so it pops first; prompting next; then the view itself.
    app.set_active_modal_for_test(crate::Modal::Host);
    app.host_preview = Some("transcript".to_string());
    app.host_prompting = true;
    assert!(app.pop_sublayer());
    assert!(app.host_preview.is_none(), "deepest layer (preview) closed");
    assert!(app.host_prompting, "prompting still open beneath");
    assert!(app.pop_sublayer());
    assert!(!app.host_prompting);
    assert!(!app.pop_sublayer());
}

#[test]
fn pop_sublayer_pops_telemetry_turn_page_before_round_detail() {
    // Session Telemetry has three levels (round list -> round detail -> attempt inspector):
    // Esc walks back one level at a time, attempt inspector first.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.set_active_modal_for_test(crate::Modal::Telemetry);
    app.telemetry_detail = true;
    app.telemetry_turn = Some((2, 1));
    app.telemetry_turn_cursor = 1;
    app.telemetry_scroll = 4;
    assert!(app.pop_sublayer());
    assert!(
        app.telemetry_turn.is_none(),
        "attempt inspector closed first"
    );
    assert!(app.telemetry_detail, "round detail stays open");
    assert_eq!(app.telemetry_scroll, 0);
    assert_eq!(app.telemetry_turn_cursor, 1, "cursor retained");
    assert!(app.pop_sublayer());
    assert!(!app.telemetry_detail, "round detail closed next");
    assert_eq!(app.telemetry_turn_cursor, 0, "cursor reset");
    assert_eq!(
        app.active_modal(),
        crate::Modal::Telemetry,
        "view stays up"
    );
    assert!(!app.pop_sublayer(), "no sub-layer left");
}

#[test]
fn dashboard_reopen_keeps_selection_and_log() {
    // The dashboard is a full-screen view (ADR-0141) whose dock selection
    // and cockpit log persist natively on `App` across hide.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.show_view_surface(crate::surfaces::View::Dashboard);
    app.host_console_log
        .push(crate::overlays::ConsoleLine::Receipt {
            ok: true,
            target: None,
            text: "ok".to_string(),
        });
    app.modal_index = 3;
    assert!(app.dismiss_surface());

    app.show_view_surface(crate::surfaces::View::Dashboard);
    assert_eq!(app.modal_index, 3, "dock selection retained");
    assert_eq!(app.host_console_log.len(), 1, "cockpit log retained");
}
