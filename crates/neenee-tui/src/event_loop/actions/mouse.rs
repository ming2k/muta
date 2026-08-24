//! Mouse-action handlers for the input dispatch match — selection drags,
//! block select, right-click, and hover affordance. Extracted verbatim from
//! the corresponding arms of `dispatch_action`'s match.

use std::sync::atomic::Ordering;

use neenee_contracts::AgentRequest;

use crate::input;
use crate::interaction::{self, ClickTarget};
use crate::model::document::{NoticeSeverity, TranscriptMessage};
use crate::model::layout::{InteractiveTarget, SemanticCursor};
use crate::model::selection::{CellDragInfo, SelectionState, floor_grapheme_boundary};
use crate::step_interaction::StepKind;
use crate::{App, CaretOwner, Modal, ProviderDeleteChoice, SelectionEdge};

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
    } else if app.active_modal == Modal::OauthPending {
        if let Some(cursor) = app.layout_map.cursor_at(x, y) {
            app.drag.start(cursor);
            app.selection = SelectionState::start_range(cursor);
        } else {
            app.selection = SelectionState::None;
            app.drag.cancel();
        }
        app.focused_target = None;
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
        && let Some(cursor) = app
            .layout_map
            .cursor_at(x, y)
            .filter(|c| c.message_idx == crate::model::layout::MODAL_DOC_MSG_IDX)
    {
        // The sheet's body is a selectable document (tool arguments,
        // description): a press on the text arms a drag-select so the
        // payload can be copied while deciding. Buttons above stay
        // keyboard-driven; presses on the sheet's chrome are inert as before.
        app.drag.begin_range(&mut app.selection, cursor);
    } else if app.active_modal == Modal::Permission
        && app.modal_hit_map.permission_sheet_contains(x, y)
    {
        app.selection = SelectionState::None;
        app.focused_target = None;
        app.drag.cancel();
    } else if app.active_modal.dismissable_by_outside_click() {
        // Selectable modal documents (the `render_selectable_body` family
        // register their rows under `MODAL_DOC_MSG_IDX`): a press that lands
        // on registered text arms a drag-select instead of being a dead
        // click, so modal content is copyable exactly like transcript text.
        // Checked *before* the dismiss logic so a press inside the panel on
        // text never closes the modal; presses on chrome/blank areas keep
        // the previous behaviour.
        if let Some(cursor) = app
            .layout_map
            .cursor_at(x, y)
            .filter(|c| c.message_idx == crate::model::layout::MODAL_DOC_MSG_IDX)
        {
            app.drag.begin_range(&mut app.selection, cursor);
        } else {
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
                if app.pop_sublayer() {
                    // A drill-in sub-layer was open: one step back to the
                    // parent view (the exact same pop Esc performs).
                } else {
                    // Retained browse views hide with state saved; the
                    // quick switcher cancels to its origin (ADR-0133).
                    // Mirrors the Esc path exactly.
                    if !app.dismiss_surface() {
                        // Queue's exit hook in `hide_active_view` (via
                        // dismiss_surface) releases its open-time
                        // auto-block, mirroring Esc (ADR-0133 phase 4).
                        app.active_modal = Modal::None;
                    }
                    // `neene resume` (no id): the startup picker has
                    // no conversation behind it, so a click-outside
                    // (mirroring Esc) quits instead of landing in an
                    // empty chat.
                    if app.startup_overlay == crate::StartupOverlay::SessionsPicker {
                        tracing::info!(reason = "startup_picker_cancelled", "app exiting");
                        app.should_quit.store(true, Ordering::SeqCst);
                    }
                }
            }
            app.selection = SelectionState::None;
            app.focused_target = None;
            app.drag.cancel();
        }
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
        // A retained view (ADR-0133): reopen restores the scroll the user
        // left; only the first open initialises.
        app.open_view(crate::views::ViewId::Todos);
        app.selection = SelectionState::None;
        app.focused_target = None;
        app.drag.cancel();
    } else if app.active_modal == Modal::None
        && app
            .activity_rect
            .is_some_and(|r| r.x <= x && x < r.x + r.width && r.y <= y && y < r.y + r.height)
    {
        // A retained view (ADR-0133): reopen restores the scroll the user
        // left; only the first open initialises.
        app.open_view(crate::views::ViewId::Activity);
        app.selection = SelectionState::None;
        app.focused_target = None;
        app.drag.cancel();
    } else if app.active_modal == Modal::None
        && app
            .queue_rect
            .is_some_and(|r| r.x <= x && x < r.x + r.width && r.y <= y && y < r.y + r.height)
    {
        // Click anywhere on the persistent queue bar → expand
        // the full Queue view. Retained (ADR-0133): cursor/scroll
        // survive hide; the auto-block runs on every entry (an editing
        // safety latch, mirrored by the hide-time resume).
        app.open_view(crate::views::ViewId::Queue);
        app.selection = SelectionState::None;
        app.focused_target = None;
        app.drag.cancel();
        app.block_queue(viewed_session_id);
        app.queue_exit_session = Some(viewed_session_id.to_string());
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
        // A retained view (ADR-0133): reopen restores the scroll/selection
        // (including the drill-in state, which persists on App); only the
        // first open initialises. The attach-mode report fetch stays tied to
        // the ledger being absent — it is a data-lifecycle concern, not an
        // open ritual, so it runs whenever the report is missing.
        app.open_view(crate::views::ViewId::TokenReport);
        if app.token_ledger.is_none() {
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
                // Relay hand-off: place the (possibly hidden) caret at the
                // clicked character so any pending whole-input selection is
                // broken exactly where the user clicked, and the next
                // direction key continues from there. Without this, clicking
                // into a selected input and pressing ← would jump from the
                // stale pre-selection caret instead of the click point.
                app.adopt_caret_from_input_selection(SelectionEdge::Tail);
                app.selection = SelectionState::None;
                let byte = floor_grapheme_boundary(&app.input, cursor.byte_offset);
                app.set_cursor(app.input[..byte].chars().count());
                // Arm a fresh drag from the click point; a plain click (no
                // drag) collapses to a zero-length range that paints nothing,
                // exactly like the previous behaviour.
                app.drag.begin_range(
                    &mut app.selection,
                    SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, byte),
                );
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
                    StepKind::Thinking
                    | StepKind::ProviderRetry
                    | StepKind::CommandResult
                    | StepKind::Notice => {
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
    handle_selection_end_impl(app);
}

/// Test entry for [`handle_selection_end`] — same logic, callable from the
/// event loop's relay tests (the handler itself is `pub(super)` to actions).
#[cfg(test)]
pub(crate) fn handle_selection_end_for_test(app: &mut App) {
    handle_selection_end_impl(app);
}

fn handle_selection_end_impl(app: &mut App) {
    app.drag.finish(&mut app.selection);
    // Caret relay: when the finished drag selected (part of) the live input,
    // the caret is hidden for as long as the selection paints — but its
    // position is defined to be the drag's head, the point where the mouse
    // button was released. Record that position now, so the first direction
    // key after the drag relays from the release point instead of the stale
    // pre-drag caret (the event loop's `probe_input_selection_relay` resolves
    // it when the selection is next touched).
    if let SelectionState::Range { head, .. } = app.selection
        && head.message_idx == crate::view::INPUT_MSG_IDX
        && app.caret_owner() == CaretOwner::Composer
    {
        let byte = floor_grapheme_boundary(&app.input, head.byte_offset);
        app.set_cursor(app.input[..byte].chars().count());
    }
    // Middle-click-style whole-block select on the input selects the entire
    // buffer; the caret's hidden position is defined as the end (head).
    if let SelectionState::Block {
        message_idx: crate::view::INPUT_MSG_IDX,
        ..
    } = app.selection
    {
        app.set_cursor(app.input.chars().count());
    }
}

/// Loop stage (input dispatch): the `SelectBlock` arm of the action match.
pub(super) fn handle_select_block(app: &mut App, x: u16, y: u16) {
    if let Some((mi, bi)) = input::resolve_block(&app.layout_map, x, y) {
        app.selection = SelectionState::Block {
            message_idx: mi,
            block_idx: bi,
        };
        // Whole-input select (middle-click on the composer): the hidden
        // caret's position is defined as the buffer's end, so a following
        // ←/Backspace relays from there once the selection breaks.
        if mi == crate::view::INPUT_MSG_IDX {
            app.set_cursor(app.input.chars().count());
        }
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
