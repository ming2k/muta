//! Input-action dispatch for the TUI event loop: the `match` over
//! [`input::InputAction`] that `run_app_loop`'s input-drain stage ran inline,
//! moved here verbatim (one arm per variant) with only the loop-control
//! statements rewritten as [`ActionFlow`] values.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use mutx_engine::Terminal;
use tokio::sync::mpsc;

use muta_contracts::{AgentRequest, PermissionDecision, PermissionRequest};

use crate::clipboard;
use crate::clipboard_ops;
use crate::input;
use crate::model::layout::InteractiveTargetKind;
use crate::model::selection::SelectionState;
use crate::view;
use crate::view::Theme;
use crate::{App, Modal};

use super::runtime::UiRuntime;
use super::sync::show_local_toast;
use super::transcript::{extract_selection_text, resolve_focused_mut};

mod commands;
mod host;
mod modals;
mod mouse;

pub(crate) use modals::{
    activate_picked_model, effective_reasoning_effort, handle_permission_submit, modal_page_step,
    question_effects,
};

pub(crate) use commands::handle_esc_interrupt;
pub(super) use commands::split_command_word;

#[cfg(test)]
pub(crate) use commands::handle_ctrl_c;

#[cfg(test)]
pub(crate) use commands::handle_send_slash;
#[cfg(test)]
pub(crate) use modals::{
    handle_close_modal, handle_modal_down, handle_modal_up, handle_submit_custom_provider,
};

/// How the event loop proceeds after a dispatched action. Arms that ended in
/// `continue` (skip to the next drained input event) or `return Ok(())` (exit
/// the loop) when the match was inline in `run_app_loop` return these instead;
/// the call site maps them back onto the same control flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActionFlow {
    /// Action handled; the drain loop proceeds to the next statement.
    Handled,
    /// `continue` the input-drain loop.
    NextEvent,
    /// `return Ok(())` from `run_app_loop`.
    Exit,
}

pub(super) struct ActionContext<'a> {
    pub runtime: &'a UiRuntime,
    pub session: &'a crate::SessionSource,
    pub viewed_session_id: &'a str,
    pub copy_tx: &'a mpsc::UnboundedSender<Result<clipboard::CopyOutcome, String>>,
    pub copy_pending: &'a Arc<AtomicUsize>,
    pub paste_tx: &'a mpsc::UnboundedSender<clipboard::ClipboardRead>,
    pub sgr_guard: &'a mut input::SgrLeakGuard,
}

/// Shared scroll step for keyboard scroll keys and out-of-panel wheel
/// ticks: a modal body (including the permission sheet's details) takes the
/// tick one line at a time; with no modal the transcript scrolls by four
/// lines per tick so browsing feels fast instead of crawling line-by-line.
/// Wheel ticks landing inside the composer panel never reach this — the
/// `Wheel` arm routes them to the input's own viewport first.
fn scroll_tick(app: &mut App, down: bool) {
    if let Some((scroll, follow)) = app.modal_scroll_field() {
        if let Some(f) = follow {
            *f = false;
        }
        *scroll = if down {
            scroll.saturating_add(1)
        } else {
            scroll.saturating_sub(1)
        };
    } else if down {
        app.pin_summary_line = None;
        app.scroll = app.scroll.saturating_add(4).min(app.max_scroll);
        if app.scroll >= app.max_scroll {
            app.follow_bottom = true;
        }
    } else {
        // While a permission sheet is open the transcript stays scrollable,
        // so the wheel / page keys drive the conversation behind it, not the
        // sheet's own body.
        app.follow_bottom = false;
        app.pin_summary_line = None;
        app.scroll = app.scroll.saturating_sub(4);
    }
}

fn scroll_transcript_page(app: &mut App, down: bool) {
    let step = app.view_height.saturating_sub(1).max(1);
    app.pin_summary_line = None;
    if down {
        app.scroll = app.scroll.saturating_add(step).min(app.max_scroll);
        if app.scroll >= app.max_scroll {
            app.follow_bottom = true;
        }
    } else {
        app.follow_bottom = false;
        app.scroll = app.scroll.saturating_sub(step);
    }
}

fn scroll_transcript_to_edge(app: &mut App, bottom: bool) {
    app.pin_summary_line = None;
    if bottom {
        app.scroll = app.max_scroll;
        app.follow_bottom = true;
    } else {
        app.scroll = 0;
        app.follow_bottom = false;
    }
}

fn select_connection_preset(app: &mut App, forced_method: Option<muta_contracts::LoginMethod>) {
    if app.active_modal() != Modal::ProviderPreset {
        return;
    }
    let Some(preset) = crate::PROVIDER_PRESETS.get(app.preset_choice) else {
        return;
    };
    if !preset.oauth_first() {
        if forced_method.is_some() {
            show_local_toast(
                app,
                "This connection signs in with an API key; there is no OAuth method to choose.",
                true,
                std::time::Duration::from_millis(2600),
            );
        } else {
            app.open_custom_provider_editor(preset);
        }
        return;
    }

    let config = preset
        .auth
        .oauth_provider_id()
        .and_then(muta_providers::oauth::config_by_provider_id);
    let method = forced_method.or_else(|| {
        config
            .as_ref()
            .and_then(|config| config.effective_default_login_method())
    });
    let Some(method) = method else {
        show_local_toast(
            app,
            "This connection has no available OAuth login method.",
            true,
            std::time::Duration::from_millis(2600),
        );
        return;
    };
    if config
        .as_ref()
        .is_none_or(|config| !config.supports_login_method(method))
    {
        show_local_toast(
            app,
            match method {
                muta_contracts::LoginMethod::Browser => {
                    "Browser PKCE login is not supported by this connection."
                }
                muta_contracts::LoginMethod::Device => {
                    "Device login is not supported by this connection."
                }
            },
            true,
            std::time::Duration::from_millis(2600),
        );
        return;
    }

    app.begin_oauth_add(preset, method);
    let _ = app.tx.send(AgentRequest::AuthorizeOAuth {
        method,
        auth: preset.auth,
    });
}

/// Loop stage: dispatch one drained [`input::InputAction`]. The match body is
/// verbatim from `run_app_loop`; only `continue` / `return Ok(())` inside arms
/// became [`ActionFlow`] values, and the clipboard senders / viewed session id
/// are passed explicitly instead of captured.
pub(super) async fn dispatch_action<W: std::io::Write>(
    app: &mut App,
    _terminal: &mut Terminal<W>,
    action: input::InputAction,
    ctx: &mut ActionContext<'_>,
) -> ActionFlow {
    let ActionContext {
        runtime,
        session,
        viewed_session_id,
        copy_tx,
        copy_pending,
        paste_tx,
        sgr_guard,
    } = ctx;
    let runtime = *runtime;
    let session = *session;
    let viewed_session_id = *viewed_session_id;
    let copy_tx = *copy_tx;
    let copy_pending = *copy_pending;
    let paste_tx = *paste_tx;
    let sgr_guard = &mut **sgr_guard;
    // While the dashboard's kill confirm is armed, only the confirming `k`
    // (or the confirm-cancelling paths inside the dashboard arms) keeps it
    // alive: any other action — navigation, prompt, focus toggle, Esc —
    // disarms it. The armed state lives exactly one keystroke.
    if app.host_kill_confirm.is_some()
        && app.active_modal() == Modal::Host
        && !matches!(
            action,
            input::InputAction::HostKillSelected | input::InputAction::None
        )
    {
        host::cancel_kill_confirm(app);
    }

    match action {
        input::InputAction::None => {}
        input::InputAction::TerminalResized => {
            // A resize is the prime trigger for crossterm splitting an
            // in-flight SGR mouse sequence across reads (issue #854).
            // Re-arm mouse capture so both crossterm's parser and the
            // terminal's mouse-tracking state start from a clean slate,
            // and force an immediate redraw to replace the stale frame
            // at the old geometry. The re-arm is best-effort: if the
            // terminal is mid-shutdown the write is ignored.
            use crossterm::event::EnableMouseCapture;
            let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture);
            sgr_guard.reset();
            // No need to set `frame_dirty` here: every drained event
            // already raised `input_redraw_pending`, which forces a
            // redraw on the very next frame at the new geometry.
        }
        input::InputAction::Quit => {
            let _ = app.tx.send(AgentRequest::EndSession);
            tracing::info!(reason = "slash_exit", "app exiting");
            return ActionFlow::Exit;
        }
        input::InputAction::SendChat(text) => {
            commands::handle_send_chat(app, runtime, viewed_session_id, text).await;
        }
        input::InputAction::SteerImmediate(text) => {
            commands::handle_send_steer(app, runtime, viewed_session_id, text).await;
        }
        input::InputAction::QueueFollowUp(text) => {
            commands::handle_queue_follow_up(app, runtime, viewed_session_id, text).await;
        }
        input::InputAction::SendSlash(cmd) => {
            return commands::handle_send_slash(app, runtime, session, cmd).await;
        }
        input::InputAction::ProviderPickerActivate => {
            // Activate is a Models-only action: the flat (provider,
            // model) pair under the highlight. The Connections list has
            // no activate concept — it only manages instances, so Enter
            // never produces this action there. Both the Models picker
            // and the key editor share one activation path (key-ready /
            // OAuth / key editor) via `activate_picked_model`.
            let key_ready = |app: &App, id: &str| app.key_status.get(id).copied().unwrap_or(true);
            let target = if app.active_modal() == Modal::Models {
                let rows = app.models_flat_filtered();
                rows.get(app.modal_index)
                    .or_else(|| rows.first())
                    .map(|row| (row.provider_id.clone(), row.model.clone()))
            } else {
                None
            };
            if let Some((id, model)) = target {
                let ready = key_ready(app, &id);
                activate_picked_model(app, id, model, ready);
            }
        }
        input::InputAction::CustomProviderNextField => {
            if app.active_modal() == Modal::CustomProvider {
                app.cycle_custom_field(true);
            }
        }
        input::InputAction::CustomProviderPrevField => {
            if app.active_modal() == Modal::CustomProvider {
                app.cycle_custom_field(false);
            }
        }
        input::InputAction::ScrollCustomProvider { forward } => {
            if app.active_modal() == Modal::CustomProvider {
                app.scroll_custom_provider(forward);
            }
        }
        input::InputAction::CycleCustomProviderChoice { forward } => {
            if app.active_modal() == Modal::CustomProvider {
                app.cycle_custom_choice(forward);
            }
        }
        input::InputAction::MovePresetChoice { forward } => {
            if app.active_modal() == Modal::ProviderPreset {
                app.move_preset_choice(forward);
            }
        }
        input::InputAction::SelectPreset => {
            select_connection_preset(app, None);
        }
        input::InputAction::SelectPresetWithOauthMethod { method } => {
            select_connection_preset(app, Some(method));
        }
        input::InputAction::CancelOauthPending => {
            if app.active_modal() == Modal::OauthPending {
                let _ = app.tx.send(AgentRequest::CancelAuthorizeOAuth);
                app.awaiting_oauth_add = false;
                app.oauth_pending_url.clear();
                app.oauth_pending_user_code.clear();
                app.oauth_pending_message.clear();
                app.oauth_pending_error = None;
                app.open_preset_chooser();
            }
        }
        input::InputAction::CycleOauthSelection => {
            if app.active_modal() == Modal::OauthPending {
                app.cycle_oauth_selection();
            }
        }
        input::InputAction::CopyOauthContent { target } => {
            // If the user has active text selected in the modal, copy that selection first
            if let Some(text) = extract_selection_text(
                &app.selection,
                app.focused_messages(),
                &app.input,
                &app.layout_map,
                app.drag.cell_info.as_ref(),
            ) {
                clipboard_ops::spawn_clipboard_copy(copy_tx, copy_pending.clone(), text);
                app.copy_toast_message = "Selection copied to clipboard".to_string();
                app.copy_toast_failed = false;
                app.copy_toast_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(1500));
                return ActionFlow::Handled;
            }
            // Copy the OAuth pending sheet's primary field to the
            // system clipboard.
            let actual_target = match target {
                input::OauthCopyTarget::Selected => app.oauth_selected_target(),
                input::OauthCopyTarget::UserCode => input::OauthCopyTarget::UserCode,
                input::OauthCopyTarget::Url => input::OauthCopyTarget::Url,
            };
            let (text, label) = match actual_target {
                input::OauthCopyTarget::UserCode => (
                    app.oauth_pending_user_code.clone(),
                    "Code copied to clipboard",
                ),
                input::OauthCopyTarget::Url | input::OauthCopyTarget::Selected => {
                    (app.oauth_pending_url.clone(), "Link copied to clipboard")
                }
            };
            if !text.is_empty() {
                clipboard_ops::spawn_clipboard_copy(copy_tx, copy_pending.clone(), text);
                app.copy_toast_message = label.to_string();
                app.copy_toast_failed = false;
                app.copy_toast_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(1500));
            }
        }
        input::InputAction::CancelPresetChooser => {
            // Return to the Connections list the chooser was opened
            // from; the chat draft stays parked in stashed_input.
            if app.active_modal() == Modal::ProviderPreset {
                app.input.clear();
                app.set_cursor(0);
                app.pop_transient_surface();
                app.model_search = false;
                app.model_scroll = 0;
                app.model_modal_follow = true;
                app.modal_index = 0;
            }
        }
        input::InputAction::DeleteProvider => {
            // Connections `Shift+D`: stage the highlighted custom
            // provider for deletion and open the confirm overlay over
            // the list (dimmed backdrop + centered panel). The actual
            // `AgentRequest::DeleteProvider` only fires once the user
            // confirms inside the overlay. Built-in providers and the
            // synthetic "＋ Add connection" row are ignored by the
            // helper.
            app.stage_provider_delete();
        }
        input::InputAction::DeleteProviderConfirm => {
            // The confirm overlay's Enter-on-Delete: dispatch the
            // staged deletion and tear the overlay down.
            if let Some(req) = app.confirm_provider_delete() {
                let _ = app.tx.send(req);
            }
        }
        input::InputAction::DeleteProviderCancel => {
            // Esc / Ctrl+C / Enter-on-Cancel inside the confirm
            // overlay: drop the staged provider id and return keyboard
            // focus to the Connections list. The modal itself stays
            // open.
            app.cancel_provider_delete();
        }
        input::InputAction::CancelCustomProvider => {
            // Return to the Connections list the editor was opened
            // from; the chat draft stays parked in stashed_input.
            if app.active_modal() == Modal::CustomProvider {
                app.input.clear();
                app.set_cursor(0);
                app.custom_field = 0;
                app.custom_edit_id = None;
                app.pop_transient_surface();
                app.model_search = false;
                app.model_scroll = 0;
                app.model_modal_follow = true;
                app.modal_index = 0;
            }
        }
        input::InputAction::SubmitCustomProvider => {
            modals::handle_submit_custom_provider(app);
        }
        input::InputAction::ModelEnterSearch => {
            // `/` in browse mode: enter the search sub-layer. The input
            // line is already empty (held in `stashed_input`); typing now
            // builds the fuzzy query and re-ranks the active picker's
            // rows. Shared by the Connections and Models pickers.
            if matches!(app.active_modal(), Modal::Connections | Modal::Models) {
                app.model_search = true;
                app.modal_index = 0;
                app.model_scroll = 0;
                app.model_modal_follow = true;
            }
        }
        input::InputAction::ModelExitSearch => {
            // First Esc while searching: drop the query and return to the
            // full browse list. The chat draft stays parked in
            // `stashed_input` until the modal closes for real.
            if matches!(app.active_modal(), Modal::Connections | Modal::Models) {
                app.model_search = false;
                app.input.clear();
                app.set_cursor(0);
                app.input_scroll = 0;
                app.suggestion_index = None;
                app.modal_index = 0;
                app.model_scroll = 0;
                app.model_modal_follow = true;
            }
        }
        input::InputAction::ProviderPickerToggleFavorite => {
            // Models only (gated in input): toggle the favorite on the
            // highlighted MODEL (falling back to the first visible row).
            // Favorite is model-level (ADR-0046), so the id is the
            // model wire id. Sending the request is enough; the backend
            // pushes a fresh snapshot that flips the ★ next frame.
            if app.active_modal() == Modal::Models {
                let ranked = app.models_flat_filtered();
                if let Some(row) = ranked.get(app.modal_index).or_else(|| ranked.first()) {
                    let _ = app.tx.send(AgentRequest::ToggleFavorite {
                        id: row.model.clone(),
                    });
                }
            }
        }
        input::InputAction::OpenModelEditor => {
            modals::handle_open_model_editor(app);
        }
        input::InputAction::ModelEditorNextField => {
            // Cycle focus through the per-model editor's fields: effort
            // (1) ↔ thinking (2). ADR-0046: the provider key editor has
            // only an API-key field, so Tab is a no-op there
            // (it never reaches this branch — `editor_model_settings_only`
            // gates it). The focused text field owns the composer line;
            // the thinking field is a toggle (no text), so it clears the
            // line while focused.
            if app.editor_model_settings_only {
                // The settings editor owns fields 1..=4 (effort, thinking
                // when available, then the capability overrides 3/4).
                // Tab wraps 1 ↔ 4 when the override rows are shown.
                let has_overrides = app.editor_model_settings_only;
                match app.editor_field {
                    1 if app.editor_thinking_available => {
                        app.editor_effort = app.input.clone();
                        app.input.clear();
                        app.set_cursor(0);
                        app.editor_field = 2;
                    }
                    2 if app.editor_thinking_available => {
                        app.input = app.editor_effort.clone();
                        app.set_cursor_end();
                        if has_overrides {
                            app.editor_field = 3;
                        } else {
                            app.editor_field = 1;
                        }
                    }
                    3 => {
                        app.editor_field = 4;
                    }
                    4 => {
                        app.input = app.editor_effort.clone();
                        app.set_cursor_end();
                        app.editor_field = 1;
                    }
                    _ => {
                        app.input = app.editor_effort.clone();
                        app.set_cursor_end();
                        if has_overrides {
                            app.editor_field = 3;
                        } else {
                            app.editor_field = 1;
                        }
                    }
                }
            }
        }
        input::InputAction::ModelEditorEffortCycle { delta } => {
            // Cycle the effort selector through the selected model's
            // supported wire levels, wrapping at both ends. Mirrored
            // into app.input so the renderer shows the live value.
            let model = muta_contracts::resolve_model(&app.editor_model);
            let levels: Vec<&str> = model
                .effort_levels
                .iter()
                .map(|level| level.as_str())
                .collect();
            if levels.is_empty() {
                return ActionFlow::NextEvent;
            }
            let cur = levels
                .iter()
                .position(|l| *l == app.editor_effort)
                .unwrap_or_else(|| {
                    levels
                        .iter()
                        .position(|l| *l == "medium")
                        .or_else(|| levels.iter().position(|l| *l == "high"))
                        .unwrap_or(0)
                }) as isize;
            let n = levels.len() as isize;
            let next = ((cur + delta as isize).rem_euclid(n)) as usize;
            app.editor_effort = levels[next].to_string();
            app.input = app.editor_effort.clone();
            app.set_cursor_end();
        }
        input::InputAction::ModelEditorEffortJump { index } => {
            // Jump straight to a ladder rung (digit on the effort
            // field). Out-of-range digits are ignored so a 7-rung key
            // on a 3-rung ladder is a no-op, never a clamp that would
            // surprise. Mirrors `editor_effort` into `app.input` like
            // the cycle path so the renderer shows the live value.
            let model = muta_contracts::resolve_model(&app.editor_model);
            if let Some(level) = model.effort_levels.get(index) {
                app.editor_effort = level.as_str().to_string();
                app.input = app.editor_effort.clone();
                app.set_cursor_end();
            }
        }
        input::InputAction::ModelEditorThinkingToggle => {
            // Toggle extended thinking on/off (Space). Orthogonal to
            // effort — the two knobs are independent on the wire.
            if app.editor_thinking_available {
                app.editor_thinking = !app.editor_thinking;
            }
        }
        input::InputAction::ModelEditorVisionCycle => {
            // Cycle the vision capability override (ADR-0149 layer 1):
            // inherit → force on → force off → inherit.
            app.editor_vision_override = cycle_tri_state(app.editor_vision_override);
        }
        input::InputAction::ModelEditorToolCycle => {
            // Cycle the tool-call capability override, same tri-state.
            app.editor_tool_override = cycle_tri_state(app.editor_tool_override);
        }
        input::InputAction::SubmitModelEditor => {
            return modals::handle_submit_model_editor(app);
        }
        input::InputAction::Interrupt => {
            // Mirror Ctrl+C's quit pattern: the first Esc only arms a
            // wall-clock 2s window (and shows a toast); the second Esc
            // within that window actually interrupts the running task. A
            // press after the window lapsed starts a fresh window rather
            // than firing a stale confirmation.
            handle_esc_interrupt(app, false);
        }
        input::InputAction::OpenModels => {
            enter_panel(
                app,
                crate::surfaces::PanelId::Models,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::OpenConnections => {
            enter_panel(
                app,
                crate::surfaces::PanelId::Connections,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::OpenPresetChooser => {
            // `a` in Connections opens the curated preset branch.
            // Only meaningful from Connections; ignored otherwise.
            if app.active_modal() == Modal::Connections {
                app.open_preset_chooser();
            }
        }
        input::InputAction::OpenCustomConnection => {
            // `c` in Connections opens the custom branch directly. Custom is
            // deliberately not one of the curated preset rows.
            if app.active_modal() == Modal::Connections {
                app.open_custom_connection_editor();
            }
        }
        input::InputAction::RefreshProviderModels => {
            if app.active_modal() == Modal::Connections && app.connection_info_detail {
                let id = app
                    .connection_detail
                    .as_ref()
                    .map(|d| d.id.clone())
                    .or_else(|| {
                        let providers = app.providers_filtered();
                        providers
                            .get(app.modal_index.min(providers.len().saturating_sub(1)))
                            .map(|p| p.id.clone())
                    });
                if let Some(id) = id {
                    if let Some(detail) = app.connection_detail.as_mut() {
                        detail.usage = muta_contracts::ConnectionUsageState::Fetching;
                    }
                    let _ = app.tx.send(AgentRequest::QueryConnectionDetail { id });
                }
            } else if matches!(app.active_modal(), Modal::Models | Modal::Connections) {
                let _ = app.tx.send(AgentRequest::RefreshProviderModels {
                    user_initiated: true,
                });
            }
        }
        input::InputAction::OpenHistory => {
            enter_panel(
                app,
                crate::surfaces::PanelId::HistorySearch,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::HistoryInsert => {
            // Enter / Tab inside the Ctrl+R panel: pull the focused entry out
            // of `history_rows` (the filtered matches) and drop it into
            // the input box for further editing / sending. The message
            // is not shipped here — the user hits Enter again to send.
            app.save_panel_state(crate::surfaces::PanelId::HistorySearch);
            let ranked = app.history_rows();
            let pick = ranked.get(app.modal_index).or_else(|| ranked.first());
            let Some((orig_idx, _)) = pick else {
                return ActionFlow::Handled;
            };
            let original = *orig_idx;
            let text = app.input_history[original].text.clone();
            // Restore the attachments cached behind this entry (if
            // any) so a re-send ships the real image / paste
            // payloads rather than a bare chip label; with no
            // cache the staged vectors are cleared.
            app.restore_history_attachments(original);
            // The inserted entry becomes the new draft: it is the
            // newest *unsent* input, so ↓ past the newest history
            // row restores it, never a stale remembered draft.
            app.adopt_as_draft(
                text,
                app.pending_images.clone(),
                app.pending_text_pastes.clone(),
                crate::app::DraftAdoption::Replace,
            );
            // The selection replaces the in-progress draft, and the search
            // filter query buffer's task is completed, so query and draft are cleared.
            if let Some(state) = app
                .panels
                .states_mut(&crate::surfaces::PanelId::HistorySearch)
            {
                state.draft = None;
                state.query.clear();
                state.index = 0;
            }
            app.panels.hide(crate::surfaces::PanelId::HistorySearch);
            app.history_search = false;
            app.input_scroll = 0;
            app.suggestion_index = None;
            // A programmatic input replacement — latch the dismissal so
            // a slash-command selection doesn't flash its completion
            // popup until the next real edit.
            app.completion_dismissed = true;
            app.modal_index = 0;
            app.show_chat_surface();
        }
        input::InputAction::HistoryTogglePreview => {
            // Tab inside the Ctrl+R modal: flip between the fuzzy list
            // and a full-text view of the selected entry. Reusing
            // `history_scroll` as the per-entry scroll means entering
            // preview or moving to another entry starts from the top.
            app.history_preview = !app.history_preview;
            app.history_scroll = 0;
            app.history_modal_follow = true;
        }
        input::InputAction::HistoryClearAll => {
            // Ctrl+X inside the Ctrl+R modal: arm the clear-history
            // confirmation. Nothing is deleted yet — the next `y`
            // (or Enter) wipes, any other key cancels.
            app.history_clear_confirm = true;
            let n = app.input_history.len();
            show_local_toast(
                app,
                format!(
                    "Press y to clear all {n} history entr{} (any other key cancels)",
                    if n == 1 { "y" } else { "ies" }
                ),
                false,
                std::time::Duration::from_millis(2600),
            );
        }
        input::InputAction::HistoryClearConfirm => {
            let n = app.input_history.len();
            app.clear_input_history();
            show_local_toast(
                app,
                if n == 0 {
                    "Input history is already empty."
                } else {
                    "Input history cleared."
                },
                false,
                std::time::Duration::from_millis(2600),
            );
        }
        input::InputAction::HistoryClearCancel => {
            app.history_clear_confirm = false;
        }
        input::InputAction::OpenHelp => {
            enter_panel(
                app,
                crate::surfaces::PanelId::Help,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::OpenPermissions => {
            enter_panel(
                app,
                crate::surfaces::PanelId::Permissions,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::OpenTools => {
            enter_panel(
                app,
                crate::surfaces::PanelId::Tools,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::OpenUsage => {
            enter_panel(
                app,
                crate::surfaces::PanelId::UsageStats,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::OpenMcp => {
            enter_panel(
                app,
                crate::surfaces::PanelId::Mcp,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::OpenSkills => {
            enter_panel(
                app,
                crate::surfaces::PanelId::Skills,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::SkillsToggleDetail => {
            // Toggle the detail block of the selected skill row. Re-pressing
            // Enter on an already-expanded row collapses it.
            app.skills_expanded = if app.skills_expanded == Some(app.modal_index) {
                None
            } else {
                Some(app.modal_index)
            };
            app.session_modal_follow = true;
        }
        input::InputAction::OpenConfig => {
            enter_view(app, crate::surfaces::View::Settings, runtime);
        }
        input::InputAction::ConfigFocusToggle => {
            if app.active_modal() == Modal::Config {
                app.config_focus = match app.config_focus {
                    crate::overlays::ConfigFocus::Categories => {
                        crate::overlays::ConfigFocus::Detail
                    }
                    crate::overlays::ConfigFocus::Detail => {
                        crate::overlays::ConfigFocus::Categories
                    }
                };
            }
        }
        input::InputAction::ConfigActivate => {
            if app.active_modal() == Modal::Config {
                let ws_path = if app.current_workspace.is_empty() {
                    None
                } else {
                    Some(std::path::Path::new(&app.current_workspace))
                };
                match app.config_focus {
                    crate::overlays::ConfigFocus::Categories => {
                        app.config_focus = crate::overlays::ConfigFocus::Detail;
                        if app.config_category == 0 {
                            app.config_detail_index = Theme::color_scheme_index_with_workspace(
                                &app.color_scheme,
                                ws_path,
                            );
                        } else {
                            app.config_detail_index = 0;
                        }
                    }
                    crate::overlays::ConfigFocus::Detail => {
                        match app.config_category {
                            0 => {
                                // Appearance category:
                                let schemes =
                                    Theme::available_color_schemes_with_workspace(ws_path);
                                let sel_idx = app.config_detail_index % schemes.len().max(1);
                                if let Some(scheme) = schemes.get(sel_idx) {
                                    let name = &scheme.id;
                                    app.color_scheme = name.to_string();
                                    app.theme = Theme::from_color_scheme_with_workspace(
                                        name.as_ref(),
                                        &app.custom_color_scheme,
                                        ws_path,
                                    );
                                    let _ = app.tx.send(AgentRequest::UpdateTuiColorScheme {
                                        name: app.color_scheme.clone(),
                                        custom: app.custom_color_scheme.clone(),
                                    });
                                    app.save_tui_config();
                                }
                            }
                            1 if app.config_detail_index == 1 => {
                                // Transcript category:
                                app.expand_auto_scroll = !app.expand_auto_scroll;
                                app.save_tui_config();
                            }
                            2 if app.config_detail_index == 0 => {
                                // Behavior category:
                                app.click_outside_dismiss = !app.click_outside_dismiss;
                                app.save_tui_config();
                            }
                            3 | 4 => {
                                let is_search = app.config_category == 3;
                                let connection_count = app
                                    .websearch_config
                                    .as_ref()
                                    .map(|ws| {
                                        if is_search {
                                            ws.search_connections.len()
                                        } else {
                                            ws.reader_connections.len()
                                        }
                                    })
                                    .unwrap_or(0);
                                match app.config_detail_index {
                                    0 => {
                                        let anchor =
                                            crate::components::dropdown::DropdownAnchor::center_screen();
                                        if is_search {
                                            let current = app
                                                .websearch_config
                                                .as_ref()
                                                .map(|ws| ws.provider.as_str())
                                                .unwrap_or("exa");
                                            let dropdown =
                                                crate::views::settings::build_websearch_provider_dropdown(
                                                    current,
                                                    app.websearch_config.as_ref(),
                                                );
                                            app.config_dropdown = Some((dropdown, anchor));
                                        } else {
                                            let current = app
                                                .websearch_config
                                                .as_ref()
                                                .map(|ws| ws.reader.as_str())
                                                .unwrap_or("none");
                                            let dropdown =
                                                crate::views::settings::build_websearch_reader_dropdown(
                                                    current,
                                                    app.websearch_config.as_ref(),
                                                );
                                            app.config_dropdown = Some((dropdown, anchor));
                                        }
                                    }
                                    1 => {
                                        let current = app
                                            .websearch_config
                                            .as_ref()
                                            .map(|ws| ws.timeout_secs)
                                            .unwrap_or(20);
                                        let next = if current >= 120 {
                                            5
                                        } else {
                                            (current + 5).max(5)
                                        };
                                        let _ = app.tx.send(AgentRequest::UpdateWebSearchConfig(
                                            Box::new(muta_contracts::WebSearchConfigUpdate {
                                                timeout_secs: Some(next),
                                                ..Default::default()
                                            }),
                                        ));
                                    }
                                    idx if idx < 2 + connection_count => {
                                        let conn_idx = idx - 2;
                                        if is_search {
                                            if let Some(conn) = app
                                                .websearch_config
                                                .as_ref()
                                                .and_then(|ws| ws.search_connections.get(conn_idx))
                                            {
                                                let _ =
                                                    app.tx
                                                        .send(AgentRequest::UpdateWebSearchConfig(
                                                        Box::new(
                                                            muta_contracts::WebSearchConfigUpdate {
                                                                provider: Some(conn.id.clone()),
                                                                ..Default::default()
                                                            },
                                                        ),
                                                    ));
                                            }
                                        } else if let Some(conn) = app
                                            .websearch_config
                                            .as_ref()
                                            .and_then(|ws| ws.reader_connections.get(conn_idx))
                                        {
                                            let _ = app.tx.send(
                                                AgentRequest::UpdateWebSearchConfig(Box::new(
                                                    muta_contracts::WebSearchConfigUpdate {
                                                        reader: Some(conn.id.clone()),
                                                        ..Default::default()
                                                    },
                                                )),
                                            );
                                        }
                                    }
                                    idx if idx == 2 + connection_count => {
                                        let dropdown =
                                            crate::views::settings::build_add_web_connection_dropdown(
                                                usize::from(!is_search),
                                            );
                                        let anchor =
                                            crate::components::dropdown::DropdownAnchor::center_screen();
                                        app.config_dropdown = Some((dropdown, anchor));
                                    }
                                    _ => {}
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        input::InputAction::ConfigDeleteConnection => {
            if app.active_modal() == Modal::Config
                && matches!(app.config_category, 3 | 4)
                && app.config_detail_index >= 2
            {
                let conn_idx = app.config_detail_index - 2;
                if app.config_category == 3 {
                    if let Some(conn) = app
                        .websearch_config
                        .as_ref()
                        .and_then(|ws| ws.search_connections.get(conn_idx))
                    {
                        let _ = app.tx.send(AgentRequest::UpdateWebSearchConfig(Box::new(
                            muta_contracts::WebSearchConfigUpdate {
                                delete_search_connection: Some(conn.id.clone()),
                                ..Default::default()
                            },
                        )));
                    }
                } else if let Some(conn) = app
                    .websearch_config
                    .as_ref()
                    .and_then(|ws| ws.reader_connections.get(conn_idx))
                {
                    let _ = app.tx.send(AgentRequest::UpdateWebSearchConfig(Box::new(
                        muta_contracts::WebSearchConfigUpdate {
                            delete_reader_connection: Some(conn.id.clone()),
                            ..Default::default()
                        },
                    )));
                }
            }
        }
        input::InputAction::ConfigSegmentPrev => {
            if app.active_modal() == Modal::Config && app.config_category == 4 {
                app.config_category = 3;
                app.config_detail_index = 0;
                app.config_detail_scroll = 0;
            }
        }
        input::InputAction::ConfigSegmentNext => {
            if app.active_modal() == Modal::Config && app.config_category == 3 {
                app.config_category = 4;
                app.config_detail_index = 0;
                app.config_detail_scroll = 0;
            }
        }
        input::InputAction::ConfigBack => {
            if app.active_modal() == Modal::Config {
                if app.config_dropdown.is_some() {
                    app.config_dropdown = None;
                } else if app.config_focus == crate::overlays::ConfigFocus::Detail {
                    if app.config_category == 0 {
                        // Revert preview theme to persisted color scheme
                        let ws_path = if app.current_workspace.is_empty() {
                            None
                        } else {
                            Some(std::path::Path::new(&app.current_workspace))
                        };
                        app.theme = Theme::from_color_scheme_with_workspace(
                            &app.color_scheme,
                            &app.custom_color_scheme,
                            ws_path,
                        );
                        app.config_detail_index =
                            Theme::color_scheme_index_with_workspace(&app.color_scheme, ws_path);
                    }
                    app.config_focus = crate::overlays::ConfigFocus::Categories;
                } else {
                    app.dismiss_surface();
                }
            }
        }
        input::InputAction::McpToggle => {
            // Connect/disconnect the selected server for the session.
            // The "enabled intent" is the inverse of its disabled flag;
            // the harness replies with a fresh snapshot.
            if let Some(server) = app
                .session_context
                .as_ref()
                .and_then(|s| s.mcp.get(app.modal_index))
            {
                let _ = app.tx.send(AgentRequest::ToggleMcpServer {
                    name: server.name.clone(),
                    enabled: server.disabled,
                });
            }
        }
        input::InputAction::McpReconnect => {
            // Reconnect the selected server on demand. The harness
            // replies with a fresh snapshot reflecting the new status.
            if let Some(server) = app
                .session_context
                .as_ref()
                .and_then(|s| s.mcp.get(app.modal_index))
            {
                let _ = app.tx.send(AgentRequest::ReconnectMcpServer {
                    name: server.name.clone(),
                });
            }
        }
        input::InputAction::PermissionsActivate => {
            // Revoke the selected "always allow" rule. The harness
            // replies with a fresh snapshot so the list re-renders.
            if let Some(snapshot) = app.session_context.as_ref()
                && let Some(rule) = snapshot.permissions.get(app.modal_index)
            {
                let _ = app.tx.send(AgentRequest::RevokePermission {
                    tool: rule.tool.clone(),
                    scope: rule.scope.clone(),
                });
            }
        }
        input::InputAction::PermissionsClearAll => {
            // Clear every cached rule. The harness replies with a fresh
            // (empty) snapshot.
            let _ = app.tx.send(AgentRequest::ClearAllPermissions);
            app.modal_index = 0;
        }
        input::InputAction::SessionSelect { forward } => {
            // Move the selection cursor (the body scroll follows it).
            // The list is the tools list, except in the MCP manager
            // where it is the configured-server list. When empty (still
            // loading / none), Up/Down scrolls the body directly so the
            // other content stays reachable.
            let list_len = if app.active_modal() == Modal::Mcp {
                app.session_context
                    .as_ref()
                    .map(|s| s.mcp.len())
                    .unwrap_or(0)
            } else if app.active_modal() == Modal::Skills {
                app.session_context
                    .as_ref()
                    .map(|s| s.skills.len())
                    .unwrap_or(0)
            } else if app.active_modal() == Modal::Queue {
                app.pending_dispatch
                    .iter()
                    .filter(|item| item.session_id == viewed_session_id)
                    .count()
            } else if app.active_modal() == Modal::Btw {
                app.btw_list.len()
            } else {
                app.session_tools_len()
            };
            if list_len > 0 {
                app.modal_index = if forward {
                    (app.modal_index + 1) % list_len
                } else if app.modal_index == 0 {
                    list_len - 1
                } else {
                    app.modal_index - 1
                };
                // The queue modal tracks its own follow flag so it can
                // be scrolled independently of the shared session
                // scroll the other list modals reuse.
                if app.active_modal() == Modal::Queue || app.active_modal() == Modal::Btw {
                    app.queue_modal_follow = true;
                } else {
                    app.session_modal_follow = true;
                }
            } else if app.active_modal() == Modal::Queue {
                // Empty queue: Up/Down is inert.
            } else if app.active_modal() == Modal::Btw {
                // Empty asides list: Up/Down is inert.
            } else {
                app.session_scroll = if forward {
                    app.session_scroll.saturating_add(1)
                } else {
                    app.session_scroll.saturating_sub(1)
                };
            }
        }
        input::InputAction::SessionActivate => {
            // Toggle the selected tool. The request is sent through the
            // normal agent channel; the harness replies with a fresh
            // snapshot that re-renders the dashboard.
            if let Some(req) = app.session_activate_request() {
                let _ = app.tx.send(req);
            }
        }
        input::InputAction::OpenSelectedSession => {
            if let Some(session) = app.sessions_overview.get(
                app.modal_index
                    .min(app.sessions_overview.len().saturating_sub(1)),
            ) {
                let id = session.id.clone();
                app.hide_active_panel();
                app.modal_index = 0;
                // A session was chosen from the startup picker, so a
                // real conversation now backs the view: subsequent
                // `/sessions` modals should behave as ordinary
                // transient overlays (Esc = dismiss, not quit).
                app.startup_overlay = crate::StartupOverlay::None;
                let _ = app
                    .tx
                    .send(AgentRequest::SlashCommand(format!("/sessions {}", id)));
            }
        }
        input::InputAction::HostPreviewSelected => {
            // Enter on a dock selection opens the read-only preview
            // modal. Selection alone never opens it; Esc closes.
            let idx = app
                .modal_index
                .min(app.host_sessions.len().saturating_sub(1));
            let order = crate::overlays::creation_order(&app.host_sessions);
            if let Some(row) = order.get(idx).map(|&i| &app.host_sessions[i]) {
                app.host_preview = Some(row.id.clone());
                app.host_preview_scroll = 0;
            }
        }
        input::InputAction::HostSwitchSelected => {
            let idx = app
                .modal_index
                .min(app.host_sessions.len().saturating_sub(1));
            // The dock renders sessions in creation order (`#seq`);
            // the selection indexes that sequence, not the raw
            // newest-first snapshot.
            let order = crate::overlays::creation_order(&app.host_sessions);
            if let Some(row) = order.get(idx).map(|&i| &app.host_sessions[i]) {
                // Switching to the current session is a no-op.
                let switchable = row.id != viewed_session_id;
                if switchable {
                    app.switch_to_target = Some(row.id.clone());
                    app.should_quit.store(true, Ordering::SeqCst);
                }
                app.hide_active_panel();
                app.modal_index = 0;
                app.host_prompting = false;
            }
        }
        input::InputAction::HostFocusToggle => {
            app.host_focus = match app.host_focus {
                crate::overlays::DashboardFocus::List => crate::overlays::DashboardFocus::Detail,
                crate::overlays::DashboardFocus::Detail => crate::overlays::DashboardFocus::List,
            };
        }
        input::InputAction::HostInterruptSelected => {
            // `i` on the dock: interrupt the selection. Routed through the
            // console dispatcher so the dispatch line + receipt land in the
            // cockpit log alongside `/interrupt`.
            host::dispatch_console_command(app, runtime, "/interrupt", false).await;
        }
        input::InputAction::HostKillSelected => {
            // `k` on the dock: two-press confirm, then the kill verb.
            host::kill_selected(app, runtime);
        }
        input::InputAction::HostSuspendSelected => {
            // `s` on the dock: suspend the selection (park in memory).
            host::suspend_selected(app, runtime);
        }
        input::InputAction::HostPromptOpen => {
            // `p`: prompt the selected session. The composer buffer
            // becomes the task text.
            app.host_prompting = true;
            app.host_prompt_new = false;
            app.input.clear();
            app.set_cursor(0);
        }
        input::InputAction::HostNewSession => {
            // `n`: create a new session with the text as opening task.
            app.host_prompting = true;
            app.host_prompt_new = true;
            app.input.clear();
            app.set_cursor(0);
        }
        input::InputAction::HostPromptSeed(c) => {
            // A printable key on the dashboard opens the console composer
            // with that key as the first character — typing is opening.
            // The seeded role is "prompt the selection" (the `p` default):
            // an explicit `@N`/`/verb` in the line routes itself anyway.
            app.host_prompting = true;
            app.host_prompt_new = false;
            app.input.clear();
            app.input.insert(0, c);
            app.set_cursor(1);
        }
        input::InputAction::HostPromptSubmit => {
            let text = app.input.trim().to_string();
            // The `n`-opened prompt's default role is "create"; an explicit
            // address or verb in the text overrides it (`@3 …` still routes
            // to #3, `/kill` still kills).
            let create_new = app.host_prompt_new;
            app.host_prompting = false;
            app.host_prompt_new = false;
            app.input.clear();
            app.set_cursor(0);
            if text.is_empty() {
                return ActionFlow::Handled;
            }
            // The composer is a command line now (ADR-0097 §2 grammar plus
            // slash verbs): `@3 text` addresses, `/kill`-family manages,
            // bare text keeps the legacy role — prompt the selection, or
            // create when the prompt was opened with `n`.
            host::dispatch_console_command(app, runtime, &text, create_new).await;
        }
        input::InputAction::DeleteSelectedSession => {
            let idx = app
                .modal_index
                .min(app.sessions_overview.len().saturating_sub(1));
            if idx < app.sessions_overview.len() {
                let deleted = app.sessions_overview.remove(idx);
                app.modal_index = app
                    .modal_index
                    .min(app.sessions_overview.len().saturating_sub(1));
                let _ = app.tx.send(AgentRequest::DeleteSession { id: deleted.id });
            }
        }
        input::InputAction::CreateNewSession => {
            app.startup_overlay = crate::StartupOverlay::None;
            app.hide_active_panel();
            let _ = app.tx.send(AgentRequest::SlashCommand("/new".to_string()));
        }
        input::InputAction::OpenSessionInfo => {
            // Drill into the session-info sub-view for the highlighted
            // row. Request the full detail (complete last prompt,
            // timestamps) on demand — the picker rows only carry a
            // truncated preview. While the round-trip is in flight the
            // body shows a loading state.
            if let Some(session) = app.sessions_overview.get(
                app.modal_index
                    .min(app.sessions_overview.len().saturating_sub(1)),
            ) {
                app.session_info_detail = true;
                app.session_detail = None;
                app.session_info_scroll = 0;
                let _ = app.tx.send(AgentRequest::QuerySessionDetail {
                    id: session.id.clone(),
                });
            }
        }
        input::InputAction::OpenConnectionDetail => {
            let providers = app.providers_filtered();
            if let Some(ranked) =
                providers.get(app.modal_index.min(providers.len().saturating_sub(1)))
            {
                app.connection_info_detail = true;
                app.connection_info_standalone = false;
                app.connection_detail = None;
                app.connection_info_scroll = 0;
                let _ = app.tx.send(AgentRequest::QueryConnectionDetail {
                    id: ranked.id.clone(),
                });
            }
        }
        input::InputAction::OpenActiveConnectionDetail => {
            open_active_connection_detail(app, runtime, viewed_session_id);
        }
        input::InputAction::CloseModal => {
            modals::handle_close_modal(app, viewed_session_id);
        }
        input::InputAction::TelemetryActivate => {
            if app.active_modal() == Modal::Telemetry {
                if app.telemetry_tab == crate::modal::TelemetryTab::Overview {
                    app.telemetry_tab = crate::modal::TelemetryTab::Activity;
                    app.telemetry_scroll = 0;
                } else if !app.telemetry_detail {
                    let has_rounds = app
                        .token_source_report(viewed_session_id)
                        .map(|report| view::telemetry_round_count(&report) > 0)
                        .unwrap_or(false);
                    if has_rounds {
                        app.telemetry_detail = true;
                        app.telemetry_turn_cursor = 0;
                        app.telemetry_scroll = 0;
                    }
                } else if app.telemetry_turn.is_none() {
                    let report = app.token_source_report(viewed_session_id);
                    let round_index = app.modal_index.min(
                        report
                            .as_ref()
                            .map(|report| view::telemetry_round_count(report).saturating_sub(1))
                            .unwrap_or(0),
                    );
                    if let Some(key) = report.as_ref().and_then(|report| {
                        view::telemetry_attempt_key(report, round_index, app.telemetry_turn_cursor)
                    }) {
                        app.telemetry_turn = Some(key);
                        app.telemetry_scroll = 0;
                    }
                }
            }
        }
        input::InputAction::TelemetryNextTab => {
            if app.active_modal() == Modal::Telemetry {
                app.telemetry_tab = match app.telemetry_tab {
                    crate::modal::TelemetryTab::Overview => crate::modal::TelemetryTab::Activity,
                    crate::modal::TelemetryTab::Activity => crate::modal::TelemetryTab::Overview,
                };
                app.telemetry_scroll = 0;
            }
        }
        input::InputAction::TelemetryPrevTab => {
            if app.active_modal() == Modal::Telemetry {
                app.telemetry_tab = match app.telemetry_tab {
                    crate::modal::TelemetryTab::Overview => crate::modal::TelemetryTab::Activity,
                    crate::modal::TelemetryTab::Activity => crate::modal::TelemetryTab::Overview,
                };
                app.telemetry_scroll = 0;
            }
        }
        input::InputAction::TelemetrySetTab(tab) => {
            if app.active_modal() == Modal::Telemetry && app.telemetry_tab != tab {
                app.telemetry_tab = tab;
                app.telemetry_scroll = 0;
            }
        }
        input::InputAction::ScrollUp => {
            scroll_tick(app, false);
        }
        input::InputAction::ScrollDown => {
            scroll_tick(app, true);
        }
        input::InputAction::Wheel { up, x, y } => {
            handle_wheel(app, up, x, y);
        }
        input::InputAction::ScrollPageUp => {
            // Read the (Copy) page step up front so the subsequent
            // mutable borrow of the scroll field doesn't conflict.
            let step = modal_page_step(app);
            if let Some((scroll, follow)) = app.modal_scroll_field() {
                if let Some(f) = follow {
                    *f = false;
                }
                *scroll = scroll.saturating_sub(step);
            } else {
                scroll_transcript_page(app, false);
            }
        }
        input::InputAction::ScrollPageDown => {
            // Read the (Copy) page step up front so the subsequent
            // mutable borrow of the scroll field doesn't conflict.
            let step = modal_page_step(app);
            if let Some((scroll, follow)) = app.modal_scroll_field() {
                if let Some(f) = follow {
                    *f = false;
                }
                *scroll = scroll.saturating_add(step);
            } else {
                scroll_transcript_page(app, true);
            }
        }
        input::InputAction::ScrollTop => {
            if let Some((scroll, follow)) = app.modal_scroll_field() {
                if let Some(f) = follow {
                    *f = false;
                }
                *scroll = 0;
            } else {
                scroll_transcript_to_edge(app, false);
            }
        }
        input::InputAction::ScrollBottom => {
            // Modal scroll bounds are clamped by render_body each
            // frame, so a large number here just means "go to end".
            if let Some((scroll, follow)) = app.modal_scroll_field() {
                if let Some(f) = follow {
                    *f = false;
                }
                *scroll = usize::MAX;
            } else {
                scroll_transcript_to_edge(app, true);
            }
        }
        input::InputAction::PermissionDetailsUp => {
            app.permission_scroll = app.permission_scroll.saturating_sub(1);
        }
        input::InputAction::PermissionDetailsDown => {
            app.permission_scroll = app
                .permission_scroll
                .saturating_add(1)
                .min(app.permission_max_scroll);
        }
        input::InputAction::CopySelection => {
            if let Some(text) = extract_selection_text(
                &app.selection,
                app.focused_messages(),
                &app.input,
                &app.layout_map,
                app.drag.cell_info.as_ref(),
            ) {
                clipboard_ops::spawn_clipboard_copy(copy_tx, copy_pending.clone(), text);
            }
        }
        input::InputAction::CtrlC => {
            return commands::handle_ctrl_c(app, viewed_session_id, copy_tx, copy_pending);
        }
        input::InputAction::OpenTodos => {
            // Ctrl+T opens the Todos modal — the agent's live task
            // list surfaced on its own overlay. The list is
            // agent-owned and read-only in the TUI; this opens the
            // Activity view pinned to the Todos section, exactly
            // like clicking the todo bar. A retained view (ADR-0133):
            // reopen restores the retained scroll.
            enter_panel(
                app,
                crate::surfaces::PanelId::Todos,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::OpenQueue => {
            // F2 opens the queue overview — the full outbox list that
            // the persistent queue bar previews. The selection starts
            // at the front (the next item to pop). This mirrors a
            // click on the queue bar.
            //
            // A retained view (ADR-0133 phase 4): the cursor/scroll
            // survive hide; the auto-block runs on EVERY entry (not just
            // first open) because it is an editing safety latch — the
            // matching resume is the view's exit hook (hide). A
            // persistent user block is a different thing (`F3` /
            // Ctrl+P), unaffected here.
            enter_panel(
                app,
                crate::surfaces::PanelId::Queue,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::OpenTelemetry => {
            // Ctrl+O opens the session telemetry report (Context & Performance).
            enter_panel(
                app,
                crate::surfaces::PanelId::Telemetry,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::FocusNextTarget => {
            app.session_focus = crate::app::SessionFocusRegion::Transcript;
            app.focus_interactive_target(1);
        }
        input::InputAction::FocusPrevTarget => {
            app.session_focus = crate::app::SessionFocusRegion::Transcript;
            app.focus_interactive_target(-1);
        }
        input::InputAction::ClearFocusedTarget => {
            app.session_focus = crate::app::SessionFocusRegion::Composer;
            app.focused_target = None;
        }
        input::InputAction::ActivateFocusedTarget => {
            if let Some(target) = app.focused_target {
                match target.kind {
                    InteractiveTargetKind::ToolStep => {
                        let mut messages = runtime.messages.write().await;
                        let enter_id = resolve_focused_mut(
                            &mut messages,
                            &app.focus_stack,
                            target.message_idx,
                        )
                        .and_then(|message| {
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
                            // Enter mirrors the mouse click on a tool
                            // step's summary: toggle its inline
                            // disclosure (expand/collapse) rather than
                            // popping a modal. Keeping keyboard and
                            // pointer parity is the expected behavior
                            // for the disclosure affordance.
                            app.toggle_step_pinned(&mut messages, target.message_idx);
                            drop(messages);
                        }
                    }
                    InteractiveTargetKind::Thinking => {
                        let mut messages = runtime.messages.write().await;
                        let toggled = app.toggle_step_pinned(&mut messages, target.message_idx);
                        drop(messages);
                        if toggled {
                            app.selection = SelectionState::None;
                        }
                    }
                    InteractiveTargetKind::ProviderRetry => {
                        let mut messages = runtime.messages.write().await;
                        let toggled = app.toggle_step_pinned(&mut messages, target.message_idx);
                        drop(messages);
                        if toggled {
                            app.selection = SelectionState::None;
                        }
                    }
                    InteractiveTargetKind::CommandResult => {
                        // Enter mirrors the mouse click on a command
                        // row: toggle its expandable result body.
                        let mut messages = runtime.messages.write().await;
                        let toggled = app.toggle_step_pinned(&mut messages, target.message_idx);
                        drop(messages);
                        if toggled {
                            app.selection = SelectionState::None;
                        }
                    }
                    InteractiveTargetKind::Notice => {
                        // Enter mirrors the mouse click on a notice header:
                        // toggle its expandable detail/JSON body.
                        let mut messages = runtime.messages.write().await;
                        let toggled = app.toggle_step_pinned(&mut messages, target.message_idx);
                        drop(messages);
                        if toggled {
                            app.selection = SelectionState::None;
                        }
                    }
                }
            }
        }
        input::InputAction::Paste => {
            // Ctrl+V: read the system clipboard off the event loop.
            // The result is delivered back through `paste_rx` and
            // applied on a later frame (image -> attach, text ->
            // insert on the main prompt, or inline splice into the
            // focused modal field). `apply_clipboard_paste` branches
            // on the active modal at apply time, so a paste spawned
            // inside a modal that the user closed before the read
            // returned lands in the main prompt rather than being
            // dropped.
            clipboard_ops::spawn_clipboard_paste(paste_tx);
        }
        input::InputAction::BracketedPaste(text) => {
            // Terminal-level paste (bracketed paste mode). The payload
            // is already in hand, so route it directly through the same
            // chip-or-inline logic as Ctrl+V without an async hop.
            clipboard_ops::apply_clipboard_paste(app, clipboard::ClipboardRead::Text(text));
        }
        input::InputAction::ExitRunner => {
            app.exit_runner();
        }
        input::InputAction::ExitSideView => {
            // `/btw`: detach from the aside view and return to the primary
            // transcript (ADR-0103). Optimistically flip the view for
            // snappiness and tell the harness to detach its active pointer —
            // the aside keeps running. `SideViewClosed` is the backstop in
            // case this fires twice. The interrupt arm is cleared too so the
            // main view's next Esc starts a fresh interrupt confirmation
            // instead of firing a leftover armed state.
            if app.in_side_view {
                app.exit_side_view();
                app.arm_esc(None);
                let _ = app.tx.send(AgentRequest::ExitSideView);
            }
        }
        input::InputAction::InterruptSide => {
            // Esc inside an aside view (ADR-0103 §2): interrupt the viewed
            // aside's round with the same armed press-twice contract as the
            // main view's Esc interrupt. Never leaves the view, never closes
            // the aside.
            handle_esc_interrupt(app, true);
        }
        input::InputAction::OpenBtwList => {
            // F5 / `/btw list` (ADR-0103 §5): ask the harness for a fresh
            // list and pop the modal once the rows land. The open signal is
            // consumed by the loop's sync stage, so a slow harness reply
            // simply opens with the last known rows and refreshes in place.
            enter_panel(
                app,
                crate::surfaces::PanelId::Btw,
                runtime,
                viewed_session_id,
            );
        }
        input::InputAction::ViewSwitcherFilter { ch } => {
            if app.active_modal() == Modal::ViewSwitcher {
                app.command_palette_query.push(ch);
                app.command_palette_selected = 0;
                app.command_palette_scroll = 0;
                app.session_scroll = 0;
                app.session_modal_follow = true;
            }
        }
        input::InputAction::ViewSwitcherBackspace => {
            if app.active_modal() == Modal::ViewSwitcher {
                if !app.command_palette_query.is_empty() {
                    let start = mutx_engine::text::floor_grapheme_boundary(
                        &app.command_palette_query,
                        app.command_palette_query.len() - 1,
                    );
                    app.command_palette_query.truncate(start);
                }
                app.command_palette_selected = 0;
                app.command_palette_scroll = 0;
                app.session_scroll = 0;
                app.session_modal_follow = true;
            }
        }
        input::InputAction::ViewSwitcherToggle => {
            if app.active_modal() == Modal::ViewSwitcher {
                app.dismiss_surface();
            } else if app.can_open_view_switcher() {
                if app.saved_focus.is_none() {
                    app.saved_focus = Some(app.session_focus);
                }
                app.push_transient_surface(Modal::ViewSwitcher);
                app.command_palette_query.clear();
                app.command_palette_selected = 0;
                app.command_palette_scroll = 0;
                app.session_scroll = 0;
                app.session_modal_follow = true;
            }
        }
        input::InputAction::ViewSwitchActivate => {
            if app.active_modal() != Modal::ViewSwitcher {
                return ActionFlow::Handled;
            }
            let is_busy = app.running_sessions.contains(viewed_session_id);
            let app_ctx = crate::keymap::AppContext {
                active_view: app.current_view(),
                active_modal: app.active_modal(),
                session_focus: app.session_focus,
                is_responding: is_busy,
                has_input: !app.input.is_empty(),
                has_selection: !matches!(
                    app.selection,
                    crate::model::selection::SelectionState::None
                ),
                has_running_task: is_busy,
                in_runner_view: app.in_runner_view(),
                in_side_view: app.in_side_view,
                queue_count: app.pending_dispatch.len(),
                has_focused_target: app.focused_target.is_some(),
            };
            let entries = crate::overlays::command_palette::filter_palette_commands(
                &app.command_palette_query,
                &app.recent_commands,
                &app_ctx,
            );
            if let Some(entry) = entries
                .get(app.command_palette_selected)
                .or_else(|| entries.first())
            {
                if matches!(entry.availability, crate::keymap::Availability::Available) {
                    let cmd_id = entry.spec.id;
                    if let Some(pos) = app.recent_commands.iter().position(|&id| id == cmd_id) {
                        app.recent_commands.remove(pos);
                    }
                    app.recent_commands.insert(0, cmd_id);
                    if app.recent_commands.len() > 10 {
                        app.recent_commands.truncate(10);
                    }

                    app.pop_transient_surface();
                    return execute_command_by_id(
                        app,
                        runtime,
                        session,
                        viewed_session_id,
                        copy_tx,
                        copy_pending,
                        cmd_id,
                    )
                    .await;
                }
            }
        }
        input::InputAction::ViewCloseSelected => {
            if app.active_modal() != Modal::ViewSwitcher {
                return ActionFlow::Handled;
            }
        }
        input::InputAction::BtwFocusSelected => {
            // Asides modal Enter (ADR-0103 §5): jump back into the selected
            // aside. The harness replies with `SideViewOpened` carrying the
            // full transcript back-fill; the modal closes on arrival.
            if let Some(row) = app.btw_list.get(app.modal_index) {
                let side_id = row.id.clone();
                app.hide_active_panel();
                let _ = app.tx.send(AgentRequest::FocusSide { side_id });
            }
        }
        input::InputAction::BtwCloseSelected => {
            // Asides modal `D` (ADR-0103 §5): close + discard the selected
            // aside (cancel its round, drop it from the list, delete its
            // session files). The modal stays open on the refreshed list.
            if let Some(row) = app.btw_list.get(app.modal_index) {
                let side_id = row.id.clone();
                let _ = app.tx.send(AgentRequest::CloseSide { side_id });
                // Optimistically drop the row so the selection does not
                // point at a stale entry before the fresh list lands; clamp
                // the cursor in case the last row was removed.
                app.btw_list.remove(app.modal_index);
                if app.modal_index >= app.btw_list.len() {
                    app.modal_index = app.btw_list.len().saturating_sub(1);
                }
            }
        }
        input::InputAction::PrevSibling => {
            app.cycle_sibling(-1);
        }
        input::InputAction::NextSibling => {
            app.cycle_sibling(1);
        }
        input::InputAction::InsertChar(c) => {
            // Already handled by process_event mutating app.input
            let _ = c;
            app.suggestion_index = None;
            // The user is editing again, so live completions are
            // once again useful — clear the Enter-commit dismissal.
            app.completion_dismissed = false;
            // Typing into the input box reclaims it as the active
            // surface: drop any transcript-step focus so the composer
            // re-brightens and the next arrow key resumes caret movement
            // rather than step navigation.
            app.focused_target = None;
            // Reconcile attachments: if the user typed inside a chip
            // (breaking its syntax) the backing staged entry must be
            // dropped, and surviving chips relabeled.
            app.reconcile_attachments();
        }
        input::InputAction::Backspace => {
            app.suggestion_index = None;
            app.completion_dismissed = false;
            // Same as InsertChar: editing the input box reclaims focus
            // from any transcript step.
            app.focused_target = None;
            // Reconcile attachments: a chip-aware backspace has
            // already spliced the chip out of `app.input`; this
            // drops the orphaned entry from `pending_images` /
            // `pending_text_pastes` and relabels survivors.
            app.reconcile_attachments();
        }
        input::InputAction::DeleteForward => {
            // Forward delete runs the same post-edit passes as Backspace: the
            // text mutation already happened in `process_event`; this arm
            // only keeps the completion latch, focus ownership, and staged
            // attachments consistent with the new buffer (a chip-aware
            // forward delete may have orphaned a staged entry).
            app.suggestion_index = None;
            app.completion_dismissed = false;
            // Editing the input box reclaims focus from any transcript step,
            // mirroring Backspace.
            app.focused_target = None;
            app.reconcile_attachments();
        }
        input::InputAction::SuggestNext => {
            let count = app.completions().len();
            if count > 0 {
                let next = match app.suggestion_index {
                    Some(i) => (i + 1) % count,
                    None => 0,
                };
                app.suggestion_index = Some(next);
            }
        }
        input::InputAction::SuggestPrev => {
            let count = app.completions().len();
            if count > 0 {
                let prev = match app.suggestion_index {
                    Some(i) => {
                        if i == 0 {
                            count - 1
                        } else {
                            i - 1
                        }
                    }
                    None => count - 1,
                };
                app.suggestion_index = Some(prev);
            }
        }
        input::InputAction::AcceptSuggestion(idx_str) => {
            if let Ok(idx) = idx_str.parse::<usize>() {
                app.accept_completion(idx);
            }
            // Legacy accept-without-closing arm (no longer bound to a key at
            // the top level — Tab now commits like Enter). Kept for callers
            // that want a live splice; the popup stays open only for
            // directory descents, which accept_completion decides by kind.
        }
        input::InputAction::CommitSuggestion(idx_str) => {
            if let Ok(idx) = idx_str.parse::<usize>() {
                app.accept_completion(idx);
            }
            // Enter — and now Tab — always "finish" the completion
            // regardless of kind: drop the highlight and latch the
            // dismissal flag so the popup stays hidden until the next edit.
            // For slash commands and path files this mirrors what
            // accept_completion already did; for a path *directory* accept
            // (which stays live so Tab can keep descending) it is the
            // commit-specific close. Tab re-opens via
            // ReopenCompletion, so closing here costs nothing.
            app.suggestion_index = None;
            app.completion_dismissed = true;
        }
        input::InputAction::ReopenCompletion => {
            // The other half of the Esc/Tab toggle: bring a dismissed
            // completion menu back without accepting anything. The next
            // anchor pass (post-dispatch, same iteration) seeds the
            // highlight onto the first candidate, so the reopened menu
            // lands already selected with its details flyout showing.
            app.completion_dismissed = false;
        }
        input::InputAction::CloseCompletion => {
            // Esc dismisses the popup without accepting anything.
            // Same latch as Enter-commit so the popup stays hidden
            // until the next edit clears `completion_dismissed` — or
            // until Tab re-opens it (ReopenCompletion).
            app.suggestion_index = None;
            app.completion_dismissed = true;
        }
        input::InputAction::HistoryPrev => {
            // Inline ↑ walks the **current session's** history only
            // (newest-first), not the whole cross-session log — Ctrl+R
            // is the global search surface. We recompute the session
            // slice each press so newly-recorded entries appear
            // without a restart; `history_index` is a position into
            // that slice. `App::history_prev` advances toward older
            // entries and stashes the in-progress draft on the first
            // press (so ↓ can restore it).
            let session_rows = app.current_session_history();
            app.history_prev(&session_rows);
        }
        input::InputAction::RecallQueued => {
            // Destructive recall for the queue modal's explicit pull-to-composer
            // gesture, where removing the item from the list is the point.
            if let Some(crate::app::RecallQueued::Restored(dispatch)) =
                app.recall_queued(viewed_session_id)
            {
                app.restore_dispatch(dispatch);
            }
        }
        input::InputAction::RecallQueuedSelected => {
            // The queue modal's `Enter` recalls the *selected* item
            // (the `↑/↓` highlight, not always the newest) into the
            // composer and closes the modal. Closing resumes the
            // auto-block the modal set on open.
            let idx = app.modal_index;
            app.hide_active_panel();
            if let Some(crate::app::RecallQueued::Restored(dispatch)) =
                app.recall_queued_at(viewed_session_id, idx)
            {
                app.restore_dispatch(dispatch);
            }
        }
        input::InputAction::QueueToggleBlock => {
            // `Ctrl+P` (top-level or inside the queue modal): toggle the
            // hard block on the viewed session's outbox. While blocked
            // no queued message auto-drains, even after the round
            // completes. This is the persistent user choice, distinct
            // from the modal's editing-safety auto-block.
            app.toggle_queue_block(viewed_session_id);
        }
        input::InputAction::QueueDelete => {
            // `D` in the queue modal: remove the highlighted
            // item outright. The queue is auto-blocked on open, so the
            // index can't drift under us. Clamp the selection to the
            // now-shorter list.
            if app.active_modal() == Modal::Queue {
                let idx = app.modal_index;
                let _removed = app.remove_queued_at(viewed_session_id, idx);
                let count = app.pending_count(viewed_session_id);
                if count == 0 {
                    app.modal_index = 0;
                } else if app.modal_index >= count {
                    app.modal_index = count - 1;
                }
                app.queue_modal_follow = true;
            }
        }
        input::InputAction::QueueMoveItem { delta } => {
            // `K`/`J` in the queue modal: reorder the highlighted item
            // toward the front (next to pop) or the tail. Clamp at the
            // session slice boundaries so it can't escape into another
            // session's items.
            if app.active_modal() == Modal::Queue {
                let idx = app.modal_index;
                app.move_queued(viewed_session_id, idx, delta);
                // Follow the moved item if it changed position.
                let count = app.pending_count(viewed_session_id);
                if count > 0 {
                    app.modal_index = (idx as i32 + delta).clamp(0, count as i32 - 1) as usize;
                    app.queue_modal_follow = true;
                }
            }
        }
        input::InputAction::HistoryNext => {
            // Inline ↓ walks the current session's history forward
            // (toward the newest), mirroring HistoryPrev. Walking past
            // the newest entry restores the stashed draft.
            let session_rows = app.current_session_history();
            app.history_next(&session_rows);
        }
        input::InputAction::ModalUp => {
            modals::handle_modal_up(app, viewed_session_id);
        }
        input::InputAction::ModalDown => {
            modals::handle_modal_down(app, viewed_session_id);
        }
        input::InputAction::QuestionUp => {
            if app.active_modal() == Modal::Question
                && let Some(qm) = app.question.take()
            {
                app.question = Some(qm.update(crate::question_model::QuestionAction::Up).0);
                // Moving the highlight re-enables follow so the body
                // scrolls to keep the cursor visible.
                app.question_modal_follow = true;
            }
        }
        input::InputAction::QuestionDown => {
            if app.active_modal() == Modal::Question
                && let Some(qm) = app.question.take()
            {
                app.question = Some(qm.update(crate::question_model::QuestionAction::Down).0);
                app.question_modal_follow = true;
            }
        }
        input::InputAction::QuestionToggle => {
            if app.active_modal() == Modal::Question
                && let Some(qm) = app.question.take()
            {
                app.question = Some(qm.update(crate::question_model::QuestionAction::Toggle).0);
            }
        }
        input::InputAction::QuestionSelect(n) => {
            if app.active_modal() == Modal::Question
                && let Some(qm) = app.question.take()
            {
                app.question = Some(
                    qm.update(crate::question_model::QuestionAction::Select(n))
                        .0,
                );
                // A digit jump moves the highlight, so follow it.
                app.question_modal_follow = true;
            }
        }
        input::InputAction::QuestionSubmit => {
            if app.active_modal() == Modal::Question
                && let Some(qm) = app.question.take()
            {
                let (qm, effects) = qm.update(crate::question_model::QuestionAction::Submit);
                // Keep the model until the per-frame queue sync clears
                // it; the Closed effect drives the channel reply + drain.
                app.question = Some(qm);
                question_effects::apply(&effects, app, runtime).await;
                app.question_scroll = 0;
                app.question_modal_follow = true;
            }
        }
        input::InputAction::QuestionPrevious => {
            if app.active_modal() == Modal::Question
                && let Some(qm) = app.question.take()
            {
                app.question = Some(qm.update(crate::question_model::QuestionAction::Previous).0);
                app.question_scroll = 0;
                app.question_modal_follow = true;
            }
        }
        input::InputAction::QuestionCancel => {
            if app.active_modal() == Modal::Question
                && let Some(qm) = app.question.take()
            {
                let (_qm, effects) = qm.update(crate::question_model::QuestionAction::Cancel);
                // Cancel discards the model immediately; the Closed
                // effect drives the (empty-answers) reply + drain.
                question_effects::apply(&effects, app, runtime).await;
            }
        }
        input::InputAction::InputSubmit => {
            if app.active_modal() == Modal::InputInjection {
                let text = std::mem::take(&mut app.input);
                if let Some(req) = app.pending_input.take() {
                    // Drain the matching front so the per-frame sync
                    // closes the modal and restores the composer draft.
                    runtime.pending_input.lock().await.pop_front();
                    let parent_call_id =
                        runtime.runner_question_parent.lock().await.remove(&req.id);
                    let _ = app.tx.send(AgentRequest::StdinReply {
                        request_id: req.id.clone(),
                        text,
                        parent_call_id,
                    });
                }
                let next = runtime.pending_input.lock().await.front().cloned();
                if let Some(next) = next {
                    app.pending_input = Some(next);
                    app.input.clear();
                    app.set_cursor(0);
                } else {
                    app.restore_input_draft();
                    app.pop_transient_surface();
                }
            }
        }
        input::InputAction::InputCancel => {
            if app.active_modal() == Modal::InputInjection
                && let Some(req) = app.pending_input.take()
            {
                // Empty reply = cancel → the command runs with closed
                // stdin and fails fast with a non-interactive remedy.
                let next = {
                    let mut queue = runtime.pending_input.lock().await;
                    queue.pop_front();
                    queue.front().cloned()
                };
                let parent_call_id = runtime.runner_question_parent.lock().await.remove(&req.id);
                let _ = app.tx.send(AgentRequest::StdinReply {
                    request_id: req.id.clone(),
                    text: String::new(),
                    parent_call_id,
                });
                if let Some(next) = next {
                    app.pending_input = Some(next);
                    app.input.clear();
                    app.set_cursor(0);
                } else {
                    app.restore_input_draft();
                    app.pop_transient_surface();
                }
            }
        }
        input::InputAction::QuestionInsertChar(c) => {
            if app.active_modal() == Modal::Question
                && let Some(qm) = app.question.take()
            {
                app.question = Some(
                    qm.update(crate::question_model::QuestionAction::InsertChar(c))
                        .0,
                );
                // Typing into the "Other" field may grow it onto a new
                // wrapped line, pushing the caret below the viewport.
                // Re-arm follow so the body scrolls to track the
                // caret (not just the "Other" label row).
                app.question_modal_follow = true;
            }
        }
        input::InputAction::QuestionBackspace => {
            if app.active_modal() == Modal::Question
                && let Some(qm) = app.question.take()
            {
                app.question = Some(
                    qm.update(crate::question_model::QuestionAction::Backspace)
                        .0,
                );
                // Backspace can collapse the field back up a line;
                // re-arm follow so the caret stays on screen.
                app.question_modal_follow = true;
            }
        }
        input::InputAction::PermissionSubmit => {
            handle_permission_submit(app, runtime).await;
        }
        input::InputAction::PermissionReject => {
            // Rejecting settles the whole concurrent permission batch;
            // resolve every queued request so its tool futures finish.
            let queued: Vec<PermissionRequest> =
                runtime.pending_permission.lock().await.drain(..).collect();
            app.pending_permission = None;
            app.pop_transient_surface();
            app.modal_index = 0;
            app.permission_confirm_always = false;
            app.permission_show_details = false;
            let mut parents = runtime.runner_permission_parent.lock().await;
            for pending in queued {
                let parent_call_id = parents.remove(&pending.id);
                let _ = app.tx.send(AgentRequest::PermissionReply {
                    request_id: pending.id,
                    decision: PermissionDecision::Reject,
                    parent_call_id,
                });
            }
        }
        input::InputAction::PermissionBack => {
            app.permission_confirm_always = false;
            app.modal_index = 1;
        }
        input::InputAction::SelectionStart { x, y } => {
            mouse::handle_selection_start(app, runtime, viewed_session_id, x, y).await;
        }
        input::InputAction::RightClick { x, y } => {
            mouse::handle_right_click(app, runtime, x, y).await;
        }
        input::InputAction::SelectionUpdate { x, y } => {
            mouse::handle_selection_update(app, x, y);
        }
        input::InputAction::SelectionEnd => {
            mouse::handle_selection_end(app);
        }
        input::InputAction::SelectBlock { x, y } => {
            mouse::handle_select_block(app, x, y);
        }
        input::InputAction::Hover { x, y } => {
            mouse::handle_hover(app, runtime, x, y).await;
        }
    }
    ActionFlow::Handled
}

/// The sole retained-panel entry transaction (ADR-0141: panels are
/// retained modals). It focuses/restores the panel, runs one-time
/// initialization, then applies the panel's refresh-on-show and enter-hook
/// policy. Dedicated shortcuts, mouse targets, backend open signals and the
/// quick switcher all route here. Full-screen views use [`enter_view`].
pub(crate) fn open_active_connection_detail(
    app: &mut App,
    runtime: &UiRuntime,
    viewed_session_id: &str,
) {
    enter_panel(
        app,
        crate::surfaces::PanelId::Connections,
        runtime,
        viewed_session_id,
    );
    let target_id = if !app.current_provider.is_empty() {
        app.current_provider.clone()
    } else {
        app.providers_filtered()
            .first()
            .map(|p| p.id.clone())
            .unwrap_or_default()
    };
    if let Some(pos) = app
        .providers_filtered()
        .iter()
        .position(|p| p.id == target_id)
    {
        app.modal_index = pos;
    }
    app.connection_info_detail = true;
    app.connection_info_standalone = true;
    app.connection_detail = None;
    app.connection_info_scroll = 0;
    if !target_id.is_empty() {
        let _ = app
            .tx
            .send(AgentRequest::QueryConnectionDetail { id: target_id });
    }
}

pub(super) fn enter_panel(
    app: &mut App,
    id: crate::surfaces::PanelId,
    runtime: &UiRuntime,
    viewed_session_id: &str,
) -> bool {
    use crate::surfaces::PanelId;

    let first = app.open_panel(id);
    app.selection = SelectionState::None;
    app.focused_target = None;
    app.drag.cancel();

    if first {
        match id {
            PanelId::Models => {
                app.model_search = false;
                app.model_modal_follow = true;
                let rows = app.models_flat_filtered();
                app.modal_index = rows
                    .iter()
                    .position(|row| {
                        row.provider_id == app.current_provider && row.model == app.current_model
                    })
                    .unwrap_or(0);
                app.suggestion_index = None;
            }
            PanelId::Connections => {
                app.model_search = false;
                app.model_modal_follow = true;
                let ranked = app.providers_filtered();
                app.modal_index = ranked
                    .iter()
                    .position(|row| row.id == app.current_provider)
                    .or_else(|| {
                        ranked
                            .iter()
                            .position(|row| row.id == app.provider_picker.default_id)
                    })
                    .unwrap_or(0);
                app.suggestion_index = None;
            }
            PanelId::HistorySearch => {
                app.history_clear_confirm = false;
                app.modal_index = 0;
                app.history_scroll = 0;
                app.history_modal_follow = true;
                app.history_preview = false;
            }
            _ => {}
        }
    }

    if id == PanelId::HistorySearch {
        app.history_search = true;
    }
    if id == PanelId::Queue {
        app.block_queue(viewed_session_id);
        app.queue_exit_session = Some(viewed_session_id.to_string());
    }

    let request = match id {
        PanelId::Permissions | PanelId::Tools | PanelId::Mcp | PanelId::Skills => {
            Some(AgentRequest::QuerySessionContext)
        }
        PanelId::UsageStats => {
            app.usage_stats = None;
            Some(AgentRequest::QueryUsageStats { event_cap: 200 })
        }
        PanelId::Telemetry if app.token_ledger.is_none() => {
            app.token_report = None;
            Some(AgentRequest::QueryTokenUsage {
                session_id: viewed_session_id.to_string(),
            })
        }
        PanelId::Btw => Some(AgentRequest::QueryBtwList),
        PanelId::Sessions => Some(AgentRequest::QuerySessionsOverview),
        PanelId::Tree => Some(AgentRequest::QuerySessionTree),
        _ => None,
    };
    if let Some(request) = request
        && app.tx.send(request).is_err()
    {
        show_local_toast(
            app,
            format!("Could not refresh {}: backend disconnected.", id.label()),
            true,
            std::time::Duration::from_millis(3200),
        );
    }

    let _ = runtime;
    first
}

/// Enter a full-screen view (ADR-0141): navigate the router, run the view's
/// every-show UI refresh, and fire its data-refresh request. Views keep
/// their retained fields natively on `App` (no registry), so — unlike
/// panels — there is no first-open distinction.
pub(super) fn enter_view(app: &mut App, view: crate::surfaces::View, runtime: &UiRuntime) {
    use crate::surfaces::View;

    let previous = app.current_view();
    if previous != view {
        app.leave_view_for_navigation(previous);
    }
    app.show_view_surface(view);
    app.selection = SelectionState::None;
    app.focused_target = None;
    app.drag.cancel();

    let request = match view {
        View::Settings => {
            app.config_focus = crate::overlays::ConfigFocus::Categories;
            app.config_category = 0;
            app.config_detail_index = Theme::color_scheme_index_with_workspace(
                &app.color_scheme,
                if app.current_workspace.is_empty() {
                    None
                } else {
                    Some(std::path::Path::new(&app.current_workspace))
                },
            );
            app.config_scroll = 0;
            app.config_detail_scroll = 0;
            app.config_dropdown = None;
            Some(AgentRequest::QueryWebSearchConfig)
        }
        View::Dashboard => {
            app.host_modal_follow = true;
            app.host_focus = crate::overlays::DashboardFocus::Detail;
            app.host_console_log.clear();
            app.host_kill_confirm = None;
            app.host_kill_confirm_id = None;
            None
        }
        View::Session | View::Runner | View::Side => None,
    };
    if let Some(request) = request
        && app.tx.send(request).is_err()
    {
        show_local_toast(
            app,
            "Could not refresh view: backend disconnected.".to_string(),
            true,
            std::time::Duration::from_millis(3200),
        );
    }
    let _ = runtime;
}

pub(crate) fn handle_wheel(app: &mut App, up: bool, x: u16, y: u16) {
    if app.active_modal() == Modal::Permission {
        if app.modal_hit_map.permission_sheet_contains(x, y) {
            if app.permission_show_details {
                if up {
                    app.permission_scroll = app.permission_scroll.saturating_sub(1);
                } else {
                    app.permission_scroll = app
                        .permission_scroll
                        .saturating_add(1)
                        .min(app.permission_max_scroll);
                }
            }
        } else {
            scroll_tick(app, !up);
        }
    } else if app.active_modal() != Modal::None {
        let inside_modal = app
            .modal_rect
            .is_some_and(|r| r.x <= x && x < r.x + r.width && r.y <= y && y < r.y + r.height)
            || app.modal_hit_map.oauth_modal_contains(x, y);

        if inside_modal {
            scroll_tick(app, !up);
        }
    } else if app.modal_hit_map.completion_menu_contains(x, y) {
        let count = app.completions().len();
        if count > 0 {
            if up {
                let prev = match app.suggestion_index {
                    Some(0) | None => count.saturating_sub(1),
                    Some(i) => i.saturating_sub(1),
                };
                app.suggestion_index = Some(prev);
            } else {
                let next = match app.suggestion_index {
                    Some(i) if i + 1 < count => i + 1,
                    _ => 0,
                };
                app.suggestion_index = Some(next);
            }
        }
    } else {
        let over_composer = app.input_rect.is_some_and(|r| {
            r.height > crate::design::COMPOSER_VERTICAL_CHROME_ROWS
                && r.x <= x
                && x < r.x + r.width
                && r.y <= y
                && y < r.y + r.height
        });
        if !(over_composer && app.step_input_scroll(up, 4).is_some()) {
            scroll_tick(app, !up);
        }
    }
}

#[cfg(test)]
mod transcript_scroll_tests {
    use super::*;

    fn scrollable_app() -> App {
        let mut app = crate::tests::new_app_for_relay_tests();
        app.max_scroll = 100;
        app.scroll = 100;
        app.view_height = 20;
        app.follow_bottom = true;
        app
    }

    #[test]
    fn wheel_ticks_move_the_transcript_and_rearm_bottom_follow() {
        let mut app = scrollable_app();

        scroll_tick(&mut app, false);
        assert_eq!(app.scroll, 96);
        assert!(!app.follow_bottom);

        scroll_tick(&mut app, true);
        assert_eq!(app.scroll, 100);
        assert!(app.follow_bottom);
    }

    #[test]
    fn page_navigation_uses_the_measured_viewport() {
        let mut app = scrollable_app();

        scroll_transcript_page(&mut app, false);
        assert_eq!(app.scroll, 81);
        assert!(!app.follow_bottom);

        scroll_transcript_page(&mut app, true);
        assert_eq!(app.scroll, 100);
        assert!(app.follow_bottom);
    }

    #[test]
    fn edge_navigation_updates_position_and_follow_mode_together() {
        let mut app = scrollable_app();

        scroll_transcript_to_edge(&mut app, false);
        assert_eq!(app.scroll, 0);
        assert!(!app.follow_bottom);

        scroll_transcript_to_edge(&mut app, true);
        assert_eq!(app.scroll, 100);
        assert!(app.follow_bottom);
    }

    #[test]
    fn wheel_spatial_routing_under_permission_modal() {
        let mut app = scrollable_app();
        app.set_active_modal_for_test(Modal::Permission);
        app.modal_hit_map
            .set_permission_sheet(mutx_engine::Rect::new(0, 15, 80, 5));
        app.permission_show_details = true;
        app.permission_max_scroll = 10;
        app.permission_scroll = 2;

        // 1. Wheel over permission sheet (y=16) scrolls details down
        handle_wheel(&mut app, false, 10, 16);
        assert_eq!(app.permission_scroll, 3);
        assert_eq!(app.scroll, 100, "transcript scroll untouched");

        // 2. Wheel over permission sheet (y=16) scrolls details up
        handle_wheel(&mut app, true, 10, 16);
        assert_eq!(app.permission_scroll, 2);
        assert_eq!(app.scroll, 100, "transcript scroll untouched");

        // 3. Wheel above permission sheet (y=5) scrolls transcript
        handle_wheel(&mut app, true, 10, 5);
        assert_eq!(app.scroll, 96, "transcript scrolled up");
        assert_eq!(app.permission_scroll, 2, "permission scroll untouched");
    }

    #[test]
    fn wheel_spatial_routing_under_overlay_modal_isolates_backdrop() {
        let mut app = scrollable_app();
        app.set_active_modal_for_test(Modal::Help);
        app.modal_rect = Some(mutx_engine::Rect::new(10, 5, 60, 10));
        app.help_scroll = 5;

        // 1. Wheel on backdrop (x=2, y=2) outside modal_rect: absorbed, neither modal nor transcript scrolls
        handle_wheel(&mut app, false, 2, 2);
        assert_eq!(app.help_scroll, 5, "modal scroll untouched on backdrop");
        assert_eq!(app.scroll, 100, "transcript scroll untouched on backdrop");

        // 2. Wheel inside modal (x=20, y=8): scrolls modal body
        handle_wheel(&mut app, false, 20, 8);
        assert_eq!(app.help_scroll, 6, "modal scrolled inside modal_rect");
        assert_eq!(app.scroll, 100, "transcript scroll untouched");
    }

    #[tokio::test]
    async fn completion_menu_mouse_wheel_and_click() {
        let mut app = scrollable_app();
        app.input = "/m".to_string();
        app.cursor_position = 2;
        app.modal_hit_map
            .set_completion_menu_rect(mutx_engine::Rect::new(0, 8, 30, 2));
        app.modal_hit_map
            .push_completion_item(0, mutx_engine::Rect::new(0, 8, 30, 1));
        app.modal_hit_map
            .push_completion_item(1, mutx_engine::Rect::new(0, 9, 30, 1));

        let runtime = UiRuntime::minimal_for_test();

        // Wheel over menu cycles suggestions
        handle_wheel(&mut app, false, 5, 8);
        assert!(app.suggestion_index.is_some());

        // Click on completion item 0 accepts completion
        mouse::handle_selection_start(&mut app, &runtime, "s1", 5, 8).await;
        assert!(app.completion_dismissed);
        assert_eq!(app.suggestion_index, None);
    }
}

#[cfg(test)]
mod view_entry_tests {
    use super::*;

    #[test]
    fn every_show_refreshes_remote_view_data() {
        let mut app = crate::tests::new_app_for_relay_tests();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        app.tx = tx;
        let runtime = UiRuntime::minimal_for_test();

        assert!(enter_panel(
            &mut app,
            crate::surfaces::PanelId::Tree,
            &runtime,
            "s1"
        ));
        assert!(matches!(rx.try_recv(), Ok(AgentRequest::QuerySessionTree)));
        app.dismiss_surface();
        assert!(!enter_panel(
            &mut app,
            crate::surfaces::PanelId::Tree,
            &runtime,
            "s1"
        ));
        assert!(matches!(rx.try_recv(), Ok(AgentRequest::QuerySessionTree)));
    }

    #[test]
    fn sessions_and_skills_have_complete_query_paths() {
        let mut app = crate::tests::new_app_for_relay_tests();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        app.tx = tx;
        let runtime = UiRuntime::minimal_for_test();

        enter_panel(&mut app, crate::surfaces::PanelId::Sessions, &runtime, "s1");
        assert!(matches!(
            rx.try_recv(),
            Ok(AgentRequest::QuerySessionsOverview)
        ));
        enter_panel(&mut app, crate::surfaces::PanelId::Skills, &runtime, "s1");
        assert!(matches!(
            rx.try_recv(),
            Ok(AgentRequest::QuerySessionContext)
        ));
    }
}

#[cfg(test)]
pub(crate) async fn dispatch_action_for_test(
    app: &mut App,
    runtime: &UiRuntime,
    action: input::InputAction,
    viewed_session_id: &str,
) -> ActionFlow {
    let (copy_tx, _copy_rx) = mpsc::unbounded_channel();
    let copy_pending = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (paste_tx, _paste_rx) = mpsc::unbounded_channel();
    let mut sgr_guard = input::SgrLeakGuard::default();
    let session = crate::SessionSource::Remote {
        session_id: viewed_session_id.to_string(),
    };
    let mut ctx = ActionContext {
        runtime,
        session: &session,
        viewed_session_id,
        copy_tx: &copy_tx,
        copy_pending: &copy_pending,
        paste_tx: &paste_tx,
        sgr_guard: &mut sgr_guard,
    };
    let backend = mutx_engine::Backend::with_bce(Vec::new(), mutx_engine::backend::Bce::No);
    let mut terminal = mutx_engine::Terminal::new(backend);
    dispatch_action(app, &mut terminal, action, &mut ctx).await
}

/// Test shims for the dashboard console dispatcher: the internal helpers the
/// console tests drive directly (they need no terminal or clipboard plumbing
/// — just `App` + `UiRuntime`).
#[cfg(test)]
pub(crate) mod host_test_shims {
    use super::{UiRuntime, host};
    use crate::App;

    pub(crate) async fn dispatch(
        app: &mut App,
        runtime: &UiRuntime,
        line: &str,
        create_when_bare: bool,
    ) {
        host::dispatch_console_command(app, runtime, line, create_when_bare).await;
    }

    pub(crate) fn kill(app: &mut App, runtime: &UiRuntime) {
        host::kill_selected(app, runtime);
    }

    pub(crate) fn kill_cancel(app: &mut App) {
        host::cancel_kill_confirm(app);
    }
}

/// Cycle a capability-override tri-state (ADR-0149 layer 1): unset (inherit
/// from the lower layers) → forced on → forced off → unset. Used by the
/// settings editor's Space cycling on fields 3/4.
fn cycle_tri_state(v: Option<bool>) -> Option<bool> {
    match v {
        None => Some(true),
        Some(true) => Some(false),
        Some(false) => None,
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_command_by_id(
    app: &mut App,
    runtime: &UiRuntime,
    _session: &crate::SessionSource,
    viewed_session_id: &str,
    copy_tx: &mpsc::UnboundedSender<Result<clipboard::CopyOutcome, String>>,
    copy_pending: &Arc<AtomicUsize>,
    cmd_id: crate::keymap::CommandId,
) -> ActionFlow {
    use crate::keymap::CommandId;
    match cmd_id {
        CommandId::Help => {
            enter_panel(
                app,
                crate::surfaces::PanelId::Help,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::CommandPalette => {}
        CommandId::CancelOrBack => {
            if app.active_modal() != Modal::None {
                modals::handle_close_modal(app, viewed_session_id);
            } else if app.focused_target.is_some() {
                app.session_focus = crate::app::SessionFocusRegion::Composer;
                app.focused_target = None;
            }
        }
        CommandId::InterruptTask => {
            return commands::handle_ctrl_c(app, viewed_session_id, copy_tx, copy_pending);
        }
        CommandId::Quit => {
            let _ = app.tx.send(AgentRequest::EndSession);
            return ActionFlow::Exit;
        }
        CommandId::CopySelection => {
            if let Some(text) = extract_selection_text(
                &app.selection,
                app.focused_messages(),
                &app.input,
                &app.layout_map,
                app.drag.cell_info.as_ref(),
            ) {
                clipboard_ops::spawn_clipboard_copy(copy_tx, copy_pending.clone(), text);
            }
        }
        CommandId::SendPrompt => {
            let text = std::mem::take(&mut app.input);
            app.set_cursor(0);
            commands::handle_send_chat(app, runtime, viewed_session_id, text).await;
        }
        CommandId::QueueFollowUp => {
            let text = std::mem::take(&mut app.input);
            app.set_cursor(0);
            commands::handle_queue_follow_up(app, runtime, viewed_session_id, text).await;
        }
        CommandId::SteerImmediate => {
            let text = std::mem::take(&mut app.input);
            app.set_cursor(0);
            commands::handle_send_steer(app, runtime, viewed_session_id, text).await;
        }
        CommandId::InsertNewline => {
            app.input.push('\n');
            app.set_cursor_end();
        }
        CommandId::HistorySearch => {
            enter_panel(
                app,
                crate::surfaces::PanelId::HistorySearch,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::FocusTranscript => {
            app.session_focus = crate::app::SessionFocusRegion::Transcript;
            app.focus_interactive_target(1);
        }
        CommandId::FocusComposer => {
            app.session_focus = crate::app::SessionFocusRegion::Composer;
            app.focused_target = None;
        }
        CommandId::ScrollTranscriptUp => {
            app.scroll = app
                .scroll
                .saturating_sub(app.view_height.saturating_sub(2).max(1));
        }
        CommandId::ScrollTranscriptDown => {
            app.scroll = app
                .scroll
                .saturating_add(app.view_height.saturating_sub(2).max(1))
                .min(app.max_scroll);
        }
        CommandId::TranscriptMoveUp => {
            app.focus_interactive_target(-1);
        }
        CommandId::TranscriptMoveDown => {
            app.focus_interactive_target(1);
        }
        CommandId::TranscriptOpenOrToggle => {
            if let Some(target) = app.focused_target {
                if target.kind == InteractiveTargetKind::ToolStep {
                    let mut messages = runtime.messages.write().await;
                    let enter_id =
                        resolve_focused_mut(&mut messages, &app.focus_stack, target.message_idx)
                            .and_then(|message| {
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
                        app.toggle_step_pinned(&mut messages, target.message_idx);
                        drop(messages);
                    }
                }
            }
        }
        CommandId::TranscriptTop => {
            app.scroll = 0;
            app.follow_bottom = false;
        }
        CommandId::TranscriptBottom => {
            app.scroll = app.max_scroll;
            app.follow_bottom = true;
        }
        CommandId::NavigateSession => {
            enter_view(app, crate::surfaces::View::Session, runtime);
        }
        CommandId::NavigateDashboard => {
            enter_view(app, crate::surfaces::View::Dashboard, runtime);
        }
        CommandId::NavigateSettings => {
            enter_view(app, crate::surfaces::View::Settings, runtime);
        }
        CommandId::OpenTodos => {
            enter_panel(
                app,
                crate::surfaces::PanelId::Todos,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenQueue => {
            enter_panel(
                app,
                crate::surfaces::PanelId::Queue,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenTelemetry => {
            enter_panel(
                app,
                crate::surfaces::PanelId::Telemetry,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenModels => {
            enter_panel(
                app,
                crate::surfaces::PanelId::Models,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenConnections => {
            enter_panel(
                app,
                crate::surfaces::PanelId::Connections,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenTools => {
            enter_panel(
                app,
                crate::surfaces::PanelId::Tools,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenMcp => {
            enter_panel(
                app,
                crate::surfaces::PanelId::Mcp,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenSkills => {
            enter_panel(
                app,
                crate::surfaces::PanelId::Skills,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenPermissions => {
            enter_panel(
                app,
                crate::surfaces::PanelId::Permissions,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenUsage => {
            enter_panel(
                app,
                crate::surfaces::PanelId::UsageStats,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenTree => {
            enter_panel(
                app,
                crate::surfaces::PanelId::Tree,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenBtw => {
            enter_panel(
                app,
                crate::surfaces::PanelId::Btw,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::OpenSessions => {
            enter_panel(
                app,
                crate::surfaces::PanelId::Sessions,
                runtime,
                viewed_session_id,
            );
        }
        CommandId::ToggleQueueBlock => {
            let sid = viewed_session_id.to_string();
            if app.is_queue_blocked(&sid) {
                app.resume_queue(&sid);
                show_local_toast(
                    app,
                    "Queue resumed",
                    false,
                    std::time::Duration::from_millis(2000),
                );
            } else {
                app.block_queue(&sid);
                show_local_toast(
                    app,
                    "Queue paused",
                    false,
                    std::time::Duration::from_millis(2000),
                );
            }
        }
        CommandId::ClearQueue => {
            app.pending_dispatch.clear();
            show_local_toast(
                app,
                "Queue cleared",
                false,
                std::time::Duration::from_millis(2000),
            );
        }
        CommandId::McpReconnectSelected => {}
        CommandId::McpToggleSelected => {}
        CommandId::ToolsToggleSelected => {}
        CommandId::PermissionsRevokeSelected => {}
        CommandId::PermissionsClearAll => {
            if let Some(ref ctx) = app.session_context {
                for perm in &ctx.permissions {
                    let _ = app.tx.send(AgentRequest::RevokePermission {
                        tool: perm.tool.clone(),
                        scope: perm.scope.clone(),
                    });
                }
            }
            show_local_toast(
                app,
                "All permissions revoked",
                false,
                std::time::Duration::from_millis(2000),
            );
        }
        CommandId::SkillsToggleDetail => {}
        CommandId::ProviderAddConnection => {
            app.open_preset_chooser();
        }
        CommandId::ProviderEditSelected => {}
        CommandId::ProviderDeleteSelected => {}
        CommandId::ProviderToggleFavorite => {}
        CommandId::RedrawScreen => {
            show_local_toast(
                app,
                "Screen redrawn",
                false,
                std::time::Duration::from_millis(1000),
            );
        }
    }
    ActionFlow::Handled
}
