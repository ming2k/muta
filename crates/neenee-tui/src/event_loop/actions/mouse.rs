//! Mouse-action handlers for the input dispatch match — selection drags,
//! block select, right-click, and hover affordance. Extracted verbatim from
//! the corresponding arms of `dispatch_action`'s match.

use std::sync::atomic::Ordering;

use neenee_contracts::AgentRequest;

use crate::input;
use crate::interaction::{self, ClickTarget};
use crate::model::document::{NoticeSeverity, TranscriptMessage};
use crate::model::layout::InteractiveTarget;
use crate::model::selection::{CellDragInfo, SelectionState};
use crate::step_interaction::StepKind;
use crate::{ActivityTab, App, Modal, ProviderDeleteChoice};

use super::super::{UiRuntime, handle_permission_submit, resolve_focused_mut};

/// Loop stage (input dispatch): the `SelectionStart` arm of the action match.
pub(super) async fn handle_selection_start(
    app: &mut App,
    runtime: &UiRuntime,
    viewed_session_id: &str,
    x: u16,
    y: u16,
) {
    // Provider-delete confirm overlay owns clicks while open: a
    // press outside the panel cancels the staged deletion
    // (mirrors Esc) but leaves the provider picker open, and a
    // press inside is a no-op (the buttons are keyboard-only).
    // Either way the click is consumed so it never reaches the
    // picker or transcript behind the backdrop.
    if app.pending_provider_delete.is_some()
        && let Some(r) = app.provider_delete_rect
    {
        let inside = r.x <= x && x < r.x + r.width && r.y <= y && y < r.y + r.height;
        if !inside {
            app.pending_provider_delete = None;
            app.provider_delete_focus = ProviderDeleteChoice::default();
        }
        app.selection = SelectionState::None;
        app.focused_target = None;
        app.drag.cancel();
    } else if app.active_modal == Modal::Question {
        if let Some(hit) = app.modal_hit_map.question_option_at(x, y)
            && let Some(qm) = app.question.take()
        {
            app.question = Some(
                qm.update(crate::question_model::QuestionAction::Select(
                    hit.option_index + 1,
                ))
                .0,
            );
            app.question_modal_follow = true;
        }
        app.selection = SelectionState::None;
        app.focused_target = None;
        app.drag.cancel();
    } else if app.active_modal == Modal::Permission
        && let Some(hit) = app.modal_hit_map.permission_action_at(x, y)
    {
        app.modal_index = hit.action_index;
        handle_permission_submit(app, runtime).await;
        app.selection = SelectionState::None;
        app.focused_target = None;
        app.drag.cancel();
    } else if app.active_modal == Modal::Permission
        && app.modal_hit_map.permission_sheet_contains(x, y)
    {
        app.selection = SelectionState::None;
        app.focused_target = None;
        app.drag.cancel();
    } else if app.active_modal.dismissable_by_outside_click() {
        // Click-to-dismiss: while a dismissable overlay modal is
        // open, the full-screen backdrop owns the click — a press
        // outside the panel closes the modal (mirroring Esc), and a
        // press inside is a no-op (these info modals have no click
        // targets yet). Either way the click is consumed so it does
        // not also fall through to the transcript behind the
        // backdrop. Modals that hold precious input and need their
        // own restore path (Provider / ModelEditor) report no rect
        // and are skipped here, so a stray click never discards an
        // API key. HistorySearch *is* dismissable: its filter is
        // ephemeral and the draft is parked, so an outside click
        // restores the draft (mirroring Esc / CloseModal).
        //
        // The close decision mirrors the `CloseModal` arm
        // exactly, *including the deepest-level-first ordering*:
        // an outside click while inside a drill-in sub-view (e.g.
        // Sessions › Info) backs out to the parent view, not out
        // to chat / quit — so the hierarchy is consistent between
        // Esc and outside-click.
        let inside = app
            .modal_rect
            .is_some_and(|r| r.x <= x && x < r.x + r.width && r.y <= y && y < r.y + r.height);
        if !inside && app.click_outside_dismiss {
            // Dismiss when `[tui] click_outside_dismiss` is on
            // (default on): a click outside the panel closes
            // a dismissable overlay like Esc. The dismissable
            // set excludes modals holding precious in-progress
            // input (they report no rect and are skipped above),
            // so a stray click never discards an API key. When
            // the flag is off the click is still consumed (this
            // whole branch owns it) so it does not fall through
            // to the transcript behind the backdrop.
            //
            // The close decision mirrors the `CloseModal` arm
            // exactly, including the deepest-level-first
            // ordering: an outside click while inside a drill-in
            // sub-view (e.g. Sessions › Info) backs out to the
            // parent view, not out to chat / quit — so the
            // hierarchy is consistent between Esc and outside-
            // click.
            if app.active_modal == Modal::Sessions && app.session_info_detail {
                // Outside-click from the info sub-view → back to
                // the sessions list (mirrors Esc).
                app.session_info_detail = false;
                app.session_detail = None;
                app.session_info_scroll = 0;
            } else if app.active_modal == Modal::TokenReport && app.token_report_detail {
                // Outside-click from the turn breakdown → back to
                // the round list (mirrors Esc).
                app.token_report_detail = false;
                app.token_report_scroll = 0;
            } else {
                if app.active_modal == Modal::HistorySearch {
                    app.restore_history_draft();
                }
                // The queue modal auto-blocked on open; an
                // outside-click closes it like Esc, so resume
                // auto-drain to match.
                if app.active_modal == Modal::Queue {
                    app.resume_queue(viewed_session_id);
                }
                // `neenee resume` (no id): the startup picker has
                // no conversation behind it, so a click-outside
                // (mirroring Esc) quits instead of landing in an
                // empty chat.
                if app.startup_overlay == crate::StartupOverlay::SessionsPicker {
                    tracing::info!(reason = "startup_picker_cancelled", "app exiting");
                    app.should_quit.store(true, Ordering::SeqCst);
                }
                app.active_modal = Modal::None;
            }
        }
        app.selection = SelectionState::None;
        app.focused_target = None;
        app.drag.cancel();
    } else if app.active_modal == Modal::None
        && app.todos_rect.is_some_and(|r| {
            // Todo bar: open the Activity modal on the Todos
            // section directly. Checked before the activity-bar
            // rect in case the two bars ever overlap.
            r.x <= x && x < r.x + r.width && r.y <= y && y < r.y + r.height
        })
    {
        // The activity bar may still be painted while a modal
        // owns the surface — especially the pending Permission
        // sheet, whose expanded body grows up over this row.
        // Gate on `Modal::None` so a click never stacks an
        // Activity modal on top of an in-progress decision.
        app.active_modal = Modal::Activity;
        app.activity_tab = ActivityTab::Todos;
        app.modal_index = 0;
        app.activity_scroll = 0;
        app.selection = SelectionState::None;
        app.focused_target = None;
        app.drag.cancel();
    } else if app.active_modal == Modal::None
        && app
            .activity_rect
            .is_some_and(|r| r.x <= x && x < r.x + r.width && r.y <= y && y < r.y + r.height)
    {
        app.active_modal = Modal::Activity;
        app.activity_tab = ActivityTab::Activity;
        app.modal_index = 0;
        app.activity_scroll = 0;
        app.selection = SelectionState::None;
        app.focused_target = None;
        app.drag.cancel();
    } else if app.active_modal == Modal::None
        && app
            .queue_rect
            .is_some_and(|r| r.x <= x && x < r.x + r.width && r.y <= y && y < r.y + r.height)
    {
        // Click anywhere on the persistent queue bar → expand
        // the full Queue modal. Selection starts at the front
        // (the next manageable item — in-flight steers are not
        // listed). Auto-blocks the outbox for safe editing
        // (mirrors the F2 open path); closing the modal
        // resumes.
        app.active_modal = Modal::Queue;
        app.modal_keymap_open = false;
        app.modal_index = 0;
        app.queue_scroll = 0;
        app.queue_modal_follow = true;
        app.selection = SelectionState::None;
        app.focused_target = None;
        app.drag.cancel();
        app.block_queue(viewed_session_id);
    } else if app.active_modal == Modal::None
        && app
            .hint_context_rect
            .is_some_and(|r| r.x <= x && x < r.x + r.width && r.y <= y && y < r.y + r.height)
    {
        // Click on the context meter in the hint bar → token
        // source report modal. In attach mode there is no
        // local ledger (token accounting lives server-side),
        // so fetch the report from the harness on demand and
        // render a loading placeholder until the reply lands.
        app.active_modal = Modal::TokenReport;
        app.modal_index = 0;
        app.token_report_scroll = 0;
        app.token_report_detail = false;
        if app.token_ledger.is_none() {
            app.token_report = None;
            let _ = app.tx.send(AgentRequest::QueryTokenUsage {
                session_id: viewed_session_id.to_string(),
            });
        }
        app.selection = SelectionState::None;
        app.focused_target = None;
        app.drag.cancel();
    } else if app.sticky_rect.is_some_and(|r| {
        // Sticky pinned step header: collapse it on click.
        r.x <= x && x < r.x + r.width && r.y <= y && y < r.y + r.height
    }) {
        if let Some(mi) = app.sticky_step {
            let mut messages = runtime.messages.write().await;
            app.focused_target = app.focused_messages().get(mi).and_then(|message| {
                if message.is_thinking() {
                    Some(InteractiveTarget::thinking(mi))
                } else if message.is_tool_step() || message.is_envoy_task() {
                    Some(InteractiveTarget::tool_step(mi))
                } else {
                    None
                }
            });
            app.toggle_step_pinned(&mut messages, mi);
            drop(messages);
        }
        // Clicking the sticky header focuses that step (set
        // above), so keyboard navigation can continue from it.
        app.selection = SelectionState::None;
        app.drag.cancel();
    } else {
        // ── Unified content hit-test cascade ──
        // interaction::classify_click runs the full priority
        // chain (input box → step summary → table cell →
        // generic content → gap → dead) so the event loop
        // only needs a single match.
        match interaction::classify_click(&app.layout_map, x, y) {
            ClickTarget::InputBox { cursor } => {
                // Click inside the live input box: clear any
                // focused step so the next keypress edits rather
                // than acting on a step.
                app.focused_target = None;
                app.drag.begin_range(&mut app.selection, cursor);
            }
            ClickTarget::StepSummary { message_idx, kind } => {
                // Clicked a step summary: navigate into an envoy
                // task, otherwise toggle that step's disclosure.
                let mi = message_idx;
                app.focused_target = Some(kind.focus_target(mi));
                let mut messages = runtime.messages.write().await;
                match kind {
                    StepKind::ToolStep => {
                        let enter_id = resolve_focused_mut(&mut messages, &app.focus_stack, mi)
                            .and_then(|message| {
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
                            app.toggle_step_pinned(&mut messages, mi);
                            drop(messages);
                        }
                    }
                    StepKind::Thinking => {
                        app.toggle_step_pinned(&mut messages, mi);
                        drop(messages);
                    }
                    StepKind::ProviderRetry => {
                        app.toggle_step_pinned(&mut messages, mi);
                        drop(messages);
                    }
                }
                app.selection = SelectionState::None;
                app.drag.cancel();
            }
            ClickTarget::TableCell {
                message_idx,
                block_idx,
                cursor,
                cell_text,
                cell_segments,
                ..
            } => {
                // A cell drag is clamped to `│` boundaries: the
                // pointer may wander anywhere but the selection
                // can never cross a `│` border into an adjacent
                // cell.  Within the cell the user has free
                // substring selection — no auto-full-select.
                app.drag.begin_cell(
                    &mut app.selection,
                    cursor,
                    CellDragInfo {
                        message_idx,
                        block_idx,
                        cell_text,
                        segments: cell_segments,
                    },
                );
                app.focused_target = None;
            }
            ClickTarget::Link { url, .. } => {
                app.selection = SelectionState::None;
                app.focused_target = None;
                app.drag.cancel();
                if let Err(err) = webbrowser::open(&url) {
                    runtime
                        .messages
                        .write()
                        .await
                        .push(TranscriptMessage::notice(
                            NoticeSeverity::Warning,
                            format!("Failed to open link {url}: {err}"),
                        ));
                }
            }
            ClickTarget::Content { cursor } => {
                // A plain click does NOT select — it only arms a
                // drag. A zero-length range is created so an
                // immediate drag extends it normally.
                app.drag.begin_range(&mut app.selection, cursor);
                app.focused_target = None;
            }
            ClickTarget::ContentGap => {
                // Click inside the content band but not on a
                // region: clear any step focus and selection
                // without starting a text selection.
                app.selection = SelectionState::None;
                app.focused_target = None;
                app.drag.cancel();
            }
            ClickTarget::Dead => {
                // Click outside all known areas (outer gutters,
                // below content). Fully inert.
                app.selection = SelectionState::None;
                app.focused_target = None;
                app.drag.cancel();
            }
        }
    }
}

/// Loop stage (input dispatch): the `RightClick` arm of the action match.
pub(super) async fn handle_right_click(app: &mut App, runtime: &UiRuntime, x: u16, y: u16) {
    // Right-click on a tool-step summary toggles its inline
    // disclosure (same as left-click / Enter). For
    // permission-denied steps the inline body surfaces the
    // "Permission denied" message directly.
    if let ClickTarget::StepSummary {
        message_idx,
        kind: StepKind::ToolStep,
    } = interaction::classify_click(&app.layout_map, x, y)
    {
        app.focused_target = Some(InteractiveTarget::tool_step(message_idx));
        let mut messages = runtime.messages.write().await;
        app.toggle_step_pinned(&mut messages, message_idx);
        drop(messages);
    }
    app.selection = SelectionState::None;
    app.drag.cancel();
}

/// Loop stage (input dispatch): the `SelectionUpdate` arm of the action match.
pub(super) fn handle_selection_update(app: &mut App, x: u16, y: u16) {
    app.drag
        .update_from_point(&mut app.selection, &app.layout_map, x, y);
}

/// Loop stage (input dispatch): the `SelectionEnd` arm of the action match.
pub(super) fn handle_selection_end(app: &mut App) {
    app.drag.finish(&mut app.selection);
}

/// Loop stage (input dispatch): the `SelectBlock` arm of the action match.
pub(super) fn handle_select_block(app: &mut App, x: u16, y: u16) {
    if let Some((mi, bi)) = input::resolve_block(&app.layout_map, x, y) {
        app.selection = SelectionState::Block {
            message_idx: mi,
            block_idx: bi,
        };
    }
}

/// Loop stage (input dispatch): the `Hover` arm of the action match.
pub(super) async fn handle_hover(app: &mut App, runtime: &UiRuntime, x: u16, y: u16) {
    // Every step summary (tool step, envoy task, reasoning
    // trace) carries the same hover affordance. When the pointer
    // rests on one — either the inline summary or the sticky
    // pinned variant — record its message index so the next draw
    // lights it up to the intermediate hover tone; otherwise
    // clear it.
    if app
        .sticky_rect
        .is_some_and(|r| r.x <= x && x < r.x + r.width && r.y <= y && y < r.y + r.height)
    {
        if let Some(mi) = app.sticky_step {
            let is_step = runtime
                .messages
                .read()
                .await
                .get(mi)
                .map(|m| m.is_thinking() || m.is_tool_step() || m.is_envoy_task())
                .unwrap_or(false);
            app.hovered_step = is_step.then_some(mi);
        }
    } else {
        app.hovered_step = match interaction::classify_click(&app.layout_map, x, y) {
            ClickTarget::StepSummary { message_idx, .. } => Some(message_idx),
            _ => None,
        };
    }
}
