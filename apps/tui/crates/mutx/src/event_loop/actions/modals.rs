//! Modal-surface handlers for the input dispatch match: the model/provider
//! editors' submit paths, the generic close-modal routing, and the shared
//! ↑/↓ modal navigation. Extracted verbatim from the corresponding arms of
//! `dispatch_action`'s match; only `SubmitModelEditor`'s arm-level `continue`
//! became an [`ActionFlow`] value (it already was, inside `dispatch_action`).

use std::sync::atomic::Ordering;

use muta_contracts::AgentRequest;

use crate::overlays;
use crate::{App, Modal};

use super::ActionFlow;

/// Loop stage (input dispatch): the `SubmitCustomProvider` arm.
pub(super) fn handle_submit_custom_provider(app: &mut App) {
    if app.active_modal() == Modal::CustomProvider {
        // Commit the focused text field's live value first.
        app.stash_custom_field();
        let name = app.custom_name.trim().to_string();
        let protocol = app
            .custom_protocol_wire
            .parse::<muta_contracts::WireProtocol>()
            .expect("provider editor must carry a registered wire protocol");
        let base_url = app.custom_base_url.trim().to_string();
        let api_key = muta_contracts::SecretString::from(app.custom_token.trim());
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
                    client_identity: None,
                });
                // Phase 3 (ADR-0133): the chain ends at chat. Pop the nav
                // frame (the picker this editor was opened over) and hand
                // the composer draft back from that view's per-view slot.
                app.restore_chat_after_editor_chain();
                app.custom_field = 0;
                app.custom_edit_id = None;
            }
        } else {
            // Create mode: the model list comes from the preset's
            // seeded models, or the single typed Model field when
            // the preset exposes one.
            // ADR-0046: new channels start with thinking off;
            // reasoning is opted in per model from the Models
            // picker.
            let models: Vec<String> = if app.custom_fields.contains(&crate::CustomField::Model) {
                vec![muta_contracts::sanitize_model_id(&app.custom_model)]
            } else {
                app.custom_models
                    .iter()
                    .map(|m| muta_contracts::sanitize_model_id(m))
                    .collect()
            };
            let usable = models.iter().any(|m| !m.is_empty());
            if name.is_empty() || !usable {
                app.load_custom_field();
            } else {
                // `custom-openai` is an editor definition, not a curated
                // preset. New custom connections persist as pure-custom
                // declarations with no preset id; the catalog still accepts
                // the old id when loading existing configurations.
                let preset_id = created_connection_preset_id(app.custom_preset_id.take());
                let _ = app.tx.send(AgentRequest::AddProvider {
                    name,
                    protocol,
                    base_url,
                    api_key,
                    user_agent: app.custom_user_agent.clone(),
                    models,
                    auth: app.custom_auth,
                    preset_id,
                    client_identity: None,
                });
                app.restore_chat_after_editor_chain();
                app.custom_field = 0;
            }
        }
    }
}

/// Convert the editor definition id into the persisted connection shape.
/// `custom-openai` remains load-compatible, but new custom connections are
/// pure-custom records rather than preset-derived records.
fn created_connection_preset_id(preset_id: Option<String>) -> Option<String> {
    preset_id.filter(|id| id != "custom-openai")
}

/// Loop stage (input dispatch): the `OpenModelEditor` arm.
pub(super) fn handle_open_model_editor(app: &mut App) {
    if app.active_modal() == Modal::Models {
        // `e` on a flat model row. The per-model settings popup
        // opens for any model that exposes effort and/or a
        // separate thinking switch.
        let rows = app.models_flat_filtered();
        if let Some(row) = rows.get(app.modal_index).or_else(|| rows.first())
            && (row.effort.is_some() || row.thinking.is_some())
        {
            let is_builtin = !app.provider_is_custom(&row.provider_id);
            // Phase 3 (ADR-0133): the picker that opened this editor goes on
            // the navigation stack; its Esc/submit pops back to it.
            app.push_transient_surface(Modal::ModelEditor);
            app.editor_target = Some(row.provider_id.clone());
            app.editor_model = row.model.clone();
            app.editor_model_settings_only = true;
            app.editor_target_is_builtin = is_builtin;
            app.editor_key.clear();
            // Load the stored capability overrides (ADR-0149 layer 1) so
            // the editor opens showing what is already forced, if anything.
            let stored = muta_persistence::route_settings::RouteSettingsStore::load()
                .settings_for(&row.provider_id, &row.model)
                .and_then(|r| r.capability_overrides.clone());
            app.editor_vision_override = stored.as_ref().and_then(|o| o.vision);
            app.editor_tool_override = stored.as_ref().and_then(|o| o.tool_call);
            // Default the effort to the model's own configured
            // value, else `medium` clamped onto the model's
            // ladder — a ladder without `medium` (e.g. Kimi
            // K3's low/high/max) must still open with a rung
            // the segmented selector can highlight.
            app.editor_effort = row.effort.clone().unwrap_or_else(|| {
                let model = muta_contracts::resolve_model(&row.model);
                muta_contracts::effort::Effort::Medium
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
        }
    } else if app.active_modal() == Modal::Connections {
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
                app.push_transient_surface(Modal::ModelEditor);
                app.editor_target = Some(id);
                app.editor_field = 0;
                app.editor_key.clear();
                app.editor_model = model;
                app.editor_model_settings_only = false;
                app.editor_target_is_builtin = false;
                app.editor_effort = "high".to_string();
                app.editor_thinking_available = false;
                app.editor_vision_override = None;
                app.editor_tool_override = None;
                app.editor_thinking = true;
                app.input.clear();
                app.set_cursor(0);
                app.model_search = false;
            } else {
                // Pre-fill the edit form from the snapshot row.
                let row = app
                    .provider_picker
                    .rows
                    .iter()
                    .find(|r| r.id == id)
                    .cloned();
                let (name, protocol, base_url, auth, is_preset) = row
                    .map(|r| {
                        (
                            r.name,
                            r.protocol,
                            r.base_url,
                            r.auth,
                            !r.preset_id.is_empty() && r.preset_id != "custom-openai",
                        )
                    })
                    .unwrap_or((
                        String::new(),
                        String::new(),
                        String::new(),
                        muta_contracts::ConnectionAuth::ApiKey,
                        false,
                    ));
                app.model_search = false;
                app.open_edit_provider_editor(id, name, protocol, base_url, auth, is_preset);
            }
        }
    }
}

/// Loop stage (input dispatch): the `SubmitModelEditor` arm.
pub(super) fn handle_submit_model_editor(app: &mut App) -> ActionFlow {
    if app.active_modal() == Modal::ModelEditor
        && let Some(target) = app.editor_target.clone()
    {
        if let Some(payload) = target.strip_prefix("web_search:") {
            let key = app.input.trim().to_string();
            let (preset_id, name) = match payload {
                "tavily" => (Some("tavily".to_string()), "Tavily AI Search".to_string()),
                "bocha" => (Some("bocha".to_string()), "Bocha AI Search".to_string()),
                "searxng" => (Some("searxng".to_string()), "SearXNG Instance".to_string()),
                "parallel" => (Some("parallel".to_string()), "Parallel Search".to_string()),
                "custom-search" => (None, "Custom Search Relay".to_string()),
                _ => (Some("exa".to_string()), "Exa Search".to_string()),
            };
            let id = format!("{}-{}", payload, chrono::Utc::now().timestamp() % 10000);
            let new_conn = muta_contracts::WebSearchConnection {
                id: id.clone(),
                name: Some(name),
                preset_id,
                api_key_env: None,
                base_url: None,
                custom_headers: None,
                enabled: true,
            };

            let mut update = muta_contracts::WebSearchConfigUpdate {
                upsert_search_connection: Some(new_conn),
                provider: Some(id),
                ..Default::default()
            };
            if !key.is_empty() {
                match payload {
                    "tavily" => update.tavily_api_key = Some(key),
                    "bocha" => update.bocha_api_key = Some(key),
                    "parallel" => update.parallel_api_key = Some(key),
                    "exa" => update.exa_api_key = Some(key),
                    _ => {}
                }
            }
            let _ = app.tx.send(AgentRequest::UpdateWebSearchConfig(Box::new(update)));

            app.input.clear();
            app.set_cursor(0);
            app.editor_target = None;
            app.pop_transient_surface();
            return ActionFlow::NextEvent;
        }

        if let Some(payload) = target.strip_prefix("web_reader:") {
            let key = app.input.trim().to_string();
            let (preset_id, name) = match payload {
                "firecrawl" => (Some("firecrawl".to_string()), "Firecrawl Reader".to_string()),
                "custom-reader" => (None, "Custom Web Reader".to_string()),
                _ => (Some("jina".to_string()), "Jina Reader".to_string()),
            };
            let id = format!("{}-{}", payload, chrono::Utc::now().timestamp() % 10000);
            let new_conn = muta_contracts::WebReaderConnection {
                id: id.clone(),
                name: Some(name),
                preset_id,
                api_key_env: None,
                base_url: None,
                custom_headers: None,
                enabled: true,
            };

            let mut update = muta_contracts::WebSearchConfigUpdate {
                upsert_reader_connection: Some(new_conn),
                reader: Some(id),
                ..Default::default()
            };
            if !key.is_empty() {
                match payload {
                    "jina" => update.jina_api_key = Some(key),
                    _ => {}
                }
            }
            let _ = app.tx.send(AgentRequest::UpdateWebSearchConfig(Box::new(update)));

            app.input.clear();
            app.set_cursor(0);
            app.editor_target = None;
            app.pop_transient_surface();
            return ActionFlow::NextEvent;
        }

        let id = target;
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
                    overrides: Some(muta_contracts::CapabilityOverrides {
                        vision: app.editor_vision_override,
                        tool_call: app.editor_tool_override,
                        ..Default::default()
                    }),
                });
            } else {
                let _ = app.tx.send(AgentRequest::EditProviderModel {
                    provider_id: id,
                    model,
                    effort: Some(effort),
                    thinking: app.editor_thinking_available.then_some(app.editor_thinking),
                    overrides: Some(muta_contracts::CapabilityOverrides {
                        vision: app.editor_vision_override,
                        tool_call: app.editor_tool_override,
                        ..Default::default()
                    }),
                });
            }
            app.input.clear();
            app.set_cursor(0);
            app.editor_target = None;
            app.editor_model_settings_only = false;
            app.editor_target_is_builtin = false;
            app.editor_thinking_available = false;
            app.editor_vision_override = None;
            app.editor_tool_override = None;
            app.model_search = false;
            app.model_modal_follow = true;
            app.pop_transient_surface();
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
        // Close to chat: the chain ends here (phase 3, ADR-0133). Pop the
        // nav frame (the picker the editor was opened over) and hand the
        // composer draft back from that view's per-view slot.
        app.restore_chat_after_editor_chain();
        app.editor_target = None;
        app.editor_model_settings_only = false;
        app.editor_target_is_builtin = false;
    }
    ActionFlow::Handled
}

/// Loop stage (input dispatch): the `CloseModal` arm (Esc / generic close
/// routing per open surface).
pub(crate) fn handle_close_modal(app: &mut App, _viewed_session_id: &str) {
    // Sub-page back-out is checked FIRST (deepest level wins),
    // so Esc from a drill-in always returns to its parent view
    // before any close/quit logic runs — otherwise pressing Esc
    // in e.g. the Sessions › Info sub-view at startup would quit
    // the program instead of dropping back to the sessions list.
    // One step back through any drill-in sub-layer (ADR-0133 phase 4):
    // the single shared pop — Esc here and the outside-click mirror below
    // can no longer drift apart. A view with a sub-layer open stays up.
    if app.pop_sublayer() {
        // Sub-layer closed; the parent view keeps the surface.
    } else if app.startup_overlay == crate::StartupOverlay::SessionsPicker
        && app.active_modal() == Modal::Sessions
    {
        // `mutx attach` (no id) opened the picker at startup
        // instead of loading any session: there is no real
        // conversation behind the modal, so closing the *list*
        // (not a sub-view — those are handled above) must quit
        // the program rather than drop into an empty chat.
        tracing::info!(reason = "startup_picker_cancelled", "app exiting");
        app.should_quit.store(true, Ordering::SeqCst);
    } else if app.startup_overlay == crate::StartupOverlay::Dashboard
        && app.active_modal() == Modal::Host
    {
        // `mutx dashboard` opened the dashboard over a carrier
        // session the user never asked to converse with: Esc
        // here quits rather than dropping into that chat. Enter
        // on a row (HostSwitchSelected) re-attaches as usual.
        tracing::info!(reason = "startup_dashboard_cancelled", "app exiting");
        app.should_quit.store(true, Ordering::SeqCst);
    } else {
        // Retained browse views hide instead of closing (ADR-0139), and the
        // quick switcher cancels back to its origin surface — both via the
        // shared dismiss verb. State saved / origin restored, surface
        // dismissed; the next open restores exactly where the user was.
        // Handled before the modal-specific close logic below, which is for
        // surfaces that have not migrated (or are not views).
        if app.dismiss_surface() {
            return;
        }
        // Most modals close straight to chat. The model editor
        // and the custom-provider editor instead step back to
        // the picker they were opened from, so a key entry is
        // recoverable with Esc.
        // HistorySearch / Connections / Models no longer need branches here:
        // `dismiss_surface` (checked above) hides them with the per-view
        // draft handed back (ADR-0139).
        if app.active_modal() == Modal::ModelEditor {
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
            app.pop_transient_surface();
        } else if app.active_modal() == Modal::CustomProvider {
            // Same as Esc: discard the editor fields and step back
            // to the Connections list; the chat draft stays parked
            // in stashed_input.
            app.input.clear();
            app.set_cursor(0);
            app.custom_field = 0;
            app.model_search = false;
            app.model_modal_follow = true;
            app.modal_index = 0;
            app.pop_transient_surface();
        }
        // Queue's exit hook (the open-time auto-block release) now lives in
        // `hide_active_panel` — every hide path releases it, not just this
        // one (ADR-0139).
        app.modal_keymap_open = false;
        if !matches!(app.active_modal(), Modal::Models | Modal::Connections) {
            app.show_chat_surface();
        }
    }
}

/// Loop stage (input dispatch): the `ModalUp` arm (per-modal ↑ navigation).
pub(crate) fn handle_modal_up(app: &mut App, viewed_session_id: &str) {
    match app.active_modal() {
        Modal::Connections if app.connection_info_detail => {
            app.connection_info_scroll = app.connection_info_scroll.saturating_sub(1);
        }
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
                // Moving the dock selection cancels an armed kill confirm:
                // the target of the confirm is the session, not the key.
                super::super::actions::host::cancel_kill_confirm(app);
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
        Modal::Config => match app.config_focus {
            crate::overlays::ConfigFocus::Categories => {
                let count = crate::overlays::ConfigCategory::ALL.len();
                app.config_category = (app.config_category + count - 1) % count;
                app.config_detail_index = 0;
                app.config_detail_scroll = 0;
            }
            crate::overlays::ConfigFocus::Detail => {
                let ws_path = if app.current_workspace.is_empty() {
                    None
                } else {
                    Some(std::path::Path::new(&app.current_workspace))
                };
                let count = match app.config_category {
                    0 => crate::view::Theme::available_color_schemes_with_workspace(ws_path).len().max(1),
                    1 => 5usize,
                    2 => 1usize,
                    3 => 10usize,
                    _ => 4usize,
                };
                if count > 0 {
                    app.config_detail_index = (app.config_detail_index + count - 1) % count;
                }
                if app.config_category == 0 {
                    let schemes = crate::view::Theme::available_color_schemes_with_workspace(ws_path);
                    if let Some(scheme) =
                        schemes.get(app.config_detail_index % schemes.len().max(1))
                    {
                        app.theme = crate::view::Theme::from_color_scheme_with_workspace(
                            &scheme.id,
                            &app.custom_color_scheme,
                            ws_path,
                        );
                    }
                }
            }
        },
        Modal::Telemetry => {
            if app.telemetry_tab == crate::modal::TelemetryTab::Overview {
                app.telemetry_scroll = app.telemetry_scroll.saturating_sub(1);
            } else if app.telemetry_turn.is_some() {
                // Attempt inspector: a documentary body, arrows scroll.
                app.telemetry_scroll = app.telemetry_scroll.saturating_sub(1);
            } else if app.telemetry_detail {
                // Round detail (turns list): arrows move the turn cursor.
                let report = app.token_source_report(viewed_session_id);
                let round_index = app.modal_index.min(
                    report
                        .as_ref()
                        .map(|report| overlays::telemetry_round_count(report).saturating_sub(1))
                        .unwrap_or(0),
                );
                let count = report
                    .as_ref()
                    .map(|report| overlays::telemetry_attempt_count(report, round_index))
                    .unwrap_or(0)
                    .max(1);
                app.telemetry_turn_cursor = (app.telemetry_turn_cursor + count - 1) % count;
            } else {
                let count = app
                    .token_source_report(viewed_session_id)
                    .map(|report| overlays::telemetry_round_count(&report))
                    .unwrap_or(0)
                    .max(1);
                app.modal_index = (app.modal_index + count - 1) % count;
            }
        }
        Modal::UsageStats => {
            // The usage overlay scrolls as one body (no per-row selection).
            app.usage_stats_scroll = app.usage_stats_scroll.saturating_sub(1);
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
        Modal::Tree => {
            let count = crate::overlays::tree::flatten_tree(&app.session_tree).len();
            app.modal_index = if count == 0 {
                0
            } else if app.modal_index == 0 {
                count - 1
            } else {
                app.modal_index - 1
            };
            app.tree_modal_follow = true;
        }
        Modal::Help
        | Modal::Question
        | Modal::ModelEditor
        | Modal::ProviderPreset
        | Modal::OauthPending
        | Modal::CustomProvider
        | Modal::InputInjection
        | Modal::Tools
        | Modal::Mcp
        | Modal::Skills
        | Modal::Activity
        | Modal::ViewSwitcher
        | Modal::None => {}
    }
}

/// Loop stage (input dispatch): the `ModalDown` arm (per-modal ↓ navigation).
pub(crate) fn handle_modal_down(app: &mut App, viewed_session_id: &str) {
    match app.active_modal() {
        Modal::Connections if app.connection_info_detail => {
            app.connection_info_scroll = app.connection_info_scroll.saturating_add(1);
        }
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
                // Same as ModalUp: a selection move cancels the confirm.
                super::super::actions::host::cancel_kill_confirm(app);
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
        Modal::Config => match app.config_focus {
            crate::overlays::ConfigFocus::Categories => {
                let count = crate::overlays::ConfigCategory::ALL.len();
                app.config_category = (app.config_category + 1) % count;
                app.config_detail_index = 0;
                app.config_detail_scroll = 0;
            }
            crate::overlays::ConfigFocus::Detail => {
                let ws_path = if app.current_workspace.is_empty() {
                    None
                } else {
                    Some(std::path::Path::new(&app.current_workspace))
                };
                let count = match app.config_category {
                    0 => crate::view::Theme::available_color_schemes_with_workspace(ws_path).len().max(1),
                    1 => 5usize,
                    2 => 1usize,
                    3 => 10usize,
                    _ => 4usize,
                };
                if count > 0 {
                    app.config_detail_index = (app.config_detail_index + 1) % count;
                }
                if app.config_category == 0 {
                    let schemes = crate::view::Theme::available_color_schemes_with_workspace(ws_path);
                    if let Some(scheme) =
                        schemes.get(app.config_detail_index % schemes.len().max(1))
                    {
                        app.theme = crate::view::Theme::from_color_scheme_with_workspace(
                            &scheme.id,
                            &app.custom_color_scheme,
                            ws_path,
                        );
                    }
                }
            }
        },
        Modal::Telemetry => {
            if app.telemetry_tab == crate::modal::TelemetryTab::Overview {
                app.telemetry_scroll = app.telemetry_scroll.saturating_add(1);
            } else if app.telemetry_turn.is_some() {
                // Attempt inspector: a documentary body, arrows scroll.
                app.telemetry_scroll = app.telemetry_scroll.saturating_add(1);
            } else if app.telemetry_detail {
                // Round detail (turns list): arrows move the turn cursor.
                let report = app.token_source_report(viewed_session_id);
                let round_index = app.modal_index.min(
                    report
                        .as_ref()
                        .map(|report| overlays::telemetry_round_count(report).saturating_sub(1))
                        .unwrap_or(0),
                );
                let count = report
                    .as_ref()
                    .map(|report| overlays::telemetry_attempt_count(report, round_index))
                    .unwrap_or(0)
                    .max(1);
                app.telemetry_turn_cursor = (app.telemetry_turn_cursor + 1) % count;
            } else {
                let count = app
                    .token_source_report(viewed_session_id)
                    .map(|report| overlays::telemetry_round_count(&report))
                    .unwrap_or(0)
                    .max(1);
                app.modal_index = (app.modal_index + 1) % count;
            }
        }
        Modal::UsageStats => {
            // The usage overlay scrolls as one body (no per-row selection).
            app.usage_stats_scroll = app.usage_stats_scroll.saturating_add(1);
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
        Modal::Tree => {
            let count = crate::overlays::tree::flatten_tree(&app.session_tree)
                .len()
                .max(1);
            app.modal_index = (app.modal_index + 1) % count;
            app.tree_modal_follow = true;
        }
        Modal::Help
        | Modal::Question
        | Modal::ModelEditor
        | Modal::ProviderPreset
        | Modal::OauthPending
        | Modal::CustomProvider
        | Modal::InputInjection
        | Modal::Tools
        | Modal::Mcp
        | Modal::Skills
        | Modal::Activity
        | Modal::ViewSwitcher
        | Modal::None => {}
    }
}

pub(crate) fn effective_reasoning_effort(app: &App) -> Option<&str> {
    app.provider_picker
        .rows
        .iter()
        .find(|row| row.id == app.current_provider)
        .and_then(|row| row.model_info.iter().find(|m| m.model == app.current_model))
        .and_then(|m| {
            let show = match m.protocol.as_str() {
                "anthropic" => m.thinking == Some(true),
                _ => m.effort.is_some(),
            };
            if show { m.effort.as_deref() } else { None }
        })
}

pub(crate) fn arm_effort_ignition_if_max(app: &mut App) {
    if effective_reasoning_effort(app) == Some("max") && app.effort_ignition_epoch.is_none() {
        app.effort_ignition_epoch = Some(std::time::Instant::now());
    }
}

pub(crate) fn activate_picked_model(app: &mut App, id: String, model: String, key_ready: bool) {
    if key_ready {
        let _ = app.tx.send(AgentRequest::SwitchProvider {
            provider_type: id,
            model,
            api_key: None,
            base_url: None,
        });
        arm_effort_ignition_if_max(app);
        app.dismiss_surface();
    } else if app.provider_row_auth(&id).is_oauth() {
        let auth = app.provider_row_auth(&id);
        let method = auth
            .oauth_provider_id()
            .and_then(muta_providers::oauth::config_by_provider_id)
            .and_then(|config| config.effective_default_login_method())
            .or_else(|| auth.default_login_method())
            .unwrap_or(muta_contracts::LoginMethod::Device);
        let _ = app.tx.send(AgentRequest::ConnectProvider { id, method });
        app.dismiss_surface();
    } else {
        app.push_transient_surface(Modal::ModelEditor);
        app.editor_target = Some(id);
        app.editor_field = 0;
        app.editor_key.clear();
        app.editor_model = model;
        app.editor_model_settings_only = false;
        app.editor_target_is_builtin = false;
        app.editor_effort = "high".to_string();
        app.editor_thinking = true;
        app.input.clear();
        app.set_cursor(0);
        app.model_search = false;
    }
}

pub(crate) async fn handle_permission_submit(
    app: &mut App,
    runtime: &crate::event_loop::runtime::UiRuntime,
) {
    let one_off = app.pending_permission.as_ref().is_some_and(|r| r.one_off);
    let reject_idx = if one_off { 1 } else { 2 };
    let details_idx = if one_off { 2 } else { 3 };
    if app.permission_confirm_always {
        if app.modal_index == 1 {
            app.permission_confirm_always = false;
            app.modal_index = 1;
            return;
        }
    } else {
        if app.modal_index == details_idx {
            app.permission_show_details = !app.permission_show_details;
            app.permission_scroll = 0;
            return;
        }
        if !one_off && app.modal_index == 1 {
            app.permission_confirm_always = true;
            app.permission_show_details = false;
            app.modal_index = 0;
            return;
        }
    }
    if let Some(request) = app.pending_permission.take() {
        let decision = if app.permission_confirm_always {
            muta_contracts::PermissionDecision::Always
        } else {
            match app.modal_index {
                0 => muta_contracts::PermissionDecision::Once,
                i if i == reject_idx => muta_contracts::PermissionDecision::Reject,
                _ => muta_contracts::PermissionDecision::Reject,
            }
        };
        let request_id = request.id;
        let parent_call_id = runtime
            .runner_permission_parent
            .lock()
            .await
            .remove(&request_id);
        let _ = app.tx.send(AgentRequest::PermissionReply {
            request_id: request_id.clone(),
            decision,
            parent_call_id,
        });
        if decision == muta_contracts::PermissionDecision::Reject {
            let queued: Vec<muta_contracts::PermissionRequest> =
                runtime.pending_permission.lock().await.drain(..).collect();
            let mut parents = runtime.runner_permission_parent.lock().await;
            for pending in queued {
                let parent_call_id = parents.remove(&pending.id);
                let _ = app.tx.send(AgentRequest::PermissionReply {
                    request_id: pending.id,
                    decision: muta_contracts::PermissionDecision::Reject,
                    parent_call_id,
                });
            }
            app.pending_permission = None;
            app.pop_transient_surface();
        } else {
            let mut queue = runtime.pending_permission.lock().await;
            queue.retain(|r| r.id != request_id);
            app.pending_permission = queue.front().cloned();
            drop(queue);
            if app.pending_permission.is_none() {
                app.pop_transient_surface();
            }
        }
        app.modal_index = 0;
        app.permission_scroll = 0;
        app.permission_max_scroll = 0;
        app.permission_confirm_always = false;
        app.permission_show_details = false;
    }
}

pub(crate) fn modal_page_step(app: &App) -> usize {
    let h = if app.modal_body_height > 0 {
        app.modal_body_height
    } else {
        app.view_height
    };
    h.saturating_sub(1).max(1) as usize
}

pub(crate) mod question_effects {
    use super::{AgentRequest, App};
    use crate::event_loop::runtime::UiRuntime;
    use std::sync::atomic::Ordering;

    pub(crate) async fn apply(
        effects: &[crate::question_model::QuestionEffect],
        app: &mut App,
        runtime: &UiRuntime,
    ) {
        for effect in effects {
            match effect {
                crate::question_model::QuestionEffect::Reply {
                    request_id,
                    answers,
                } => {
                    if request_id == crate::trust_gate::TRUST_GATE_REQUEST_ID {
                        runtime.trust_gate_dismissed.store(true, Ordering::SeqCst);
                        if let Some(command) = crate::trust_gate::answer_to_command(answers) {
                            let _ = app.tx.send(AgentRequest::SlashCommand(command));
                        }
                        continue;
                    }
                    let parent_call_id = runtime
                        .runner_question_parent
                        .lock()
                        .await
                        .remove(request_id);
                    let _ = app.tx.send(AgentRequest::UserQuestionReply {
                        request_id: request_id.clone(),
                        answers: answers.clone(),
                        parent_call_id,
                    });
                }
                crate::question_model::QuestionEffect::Cancelled { request_id } => {
                    if request_id == crate::trust_gate::TRUST_GATE_REQUEST_ID {
                        runtime.trust_gate_dismissed.store(true, Ordering::SeqCst);
                        continue;
                    }
                    let parent_call_id = runtime
                        .runner_question_parent
                        .lock()
                        .await
                        .remove(request_id);
                    let _ = app.tx.send(AgentRequest::UserQuestionReply {
                        request_id: request_id.clone(),
                        answers: Vec::new(),
                        parent_call_id,
                    });
                }
                crate::question_model::QuestionEffect::Closed { request_id } => {
                    let mut queue = runtime.pending_question.lock().await;
                    queue.retain(|r| r.id != *request_id);
                    if queue.is_empty() {
                        app.question = None;
                        app.pop_transient_surface();
                        app.modal_index = 0;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::created_connection_preset_id;

    #[test]
    fn custom_connection_does_not_persist_a_preset_id() {
        assert_eq!(
            created_connection_preset_id(Some("custom-openai".to_string())),
            None
        );
        assert_eq!(
            created_connection_preset_id(Some("openai".to_string())).as_deref(),
            Some("openai")
        );
    }
}
