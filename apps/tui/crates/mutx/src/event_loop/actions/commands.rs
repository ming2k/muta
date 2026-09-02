//! Composer-submission and interrupt handlers for the input dispatch match
//! (`SendChat` / `SendSlash` / `CtrlC`).
//! Extracted verbatim from the corresponding arms of `dispatch_action`'s
//! match; only the arm-level `continue` / `return Ok(())` control flow became
//! [`ActionFlow`] values (it already was, inside `dispatch_action`).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::mpsc;

use muta_contracts::{AgentRequest, Role};

use crate::App;
use crate::clipboard;
use crate::composer_attachments;
use crate::model::document::TranscriptMessage;
use crate::model::selection::SelectionState;

use super::super::runtime::{UiRuntime, now_epoch_ms};
use super::super::sync::show_local_toast;
use super::super::transcript::resolve_focused_mut;
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
    app.show_chat_surface();
    app.suggestion_index = None;
    app.input_scroll = 0;

    let images = std::mem::take(&mut app.pending_images);
    let text_pastes = std::mem::take(&mut app.pending_text_pastes);

    // Stage the chips' backing payloads so they ship with
    // this message. The text is expanded into the real paste
    // contents at the moment of dispatch — either inline
    // (immediate send) or when the queue drains (queued
    // send). For queue recall, the raw chip text and the
    // staged vectors are restored verbatim so the user can
    // keep editing the placeholder.
    let has_images = !images.is_empty();

    if !text.is_empty() || has_images {
        if app.running_sessions.contains(viewed_session_id) {
            match app.composer_send_mode {
                crate::app::ComposerSendMode::Steer => {
                    let expanded = composer_attachments::expand_paste_chips(&text, &text_pastes);
                    let expanded =
                        composer_attachments::strip_orphan_image_chips(&expanded, images.len());
                    let id = app.new_insert_id();
                    app.record_input_history(text.clone(), images.clone(), text_pastes);
                    app.clear_history_draft();
                    app.follow_bottom = true;
                    app.pin_summary_line = None;
                    let sent_at_ms = now_epoch_ms();
                    let entry = TranscriptMessage::new(Role::User, text.clone())
                        .with_origin(crate::model::document::UserMessageOrigin::Steer)
                        .with_sent_at_ms(sent_at_ms)
                        .with_insert_id(id.clone())
                        .queued();
                    if !app.in_side_view {
                        runtime.messages.write().await.push(entry);
                    } else {
                        runtime.side_messages.write().await.push(entry);
                    }
                    let _ = app.tx.send(AgentRequest::Steer {
                        session_id: viewed_session_id.to_string(),
                        message: muta_contracts::QueuedMessage {
                            id,
                            text: expanded,
                            display_text: Some(text),
                            images,
                            sent_at_ms: Some(sent_at_ms),
                        },
                    });
                }
                crate::app::ComposerSendMode::FollowUp => {
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
                    app.clear_history_draft();
                    app.follow_bottom = true;
                    app.pin_summary_line = None;
                }
            }
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
                *runtime.phase.lock().await = Some(crate::phase::Phase::Queued);
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
            let _ = app.tx.send(AgentRequest::Prompt {
                text: expanded,
                images,
                sent_at_ms: Some(sent_at_ms),
            });
        }
    } else if let Some((start, end)) = app.selection.active_normalized_range() {
        // Enter on a selected step: navigate into an runner
        // task, otherwise toggle that step's expansion.
        if start.message_idx == end.message_idx {
            let mi = start.message_idx;
            let mut messages = runtime.messages.write().await;
            // An runner task navigates into its view instead
            // of expanding.
            let enter_id =
                resolve_focused_mut(&mut messages, &app.focus_stack, mi).and_then(|message| {
                    if message.is_runner_task() {
                        message.tool_step_call_id().map(String::from)
                    } else {
                        None
                    }
                });
            if let Some(id) = enter_id {
                drop(messages);
                app.enter_runner(id);
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

/// Loop stage (input dispatch): the `SendSlash` arm of the action match.
/// `pub(crate)` so behavior-lock tests in `crate::tests` can drive it directly
/// (ADR-0110).
pub(crate) async fn handle_send_slash(
    app: &mut App,
    runtime: &UiRuntime,
    _session: &crate::SessionSource,
    cmd: String,
) -> ActionFlow {
    app.suggestion_index = None;
    app.input_scroll = 0;
    // A command is a synchronous control-plane operation, not a round
    // (ADR-0110): it never enters the round state machine, so it must not
    // arm the activity bar's liveness surface — no `is_responding`, no
    // optimistic "queued", no fabricated `Esc Esc interrupt` affordance over
    // a dispatch that cannot be interrupted. The pending command row pushed
    // below (ADR-0108) is the in-flight feedback for a command; a running
    // round keeps owning the bar through its own events.
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
    let _ = app.tx.send(AgentRequest::SlashCommand(cmd));
    ActionFlow::Handled
}

pub(super) async fn handle_send_steer(
    app: &mut App,
    runtime: &UiRuntime,
    viewed_session_id: &str,
    text: String,
) {
    app.composer_send_mode = crate::app::ComposerSendMode::Steer;
    handle_send_chat(app, runtime, viewed_session_id, text).await;
}

pub(super) async fn handle_queue_follow_up(
    app: &mut App,
    runtime: &UiRuntime,
    viewed_session_id: &str,
    text: String,
) {
    app.composer_send_mode = crate::app::ComposerSendMode::FollowUp;
    handle_send_chat(app, runtime, viewed_session_id, text).await;
}

/// Loop stage (input dispatch): the `CtrlC` arm of the action match (purely interrupts running task).
pub(crate) fn handle_ctrl_c(
    app: &mut App,
    viewed_session_id: &str,
    _copy_tx: &mpsc::UnboundedSender<Result<clipboard::CopyOutcome, String>>,
    _copy_pending: &Arc<AtomicUsize>,
) -> ActionFlow {
    if app.running_sessions.contains(viewed_session_id) {
        if app.ctrl_c_armed() {
            let _ = app.tx.send(AgentRequest::Interrupt);
            show_local_toast(
                app,
                "Task interrupted",
                false,
                std::time::Duration::from_millis(2000),
            );
            app.arm_ctrl_c(None);
        } else {
            app.arm_ctrl_c(Some(std::time::Instant::now() + App::CTRL_C_ARM_WINDOW));
            show_local_toast(
                app,
                "Press Ctrl+C again to confirm interruption",
                false,
                App::CTRL_C_ARM_WINDOW,
            );
        }
    } else {
        show_local_toast(
            app,
            "No running task to interrupt (Use Ctrl+Q to quit)",
            false,
            std::time::Duration::from_millis(2000),
        );
    }
    ActionFlow::Handled
}

/// Loop stage (input dispatch): the shared `Interrupt` / `InterruptSide` arm
/// of the action match. Both views run the same press-twice contract — the
/// first Esc arms a wall-clock [`App::ESC_ARM_WINDOW`] confirmation window
/// (the "Esc again interrupts" toast), a second press inside it interrupts.
/// `side` routes the request at the viewed aside (`InterruptSide`), which is
/// only meaningful while the aside view is actually open.
pub(crate) fn handle_esc_interrupt(app: &mut App, side: bool) {
    if !app.esc_press() {
        return;
    }
    if side {
        if let Some(side_id) = app.side_session_id.clone() {
            let _ = app.tx.send(AgentRequest::InterruptSide { side_id });
        }
    } else {
        let _ = app.tx.send(AgentRequest::Interrupt);
    }
}
