//! Paste and escape routing tests.

use super::*;

#[test]
fn esc_closes_slash_completion_menu() {
    let mut input = "/mc".to_string();
    let mut cursor = 3;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(KeyEvent {
            code: KeyCode::Esc,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::None,
            completion_kind: crate::CompletionKind::Slash,
            suggestion_count: 2,
            ..Default::default()
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::CloseCompletion);
    assert_eq!(input, "/mc");
}

#[test]
fn esc_closes_path_completion_menu() {
    let mut input = "@src".to_string();
    let mut cursor = 4;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(KeyEvent {
            code: KeyCode::Esc,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::None,
            completion_kind: crate::CompletionKind::Path,
            suggestion_count: 3,
            suggestion_index: Some(1),
            ..Default::default()
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::CloseCompletion);
}

#[test]
fn esc_falls_through_when_no_completion_is_open() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(KeyEvent {
            code: KeyCode::Esc,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        &mut input,
        &mut cursor,
        InputContext::default(),
        &mut drag,
    );
    assert_eq!(action, InputAction::None);
}

#[test]
fn escape_returns_from_always_confirmation() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(KeyEvent {
            code: KeyCode::Esc,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        &mut input,
        &mut cursor,
        InputContext {
            active_sheet: Some(crate::sheet::SheetKind::Permission),
            is_responding: true,
            permission_confirm_always: true,
            ..Default::default()
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::PermissionBack);
}

#[test]
fn esc_in_models_browse_closes_the_modal() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::Models,
            ..Default::default()
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::CloseModal);
}

#[test]
fn esc_in_connections_browse_closes_the_modal() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::Connections,
            ..Default::default()
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::CloseModal);
}

#[test]
fn escape_clears_focus() {
    assert_eq!(
        key_with_focus(KeyCode::Esc),
        InputAction::ClearFocusedTarget
    );
}

#[test]
fn escape_exits_runner_view() {
    let mut input = String::new();
    assert_eq!(
        key_in_view(KeyCode::Esc, true, &mut input),
        InputAction::ExitRunner
    );
    assert_eq!(
        key_in_view(KeyCode::Esc, false, &mut input),
        InputAction::None
    );
}

#[test]
fn escape_in_side_view_exits_side_view() {
    let mut input = String::new();
    assert_eq!(
        key_in_side_view(KeyCode::Esc, &mut input),
        InputAction::ExitSideView
    );
}

#[test]
fn escape_in_btw_modal_closes_the_modal() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        )),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::Btw,
            in_side_view: true,
            ..Default::default()
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::CloseModal);
}

#[test]
fn bracket_keys_cycle_siblings_only_when_typing_is_empty() {
    let mut input = String::new();
    assert_eq!(
        key_in_view(KeyCode::Char('['), true, &mut input),
        InputAction::PrevSibling
    );
    assert_eq!(
        key_in_view(KeyCode::Char(']'), true, &mut input),
        InputAction::NextSibling
    );

    let mut typing = "x".to_string();
    key_in_view(KeyCode::Char('['), true, &mut typing);
    assert_eq!(typing, "x[");

    let mut other = String::new();
    key_in_view(KeyCode::Char(']'), false, &mut other);
    assert_eq!(other, "]");
}

#[test]
fn esc_in_history_panel_closes_modal_directly() {
    let mut input = "git".to_string();
    let mut cursor = 3;
    let action = run_history_key(&mut input, &mut cursor, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(action, InputAction::CloseModal);
}

#[test]
fn bracketed_paste_routes_in_free_text_modals() {
    let payload = "sk-test-1234";
    for modal in [
        crate::Modal::None,
        crate::Modal::ModelEditor,
        crate::Modal::Models,
        crate::Modal::Connections,
        crate::Modal::HistorySearch,
    ] {
        let mut input = String::new();
        let mut cursor = 0;
        let action = run_paste(payload, &mut input, &mut cursor, modal);
        match action {
            InputAction::BracketedPaste(text) => assert_eq!(
                text, payload,
                "bracketed paste payload should pass through in free-text modal"
            ),
            other => panic!("expected BracketedPaste in free-text modal, got {other:?}"),
        }
        assert!(
            input.is_empty(),
            "BracketedPaste must not mutate the buffer itself"
        );
    }

    let mut input = String::new();
    let mut cursor = 0;
    let action = run_paste(payload, &mut input, &mut cursor, crate::Modal::Help);
    assert_eq!(
        action,
        InputAction::None,
        "bracketed paste should be dropped in Help"
    );

    let config_context = InputContext {
        active_modal: crate::Modal::Config,
        ..Default::default()
    };
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = crate::model::selection::SelectionDrag::default();
    let action = crate::input::process_event(
        crossterm::event::Event::Paste(payload.to_string()),
        &mut input,
        &mut cursor,
        config_context,
        &mut drag,
    );
    assert_eq!(
        action,
        InputAction::None,
        "bracketed paste should be dropped in Config"
    );
}
