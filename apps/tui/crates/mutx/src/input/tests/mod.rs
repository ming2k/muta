//! The input test suite, split by interaction concern. Shared key-event
//! builders live here; per-concern groups are sibling modules.

use super::*;
use crossterm::event::{KeyEvent, KeyEventKind, KeyEventState};

fn enter(input: &mut String, exact: bool) -> InputAction {
    let mut cursor = input.chars().count();
    let mut drag = SelectionDrag::default();
    process_event(
        Event::Key(KeyEvent {
            code: KeyCode::Enter,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        input,
        &mut cursor,
        InputContext {
            completion_kind: crate::CompletionKind::None,
            suggestion_count: 0,
            has_exact_suggestion: exact,
            ..Default::default()
        },
        &mut drag,
    )
}

fn enter_with_completion(
    input: &mut String,
    kind: crate::CompletionKind,
    suggestion_count: usize,
    suggestion_index: Option<usize>,
    has_exact_suggestion: bool,
) -> InputAction {
    let mut cursor = input.chars().count();
    let mut drag = SelectionDrag::default();
    process_event(
        Event::Key(KeyEvent {
            code: KeyCode::Enter,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        input,
        &mut cursor,
        InputContext {
            completion_kind: kind,
            suggestion_count,
            has_exact_suggestion,
            suggestion_index,
            ..Default::default()
        },
        &mut drag,
    )
}

fn enter_shell(input: &mut String) -> InputAction {
    let mut cursor = input.chars().count();
    let mut drag = SelectionDrag::default();
    process_event(
        Event::Key(KeyEvent {
            code: KeyCode::Enter,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        input,
        &mut cursor,
        InputContext::default(),
        &mut drag,
    )
}

fn key_in_view(code: KeyCode, in_runner_view: bool, input: &mut String) -> InputAction {
    key_in_side_view_with(code, input, move |ctx| {
        ctx.in_runner_view = in_runner_view;
        ctx.in_side_view = false;
        // Surface dispatch keys off the explicit view (ADR-0172), not the
        // legacy flags.
        ctx.current_view = if in_runner_view {
            crate::surfaces::View::Runner
        } else {
            crate::surfaces::View::Session
        };
    })
}

fn key_in_side_view_with(
    code: KeyCode,
    input: &mut String,
    tune: impl FnOnce(&mut InputContext),
) -> InputAction {
    let mut cursor = input.chars().count();
    let mut drag = SelectionDrag::default();
    let mut context = InputContext {
        in_side_view: true,
        current_view: crate::surfaces::View::Side,
        ..Default::default()
    };
    tune(&mut context);
    process_event(
        Event::Key(crossterm::event::KeyEvent::new(code, KeyModifiers::NONE)),
        input,
        &mut cursor,
        context,
        &mut drag,
    )
}

fn key_in_side_view(code: KeyCode, input: &mut String) -> InputAction {
    key_in_side_view_with(code, input, |_| {})
}

fn key_with_focus(code: KeyCode) -> InputAction {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    process_event(
        Event::Key(crossterm::event::KeyEvent::new(code, KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        InputContext {
            has_focused_target: true,
            ..Default::default()
        },
        &mut drag,
    )
}

fn run_key(
    input: &mut String,
    cursor: &mut usize,
    code: KeyCode,
    modifiers: KeyModifiers,
    modal: crate::Modal,
    has_focus: bool,
) -> InputAction {
    let mut drag = SelectionDrag::default();
    process_event(
        Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        input,
        cursor,
        InputContext {
            active_modal: modal,
            has_focused_target: has_focus,
            history_searching: modal == crate::Modal::HistorySearch,
            model_searching: matches!(modal, crate::Modal::Models | crate::Modal::Connections),
            ..Default::default()
        },
        &mut drag,
    )
}

fn run_sheet_key(
    input: &mut String,
    cursor: &mut usize,
    code: KeyCode,
    modifiers: KeyModifiers,
    kind: crate::sheet::SheetKind,
    has_focus: bool,
) -> InputAction {
    let mut drag = SelectionDrag::default();
    process_event(
        Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        input,
        cursor,
        InputContext {
            active_modal: crate::Modal::None,
            active_sheet: Some(kind),
            has_focused_target: has_focus,
            ..Default::default()
        },
        &mut drag,
    )
}

fn run_history_key(
    input: &mut String,
    cursor: &mut usize,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> InputAction {
    let mut drag = SelectionDrag::default();
    process_event(
        Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        input,
        cursor,
        InputContext {
            active_modal: crate::Modal::HistorySearch,
            history_searching: true,
            ..Default::default()
        },
        &mut drag,
    )
}

fn editor_key(code: KeyCode, field: u8, input: &mut String) -> InputAction {
    let mut cursor = input.chars().count();
    let mut drag = SelectionDrag::default();
    process_event(
        Event::Key(crossterm::event::KeyEvent::new(code, KeyModifiers::NONE)),
        input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::ModelEditor,
            editor_field: Some(field),
            ..Default::default()
        },
        &mut drag,
    )
}

fn compose_key(
    code: KeyCode,
    modifiers: KeyModifiers,
    input: &mut String,
    cursor: &mut usize,
) -> InputAction {
    let mut drag = SelectionDrag::default();
    process_event(
        Event::Key(KeyEvent::new(code, modifiers)),
        input,
        cursor,
        InputContext::default(),
        &mut drag,
    )
}

fn pageup_key() -> InputAction {
    key_without_modal(KeyCode::PageUp)
}

fn key_without_modal(code: KeyCode) -> InputAction {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    process_event(
        Event::Key(crossterm::event::KeyEvent::new(code, KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        InputContext::default(),
        &mut drag,
    )
}

fn compose_key_with_completion(
    code: KeyCode,
    completion_kind: crate::CompletionKind,
    suggestion_count: usize,
    exact: bool,
) -> InputAction {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    process_event(
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        InputContext {
            completion_kind,
            suggestion_count,
            has_exact_suggestion: exact,
            suggestion_index: Some(0),
            ..Default::default()
        },
        &mut drag,
    )
}

fn run_paste(
    text: &str,
    input: &mut String,
    cursor: &mut usize,
    modal: crate::Modal,
) -> InputAction {
    let mut drag = SelectionDrag::default();
    process_event(
        Event::Paste(text.to_string()),
        input,
        cursor,
        InputContext {
            active_modal: modal,
            history_searching: modal == crate::Modal::HistorySearch,
            model_searching: matches!(modal, crate::Modal::Models | crate::Modal::Connections),
            ..Default::default()
        },
        &mut drag,
    )
}

fn multiline_arrow(seed: &str, cursor: usize, code: KeyCode) -> (InputAction, usize) {
    let mut input = seed.to_string();
    let mut cur = cursor;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(crossterm::event::KeyEvent::new(code, KeyModifiers::NONE)),
        &mut input,
        &mut cur,
        InputContext::default(),
        &mut drag,
    );
    (action, cur)
}

fn leaked_char(c: char) -> Event {
    Event::Key(KeyEvent {
        code: KeyCode::Char(c),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

fn leaked_esc() -> Event {
    Event::Key(KeyEvent {
        code: KeyCode::Esc,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

fn drain_guard(events: &[Event]) -> (usize, usize) {
    let mut g = SgrLeakGuard::default();
    let mut dropped = 0;
    let mut accepted = 0;
    for ev in events {
        match g.feed(ev) {
            Feed::Drop => dropped += 1,
            Feed::Accept => accepted += 1,
        }
    }
    (accepted, dropped)
}

mod editing;
mod modals;
mod navigation;
mod paste_escape;
mod submit;
