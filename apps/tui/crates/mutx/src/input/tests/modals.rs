//! Modal input routing tests: models/connections modals, fuzzy filters, question modals, queue/oauth editors, focus.

use super::*;
use crate::surfaces::{DialogKind, OverlaySurface};

#[test]
fn star_in_models_modal_toggles_model_favorite() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = route_event(
        Event::Key(KeyEvent::new(KeyCode::Char('*'), KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        Dispatch {
            overlay: Some(OverlaySurface::Dialog(DialogKind::Models)),
            ..Default::default()
        },
        &ModalKeys::default(),
        &SheetKeys::default(),
        &ViewKeys::default(),
        &mut drag,
    );
    assert_eq!(action, InputAction::ProviderPickerToggleFavorite);
}

#[test]
fn x_in_models_modal_blocks_model() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = route_event(
        Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        Dispatch {
            overlay: Some(OverlaySurface::Dialog(DialogKind::Models)),
            ..Default::default()
        },
        &ModalKeys::default(),
        &SheetKeys::default(),
        &ViewKeys::default(),
        &mut drag,
    );
    assert_eq!(action, InputAction::ProviderPickerBlockModel);
}

#[test]
fn star_in_connections_modal_is_inert_favorite_is_model_level() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = route_event(
        Event::Key(KeyEvent::new(KeyCode::Char('*'), KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        Dispatch {
            overlay: Some(OverlaySurface::Dialog(DialogKind::Connections)),
            ..Default::default()
        },
        &ModalKeys::default(),
        &SheetKeys::default(),
        &ViewKeys::default(),
        &mut drag,
    );
    assert_ne!(action, InputAction::ProviderPickerToggleFavorite);
}

#[test]
fn letter_in_models_modal_feeds_the_fuzzy_filter() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = route_event(
        Event::Key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        Dispatch {
            overlay: Some(OverlaySurface::Dialog(DialogKind::Models)),
            ..Default::default()
        },
        &ModalKeys {
            model_searching: true,
            ..Default::default()
        },
        &SheetKeys::default(),
        &ViewKeys::default(),
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
    let ctx = || Dispatch {
        overlay: Some(OverlaySurface::Dialog(DialogKind::Models)),
        ..Default::default()
    };
    let letter = route_event(
        Event::Key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        ctx(),
        &ModalKeys::default(),
        &SheetKeys::default(),
        &ViewKeys::default(),
        &mut drag,
    );
    assert_eq!(letter, InputAction::None);
    assert_eq!(input, "");
    let slash = route_event(
        Event::Key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        ctx(),
        &ModalKeys::default(),
        &SheetKeys::default(),
        &ViewKeys::default(),
        &mut drag,
    );
    assert_eq!(slash, InputAction::ModelEnterSearch);
    assert_eq!(input, "");
}

#[test]
fn q_while_focused_in_transcript_is_inert() {
    let mut input = String::new();
    let mut cursor = 0;
    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('q'),
        KeyModifiers::NONE,
        SurfaceFixture::None,
        true,
    );
    assert_eq!(action, InputAction::None);
    assert_eq!(input, "");
    assert_eq!(cursor, 0);
}

#[test]
fn mouse_wheel_scrolls_question_modal_body() {
    let mk = |kind| {
        let mut input = String::new();
        let mut cursor = 0;
        let mut drag = SelectionDrag::default();
        route_event(
            Event::Mouse(crossterm::event::MouseEvent {
                kind,
                column: 5,
                row: 5,
                modifiers: KeyModifiers::NONE,
            }),
            &mut input,
            &mut cursor,
            Dispatch {
                sheet: Some(crate::sheet::SheetKind::Question),
                ..Default::default()
            },
            &ModalKeys::default(),
            &SheetKeys::default(),
            &ViewKeys::default(),
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
    let action = route_event(
        Event::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 10,
            row: 10,
            modifiers: KeyModifiers::NONE,
        }),
        &mut input,
        &mut cursor,
        Dispatch {
            sheet: Some(crate::sheet::SheetKind::Permission),
            ..Default::default()
        },
        &ModalKeys::default(),
        &SheetKeys::default(),
        &ViewKeys::default(),
        &mut drag,
    );
    assert_eq!(
        action,
        InputAction::SelectionUpdate { x: 10, y: 10 },
        "Active drag inside selectable modal should update selection coordinates"
    );
}
