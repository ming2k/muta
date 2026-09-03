//! Modal input routing tests: models/connections modals, fuzzy filters, question modals, queue/oauth editors, focus.

use super::*;

#[test]
fn star_in_models_modal_toggles_model_favorite() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(KeyEvent::new(KeyCode::Char('*'), KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::Models,
            ..Default::default()
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::ProviderPickerToggleFavorite);
}

#[test]
fn star_in_connections_modal_is_inert_favorite_is_model_level() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(KeyEvent::new(KeyCode::Char('*'), KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::Connections,
            ..Default::default()
        },
        &mut drag,
    );
    assert_ne!(action, InputAction::ProviderPickerToggleFavorite);
}

#[test]
fn letter_in_models_modal_feeds_the_fuzzy_filter() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::Models,
            model_searching: true,
            ..Default::default()
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::InsertChar('k'));
    assert_eq!(input, "k");
}

#[test]
fn letter_in_models_browse_mode_is_inert_and_slash_enters_search() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let ctx = || InputContext {
        active_modal: crate::Modal::Models,
        ..Default::default()
    };
    let letter = process_event(
        Event::Key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        ctx(),
        &mut drag,
    );
    assert_eq!(letter, InputAction::None);
    assert_eq!(input, "");
    let slash = process_event(
        Event::Key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        ctx(),
        &mut drag,
    );
    assert_eq!(slash, InputAction::ModelEnterSearch);
    assert_eq!(input, "");
}

#[test]
fn q_while_focused_in_transcript_bounces_to_composer() {
    let mut input = String::new();
    let mut cursor = 0;
    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('q'),
        KeyModifiers::NONE,
        crate::Modal::None,
        true,
    );
    assert_eq!(action, InputAction::ClearFocusedTarget);
    assert_eq!(input, "q");
    assert_eq!(cursor, 1);
}

#[test]
fn mouse_wheel_scrolls_question_modal_body() {
    let mk = |kind| {
        let mut input = String::new();
        let mut cursor = 0;
        let mut drag = SelectionDrag::default();
        process_event(
            Event::Mouse(crossterm::event::MouseEvent {
                kind,
                column: 5,
                row: 5,
                modifiers: KeyModifiers::NONE,
            }),
            &mut input,
            &mut cursor,
            InputContext {
                active_sheet: Some(crate::sheet::SheetKind::Question),
                ..Default::default()
            },
            &mut drag,
        )
    };
    assert_eq!(
        mk(MouseEventKind::ScrollUp),
        InputAction::Wheel {
            up: true,
            x: 5,
            y: 5
        }
    );
    assert_eq!(
        mk(MouseEventKind::ScrollDown),
        InputAction::Wheel {
            up: false,
            x: 5,
            y: 5
        }
    );
}

#[test]
fn mouse_selection_drag_tracks_within_selectable_modals() {
    let mut drag = SelectionDrag::default();
    drag.start(SemanticCursor::new(0, 0, 0));
    let mut input = String::new();
    let mut cursor = 0;
    let action = process_event(
        Event::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 10,
            row: 10,
            modifiers: KeyModifiers::NONE,
        }),
        &mut input,
        &mut cursor,
        InputContext {
            active_sheet: Some(crate::sheet::SheetKind::Permission),
            ..Default::default()
        },
        &mut drag,
    );
    assert_eq!(
        action,
        InputAction::SelectionUpdate { x: 10, y: 10 },
        "Active drag inside selectable modal should update selection coordinates"
    );
}
