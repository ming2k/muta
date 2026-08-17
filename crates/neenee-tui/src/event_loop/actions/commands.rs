//! Composer-submission and interrupt handlers for the input dispatch match
//! (`SendChat` / `InsertIntoRound` / `SendSlash` / `CtrlC`).
//! Extracted verbatim from the corresponding arms of `dispatch_action`'s
//! match; only the arm-level `continue` / `return Ok(())` control flow became
//! [`ActionFlow`] values (it already was, inside `dispatch_action`).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::mpsc;

use neenee_contracts::{AgentRequest, Role};

use crate::clipboard;
use crate::clipboard_ops;
use crate::composer_attachments;
use crate::model::document::{CommandPhase, TranscriptMessage};
use crate::model::selection::SelectionState;
use crate::{App, Modal};

use super::super::{
    UiRuntime, extract_selection_text, now_epoch_ms, resolve_focused_mut, show_local_toast,
};
use super::ActionFlow;

/// Split a raw composer command into the ledger identity (`name`, `args`)
/// used by [`TranscriptMessage::pending_command`]: the command word without
/// the leading slash, and the raw argument remainder. Mirrors the runtime's
/// parse in `handlers_slash::dispatch` so the optimistic row and the eventual
/// `RoundEvent::CommandResult` agree on the invocation text.
pub(in crate::event_loop) fn split_command_word(cmd: &str) -> (&str, &str) {
    let trimmed = cmd.trim();
    let first = trimmed.split_whitespace().next().unwrap_or_default();
    let name = first.trim_start_matches('/');
    let args = trimmed.strip_prefix(first).unwrap_or("").trim();
    (name, args)
}

/// Settle the most recent pending command row matching `cmd` with `result`
/// (ADR-0108). When no pending row matches — the transcript was rebuilt, or
/// the reply arrived for a command dispatched before this view attached — a
/// fresh completed row is appended instead, so a reply is never dropped.
pub(in crate::event_loop) fn settle_pending_command(
    messages: &mut Vec<TranscriptMessage>,
    cmd: &str,
    result: neenee_contracts::CommandResult,
) {
    // The pending row's `raw` is the display invocation (`/name args`); match
    // on it (pending rows only) so `/serve on` and `/serve` are distinct rows,
    // and two identical commands each settle their own row.
    let invocation = cmd.trim().to_string();
    let candidate = messages.iter_mut().rev().find(|message| {
        message.is_command_result()
            && message.raw == invocation
            && message.command_result_phase() == Some(CommandPhase::Pending)
    });
    match candidate {
        Some(message) => {
            if !message.settle_command_result(result.clone()) {
                // Not pending (already settled or cancelled): the ledger
                // records both; push a fresh completed row so the newest
                // reply is not lost.
                let (name, args) = split_command_word(&invocation);
                messages.push(TranscriptMessage::command_result(name, args, Some(result)));
            }
        }
        None => {
            let (name, args) = split_command_word(&invocation);
            messages.push(TranscriptMessage::command_result(name, args, Some(result)));
        }
    }
}

/// Loop stage (input dispatch): the `SendChat` arm of the action match.
pub(super) async fn handle_send_chat(
    app: &mut App,
    runtime: &UiRuntime,
    viewed_session_id: &str,
    text: String,
) {
    // Note: history-search selection no longer flows through
    // here — Enter in `Modal::HistorySearch` emits the dedicated
    // `HistoryInsert` action so the chosen entry lands in the
    // input box for editing instead of being sent immediately.
    app.active_modal = Modal::None;
    app.suggestion_index = None;
    app.input_scroll = 0;

    // Stage the chips' backing payloads so they ship with
    // this message. The text is expanded into the real paste
    // contents at the moment of dispatch — either inline
    // (immediate send) or when the queue drains (queued
    // send). For queue recall, the raw chip text and the
    // staged vectors are restored verbatim so the user can
    // keep editing the placeholder.
    let images = std::mem::take(&mut app.pending_images);
    let text_pastes = std::mem::take(&mut app.pending_text_pastes);
    let has_images = !images.is_empty();

    if !text.is_empty() || has_images {
        if app.running_sessions.contains(viewed_session_id) {
            // Busy sends live in the fixed outbox, not the
            // scrollback. A staged message always waits for the
            // running round to finish naturally before starting a
            // new one (next-round only). The mid-round insert
            // path is the explicit `F4` gesture below, not a
            // busy Enter.
            let id = uuid::Uuid::new_v4().to_string();
            let queued_at_ms = now_epoch_ms();
            app.pending_dispatch.push_back(crate::app::QueuedDispatch {
                id: id.clone(),
                session_id: viewed_session_id.to_string(),
                state: crate::app::QueuedDispatchState::Waiting,
                text: text.clone(),
                queued_at_ms,
                images: images.clone(),
                text_pastes: text_pastes.clone(),
            });
            app.record_input_history(text.clone(), images.clone(), text_pastes.clone());
            // The draft's content has been taken into the
            // outbox — it is no longer the unsent slot.
            app.clear_history_draft();
            app.follow_bottom = true;
            app.pin_summary_line = None;
        } else {
            // Expand `[Pasted text #N +M lines]` chips into
            // their full staged text right before dispatch so
            // the model receives the real paste contents
            // rather than the chip label. Image chips stay
            // in the text as positional labels.
            let expanded = composer_attachments::expand_paste_chips(&text, &text_pastes);
            // An image chip with no staged payload (e.g.
            // recalled from a history entry recorded before
            // attachment staging) is a bare label — drop it
            // so the model never receives a phantom
            // `[Image #N …]` it cannot see.
            let expanded = composer_attachments::strip_orphan_image_chips(&expanded, images.len());
            if !app.in_side_view {
                runtime.is_responding.store(true, Ordering::SeqCst);
                *runtime.activity_status.lock().await = "queued".to_string();
            }
            app.idle_sessions.remove(viewed_session_id);
            app.running_sessions.insert(viewed_session_id.to_string());
            let sent_at_ms = now_epoch_ms();
            let sent = TranscriptMessage::new(Role::User, text.clone()).with_sent_at_ms(sent_at_ms);
            if !app.in_side_view {
                runtime.messages.write().await.push(sent);
            } else {
                runtime.side_messages.write().await.push(sent);
            }
            app.record_input_history(text.clone(), images.clone(), text_pastes.clone());
            // The draft's content has been sent — it is now a
            // history row, not the unsent slot.
            app.clear_history_draft();
            app.follow_bottom = true;
            app.pin_summary_line = None;
            let _ = app.tx.send(AgentRequest::Chat {
                text: expanded,
                images,
                sent_at_ms: Some(sent_at_ms),
            });
        }
    } else if let Some((start, end)) = app.selection.active_normalized_range() {
        // Enter on a selected step: navigate into an envoy
        // task, otherwise toggle that step's expansion.
        if start.message_idx == end.message_idx {
            let mi = start.message_idx;
            let mut messages = runtime.messages.write().await;
            // An envoy task navigates into its view instead
            // of expanding.
            let enter_id =
                resolve_focused_mut(&mut messages, &app.focus_stack, mi).and_then(|message| {
                    if message.is_envoy_task() {
                        message.tool_step_call_id().map(String::from)
                    } else {
                        None
                    }
                });
            if let Some(id) = enter_id {
                drop(messages);
                app.enter_envoy(id);
            } else {
                let toggled = app.toggle_step_pinned(&mut messages, mi);
                drop(messages);
                if toggled {
                    app.selection = SelectionState::None;
                }
            }
        }
    }
}

/// Loop stage (input dispatch): the `InsertIntoRound` arm of the action match.
pub(super) fn handle_insert_into_round(app: &mut App, viewed_session_id: &str) {
    // `F4` at the top level: steer the composed text into the
    // *running* round instead of staging it for the next one.
    // The registry resolved a data-less action, so take the
    // composer here — exactly like `SendChat` does.
    let text = std::mem::take(&mut app.input);
    app.cursor_position = 0;
    app.input_scroll = 0;
    app.suggestion_index = None;
    let images = std::mem::take(&mut app.pending_images);
    let text_pastes = std::mem::take(&mut app.pending_text_pastes);
    let busy = app.running_sessions.contains(viewed_session_id);
    if !busy || (text.is_empty() && images.is_empty()) {
        // Nothing to steer into (idle) or nothing to say:
        // restore the composer verbatim so a stray F4 never
        // eats the draft.
        app.input = text;
        app.pending_images = images;
        app.pending_text_pastes = text_pastes;
        app.set_cursor_end();
    } else {
        let expanded = composer_attachments::expand_paste_chips(&text, &text_pastes);
        let expanded = composer_attachments::strip_orphan_image_chips(&expanded, images.len());
        let id = app.stage_insert_dispatch(
            viewed_session_id,
            text.clone(),
            images.clone(),
            text_pastes.clone(),
        );
        app.record_input_history(text.clone(), images, text_pastes);
        // The draft now lives in the outbox as an in-flight
        // steer — it is no longer the unsent slot.
        app.clear_history_draft();
        app.follow_bottom = true;
        app.pin_summary_line = None;
        let _ = app.tx.send(AgentRequest::InsertUserInput {
            session_id: viewed_session_id.to_string(),
            input: neenee_contracts::QueuedUserInput {
                id,
                text: expanded,
                display_text: Some(text),
                images: Vec::new(),
                sent_at_ms: Some(now_epoch_ms()),
            },
        });
    }
}

/// Loop stage (input dispatch): the `SendSlash` arm of the action match
/// (including the frontend-only `/serve` interception).
pub(super) async fn handle_send_slash(
    app: &mut App,
    runtime: &UiRuntime,
    _session: &crate::SessionSource,
    viewed_session_id: &str,
    cmd: String,
) -> ActionFlow {
    app.suggestion_index = None;
    app.input_scroll = 0;
    // A running round owns the activity surface. Do not paint
    // an optimistic "queued" over its live label, and do not
    // arm the responding flag for a control-plane command the
    // round did not ask for: the round's own events keep the
    // bar truthful, and the command's reply must not be able
    // to leave a fabricated "queued" behind.
    let session_busy = app.running_sessions.contains(viewed_session_id);
    if !session_busy {
        runtime.is_responding.store(true, Ordering::SeqCst);
        *runtime.activity_status.lock().await = "queued".to_string();
    }
    app.follow_bottom = true;
    app.pin_summary_line = None;
    let sent_at_ms = now_epoch_ms();
    // The command component owns its input AND its output (ADR-0108): the
    // optimistic row pushed here is the input half — `⌘ /cmd` in the muted
    // running tone — and the `RoundEvent::CommandResult` handler settles the
    // same row in place when the typed reply arrives. A command is therefore
    // never echoed as a user bubble (the old `▌ cmd` panel duplicated the
    // invocation in a second row and split the effect across a seam), and the
    // transcript keeps one row per command, live and after resume alike.
    // The invocation is still recorded in input history for ↑/Ctrl+R recall.
    let (cmd_name, cmd_args) = split_command_word(&cmd);
    runtime
        .messages
        .write()
        .await
        .push(TranscriptMessage::pending_command(cmd_name, cmd_args).with_sent_at_ms(sent_at_ms));
    app.record_input_history(cmd.clone(), Vec::new(), Vec::new());
    // `/serve` is a pure frontend concern (hot-attach a
    // WebSocket listener to the running session). Intercept
    // it here rather than routing through SessionDriver.
    if cmd == "/serve" || cmd.starts_with("/serve ") {
        // `/serve` is answered inline: settle the pending command row with
        // the reply as its output half (ADR-0108) — same component, no
        // assistant bubble beside it.
        let reply = "Sessions are managed by the unified daemon; to manage daemons use \
             'neenee daemon' or '/host'."
            .to_string();
        let mut msgs = runtime.messages.write().await;
        settle_pending_command(
            &mut msgs,
            &cmd,
            neenee_contracts::CommandResult::Text(reply),
        );
        if !session_busy {
            runtime.is_responding.store(false, Ordering::SeqCst);
            runtime.activity_status.lock().await.clear();
        }
        return ActionFlow::NextEvent;
    }
    let _ = app.tx.send(AgentRequest::SlashCommand(cmd));
    ActionFlow::Handled
}

/// Loop stage (input dispatch): the `CtrlC` arm of the action match (copy
/// selection, close overlay, clear input, armed double-press quit).
pub(super) fn handle_ctrl_c(
    app: &mut App,
    copy_tx: &mpsc::UnboundedSender<Result<clipboard::CopyOutcome, String>>,
    copy_pending: &Arc<AtomicUsize>,
) -> ActionFlow {
    if let Some(text) = extract_selection_text(
        &app.selection,
        app.focused_messages(),
        &app.input,
        &app.layout_map,
        app.drag.cell_info.as_ref(),
    ) {
        clipboard_ops::spawn_clipboard_copy(copy_tx, copy_pending.clone(), text);
    } else if app.active_modal == Modal::HistorySearch {
        // Cancel the history modal: restore the in-progress draft
        // the user was composing before Ctrl+R (clears the search
        // query and sub-flags too).
        app.restore_history_draft();
        app.active_modal = Modal::None;
    } else if app.startup_overlay == crate::StartupOverlay::SessionsPicker
        && app.active_modal == Modal::Sessions
    {
        // `neenee resume` (no id) opened the picker at startup:
        // there is no conversation behind it, so Ctrl+C — like
        // Esc and an outside click — quits the program rather
        // than dropping into an empty session. Without this,
        // Ctrl+C used to close the modal and land the user in a
        // bare empty chat (which a stray /models then persisted
        // as an empty-session file).
        tracing::info!(reason = "startup_picker_cancelled", "app exiting");
        app.should_quit.store(true, Ordering::SeqCst);
    } else if app.active_modal != Modal::None && app.active_modal != Modal::Permission {
        app.active_modal = Modal::None;
    } else if app.in_side_view {
        // `/btw` aside view: Ctrl+C detaches back to the primary
        // transcript (ADR-0103 §2) — the aside keeps running, so
        // this is the "get me out" gesture, matching the shell/REPL
        // muscle memory. Slotted after modal-close so an open
        // overlay still wins. The composer draft is deliberately
        // preserved (it belongs to the aside's next turn, not to a
        // quit intent).
        app.exit_side_view();
        let _ = app.tx.send(AgentRequest::ExitSideView);
    } else if !app.input.is_empty() {
        // Ctrl+C is purely a compose-level action: copy,
        // close overlay, clear, or quit. It never interrupts a
        // running turn — only double-Esc does — so a task in
        // flight is left untouched here and the input is
        // cleared instead. Clearing the input also arms the
        // quit window so
        // the chain is exactly two presses total (clear,
        // then quit). The combined toast says both what
        // just happened and what the next Ctrl+C will do,
        // removing the old "silent clear → user can't tell
        // if the next press will quit or do something else"
        // ambiguity. Pending-image reminders skip their
        // per-frame refresh while the quit window is armed
        // so this toast keeps the floor.
        app.input.clear();
        app.set_cursor(0);
        app.input_scroll = 0;
        show_local_toast(
            app,
            "input cleared — Ctrl+C again to exit",
            false,
            std::time::Duration::from_millis(2000),
        );
        app.arm_ctrl_c(Some(
            std::time::Instant::now() + std::time::Duration::from_secs(2),
        ));
    } else if app.ctrl_c_armed() {
        tracing::info!(reason = "ctrl_c_double_press", "app exiting");
        return ActionFlow::Exit;
    } else {
        // Arm a real 2s window (wall-clock) in which a second
        // Ctrl+C quits.
        app.arm_ctrl_c(Some(
            std::time::Instant::now() + std::time::Duration::from_secs(2),
        ));
    }
    ActionFlow::Handled
}
