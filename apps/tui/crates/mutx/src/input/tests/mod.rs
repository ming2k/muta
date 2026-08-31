//! The input test suite, split by interaction concern. Shared key-event
//! builders live here; per-concern groups are sibling modules.

//! Tests for input handling, extracted from `mod.rs` so the production
//! input code stays focused. This module is wired in via the
//! `#[cfg(test)] mod tests;` declaration at the bottom of `mod.rs` and
//! reaches the production items through `super::*`, exactly as the
//! former inline module did.
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
            active_modal: crate::Modal::None,
            session_info_detail: false,
            connection_info_detail: false,
            is_responding: false,
            completion_kind: crate::CompletionKind::Slash,
            suggestion_count: 1,
            has_exact_suggestion: exact,
            suggestion_index: None,
            completion_dismissed: false,
            has_trigger_text: false,
            permission_confirm_always: false,
            permission_show_details: false,
            in_runner_view: false,
            in_side_view: false,
            has_focused_target: false,
            has_queued: false,
            queue_pointer_armed: false,
            history_searching: false,
            model_searching: false,
            modal_keymap_open: false,
            editor_field: None,
            custom_provider_field: None,
            question_other_highlighted: false,
            history_clear_confirm: false,
            host_prompting: false,
            config_focus: Default::default(),
            leader_chord: crate::app::LeaderChord::None,
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
            active_modal: crate::Modal::None,
            session_info_detail: false,
            connection_info_detail: false,
            is_responding: false,
            completion_kind: kind,
            suggestion_count,
            has_exact_suggestion,
            suggestion_index,
            completion_dismissed: false,
            has_trigger_text: false,
            permission_confirm_always: false,
            permission_show_details: false,
            in_runner_view: false,
            in_side_view: false,
            has_focused_target: false,
            has_queued: false,
            queue_pointer_armed: false,
            history_searching: false,
            model_searching: false,
            modal_keymap_open: false,
            editor_field: None,
            custom_provider_field: None,
            question_other_highlighted: false,
            history_clear_confirm: false,
            host_prompting: false,
            config_focus: Default::default(),
            leader_chord: crate::app::LeaderChord::None,
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
        InputContext {
            active_modal: crate::Modal::None,
            session_info_detail: false,
            connection_info_detail: false,
            is_responding: false,
            completion_kind: crate::CompletionKind::None,
            suggestion_count: 0,
            has_exact_suggestion: false,
            suggestion_index: None,
            completion_dismissed: false,
            has_trigger_text: false,
            permission_confirm_always: false,
            permission_show_details: false,
            in_runner_view: false,
            in_side_view: false,
            has_focused_target: false,
            has_queued: false,
            queue_pointer_armed: false,
            history_searching: false,
            model_searching: false,
            modal_keymap_open: false,
            editor_field: None,
            custom_provider_field: None,
            question_other_highlighted: false,
            history_clear_confirm: false,
            host_prompting: false,
            config_focus: Default::default(),
            leader_chord: crate::app::LeaderChord::None,
        },
        &mut drag,
    )
}

fn key_in_view(code: KeyCode, in_runner_view: bool, input: &mut String) -> InputAction {
    key_in_side_view_with(code, input, move |ctx| {
        ctx.in_runner_view = in_runner_view;
        ctx.in_side_view = false;
    })
}

/// Key handling inside a `/btw` aside view (ADR-0103 §2), with a context
/// hook so variants can compose view state.
fn key_in_side_view_with(
    code: KeyCode,
    input: &mut String,
    tune: impl FnOnce(&mut InputContext),
) -> InputAction {
    let mut cursor = input.chars().count();
    let mut drag = SelectionDrag::default();
    let mut context = InputContext {
        active_modal: crate::Modal::None,
        session_info_detail: false,
        connection_info_detail: false,
        is_responding: false,
        completion_kind: crate::CompletionKind::None,
        suggestion_count: 0,
        has_exact_suggestion: false,
        suggestion_index: None,
        completion_dismissed: false,
        has_trigger_text: false,
        permission_confirm_always: false,
        permission_show_details: false,
        in_runner_view: false,
        in_side_view: true,
        has_focused_target: false,
        has_queued: false,
        queue_pointer_armed: false,
        history_searching: false,
        model_searching: false,
        modal_keymap_open: false,
        editor_field: None,
        custom_provider_field: None,
        question_other_highlighted: false,
        history_clear_confirm: false,
        host_prompting: false,

        config_focus: Default::default(),
        leader_chord: crate::app::LeaderChord::None,
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
            active_modal: crate::Modal::None,
            session_info_detail: false,
            connection_info_detail: false,
            is_responding: false,
            completion_kind: crate::CompletionKind::None,
            suggestion_count: 0,
            has_exact_suggestion: false,
            suggestion_index: None,
            completion_dismissed: false,
            has_trigger_text: false,
            permission_confirm_always: false,
            permission_show_details: false,
            in_runner_view: false,
            in_side_view: false,
            has_focused_target: true,
            has_queued: false,
            queue_pointer_armed: false,
            history_searching: false,
            model_searching: false,
            modal_keymap_open: false,
            editor_field: None,
            custom_provider_field: None,
            question_other_highlighted: false,
            history_clear_confirm: false,
            host_prompting: false,
            config_focus: Default::default(),
            leader_chord: crate::app::LeaderChord::None,
        },
        &mut drag,
    )
}

/// Run `code` (+ `modifiers`) against a fully-specified context and return
/// the resulting action plus the final cursor position. The input buffer is
/// mutated in place so callers can assert on its contents too.
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
            session_info_detail: false,
            connection_info_detail: false,
            is_responding: false,
            completion_kind: crate::CompletionKind::None,
            suggestion_count: 0,
            has_exact_suggestion: false,
            suggestion_index: None,
            completion_dismissed: false,
            has_trigger_text: false,
            permission_confirm_always: false,
            permission_show_details: false,
            in_runner_view: false,
            in_side_view: false,
            has_focused_target: has_focus,
            has_queued: false,
            queue_pointer_armed: false,
            // Editing text in the history and model-picker modals only
            // happens inside their search sub-layer, so treat those cases
            // here as search mode (browse mode never reaches editing keys).
            history_searching: modal == crate::Modal::HistorySearch,
            model_searching: matches!(modal, crate::Modal::Models | crate::Modal::Connections),
            modal_keymap_open: false,
            editor_field: None,
            custom_provider_field: None,
            question_other_highlighted: false,
            history_clear_confirm: false,
            host_prompting: false,
            config_focus: Default::default(),
            leader_chord: crate::app::LeaderChord::None,
        },
        &mut drag,
    )
}

/// Drive the history modal's **search sub-layer** with `code` (+
/// `modifiers`) and return the resulting action. `history_searching` is set
/// so the modal borrows the input line as the fuzzy query — matching the
/// live state once the user has pressed `/` to enter search.
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
            session_info_detail: false,
            connection_info_detail: false,
            is_responding: false,
            completion_kind: crate::CompletionKind::None,
            suggestion_count: 0,
            has_exact_suggestion: false,
            suggestion_index: None,
            completion_dismissed: false,
            has_trigger_text: false,
            permission_confirm_always: false,
            permission_show_details: false,
            in_runner_view: false,
            in_side_view: false,
            has_focused_target: false,
            has_queued: false,
            queue_pointer_armed: false,
            history_searching: true,
            model_searching: false,
            modal_keymap_open: false,
            editor_field: None,
            custom_provider_field: None,
            question_other_highlighted: false,
            history_clear_confirm: false,
            host_prompting: false,
            config_focus: Default::default(),
            leader_chord: crate::app::LeaderChord::None,
        },
        &mut drag,
    )
}

/// Helper: send `code` in the compose zone with explicit `has_queued`.
fn up_with_queued(has_queued: bool) -> InputAction {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    process_event(
        Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Up,
            KeyModifiers::NONE,
        )),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::None,
            session_info_detail: false,
            connection_info_detail: false,
            is_responding: false,
            completion_kind: crate::CompletionKind::None,
            suggestion_count: 0,
            has_exact_suggestion: false,
            suggestion_index: None,
            completion_dismissed: false,
            has_trigger_text: false,
            permission_confirm_always: false,
            permission_show_details: false,
            in_runner_view: false,
            in_side_view: false,
            has_focused_target: false,
            has_queued,
            queue_pointer_armed: false,
            history_searching: false,
            model_searching: false,
            modal_keymap_open: false,
            editor_field: None,
            custom_provider_field: None,
            question_other_highlighted: false,
            history_clear_confirm: false,
            host_prompting: false,
            config_focus: Default::default(),
            leader_chord: crate::app::LeaderChord::None,
        },
        &mut drag,
    )
}

/// Helper: dispatch a bare `code` in the compose zone against a completion
/// menu that reports `suggestion_count` candidates and `exact` = whether
/// the composer text exactly matches one of them. `suggestion_index` is
/// pinned to 0 so Tab-cycling assertions have a deterministic "next"
/// candidate.
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
            active_modal: crate::Modal::None,
            is_responding: false,
            completion_kind,
            suggestion_count,
            has_exact_suggestion: exact,
            suggestion_index: Some(0),
            completion_dismissed: false,
            has_trigger_text: false,
            permission_confirm_always: false,
            permission_show_details: false,
            in_runner_view: false,
            in_side_view: false,
            has_focused_target: false,
            has_queued: false,
            queue_pointer_armed: false,
            history_searching: false,
            ..Default::default()
        },
        &mut drag,
    )
}

/// Helper: send a printable char inside the Queue modal. The queue modal
/// is a pure browse/manage surface (no text field), so printable chars
/// route to its management verbs rather than into a borrowed input.
fn queue_modal_char(c: char) -> InputAction {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    process_event(
        Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char(c),
            KeyModifiers::SHIFT,
        )),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::Queue,
            session_info_detail: false,
            connection_info_detail: false,
            is_responding: false,
            completion_kind: crate::CompletionKind::None,
            suggestion_count: 0,
            has_exact_suggestion: false,
            suggestion_index: None,
            completion_dismissed: false,
            has_trigger_text: false,
            permission_confirm_always: false,
            permission_show_details: false,
            in_runner_view: false,
            in_side_view: false,
            has_focused_target: false,
            has_queued: true,
            queue_pointer_armed: false,
            history_searching: false,
            model_searching: false,
            modal_keymap_open: false,
            editor_field: None,
            custom_provider_field: None,
            question_other_highlighted: false,
            history_clear_confirm: false,
            host_prompting: false,
            config_focus: Default::default(),
            leader_chord: crate::app::LeaderChord::None,
        },
        &mut drag,
    )
}

/// Helper: send a bare (no-modifier) key inside the Queue modal. Used for
/// Enter (re-edit) routing.
fn queue_modal_key(code: KeyCode) -> InputAction {
    queue_modal_key_with_modifiers(code, KeyModifiers::NONE)
}

/// Helper: send a key (with explicit modifiers) inside the Queue modal. Used
/// for Enter (re-edit) and Ctrl+P (block toggle) routing.
fn queue_modal_key_with_modifiers(code: KeyCode, modifiers: KeyModifiers) -> InputAction {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    process_event(
        Event::Key(crossterm::event::KeyEvent::new(code, modifiers)),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::Queue,
            session_info_detail: false,
            connection_info_detail: false,
            is_responding: false,
            completion_kind: crate::CompletionKind::None,
            suggestion_count: 0,
            has_exact_suggestion: false,
            suggestion_index: None,
            completion_dismissed: false,
            has_trigger_text: false,
            permission_confirm_always: false,
            permission_show_details: false,
            in_runner_view: false,
            in_side_view: false,
            has_focused_target: false,
            has_queued: true,
            queue_pointer_armed: false,
            history_searching: false,
            model_searching: false,
            modal_keymap_open: false,
            editor_field: None,
            custom_provider_field: None,
            question_other_highlighted: false,
            history_clear_confirm: false,
            host_prompting: false,
            config_focus: Default::default(),
            leader_chord: crate::app::LeaderChord::None,
        },
        &mut drag,
    )
}

/// Drive `Event::Paste` (terminal bracketed paste) through `process_event`
/// against the given modal and return the resulting action. The input
/// buffer is mutated in place so callers can assert on its contents.
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
            session_info_detail: false,
            connection_info_detail: false,
            is_responding: false,
            completion_kind: crate::CompletionKind::None,
            suggestion_count: 0,
            has_exact_suggestion: false,
            suggestion_index: None,
            completion_dismissed: false,
            has_trigger_text: false,
            permission_confirm_always: false,
            permission_show_details: false,
            in_runner_view: false,
            in_side_view: false,
            has_focused_target: false,
            has_queued: false,
            queue_pointer_armed: false,
            // The history and model-picker modals only take text in their
            // search sub-layer; treat those cases as search mode here.
            history_searching: modal == crate::Modal::HistorySearch,
            model_searching: matches!(modal, crate::Modal::Models | crate::Modal::Connections),
            modal_keymap_open: false,
            editor_field: None,
            custom_provider_field: None,
            question_other_highlighted: false,
            history_clear_confirm: false,
            host_prompting: false,
            config_focus: Default::default(),
            leader_chord: crate::app::LeaderChord::None,
        },
        &mut drag,
    )
}

/// Helper: dispatch `code` in the compose zone against a pre-seeded
/// multi-line buffer and return the resulting action. The cursor lands
/// at `cursor` (in char units) before the keypress.
fn multiline_arrow(seed: &str, cursor: usize, code: KeyCode) -> (InputAction, usize) {
    let mut input = seed.to_string();
    let mut cur = cursor;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(crossterm::event::KeyEvent::new(code, KeyModifiers::NONE)),
        &mut input,
        &mut cur,
        InputContext {
            active_modal: crate::Modal::None,
            session_info_detail: false,
            connection_info_detail: false,
            is_responding: false,
            completion_kind: crate::CompletionKind::None,
            suggestion_count: 0,
            has_exact_suggestion: false,
            suggestion_index: None,
            completion_dismissed: false,
            has_trigger_text: false,
            permission_confirm_always: false,
            permission_show_details: false,
            in_runner_view: false,
            in_side_view: false,
            has_focused_target: false,
            has_queued: false,
            queue_pointer_armed: false,
            history_searching: false,
            model_searching: false,
            modal_keymap_open: false,
            editor_field: None,
            custom_provider_field: None,
            question_other_highlighted: false,
            history_clear_confirm: false,
            host_prompting: false,
            config_focus: Default::default(),
            leader_chord: crate::app::LeaderChord::None,
        },
        &mut drag,
    );
    (action, cur)
}

/// Build a crossterm `Event::Key` for a single character, the form crossterm
/// returns when it fails to reassemble a split escape sequence.
fn leaked_char(c: char) -> Event {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState};
    Event::Key(KeyEvent {
        code: KeyCode::Char(c),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

fn leaked_esc() -> Event {
    use crossterm::event::{KeyEvent, KeyEventKind, KeyEventState};
    Event::Key(KeyEvent {
        code: KeyCode::Esc,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    })
}

/// Drive a sequence of events through a fresh guard and report how many it
/// dropped vs accepted.
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

fn oauth_key(c: char) -> InputAction {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    process_event(
        Event::Key(KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::OauthPending,
            session_info_detail: false,
            connection_info_detail: false,
            is_responding: false,
            completion_kind: crate::CompletionKind::Slash,
            suggestion_count: 0,
            has_exact_suggestion: false,
            suggestion_index: None,
            completion_dismissed: false,
            has_trigger_text: false,
            permission_confirm_always: false,
            permission_show_details: false,
            in_runner_view: false,
            in_side_view: false,
            has_focused_target: false,
            has_queued: false,
            queue_pointer_armed: false,
            history_searching: false,
            model_searching: false,
            modal_keymap_open: false,
            editor_field: None,
            custom_provider_field: None,
            question_other_highlighted: false,
            history_clear_confirm: false,
            host_prompting: false,
            config_focus: Default::default(),
            leader_chord: crate::app::LeaderChord::None,
        },
        &mut drag,
    )
}

fn oauth_keycode(code: KeyCode) -> InputAction {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    process_event(
        Event::Key(KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::OauthPending,
            session_info_detail: false,
            connection_info_detail: false,
            is_responding: false,
            completion_kind: crate::CompletionKind::Slash,
            suggestion_count: 0,
            has_exact_suggestion: false,
            suggestion_index: None,
            completion_dismissed: false,
            has_trigger_text: false,
            permission_confirm_always: false,
            permission_show_details: false,
            in_runner_view: false,
            in_side_view: false,
            has_focused_target: false,
            has_queued: false,
            queue_pointer_armed: false,
            history_searching: false,
            model_searching: false,
            modal_keymap_open: false,
            editor_field: None,
            custom_provider_field: None,
            question_other_highlighted: false,
            history_clear_confirm: false,
            host_prompting: false,
            config_focus: Default::default(),
            leader_chord: crate::app::LeaderChord::None,
        },
        &mut drag,
    )
}

fn mouse_ctx_for(modal: crate::Modal) -> InputContext {
    InputContext {
        active_modal: modal,
        is_responding: false,
        completion_kind: crate::CompletionKind::None,
        suggestion_count: 0,
        has_exact_suggestion: false,
        suggestion_index: None,
        completion_dismissed: false,
        has_trigger_text: false,
        permission_confirm_always: false,
        permission_show_details: false,
        in_runner_view: false,
        in_side_view: false,
        has_focused_target: false,
        has_queued: false,
        queue_pointer_armed: false,
        history_searching: false,
        ..Default::default()
    }
}

/// Drive the history modal with the clear-confirmation already armed (what
/// the app loop passes after a `HistoryClearAll`), returning the action and
/// the (unmodified) filter text — the armed question must own every key.
fn run_history_clear_key(
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
            history_clear_confirm: true,
            is_responding: false,
            completion_kind: crate::CompletionKind::None,
            suggestion_count: 0,
            has_exact_suggestion: false,
            suggestion_index: None,
            completion_dismissed: false,
            has_trigger_text: false,
            permission_confirm_always: false,
            permission_show_details: false,
            in_runner_view: false,
            in_side_view: false,
            has_focused_target: false,
            has_queued: false,
            queue_pointer_armed: false,
            history_searching: false,
            ..Default::default()
        },
        &mut drag,
    )
}

/// Dispatch a key against the model-settings editor with a given focused
/// field, returning the action. Mirrors `run_key` but threads the editor's
/// `editor_field` context so the effort/thinking field gestures resolve.
fn editor_key(code: KeyCode, editor_field: u8, input: &mut String) -> InputAction {
    let mut cursor = input.chars().count();
    let mut drag = SelectionDrag::default();
    process_event(
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE)),
        input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::ModelEditor,
            is_responding: false,
            completion_kind: crate::CompletionKind::None,
            suggestion_count: 0,
            has_exact_suggestion: false,
            suggestion_index: None,
            completion_dismissed: false,
            has_trigger_text: false,
            permission_confirm_always: false,
            permission_show_details: false,
            in_runner_view: false,
            in_side_view: false,
            has_focused_target: false,
            has_queued: false,
            queue_pointer_armed: false,
            history_searching: false,
            editor_field: Some(editor_field),
            ..Default::default()
        },
        &mut drag,
    )
}

/// Dispatch a plain key against the main composer (no modal), returning the
/// action. The input and cursor are threaded through so tests can assert on
/// the mutated buffer the way the event loop sees it.
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
        InputContext {
            active_modal: crate::Modal::None,
            is_responding: false,
            completion_kind: crate::CompletionKind::None,
            suggestion_count: 0,
            has_exact_suggestion: false,
            suggestion_index: None,
            completion_dismissed: false,
            has_trigger_text: false,
            permission_confirm_always: false,
            permission_show_details: false,
            in_runner_view: false,
            in_side_view: false,
            has_focused_target: false,
            has_queued: false,
            queue_pointer_armed: false,
            history_searching: false,
            ..Default::default()
        },
        &mut drag,
    )
}

mod editing;
mod modals;
mod navigation;
mod paste_escape;
mod submit;
