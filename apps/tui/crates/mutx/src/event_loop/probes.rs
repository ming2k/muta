//! Input pre-dispatch probes (Dropdown, Selection Relay, Delete Confirm).

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

use crate::components::dropdown::DropdownEventOutcome;
use crate::input::{self};
use crate::model::selection::{SelectionState, floor_grapheme_boundary, inclusive_grapheme_end};
use crate::{App, Modal, ProviderDeleteChoice, SelectionEdge};

/// Probe a raw input event against an active **whole-input selection**.
pub(crate) fn probe_input_selection_relay(
    app: &mut App,
    event: &Event,
) -> Option<input::InputAction> {
    if !app.input_selection_relays_arrows() {
        return None;
    }
    let Event::Key(key) = event else {
        return None;
    };
    if !matches!(key.kind, KeyEventKind::Press) {
        return None;
    }

    let word_chord = key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);

    let step_from_head = |app: &mut App, forward: bool, word: bool| {
        app.adopt_caret_from_input_selection(SelectionEdge::Head);
        let count = app.input.chars().count();
        let at = app.cursor_position.min(count);
        let target = if word {
            let chars: Vec<char> = app.input.chars().collect();
            let mut i = at;
            if forward {
                while i < chars.len() && chars[i].is_whitespace() {
                    i += 1;
                }
                while i < chars.len() && !chars[i].is_whitespace() {
                    i += 1;
                }
            } else {
                while i > 0 && chars[i - 1].is_whitespace() {
                    i -= 1;
                }
                while i > 0 && !chars[i - 1].is_whitespace() {
                    i -= 1;
                }
            }
            i
        } else if forward {
            (at + 1).min(count)
        } else {
            at.saturating_sub(1)
        };
        app.set_cursor(target);
    };

    match (key.code, word_chord) {
        (KeyCode::Left, false) => {
            step_from_head(app, false, false);
            Some(input::InputAction::None)
        }
        (KeyCode::Right, false) => {
            step_from_head(app, true, false);
            Some(input::InputAction::None)
        }
        (KeyCode::Left, true) => {
            step_from_head(app, false, true);
            Some(input::InputAction::None)
        }
        (KeyCode::Right, true) => {
            step_from_head(app, true, true);
            Some(input::InputAction::None)
        }
        (KeyCode::Up | KeyCode::Down, _) => {
            app.adopt_caret_from_input_selection(SelectionEdge::Head);
            Some(input::InputAction::None)
        }
        (KeyCode::Home, _) => {
            if let Some((start, _)) = app.selection.active_normalized_range() {
                let byte =
                    floor_grapheme_boundary(&app.input, start.byte_offset).min(app.input.len());
                let pos = app.input[..byte].chars().count();
                app.selection = SelectionState::None;
                app.drag.cancel();
                app.set_cursor(pos);
            } else {
                app.adopt_caret_from_input_selection(SelectionEdge::Tail);
            }
            Some(input::InputAction::None)
        }
        (KeyCode::End, _) => {
            if let Some((_, end)) = app.selection.active_normalized_range() {
                let byte = inclusive_grapheme_end(&app.input, end.byte_offset).min(app.input.len());
                let pos = app.input[..byte].chars().count();
                app.selection = SelectionState::None;
                app.drag.cancel();
                app.set_cursor(pos);
            } else {
                app.adopt_caret_from_input_selection(SelectionEdge::Head);
            }
            Some(input::InputAction::None)
        }
        (KeyCode::Backspace | KeyCode::Delete, _) => {
            app.delete_input_selection();
            Some(input::InputAction::Backspace)
        }
        (KeyCode::Char('w') | KeyCode::Char('u') | KeyCode::Char('k'), _)
            if key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            app.delete_input_selection();
            Some(input::InputAction::Backspace)
        }
        (KeyCode::Char('d'), _) if key.modifiers.contains(KeyModifiers::ALT) => {
            app.delete_input_selection();
            Some(input::InputAction::Backspace)
        }
        _ => None,
    }
}

pub(crate) fn probe_config_dropdown(app: &mut App, event: &Event) -> Option<input::InputAction> {
    if app.config_dropdown.is_none() {
        return None;
    }

    let Event::Key(k) = event else {
        return Some(input::InputAction::None);
    };

    if !matches!(k.kind, KeyEventKind::Press) {
        return Some(input::InputAction::None);
    }

    let (mut dropdown, anchor) = app.config_dropdown.take()?;
    let outcome = dropdown.handle_key(*k);
    match outcome {
        DropdownEventOutcome::Ignored => {
            app.config_dropdown = Some((dropdown, anchor));
            None
        }
        DropdownEventOutcome::Handled => {
            app.config_dropdown = Some((dropdown, anchor));
            Some(input::InputAction::None)
        }
        DropdownEventOutcome::Cancelled => {
            app.config_dropdown = None;
            Some(input::InputAction::None)
        }
        DropdownEventOutcome::Confirmed(payload) => {
            let ctx = dropdown.context.as_deref().unwrap_or("");
            match ctx {
                "websearch_provider" => {
                    if payload == "add_new" {
                        let add_dropdown =
                            crate::views::settings::build_add_web_connection_dropdown(0);
                        let anchor = crate::components::dropdown::DropdownAnchor::center_screen();
                        app.config_dropdown = Some((add_dropdown, anchor));
                        return Some(input::InputAction::None);
                    }
                    let _ = app
                        .tx
                        .send(muta_contracts::AgentRequest::UpdateWebSearchConfig(
                            Box::new(muta_contracts::WebSearchConfigUpdate {
                                provider: Some(payload),
                                ..Default::default()
                            }),
                        ));
                }
                "websearch_reader" => {
                    if payload == "add_new" {
                        let add_dropdown =
                            crate::views::settings::build_add_web_connection_dropdown(1);
                        let anchor = crate::components::dropdown::DropdownAnchor::center_screen();
                        app.config_dropdown = Some((add_dropdown, anchor));
                        return Some(input::InputAction::None);
                    }
                    let _ = app
                        .tx
                        .send(muta_contracts::AgentRequest::UpdateWebSearchConfig(
                            Box::new(muta_contracts::WebSearchConfigUpdate {
                                reader: Some(payload),
                                ..Default::default()
                            }),
                        ));
                }
                "add_search_connection" => {
                    let (name, needs_key) = match payload.as_str() {
                        "tavily" => ("Tavily AI Search", true),
                        "bocha" => ("Bocha AI Search", true),
                        "searxng" => ("SearXNG Instance", false),
                        "parallel" => ("Parallel Search", true),
                        "custom-search" => ("Custom Search Relay", true),
                        _ => ("Exa Search", true),
                    };
                    if needs_key {
                        app.push_transient_surface(Modal::ModelEditor);
                        app.editor_target = Some(format!("web_search:{}", payload));
                        app.editor_model = name.to_string();
                        app.editor_key.clear();
                        app.editor_field = 0;
                        app.input.clear();
                        app.set_cursor(0);
                    } else {
                        let id = format!("{}-{}", payload, chrono::Utc::now().timestamp() % 10000);
                        let new_conn = muta_contracts::WebSearchConnection {
                            id: id.clone(),
                            name: Some(name.to_string()),
                            preset_id: Some(payload),
                            api_key_env: None,
                            base_url: None,
                            custom_headers: None,
                            enabled: true,
                        };
                        let _ = app
                            .tx
                            .send(muta_contracts::AgentRequest::UpdateWebSearchConfig(
                                Box::new(muta_contracts::WebSearchConfigUpdate {
                                    upsert_search_connection: Some(new_conn),
                                    provider: Some(id),
                                    ..Default::default()
                                }),
                            ));
                    }
                }
                "add_reader_connection" => {
                    let (name, needs_key) = match payload.as_str() {
                        "firecrawl" => ("Firecrawl Reader", true),
                        "custom-reader" => ("Custom Web Reader", true),
                        _ => ("Jina Reader", true),
                    };
                    if needs_key {
                        app.push_transient_surface(Modal::ModelEditor);
                        app.editor_target = Some(format!("web_reader:{}", payload));
                        app.editor_model = name.to_string();
                        app.editor_key.clear();
                        app.editor_field = 0;
                        app.input.clear();
                        app.set_cursor(0);
                    } else {
                        let id = format!("{}-{}", payload, chrono::Utc::now().timestamp() % 10000);
                        let new_conn = muta_contracts::WebReaderConnection {
                            id: id.clone(),
                            name: Some(name.to_string()),
                            preset_id: Some(payload),
                            api_key_env: None,
                            base_url: None,
                            custom_headers: None,
                            enabled: true,
                        };
                        let _ = app
                            .tx
                            .send(muta_contracts::AgentRequest::UpdateWebSearchConfig(
                                Box::new(muta_contracts::WebSearchConfigUpdate {
                                    upsert_reader_connection: Some(new_conn),
                                    reader: Some(id),
                                    ..Default::default()
                                }),
                            ));
                    }
                }
                _ => {}
            }
            app.config_dropdown = None;
            Some(input::InputAction::None)
        }
    }
}

pub(crate) fn probe_delete_overlay(app: &mut App, event: &Event) -> Option<input::InputAction> {
    if app.pending_provider_delete.is_none() || app.active_modal() != Modal::Connections {
        return None;
    }

    let Event::Key(k) = event else {
        return Some(input::InputAction::None);
    };

    if !matches!(k.kind, KeyEventKind::Press) {
        return Some(input::InputAction::None);
    }

    match (k.modifiers, k.code) {
        (KeyModifiers::CONTROL, KeyCode::Char('c')) => {
            Some(input::InputAction::DeleteProviderCancel)
        }
        (KeyModifiers::NONE, KeyCode::Esc) => Some(input::InputAction::DeleteProviderCancel),
        (KeyModifiers::NONE, KeyCode::Enter) => {
            if app.provider_delete_focus == ProviderDeleteChoice::Delete {
                Some(input::InputAction::DeleteProviderConfirm)
            } else {
                Some(input::InputAction::DeleteProviderCancel)
            }
        }
        (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Left)
        | (KeyModifiers::CONTROL, KeyCode::Char('b'))
        | (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char('h')) => {
            app.provider_delete_focus = ProviderDeleteChoice::Cancel;
            Some(input::InputAction::None)
        }
        (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Right)
        | (KeyModifiers::CONTROL, KeyCode::Char('f'))
        | (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char('l')) => {
            app.provider_delete_focus = ProviderDeleteChoice::Delete;
            Some(input::InputAction::None)
        }
        (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Tab)
        | (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Down)
        | (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Up) => {
            app.provider_delete_focus = match app.provider_delete_focus {
                ProviderDeleteChoice::Cancel => ProviderDeleteChoice::Delete,
                ProviderDeleteChoice::Delete => ProviderDeleteChoice::Cancel,
            };
            Some(input::InputAction::None)
        }
        _ => Some(input::InputAction::None),
    }
}
