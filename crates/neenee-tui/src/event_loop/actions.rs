//! Input-action dispatch for the TUI event loop: the `match` over
//! [`input::InputAction`] that `run_app_loop`'s input-drain stage ran inline,
//! moved here verbatim (one arm per variant) with only the loop-control
//! statements rewritten as [`ActionFlow`] values.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use neenee_tui_engine::Terminal;
use tokio::sync::mpsc;

use neenee_contracts::{AgentRequest, PermissionDecision, PermissionRequest};

use crate::clipboard;
use crate::clipboard_ops;
use crate::input;
use crate::model::document::NoticeSeverity;
use crate::model::layout::InteractiveTargetKind;
use crate::model::selection::SelectionState;
use crate::view;
use crate::view::Theme;
use crate::{ActivityTab, App, Modal};

use super::{
    UiRuntime, activate_picked_model, extract_selection_text, handle_permission_submit,
    modal_page_step, question_effects, resolve_focused_mut, show_local_toast,
};

mod commands;
mod modals;
mod mouse;

#[cfg(test)]
pub(crate) use mouse::handle_selection_end_for_test;

pub(super) use commands::split_command_word;

#[cfg(test)]
pub(crate) use commands::handle_send_slash;

/// How the event loop proceeds after a dispatched action. Arms that ended in
/// `continue` (skip to the next drained input event) or `return Ok(())` (exit
/// the loop) when the match was inline in `run_app_loop` return these instead;
/// the call site maps them back onto the same control flow.
pub(crate) enum ActionFlow {
    /// Action handled; the drain loop proceeds to the next statement.
    Handled,
    /// `continue` the input-drain loop.
    NextEvent,
    /// `return Ok(())` from `run_app_loop`.
    Exit,
}

/// Loop stage: dispatch one drained [`input::InputAction`]. The match body is
/// verbatim from `run_app_loop`; only `continue` / `return Ok(())` inside arms
/// became [`ActionFlow`] values, and the clipboard senders / viewed session id
/// are passed explicitly instead of captured.
#[allow(clippy::too_many_arguments)]
pub(super) async fn dispatch_action(
    app: &mut App,
    runtime: &UiRuntime,
    terminal: &mut Terminal<std::io::Stdout>,
    session: &crate::SessionSource,
    action: input::InputAction,
    viewed_session_id: &str,
    copy_tx: &mpsc::UnboundedSender<Result<clipboard::CopyOutcome, String>>,
    copy_pending: &Arc<AtomicUsize>,
    paste_tx: &mpsc::UnboundedSender<clipboard::ClipboardRead>,
    sgr_guard: &mut input::SgrLeakGuard,
) -> ActionFlow {
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
            let _ = crossterm::execute!(terminal.backend().writer(), EnableMouseCapture);
            sgr_guard.reset();
            // No need to set `frame_dirty` here: every drained event
            // already raised `input_redraw_pending`, which forces a
            // redraw on the very next frame at the new geometry.
        }
        input::InputAction::Quit => {
            // Now reachable only via the `/exit` slash command (the bare
            // `q` shortcut was removed to stop accidental first-key exits).
            // The operator's intent is "done with this session", not
            // "detach": declare the session ended (ADR-0112) so the daemon
            // tears it down instead of hosting it forever. Fire-and-forget
            // is safe — the client pump drains the request channel to the
            // wire before it closes the socket on App drop.
            let _ = app.tx.send(AgentRequest::EndSession);
            tracing::info!(reason = "slash_exit", "app exiting");
            return ActionFlow::Exit;
        }
        input::InputAction::SendChat(text) => {
            commands::handle_send_chat(app, runtime, viewed_session_id, text).await;
        }
        input::InputAction::InsertIntoRound => {
            commands::handle_insert_into_round(app, viewed_session_id);
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
            let target = if app.active_modal == Modal::Models {
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
            if app.active_modal == Modal::CustomProvider {
                app.cycle_custom_field(true);
            }
        }
        input::InputAction::CustomProviderPrevField => {
            if app.active_modal == Modal::CustomProvider {
                app.cycle_custom_field(false);
            }
        }
        input::InputAction::MoveCustomSuggestion { forward } => {
            if app.active_modal == Modal::CustomProvider {
                app.move_custom_suggestion(forward);
            }
        }
        input::InputAction::MoveProviderTemplate { forward } => {
            if app.active_modal == Modal::ProviderTemplate {
                app.move_template_choice(forward);
            }
        }
        input::InputAction::SelectProviderTemplate => {
            if app.active_modal == Modal::ProviderTemplate
                && let Some(template) = crate::PROVIDER_TEMPLATES.get(app.template_choice)
            {
                if template.oauth_first() {
                    app.begin_oauth_add(template);
                    let _ = app.tx.send(AgentRequest::AuthorizeOAuth {
                        method: template
                            .auth
                            .default_login_method()
                            .unwrap_or(neenee_contracts::LoginMethod::Device),
                        auth: template.auth,
                    });
                } else {
                    app.open_custom_provider_editor(template);
                }
            }
        }
        input::InputAction::CancelOauthPending => {
            if app.active_modal == Modal::OauthPending {
                app.awaiting_oauth_add = false;
                app.oauth_pending_url.clear();
                app.oauth_pending_user_code.clear();
                app.oauth_pending_message.clear();
                app.oauth_pending_error = None;
                app.open_provider_template_chooser();
            }
        }
        input::InputAction::CycleOauthSelection => {
            if app.active_modal == Modal::OauthPending {
                app.cycle_oauth_selection();
            }
        }
        input::InputAction::CopyOauthContent { target } => {
            // Copy the OAuth pending sheet's primary field to the
            // system clipboard. Mouse drag-select does not reach modal
            // body text (mouse events are captured), so these are the
            // in-app copy affordances.
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
        input::InputAction::CancelProviderTemplate => {
            // Return to the Connections list the chooser was opened
            // from; the chat draft stays parked in stashed_input.
            if app.active_modal == Modal::ProviderTemplate {
                app.input.clear();
                app.set_cursor(0);
                app.active_modal = Modal::Connections;
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
            if app.active_modal == Modal::CustomProvider {
                app.input.clear();
                app.set_cursor(0);
                app.custom_field = 0;
                app.custom_edit_id = None;
                app.active_modal = Modal::Connections;
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
            if matches!(app.active_modal, Modal::Connections | Modal::Models) {
                app.model_search = true;
                app.modal_keymap_open = false;
                app.modal_index = 0;
                app.model_scroll = 0;
                app.model_modal_follow = true;
            }
        }
        input::InputAction::ModelExitSearch => {
            // First Esc while searching: drop the query and return to the
            // full browse list. The chat draft stays parked in
            // `stashed_input` until the modal closes for real.
            if matches!(app.active_modal, Modal::Connections | Modal::Models) {
                app.model_search = false;
                app.modal_keymap_open = false;
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
            if app.active_modal == Modal::Models {
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
                        app.editor_field = 1;
                    }
                    _ => {
                        app.input = app.editor_effort.clone();
                        app.set_cursor_end();
                        app.editor_field = 1;
                    }
                }
            }
        }
        input::InputAction::ModelEditorEffortCycle { delta } => {
            // Cycle the effort selector through the selected model's
            // supported wire levels, wrapping at both ends. Mirrored
            // into app.input so the renderer shows the live value.
            let model = neenee_contracts::resolve_model(&app.editor_model);
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
            let model = neenee_contracts::resolve_model(&app.editor_model);
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
        input::InputAction::SubmitModelEditor => {
            return modals::handle_submit_model_editor(app);
        }
        input::InputAction::Interrupt => {
            // Mirror Ctrl+C's quit pattern: the first Esc only arms a
            // ~2s window (and shows a toast); the second Esc within
            // that window actually interrupts the running task.
            if app.esc_armed_ticks > 0 {
                app.esc_armed_ticks = 0;
                let _ = app.tx.send(AgentRequest::Interrupt);
            } else {
                app.esc_armed_ticks = 20;
            }
        }
        input::InputAction::OpenModels => {
            // Stash whatever the user was composing so Esc restores it
            // unchanged. The picker opens in browse mode, so the input
            // line stays empty until `/` enters search and borrows it as
            // the fuzzy query (same pattern as the history modal).
            app.stashed_input = std::mem::take(&mut app.input);
            app.set_cursor(0);
            app.input_scroll = 0;
            app.active_modal = Modal::Models;
            app.modal_keymap_open = false;
            app.model_search = false;
            app.model_scroll = 0;
            app.model_modal_follow = true;
            // Land the cursor on the live (provider, model) pair, so
            // "open picker + Enter" re-activates the current selection.
            let rows = app.models_flat_filtered();
            app.modal_index = rows
                .iter()
                .position(|row| {
                    row.provider_id == app.current_provider && row.model == app.current_model
                })
                .unwrap_or(0);
            app.suggestion_index = None;
        }
        input::InputAction::OpenConnections => {
            // Same stash + browse-mode open as `OpenModels`.
            app.stashed_input = std::mem::take(&mut app.input);
            app.set_cursor(0);
            app.input_scroll = 0;
            app.active_modal = Modal::Connections;
            app.modal_keymap_open = false;
            app.model_search = false;
            app.model_scroll = 0;
            app.model_modal_follow = true;
            // Land the cursor on the currently-active provider (falling
            // back to the default), so "open picker + Enter"
            // re-activates it.
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
        input::InputAction::OpenProviderTemplate => {
            // `a` in the Connections modal: open the add-provider
            // template chooser (the first step of adding a connection).
            // Only meaningful from Connections; ignored otherwise.
            if app.active_modal == Modal::Connections {
                app.open_provider_template_chooser();
            }
        }
        input::InputAction::RefreshProviderModels => {
            if matches!(app.active_modal, Modal::Models | Modal::Connections) {
                let _ = app.tx.send(AgentRequest::RefreshProviderModels {
                    user_initiated: true,
                });
            }
        }
        input::InputAction::OpenHistory => {
            // The history panel floats above the composer, and the
            // composer itself is the live filter field: typing narrows
            // the list immediately (no separate browse/search mode).
            // Stash whatever the user was composing so Esc restores it
            // unchanged, and start with an empty query (show all, newest
            // first) — they type to narrow.
            app.stashed_input = std::mem::take(&mut app.input);
            app.set_cursor(0);
            app.input_scroll = 0;
            app.suggestion_index = None;
            app.active_modal = Modal::HistorySearch;
            app.modal_keymap_open = false;
            // The composer is permanently the filter while this panel
            // is open, so `history_search` is latched true.
            app.history_search = true;
            app.history_clear_confirm = false;
            // Rows are newest-first, so index 0 is the most-recent entry
            // — focus the top so an immediate Enter re-inserts it.
            app.modal_index = 0;
            app.history_scroll = 0;
            app.history_modal_follow = true;
            app.history_preview = false;
        }
        input::InputAction::HistoryInsert => {
            // Enter inside the Ctrl+R panel: pull the focused entry out
            // of `history_rows` (the filtered matches) and drop it into
            // the input box for further editing / sending. The message
            // is not shipped here — the user hits Enter again to send.
            let ranked = app.history_rows();
            let pick = ranked.get(app.modal_index).or_else(|| ranked.first());
            if let Some((orig_idx, _)) = pick {
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
                );
            }
            // The selection replaces the in-progress draft, so the
            // stash is dropped (not restored).
            app.stashed_input.clear();
            app.input_scroll = 0;
            app.suggestion_index = None;
            // A programmatic input replacement — latch the dismissal so
            // a slash-command selection doesn't flash its completion
            // popup until the next real edit.
            app.completion_dismissed = true;
            app.modal_index = 0;
            app.active_modal = Modal::None;
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
            app.active_modal = Modal::Help;
            app.modal_keymap_open = false;
            app.modal_index = 0;
            app.help_scroll = 0;
        }
        input::InputAction::OpenPermissions => {
            // The permissions manager modal. Reached via the
            // `/permissions` slash command (intercepted locally, never
            // sent to the backend). Kick off a snapshot request so the
            // rule list populates; `/permissions clear` still goes to
            // the backend via SendSlash.
            app.active_modal = Modal::Permissions;
            app.modal_keymap_open = false;
            app.modal_index = 0;
            app.permissions_scroll = 0;
            let _ = app.tx.send(AgentRequest::QuerySessionContext);
        }
        input::InputAction::OpenTools => {
            // The tools manager modal. Reached via `/tools`
            // (intercepted locally). It shares the session-context
            // snapshot, so (re)kick a query so the list is fresh.
            app.active_modal = Modal::Tools;
            app.modal_keymap_open = false;
            app.modal_index = 0;
            app.session_scroll = 0;
            app.session_modal_follow = true;
            let _ = app.tx.send(AgentRequest::QuerySessionContext);
        }
        input::InputAction::OpenUsage => {
            // The usage-statistics overlay (`/usage`, ADR-0122). Reached via
            // the local `/usage` interception. The data is the durable
            // cross-session store, so (re)kick a `QueryUsageStats` round-trip
            // each time it opens and the numbers are always fresh; until the
            // reply lands the overlay renders a loading placeholder.
            app.active_modal = Modal::UsageStats;
            app.modal_keymap_open = false;
            app.usage_stats = None;
            app.usage_stats_scroll = 0;
            let _ = app
                .tx
                .send(AgentRequest::QueryUsageStats { event_cap: 200 });
        }
        input::InputAction::OpenMcp => {
            // The MCP manager modal. Reached via `/mcp` (intercepted
            // locally). Shares the session-context snapshot, so kick a
            // fresh query and let the modal populate from its `mcp` pane.
            app.active_modal = Modal::Mcp;
            app.modal_keymap_open = false;
            app.modal_index = 0;
            app.session_scroll = 0;
            app.session_modal_follow = true;
            let _ = app.tx.send(AgentRequest::QuerySessionContext);
        }
        input::InputAction::OpenSkills => {
            // The skills modal. Reached via `/skills` (intercepted
            // locally). Shares the session-context snapshot, so kick a
            // fresh query and let the modal populate from its `skills`
            // pane. Detail expansions start collapsed.
            app.active_modal = Modal::Skills;
            app.modal_keymap_open = false;
            app.modal_index = 0;
            app.session_scroll = 0;
            app.session_modal_follow = true;
            app.skills_expanded = None;
            let _ = app.tx.send(AgentRequest::QuerySessionContext);
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
        input::InputAction::SkillsReload => {
            // Reload the skill registry. The harness replies with a fresh
            // snapshot reflecting the reloaded skills.
            let _ = app
                .tx
                .send(AgentRequest::SlashCommand("/skills reload".to_string()));
            let _ = app.tx.send(AgentRequest::QuerySessionContext);
        }
        input::InputAction::OpenConfig => {
            // Full-screen Settings View (`/config`): dual-pane configuration center.
            app.active_modal = Modal::Config;
            app.modal_keymap_open = false;
            app.config_focus = crate::overlays::ConfigFocus::Categories;
            app.config_category = 0;
            app.config_detail_index = Theme::color_scheme_index(&app.color_scheme);
            app.config_custom_editing = false;
            app.config_scroll = 0;
            app.config_detail_scroll = 0;
        }
        input::InputAction::ConfigFocusToggle => {
            if app.active_modal == Modal::Config {
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
            if app.active_modal == Modal::Config {
                match app.config_focus {
                    crate::overlays::ConfigFocus::Categories => {
                        app.config_focus = crate::overlays::ConfigFocus::Detail;
                    }
                    crate::overlays::ConfigFocus::Detail => {
                        match app.config_category {
                            0 => {
                                // Appearance category:
                                let num_schemes = crate::view::COLOR_SCHEMES.len();
                                let sel_idx = app.config_detail_index % num_schemes;
                                let is_custom = sel_idx == num_schemes - 1;
                                if is_custom {
                                    if !app.config_custom_editing {
                                        app.config_custom_editing = true;
                                        app.custom_color_draft = app.custom_color_scheme.clone();
                                        app.input =
                                            Theme::custom_color_value(&app.custom_color_draft, 0)
                                                .unwrap_or("#000000")
                                                .to_string();
                                        app.set_cursor_end();
                                    } else {
                                        // Save custom palette
                                        if Theme::set_custom_color_value(
                                            &mut app.custom_color_draft,
                                            app.config_detail_index
                                                .saturating_sub(num_schemes)
                                                .min(7),
                                            &app.input,
                                        ) {
                                            app.custom_color_scheme =
                                                app.custom_color_draft.clone();
                                            app.color_scheme = "custom".to_string();
                                            app.theme = Theme::from_color_scheme(
                                                "custom",
                                                &app.custom_color_scheme,
                                            );
                                            let _ =
                                                app.tx.send(AgentRequest::UpdateTuiColorScheme {
                                                    name: app.color_scheme.clone(),
                                                    custom: app.custom_color_scheme.clone(),
                                                });
                                            app.config_custom_editing = false;
                                            app.input.clear();
                                            app.set_cursor(0);
                                        }
                                    }
                                } else {
                                    let schemes = Theme::available_color_schemes();
                                    if let Some(scheme) = schemes.get(sel_idx) {
                                        let name = &scheme.id;
                                        app.color_scheme = name.to_string();
                                        app.theme = Theme::from_color_scheme(
                                            name.as_ref(),
                                            &app.custom_color_scheme,
                                        );
                                        let _ = app.tx.send(AgentRequest::UpdateTuiColorScheme {
                                            name: app.color_scheme.clone(),
                                            custom: app.custom_color_scheme.clone(),
                                        });
                                    }
                                }
                            }
                            1 => {
                                // Transcript category:
                                if app.config_detail_index == 1 {
                                    app.expand_auto_scroll = !app.expand_auto_scroll;
                                }
                            }
                            2 if app.config_detail_index == 0 => {
                                // Behavior category:
                                app.click_outside_dismiss = !app.click_outside_dismiss;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        input::InputAction::ConfigBack => {
            if app.active_modal == Modal::Config {
                if app.config_custom_editing {
                    app.config_custom_editing = false;
                    app.theme =
                        Theme::from_color_scheme(&app.color_scheme, &app.custom_color_scheme);
                    app.custom_color_draft = app.custom_color_scheme.clone();
                    app.input.clear();
                    app.set_cursor(0);
                } else if app.config_focus == crate::overlays::ConfigFocus::Detail {
                    app.config_focus = crate::overlays::ConfigFocus::Categories;
                } else {
                    app.active_modal = Modal::None;
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
            let list_len = if app.active_modal == Modal::Mcp {
                app.session_context
                    .as_ref()
                    .map(|s| s.mcp.len())
                    .unwrap_or(0)
            } else if app.active_modal == Modal::Skills {
                app.session_context
                    .as_ref()
                    .map(|s| s.skills.len())
                    .unwrap_or(0)
            } else if app.active_modal == Modal::Queue {
                app.pending_dispatch
                    .iter()
                    .filter(|item| item.session_id == viewed_session_id)
                    .count()
            } else if app.active_modal == Modal::Btw {
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
                if app.active_modal == Modal::Queue || app.active_modal == Modal::Btw {
                    app.queue_modal_follow = true;
                } else {
                    app.session_modal_follow = true;
                }
            } else if app.active_modal == Modal::Queue {
                // Empty queue: Up/Down is inert.
            } else if app.active_modal == Modal::Btw {
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
                app.active_modal = Modal::None;
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
                app.active_modal = Modal::None;
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
            let idx = app
                .modal_index
                .min(app.host_sessions.len().saturating_sub(1));
            // Creation-order selection, mirroring the dock (see
            // HostSwitchSelected).
            let order = crate::overlays::creation_order(&app.host_sessions);
            if let Some(row) = order.get(idx).map(|&i| &app.host_sessions[i]) {
                let id = row.id.clone();
                tokio::spawn(async move {
                    let project_root =
                        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                    let Some(info) = neenee_runtime::client::discover(&project_root) else {
                        return;
                    };
                    let req = neenee_runtime::serve::ControlRequest::Interrupt {
                        session_id: id.clone(),
                    };
                    if let Err(e) = neenee_runtime::client::control(&info, req).await {
                        tracing::warn!(%e, session=%id, "dashboard interrupt failed");
                    }
                });
                app.notice_toast_message = "interrupt sent".to_string();
                app.notice_toast_severity = NoticeSeverity::Info;
                app.notice_toast_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(1600));
            }
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
        input::InputAction::HostPromptSubmit => {
            let text = app.input.trim().to_string();
            let create_new = app.host_prompt_new;
            app.host_prompting = false;
            app.host_prompt_new = false;
            app.input.clear();
            app.set_cursor(0);
            if !text.is_empty() {
                let project_root =
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                let idx = app
                    .modal_index
                    .min(app.host_sessions.len().saturating_sub(1));
                // Creation-order selection, mirroring the dock.
                let order = crate::overlays::creation_order(&app.host_sessions);
                let selected = order.get(idx).map(|&i| app.host_sessions[i].clone());
                tokio::spawn(async move {
                    let Some(info) = neenee_runtime::client::discover(&project_root) else {
                        tracing::warn!("dashboard control: no daemon discovered");
                        return;
                    };
                    // `n` always creates; `p` prompts the selected
                    // session (creating is impossible without a
                    // selection, so fall back to create).
                    let req = if !create_new && let Some(row) = &selected {
                        neenee_runtime::serve::ControlRequest::SendPrompt {
                            session_id: row.id.clone(),
                            text,
                        }
                    } else {
                        neenee_runtime::serve::ControlRequest::CreateSession {
                            project: project_root.display().to_string(),
                            prompt: Some(text),
                        }
                    };
                    if let Err(e) = neenee_runtime::client::control(&info, req).await {
                        tracing::warn!(%e, "dashboard prompt/create failed");
                    }
                });
                app.notice_toast_message = if create_new {
                    "session created".to_string()
                } else {
                    "task sent".to_string()
                };
                app.notice_toast_severity = NoticeSeverity::Info;
                app.notice_toast_until =
                    Some(std::time::Instant::now() + std::time::Duration::from_millis(1600));
            }
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
            app.active_modal = Modal::None;
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
        input::InputAction::CloseModal => {
            modals::handle_close_modal(app, viewed_session_id);
        }
        input::InputAction::ToggleModalKeymap => {
            // In-modal `?` expand: swap the body for the full keymap
            // page (or close it). Not a nested modal.
            app.modal_keymap_open = !app.modal_keymap_open;
            // Reset the body scroll so the keymap starts at the top.
            match app.active_modal {
                Modal::Connections | Modal::Models => {
                    app.model_scroll = 0;
                    app.model_modal_follow = true;
                }
                Modal::HistorySearch => {
                    app.history_scroll = 0;
                    app.history_modal_follow = true;
                }
                Modal::Help => app.help_scroll = 0,
                Modal::Activity => app.activity_scroll = 0,
                Modal::Permissions => app.permissions_scroll = 0,
                Modal::Tools | Modal::Mcp | Modal::Skills => {
                    app.session_scroll = 0;
                    app.session_modal_follow = true;
                }
                Modal::Config => {
                    app.config_scroll = 0;
                    app.config_detail_scroll = 0;
                }
                Modal::TokenReport => app.token_report_scroll = 0,
                _ => {}
            }
        }
        input::InputAction::TokenReportActivate => {
            if app.active_modal == Modal::TokenReport && !app.token_report_detail {
                let has_turns = app
                    .token_source_report(viewed_session_id)
                    .map(|report| view::token_report_round_count(&report) > 0)
                    .unwrap_or(false);
                if has_turns {
                    app.token_report_detail = true;
                    app.token_report_scroll = 0;
                }
            }
        }
        input::InputAction::ScrollUp => {
            if let Some((scroll, follow)) = app.modal_scroll_field() {
                if let Some(f) = follow {
                    *f = false;
                }
                *scroll = scroll.saturating_sub(1);
            } else {
                // While a permission sheet is open the transcript stays
                // scrollable, so the wheel / page keys drive the
                // conversation behind it, not the sheet's own body.
                app.follow_bottom = false;
                app.pin_summary_line = None;
                // Mouse wheel tick = 4 lines, not 1, so scrolling feels fast
                // and responsive instead of crawling line-by-line.
                app.scroll = app.scroll.saturating_sub(4);
            }
        }
        input::InputAction::ScrollDown => {
            if let Some((scroll, follow)) = app.modal_scroll_field() {
                if let Some(f) = follow {
                    *f = false;
                }
                *scroll = scroll.saturating_add(1);
            } else {
                app.pin_summary_line = None;
                app.scroll = app.scroll.saturating_add(4).min(app.max_scroll);
                if app.scroll >= app.max_scroll {
                    app.follow_bottom = true;
                }
            }
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
                let step = app.view_height.saturating_sub(1).max(1);
                app.follow_bottom = false;
                app.pin_summary_line = None;
                app.scroll = app.scroll.saturating_sub(step);
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
                let step = app.view_height.saturating_sub(1).max(1);
                app.pin_summary_line = None;
                app.scroll = app.scroll.saturating_add(step).min(app.max_scroll);
                if app.scroll >= app.max_scroll {
                    app.follow_bottom = true;
                }
            }
        }
        input::InputAction::ScrollTop => {
            if let Some((scroll, follow)) = app.modal_scroll_field() {
                if let Some(f) = follow {
                    *f = false;
                }
                *scroll = 0;
            } else {
                app.follow_bottom = false;
                app.pin_summary_line = None;
                app.scroll = 0;
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
                app.pin_summary_line = None;
                app.scroll = app.max_scroll;
                app.follow_bottom = true;
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
            return commands::handle_ctrl_c(app, copy_tx, copy_pending);
        }
        input::InputAction::OpenTodos => {
            // Ctrl+T opens the Todos modal — the agent's live task
            // list surfaced on its own overlay. The list is
            // agent-owned and read-only in the TUI; this simply opens
            // the Activity modal pinned to the Todos section, exactly
            // like clicking the todo bar.
            app.active_modal = Modal::Activity;
            app.activity_tab = ActivityTab::Todos;
            app.modal_keymap_open = false;
            app.modal_index = 0;
            app.activity_scroll = 0;
            app.selection = SelectionState::None;
            app.focused_target = None;
            app.drag.cancel();
        }
        input::InputAction::OpenQueue => {
            // F2 opens the queue overview — the full outbox list that
            // the persistent queue bar previews. The selection starts
            // at the front (the next item to pop). This mirrors a
            // click on the queue bar.
            //
            // Opening the modal auto-blocks the viewed session's
            // outbox so items can be managed safely (delete / reorder
            // / re-edit) without one auto-draining mid-edit. Closing
            // the modal (Esc / outside-click) resumes auto-drain —
            // the block here is an editing safety latch, not a
            // persistent user choice (that's `F3`). See the
            // `CloseModal` / outside-click paths for the matching
            // resume.
            app.active_modal = Modal::Queue;
            app.modal_keymap_open = false;
            app.modal_index = 0;
            app.queue_scroll = 0;
            app.queue_modal_follow = true;
            app.selection = SelectionState::None;
            app.focused_target = None;
            app.drag.cancel();
            app.block_queue(viewed_session_id);
        }
        input::InputAction::FocusNextTarget => {
            // Ctrl+↓ (or ↓ while focused): advance to the next step.
            // From no focus this lands on the first (oldest) step.
            app.focus_interactive_target(1);
        }
        input::InputAction::FocusPrevTarget => {
            // Ctrl+↑ (or ↑ while focused): step back. From no focus this
            // lands on the last (nearest-to-prompt) step.
            app.focus_interactive_target(-1);
        }
        input::InputAction::ClearFocusedTarget => {
            // Esc: drop the focus highlight, returning every key to its
            // ordinary input-box meaning.
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
        input::InputAction::ExitEnvoy => {
            app.exit_envoy();
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
                app.esc_armed_ticks = 0;
                let _ = app.tx.send(AgentRequest::ExitSideView);
            }
        }
        input::InputAction::InterruptSide => {
            // Esc inside an aside view (ADR-0103 §2): interrupt the viewed
            // aside's round with the same armed press-twice contract as the
            // main view's Esc interrupt. Never leaves the view, never closes
            // the aside.
            if app.in_side_view && app.esc_armed_ticks > 0 {
                app.esc_armed_ticks = 0;
                if let Some(side_id) = app.side_session_id.clone() {
                    let _ = app.tx.send(AgentRequest::InterruptSide { side_id });
                }
            } else {
                app.esc_armed_ticks = 20;
            }
        }
        input::InputAction::OpenBtwList => {
            // F5 / `/btw list` (ADR-0103 §5): ask the harness for a fresh
            // list and pop the modal once the rows land. The open signal is
            // consumed by the loop's sync stage, so a slow harness reply
            // simply opens with the last known rows and refreshes in place.
            let _ = app.tx.send(AgentRequest::QueryBtwList);
            runtime.open_btw.store(true, Ordering::SeqCst);
        }
        input::InputAction::BtwFocusSelected => {
            // Asides modal Enter (ADR-0103 §5): jump back into the selected
            // aside. The harness replies with `SideViewOpened` carrying the
            // full transcript back-fill; the modal closes on arrival.
            if let Some(row) = app.btw_list.get(app.modal_index) {
                let side_id = row.id.clone();
                app.active_modal = Modal::None;
                app.modal_keymap_open = false;
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
            // The custom-provider filter field re-ranks its suggestion
            // list as the query changes.
            if app.active_modal == Modal::CustomProvider {
                app.on_custom_filter_changed();
            } else if app.active_modal == Modal::Config
                && app.config_custom_editing
                && Theme::set_custom_color_value(
                    &mut app.custom_color_draft,
                    app.config_detail_index
                        .saturating_sub(Theme::available_color_schemes().len())
                        .min(7),
                    &app.input,
                )
            {
                app.theme = Theme::from_color_scheme("custom", &app.custom_color_draft);
            }
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
            if app.active_modal == Modal::CustomProvider {
                app.on_custom_filter_changed();
            } else if app.active_modal == Modal::Config
                && app.config_custom_editing
                && Theme::set_custom_color_value(
                    &mut app.custom_color_draft,
                    app.config_detail_index
                        .saturating_sub(Theme::available_color_schemes().len())
                        .min(7),
                    &app.input,
                )
            {
                app.theme = Theme::from_color_scheme("custom", &app.custom_color_draft);
            }
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
            if app.active_modal == Modal::CustomProvider {
                app.on_custom_filter_changed();
            } else if app.active_modal == Modal::Config
                && app.config_custom_editing
                && Theme::set_custom_color_value(
                    &mut app.custom_color_draft,
                    app.config_detail_index
                        .saturating_sub(Theme::available_color_schemes().len())
                        .min(7),
                    &app.input,
                )
            {
                app.theme = Theme::from_color_scheme("custom", &app.custom_color_draft);
            }
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
            // Top-level `↑` (in an empty composer while the queue is
            // non-empty) recalls the newest staged item into the
            // composer for editing. Purely local — no modal is open,
            // no block state changes.
            match app.recall_queued(viewed_session_id) {
                Some(crate::app::RecallQueued::Restored(dispatch)) => {
                    app.restore_dispatch(dispatch);
                }
                None => {}
            }
        }
        input::InputAction::RecallQueuedSelected => {
            // The queue modal's `Enter` recalls the *selected* item
            // (the `↑/↓` highlight, not always the newest) into the
            // composer and closes the modal. Closing resumes the
            // auto-block the modal set on open.
            let idx = app.modal_index;
            app.active_modal = Modal::None;
            app.resume_queue(viewed_session_id);
            match app.recall_queued_at(viewed_session_id, idx) {
                Some(crate::app::RecallQueued::Restored(dispatch)) => {
                    app.restore_dispatch(dispatch);
                }
                None => {}
            }
        }
        input::InputAction::QueueToggleBlock => {
            // `F3` (top-level or inside the queue modal): toggle the
            // hard block on the viewed session's outbox. While blocked
            // no queued message auto-drains, even after the round
            // completes. This is the persistent user choice, distinct
            // from the modal's editing-safety auto-block.
            app.toggle_queue_block(viewed_session_id);
        }
        input::InputAction::QueueDelete => {
            // `Shift+D` in the queue modal: remove the highlighted
            // item outright. The queue is auto-blocked on open, so the
            // index can't drift under us. Clamp the selection to the
            // now-shorter list.
            if app.active_modal == Modal::Queue {
                let idx = app.modal_index;
                app.remove_queued_at(viewed_session_id, idx);
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
            if app.active_modal == Modal::Queue {
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
            if app.active_modal == Modal::Question
                && let Some(qm) = app.question.take()
            {
                app.question = Some(qm.update(crate::question_model::QuestionAction::Up).0);
                // Moving the highlight re-enables follow so the body
                // scrolls to keep the cursor visible.
                app.question_modal_follow = true;
            }
        }
        input::InputAction::QuestionDown => {
            if app.active_modal == Modal::Question
                && let Some(qm) = app.question.take()
            {
                app.question = Some(qm.update(crate::question_model::QuestionAction::Down).0);
                app.question_modal_follow = true;
            }
        }
        input::InputAction::QuestionToggle => {
            if app.active_modal == Modal::Question
                && let Some(qm) = app.question.take()
            {
                app.question = Some(qm.update(crate::question_model::QuestionAction::Toggle).0);
            }
        }
        input::InputAction::QuestionSelect(n) => {
            if app.active_modal == Modal::Question
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
            if app.active_modal == Modal::Question
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
            if app.active_modal == Modal::Question
                && let Some(qm) = app.question.take()
            {
                app.question = Some(qm.update(crate::question_model::QuestionAction::Previous).0);
                app.question_scroll = 0;
                app.question_modal_follow = true;
            }
        }
        input::InputAction::QuestionCancel => {
            if app.active_modal == Modal::Question
                && let Some(qm) = app.question.take()
            {
                let (_qm, effects) = qm.update(crate::question_model::QuestionAction::Cancel);
                // Cancel discards the model immediately; the Closed
                // effect drives the (empty-answers) reply + drain.
                question_effects::apply(&effects, app, runtime).await;
            }
        }
        input::InputAction::InputSubmit => {
            if app.active_modal == Modal::InputInjection {
                let text = std::mem::take(&mut app.input);
                if let Some(req) = app.pending_input.take() {
                    // Drain the matching front so the per-frame sync
                    // closes the modal and restores the composer draft.
                    runtime.pending_input.lock().await.pop_front();
                    let parent_call_id = runtime.envoy_question_parent.lock().await.remove(&req.id);
                    let _ = app.tx.send(AgentRequest::InputReply {
                        request_id: req.id.clone(),
                        text,
                        parent_call_id,
                    });
                }
                app.restore_input_draft();
                app.active_modal = Modal::None;
            }
        }
        input::InputAction::InputCancel => {
            if app.active_modal == Modal::InputInjection
                && let Some(req) = app.pending_input.take()
            {
                // Empty reply = cancel → the command runs with closed
                // stdin and fails fast with a non-interactive remedy.
                runtime.pending_input.lock().await.pop_front();
                let parent_call_id = runtime.envoy_question_parent.lock().await.remove(&req.id);
                let _ = app.tx.send(AgentRequest::InputReply {
                    request_id: req.id.clone(),
                    text: String::new(),
                    parent_call_id,
                });
                app.restore_input_draft();
                app.active_modal = Modal::None;
            }
        }
        input::InputAction::QuestionInsertChar(c) => {
            if app.active_modal == Modal::Question
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
            if app.active_modal == Modal::Question
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
            app.active_modal = Modal::None;
            app.modal_index = 0;
            app.permission_confirm_always = false;
            app.permission_show_details = false;
            let mut parents = runtime.envoy_permission_parent.lock().await;
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
