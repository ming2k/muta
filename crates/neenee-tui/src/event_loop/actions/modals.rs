//! Modal-surface handlers for the input dispatch match: the model/provider
//! editors' submit paths, the generic close-modal routing, and the shared
//! ↑/↓ modal navigation. Extracted verbatim from the corresponding arms of
//! `dispatch_action`'s match; only `SubmitModelEditor`'s arm-level `continue`
//! became an [`ActionFlow`] value (it already was, inside `dispatch_action`).

use std::sync::atomic::Ordering;

use neenee_contracts::AgentRequest;

use crate::view;
use crate::view::Theme;
use crate::{App, Modal};

use super::super::arm_effort_ignition_if_max;
use super::ActionFlow;

/// Loop stage (input dispatch): the `SubmitCustomProvider` arm.
pub(super) fn handle_submit_custom_provider(app: &mut App) {
    if app.active_modal == Modal::CustomProvider {
        // Commit the focused text field's live value first.
        app.stash_custom_field();
        let name = app.custom_name.trim().to_string();
        let protocol = app.custom_protocol_wire.clone();
        let base_url = app.custom_base_url.trim().to_string();
        let api_key = neenee_contracts::SecretString::from(app.custom_token.trim());
        if let Some(id) = app.custom_edit_id.clone() {
            // Edit mode: update meta (models stay managed in
            // the Models picker). A name is still required.
            // ADR-0046: effort/thinking are no longer
            // provider-level.
            if name.is_empty() {
                app.load_custom_field();
            } else {
                let _ = app.tx.send(AgentRequest::EditProvider {
                    id,
                    name,
                    protocol,
                    base_url,
                    api_key,
                });
                app.input = std::mem::take(&mut app.stashed_input);
                app.set_cursor_end();
                app.custom_field = 0;
                app.custom_edit_id = None;
                app.active_modal = Modal::None;
            }
        } else {
            // Create mode: the model list comes from the template's
            // seeded models, or the single typed Model field when
            // the template exposes one.
            // ADR-0046: new channels start with thinking off;
            // reasoning is opted in per model from the Models
            // picker.
            let models: Vec<String> = if app.custom_fields.contains(&crate::CustomField::Model) {
                vec![app.custom_model.trim().to_string()]
            } else {
                app.custom_models.clone()
            };
            let usable = models.iter().any(|m| !m.trim().is_empty());
            if name.is_empty() || !usable {
                app.load_custom_field();
            } else {
                let _ = app.tx.send(AgentRequest::AddProvider {
                    name,
                    protocol,
                    base_url,
                    api_key,
                    user_agent: app.custom_user_agent.clone(),
                    models,
                    auth: app.custom_auth,
                    template_id: app.custom_template_id.take(),
                });
                app.input = std::mem::take(&mut app.stashed_input);
                app.set_cursor_end();
                app.custom_field = 0;
                app.active_modal = Modal::None;
            }
        }
    }
}

/// Loop stage (input dispatch): the `OpenModelEditor` arm.
pub(super) fn handle_open_model_editor(app: &mut App) {
    if app.active_modal == Modal::Models {
        // `e` on a flat model row. The per-model settings popup
        // opens for any model that exposes effort and/or a
        // separate thinking switch.
        let rows = app.models_flat_filtered();
        if let Some(row) = rows.get(app.modal_index).or_else(|| rows.first())
            && (row.effort.is_some() || row.thinking.is_some())
        {
            let is_builtin = !app.provider_is_custom(&row.provider_id);
            app.editor_return_to = Modal::Models;
            app.editor_target = Some(row.provider_id.clone());
            app.editor_model = row.model.clone();
            app.editor_model_settings_only = true;
            app.editor_target_is_builtin = is_builtin;
            app.editor_key.clear();
            // Default the effort to the model's own configured
            // value, else `medium` clamped onto the model's
            // ladder — a ladder without `medium` (e.g. Kimi
            // K3's low/high/max) must still open with a rung
            // the segmented selector can highlight.
            app.editor_effort = row.effort.clone().unwrap_or_else(|| {
                let model = neenee_contracts::resolve_model(&row.model);
                neenee_contracts::effort::Effort::Medium
                    .clamp_to(model.effort_levels)
                    .as_str()
                    .to_string()
            });
            app.editor_thinking_available = row.thinking.is_some();
            // ADR-0046: reasoning is opt-in where a separate
            // thinking switch exists. OpenAI GPT effort has no
            // thinking switch, so this value is ignored there.
            app.editor_thinking = row.thinking.unwrap_or(false);
            app.editor_field = 1;
            app.input = app.editor_effort.clone();
            app.set_cursor_end();
            app.model_search = false;
            app.active_modal = Modal::ModelEditor;
        }
    } else if app.active_modal == Modal::Connections {
        // `e` in the Connections list. A built-in provider opens
        // the API-key editor (only its auth changes; the model is
        // chosen from the Models picker). A user-defined provider
        // opens the full meta edit form (Name/Protocol/Base
        // URL/Token); its models stay managed in the Models
        // picker.
        let ranked = app.providers_filtered();
        let target = ranked
            .get(app.modal_index)
            .or_else(|| ranked.first())
            .map(|row| (row.id.clone(), row.model.clone(), row.builtin));
        if let Some((id, model, builtin)) = target {
            if builtin {
                app.editor_return_to = Modal::Connections;
                app.editor_target = Some(id);
                app.editor_field = 0;
                app.editor_key.clear();
                app.editor_model = model;
                app.editor_model_settings_only = false;
                app.editor_target_is_builtin = false;
                app.editor_effort = "high".to_string();
                app.editor_thinking_available = false;
                app.editor_thinking = true;
                app.input.clear();
                app.set_cursor(0);
                app.model_search = false;
                app.active_modal = Modal::ModelEditor;
            } else {
                // Pre-fill the edit form from the snapshot row.
                let row = app
                    .provider_picker
                    .rows
                    .iter()
                    .find(|r| r.id == id)
                    .cloned();
                let (name, protocol, base_url, auth) = row
                    .map(|r| (r.name, r.protocol, r.base_url, r.auth))
                    .unwrap_or_default();
                app.model_search = false;
                app.open_edit_provider_editor(id, name, protocol, base_url, auth);
            }
        }
    }
}

/// Loop stage (input dispatch): the `SubmitModelEditor` arm.
pub(super) fn handle_submit_model_editor(app: &mut App) -> ActionFlow {
    if app.active_modal == Modal::ModelEditor
        && let Some(id) = app.editor_target.clone()
    {
        let model = if app.editor_model.trim().is_empty() {
            app.provider_picker
                .rows
                .iter()
                .find(|r| r.id == id)
                .map(|r| r.model.clone())
                .unwrap_or_default()
        } else {
            app.editor_model.trim().to_string()
        };
        if app.editor_model_settings_only {
            // Per-model settings editor (opened from the Models
            // picker). Flush the focused field's
            // live text into its buffer before reading, so a
            // submit while effort is focused captures the value.
            // Field 2 (thinking) is a toggle with no text.
            if app.editor_field == 1 {
                app.editor_effort = app.input.clone();
            }
            let effort = app.editor_effort.clone();
            // Built-in models persist to `[model_reasoning]` (no
            // user-editable channel); user-defined models persist
            // to their channel. ADR-0045.
            if app.editor_target_is_builtin {
                let _ = app.tx.send(AgentRequest::EditModelReasoning {
                    model,
                    effort: Some(effort),
                    thinking: app.editor_thinking_available.then_some(app.editor_thinking),
                });
            } else {
                let _ = app.tx.send(AgentRequest::EditProviderModel {
                    provider_id: id,
                    model,
                    effort: Some(effort),
                    thinking: app.editor_thinking_available.then_some(app.editor_thinking),
                });
            }
            app.input.clear();
            app.set_cursor(0);
            app.editor_target = None;
            app.editor_model_settings_only = false;
            app.editor_target_is_builtin = false;
            app.editor_thinking_available = false;
            app.model_search = false;
            app.model_modal_follow = true;
            app.active_modal = app.editor_return_to;
            arm_effort_ignition_if_max(app);
            return ActionFlow::NextEvent;
        }
        // Key editor (not model-settings-only): this is a
        // built-in provider's API-key edit or a first-key entry.
        // ADR-0046 removed effort/thinking from the provider
        // level, so switching now carries only the key
        // (effort/thinking are set per model from the Models
        // picker `e` editor).
        let key = app.input.trim().to_string();
        let _ = app.tx.send(AgentRequest::SwitchProvider {
            provider_type: id,
            model,
            api_key: if key.is_empty() {
                None
            } else {
                Some(key.into())
            },
            base_url: None,
        });
        // Close to chat: restore the original draft.
        app.input = std::mem::take(&mut app.stashed_input);
        app.set_cursor_end();
        app.editor_target = None;
        app.editor_model_settings_only = false;
        app.editor_target_is_builtin = false;
        app.active_modal = Modal::None;
    }
    ActionFlow::Handled
}

/// Loop stage (input dispatch): the `CloseModal` arm (Esc / generic close
/// routing per open surface).
pub(super) fn handle_close_modal(app: &mut App, viewed_session_id: &str) {
    // Sub-page back-out is checked FIRST (deepest level wins),
    // so Esc from a drill-in always returns to its parent view
    // before any close/quit logic runs — otherwise pressing Esc
    // in e.g. the Sessions › Info sub-view at startup would quit
    // the program instead of dropping back to the sessions list.
    if app.active_modal == Modal::Host && app.host_preview.is_some() {
        // Deepest dashboard layer: first Esc closes the
        // session preview, returning to the dashboard; a
        // second Esc closes the dashboard itself.
        app.host_preview = None;
        app.host_preview_scroll = 0;
    } else if app.active_modal == Modal::Host && app.host_prompting {
        // First Esc cancels the dashboard's inline prompt,
        // returning to the list; a second Esc closes the
        // dashboard. Mirrors the two-stage Esc of the other
        // drill-in sub-layers below.
        app.host_prompting = false;
        app.host_prompt_new = false;
        app.input.clear();
        app.set_cursor(0);
    } else if app.active_modal == Modal::TokenReport && app.token_report_detail {
        // First Esc returns from the turn breakdown to the round list;
        // a second Esc closes the modal.
        app.token_report_detail = false;
        app.token_report_scroll = 0;
    } else if app.active_modal == Modal::Sessions && app.session_info_detail {
        // First Esc returns from the session-info sub-view to
        // the sessions list; a second Esc closes the modal.
        app.session_info_detail = false;
        app.session_detail = None;
        app.session_info_scroll = 0;
    } else if app.startup_overlay == crate::StartupOverlay::SessionsPicker
        && app.active_modal == Modal::Sessions
    {
        // `neenee resume` (no id) opened the picker at startup
        // instead of loading any session: there is no real
        // conversation behind the modal, so closing the *list*
        // (not a sub-view — those are handled above) must quit
        // the program rather than drop into an empty chat.
        tracing::info!(reason = "startup_picker_cancelled", "app exiting");
        app.should_quit.store(true, Ordering::SeqCst);
    } else if app.startup_overlay == crate::StartupOverlay::Dashboard
        && app.active_modal == Modal::Host
    {
        // `neenee dashboard` opened the dashboard over a carrier
        // session the user never asked to converse with: Esc
        // here quits rather than dropping into that chat. Enter
        // on a row (HostSwitchSelected) re-attaches as usual.
        tracing::info!(reason = "startup_dashboard_cancelled", "app exiting");
        app.should_quit.store(true, Ordering::SeqCst);
    } else {
        // Most modals close straight to chat. The model editor
        // and the custom-provider editor instead step back to
        // the picker they were opened from, so a key entry is
        // recoverable with Esc.
        let mut return_to: Option<Modal> = None;
        if app.active_modal == Modal::HistorySearch {
            // Closing from either browse or search: hand the parked
            // draft back so Esc is a true cancel, and clear the
            // search sub-layer / preview flags for the next open.
            app.restore_history_draft();
            app.history_clear_confirm = false;
        } else if matches!(app.active_modal, Modal::Connections | Modal::Models) {
            // The input box may have been borrowed as the fuzzy
            // filter (search sub-layer); hand the parked draft back
            // and clear the search/scroll flags so Esc cancels
            // cleanly. (The two-stage Esc inside search is handled
            // earlier by `ModelExitSearch`; this path is the
            // browse-mode close.)
            app.restore_model_draft();
        } else if app.active_modal == Modal::ModelEditor {
            // Cancel the editor: discard its fields and return to
            // the picker it was opened from in browse mode. The
            // original chat draft stays in stashed_input for when
            // that picker itself closes.
            app.editor_target = None;
            app.editor_model_settings_only = false;
            app.editor_target_is_builtin = false;
            app.input.clear();
            app.set_cursor(0);
            app.model_search = false;
            app.model_modal_follow = true;
            return_to = Some(app.editor_return_to);
        } else if app.active_modal == Modal::CustomProvider {
            // Same as Esc: discard the editor fields and step back
            // to the Connections list; the chat draft stays parked
            // in stashed_input.
            app.input.clear();
            app.set_cursor(0);
            app.custom_field = 0;
            app.model_search = false;
            app.model_modal_follow = true;
            app.modal_index = 0;
            return_to = Some(Modal::Connections);
        } else if app.active_modal == Modal::ConfigThemeCustom {
            // Click-outside closes the settings stack. Discard
            // the transactional custom preview before leaving.
            app.theme = Theme::from_color_scheme(&app.color_scheme, &app.custom_color_scheme);
            app.custom_color_draft = app.custom_color_scheme.clone();
            app.input.clear();
            app.set_cursor(0);
        }
        // The queue modal auto-blocked the outbox on open so
        // items could be managed safely; closing it resumes
        // normal auto-drain. (A persistent block set via `F3`
        // at the top level is unaffected, since the modal's
        // own open/close latch is what's being released here —
        // but to keep this simple and predictable we always
        // resume on close; the user can re-block with F3.)
        if app.active_modal == Modal::Queue {
            app.resume_queue(viewed_session_id);
        }
        app.modal_keymap_open = false;
        app.active_modal = return_to.unwrap_or(Modal::None);
    }
}

/// Loop stage (input dispatch): the `ModalUp` arm (per-modal ↑ navigation).
pub(super) fn handle_modal_up(app: &mut App, viewed_session_id: &str) {
    match app.active_modal {
        Modal::Connections | Modal::Models => {
            // Walk the fuzzy-filtered rows of the *active picker*
            // (providers in Connections, flat (provider, model)
            // pairs in Models), so the cursor never lands on a
            // hidden row (same rule as the history-search modal).
            let count = app.picker_row_count();
            app.modal_index = if count == 0 {
                0
            } else if app.modal_index == 0 {
                count - 1
            } else {
                app.modal_index - 1
            };
            app.model_modal_follow = true;
        }
        Modal::HistorySearch => {
            // Up/Down walk the fuzzy-filtered list, not the raw
            // history, so the cursor never lands on an entry the
            // user cannot actually see or select.
            let count = app.history_rows().len();
            app.modal_index = if count == 0 {
                0
            } else if app.modal_index == 0 {
                count - 1
            } else {
                app.modal_index - 1
            };
            app.history_modal_follow = true;
            // In preview mode the body shows the focused entry's
            // full text, so moving to another entry re-anchors it
            // to the top.
            if app.history_preview {
                app.history_scroll = 0;
            }
        }
        Modal::Permission => {
            let count = if app.permission_confirm_always { 2 } else { 4 };
            app.modal_index = if app.modal_index == 0 {
                count - 1
            } else {
                app.modal_index - 1
            };
        }
        Modal::Sessions => {
            let count = app.sessions_overview.len();
            app.modal_index = if count == 0 {
                0
            } else if app.modal_index == 0 {
                count - 1
            } else {
                app.modal_index - 1
            };
            app.session_modal_follow = true;
        }
        Modal::Host => {
            if app.host_focus == crate::overlays::DashboardFocus::List {
                let count = app.host_sessions.len();
                app.modal_index = if count == 0 {
                    0
                } else if app.modal_index == 0 {
                    count - 1
                } else {
                    app.modal_index - 1
                };
                app.host_modal_follow = true;
                // Re-engage body-follow so the moved selection stays on
                // screen (cleared again on manual page/wheel scroll).
                app.session_modal_follow = true;
            } else {
                app.host_detail_scroll = app.host_detail_scroll.saturating_sub(1);
            }
        }
        Modal::Permissions => {
            let count = app
                .session_context
                .as_ref()
                .map(|s| s.permissions.len())
                .unwrap_or(0);
            app.modal_index = if count == 0 {
                0
            } else if app.modal_index == 0 {
                count - 1
            } else {
                app.modal_index - 1
            };
        }
        Modal::Config => {
            // Config root: cycle up through the category list.
            // Count matches `categories()` in config.rs.
            let count = 2usize;
            app.modal_index = (app.modal_index + count - 1) % count;
        }
        Modal::ConfigTheme => {
            let count = crate::view::overlays::config_theme::ROW_COUNT;
            app.modal_index = (app.modal_index + count - 1) % count;
        }
        Modal::ConfigLayout => {
            let count = crate::view::overlays::config_layout::ROW_COUNT;
            app.modal_index = (app.modal_index + 1) % count;
        }
        Modal::TokenReport => {
            if app.token_report_detail {
                app.token_report_scroll = app.token_report_scroll.saturating_sub(1);
            } else {
                let count = app
                    .token_source_report(viewed_session_id)
                    .map(|report| view::token_report_round_count(&report))
                    .unwrap_or(0)
                    .max(1);
                app.modal_index = (app.modal_index + count - 1) % count;
            }
        }
        Modal::Queue => {
            // Wheel/PageUp: scroll the queue body. Clearing the
            // follow flag lets the user browse freely until they
            // navigate with ↑/↓ again.
            app.queue_scroll = app.queue_scroll.saturating_sub(1);
            app.queue_modal_follow = false;
        }
        Modal::Btw => {
            // Asides list (ADR-0103 §5): wheel/PageUp scrolls the
            // body; ↑/↓ navigation is handled by the shared
            // SessionSelect path.
            app.btw_scroll = app.btw_scroll.saturating_sub(1);
            app.btw_modal_follow = false;
        }
        Modal::Help
        | Modal::Question
        | Modal::ModelEditor
        | Modal::ProviderTemplate
        | Modal::OauthPending
        | Modal::CustomProvider
        | Modal::ConfigThemeCustom
        | Modal::InputInjection
        | Modal::Tools
        | Modal::Mcp
        | Modal::Skills
        | Modal::Activity
        | Modal::None => {}
    }
}

/// Loop stage (input dispatch): the `ModalDown` arm (per-modal ↓ navigation).
pub(super) fn handle_modal_down(app: &mut App, viewed_session_id: &str) {
    match app.active_modal {
        Modal::Connections | Modal::Models => {
            let count = app.picker_row_count().max(1);
            app.modal_index = (app.modal_index + 1) % count;
            app.model_modal_follow = true;
        }
        Modal::HistorySearch => {
            let count = app.history_rows().len().max(1);
            app.modal_index = (app.modal_index + 1) % count;
            app.history_modal_follow = true;
            if app.history_preview {
                app.history_scroll = 0;
            }
        }
        Modal::Permission => {
            let count = if app.permission_confirm_always { 2 } else { 4 };
            app.modal_index = (app.modal_index + 1) % count;
        }
        Modal::Sessions => {
            let count = app.sessions_overview.len().max(1);
            app.modal_index = (app.modal_index + 1) % count;
            // Re-engage body-follow so the moved selection stays on
            // screen (cleared again on manual page/wheel scroll).
            app.session_modal_follow = true;
        }
        Modal::Host => {
            if app.host_focus == crate::overlays::DashboardFocus::List {
                let count = app.host_sessions.len().max(1);
                app.modal_index = (app.modal_index + 1) % count;
                app.host_modal_follow = true;
            } else {
                app.host_detail_scroll = app.host_detail_scroll.saturating_add(1);
            }
        }
        Modal::Permissions => {
            let count = app
                .session_context
                .as_ref()
                .map(|s| s.permissions.len())
                .unwrap_or(0)
                .max(1);
            app.modal_index = (app.modal_index + 1) % count;
        }
        Modal::Config => {
            // Config root: cycle down through the category list.
            // Count matches `categories()` in config.rs.
            let count = 2usize;
            app.modal_index = (app.modal_index + 1) % count;
        }
        Modal::ConfigTheme => {
            let count = crate::view::overlays::config_theme::ROW_COUNT;
            app.modal_index = (app.modal_index + 1) % count;
        }
        Modal::ConfigLayout => {
            let count = crate::view::overlays::config_layout::ROW_COUNT;
            app.modal_index = (app.modal_index + 1) % count;
        }
        Modal::TokenReport => {
            if app.token_report_detail {
                app.token_report_scroll = app.token_report_scroll.saturating_add(1);
            } else {
                let count = app
                    .token_source_report(viewed_session_id)
                    .map(|report| view::token_report_round_count(&report))
                    .unwrap_or(0)
                    .max(1);
                app.modal_index = (app.modal_index + 1) % count;
            }
        }
        Modal::Queue => {
            // Wheel/PageDown: scroll the queue body. Clearing the
            // follow flag lets the user browse freely until they
            // navigate with ↑/↓ again.
            app.queue_scroll = app.queue_scroll.saturating_add(1);
            app.queue_modal_follow = false;
        }
        Modal::Btw => {
            // Asides list (ADR-0103 §5): wheel/PageDown scrolls the
            // body; ↑/↓ navigation is handled by the shared
            // SessionSelect path.
            app.btw_scroll = app.btw_scroll.saturating_add(1);
            app.btw_modal_follow = false;
        }
        Modal::Help
        | Modal::Question
        | Modal::ModelEditor
        | Modal::ProviderTemplate
        | Modal::OauthPending
        | Modal::CustomProvider
        | Modal::ConfigThemeCustom
        | Modal::InputInjection
        | Modal::Tools
        | Modal::Mcp
        | Modal::Skills
        | Modal::Activity
        | Modal::None => {}
    }
}
