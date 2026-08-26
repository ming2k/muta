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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    )
}

// Like `enter`, but exposes the full completion state so we can reproduce
// the "menu open + user highlighted an item" scenarios that decide
// whether Enter accepts the highlighted completion or sends the partial
// input as-is.
#[allow(clippy::too_many_arguments)]
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    )
}

#[test]
fn enter_executes_an_exact_slash_command() {
    let mut input = "/repeat".to_string();
    assert_eq!(
        enter(&mut input, true),
        InputAction::SendSlash("/repeat".to_string())
    );
}

#[test]
fn enter_completes_a_slash_prefix() {
    let mut input = "/go".to_string();
    assert_eq!(
        enter(&mut input, false),
        InputAction::CommitSuggestion("0".to_string())
    );
}

#[test]
fn enter_accepts_a_highlighted_slash_suggestion() {
    // User typed `/m`, menu shows `/mcp` / `/model` / `/models`, user
    // pressed ↓ to highlight `/mcp` (index 1). Enter must accept the
    // highlighted item rather than sending `/m` as a (rejected) command.
    let mut input = "/m".to_string();
    assert_eq!(
        enter_with_completion(&mut input, crate::CompletionKind::Slash, 3, Some(1), false,),
        InputAction::CommitSuggestion("1".to_string())
    );
}

#[test]
fn enter_accepts_a_highlighted_path_suggestion() {
    // User typed `@src/foo`, path menu shows three candidates, user
    // highlighted the second. Enter must accept it rather than shipping
    // the partial `@src/foo` text in the chat message.
    let mut input = "@src/foo".to_string();
    assert_eq!(
        enter_with_completion(&mut input, crate::CompletionKind::Path, 3, Some(2), false,),
        InputAction::CommitSuggestion("2".to_string())
    );
}

#[test]
fn enter_highlight_wins_over_exact_slash_match() {
    // User typed `/mcp` (exact match) but then pressed ↓ to highlight
    // `/models`. The explicit highlight is a stronger signal than the
    // exact-match fast path, so Enter accepts the highlight.
    let mut input = "/mcp".to_string();
    assert_eq!(
        enter_with_completion(&mut input, crate::CompletionKind::Slash, 2, Some(1), true,),
        InputAction::CommitSuggestion("1".to_string())
    );
}

#[test]
fn enter_without_highlight_still_sends_path_message() {
    // Defensive fallback for a state the anchor pass no longer produces
    // (a visible menu always carries a highlight now): with no highlight
    // on a path menu, Enter keeps sending the message as typed.
    let mut input = "@src/foo".to_string();
    assert_eq!(
        enter_with_completion(&mut input, crate::CompletionKind::Path, 3, None, false,),
        InputAction::SendChat("@src/foo".to_string())
    );
}

#[test]
fn enter_accepts_the_default_highlighted_path_suggestion() {
    // The anchor pass selects the first candidate the moment the popup
    // appears, so a plain Enter on `@src/foo` commits that candidate
    // instead of shipping the partial mention — the same contract as
    // every IDE autocomplete. Esc first (see the dismissal tests) is the
    // way to send the raw text.
    let mut input = "@src/foo".to_string();
    assert_eq!(
        enter_with_completion(&mut input, crate::CompletionKind::Path, 3, Some(0), false,),
        InputAction::CommitSuggestion("0".to_string())
    );
}

#[test]
fn tab_reopens_a_dismissed_completion_menu() {
    // Esc closed the popup but the partial `/mc` is still in the composer:
    // Tab brings the menu back (the toggle's other half) instead of
    // no-op'ing. The reopened menu lands with its first row highlighted —
    // the anchor pass seeds that on the next iteration.
    let mut input = "/mc".to_string();
    let mut cursor = 3;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::None,
            completion_kind: crate::CompletionKind::Slash,
            suggestion_count: 2,
            has_exact_suggestion: false,
            suggestion_index: None,
            completion_dismissed: true,
            has_trigger_text: true,
            ..Default::default()
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::ReopenCompletion);
    // The composer text is untouched — Tab only re-opens the popup.
    assert_eq!(input, "/mc");
}

#[test]
fn tab_reopens_a_dismissed_path_completion_menu() {
    // Same gesture for `@path` mentions: an Esc-dismissed mention menu
    // comes back on Tab while the `@src` trigger text survives.
    let mut input = "@src".to_string();
    let mut cursor = 4;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::None,
            completion_kind: crate::CompletionKind::Path,
            suggestion_count: 3,
            has_exact_suggestion: false,
            suggestion_index: None,
            completion_dismissed: true,
            has_trigger_text: true,
            ..Default::default()
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::ReopenCompletion);
}

#[test]
fn tab_stays_inert_when_no_trigger_text_survives() {
    // A dismissed menu whose trigger text is gone (e.g. the input was
    // cleared, or resolved to an exact command) has nothing to re-open:
    // Tab must not resurrect a popup for text that no longer asks for one.
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::None,
            completion_kind: crate::CompletionKind::Slash,
            suggestion_count: 0,
            has_exact_suggestion: false,
            suggestion_index: None,
            completion_dismissed: true,
            has_trigger_text: false,
            ..Default::default()
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::None);
}

#[test]
fn esc_closes_slash_completion_menu() {
    // When a slash completion popup is open, Esc dismisses it rather
    // than falling through to runner exit / interrupt / no-op.
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
            session_info_detail: false,
            is_responding: false,
            completion_kind: crate::CompletionKind::Slash,
            suggestion_count: 2,
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::CloseCompletion);
    // The input text is left untouched — Esc only closes the popup.
    assert_eq!(input, "/mc");
}

#[test]
fn esc_closes_path_completion_menu() {
    // Same behaviour for `@path` mention completion.
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
            session_info_detail: false,
            is_responding: false,
            completion_kind: crate::CompletionKind::Path,
            suggestion_count: 3,
            has_exact_suggestion: false,
            suggestion_index: Some(1),
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::CloseCompletion);
}

#[test]
fn esc_falls_through_when_no_completion_is_open() {
    // With no popup, Esc in Compose with nothing going on is a no-op;
    // the completion-close branch only fires when a menu is visible.
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
            active_modal: crate::Modal::None,
            session_info_detail: false,
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::None);
}

#[test]
fn typing_in_compose_returns_insert_char() {
    // process_event must signal InsertChar (not None) so the event loop
    // can reset the completion-dismissal latch after an Enter commit or
    // Esc dismiss. The char is already spliced into `input` here; the
    // event loop treats the action as a signal only.
    let mut input = "/mc".to_string();
    let mut cursor = 3;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::None,
            session_info_detail: false,
            is_responding: false,
            completion_kind: crate::CompletionKind::Slash,
            suggestion_count: 2,
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::InsertChar('p'));
    assert_eq!(input, "/mcp");
    assert_eq!(cursor, 4);
}

#[test]
fn backspace_in_compose_returns_backspace_action() {
    // Same signal contract as InsertChar: Backspace must be returned so
    // the event loop clears completion_dismissed + suggestion_index.
    let mut input = "/mcp".to_string();
    let mut cursor = 4;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::None,
            session_info_detail: false,
            is_responding: false,
            completion_kind: crate::CompletionKind::Slash,
            suggestion_count: 1,
            has_exact_suggestion: true,
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::Backspace);
    assert_eq!(input, "/mc");
    assert_eq!(cursor, 3);
}

#[test]
fn backspace_atomically_deletes_an_image_chip() {
    // Pasting an image inserts `[Image #1 · size] ` (chip + trailing
    // space). A single Backspace right after the space must erase both
    // the space and the chip — mirroring codex / claude-code / opencode's
    // atomic chip backspace. The reconcile pass in the event loop
    // drops the orphaned `pending_images` entry.
    let chip = crate::composer_attachments::image_chip(1, 0);
    let mut input = format!("look {chip} ");
    let mut cursor = input.chars().count();
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::None,
            session_info_detail: false,
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::Backspace);
    assert_eq!(input, "look ");
    assert_eq!(cursor, "look ".chars().count());
}

#[test]
fn backspace_atomically_deletes_a_paste_chip_without_trailing_space() {
    // When the cursor lands right after `]` (no trailing space), a
    // single Backspace still removes the whole chip rather than
    // chipping away at the `]`.
    let chip = crate::composer_attachments::paste_chip(1, 5, 0);
    let mut input = format!("see {chip}!");
    // Cursor right after `]`, before `!`.
    let prefix_chars = "see ".chars().count() + chip.chars().count();
    let mut cursor = prefix_chars;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::None,
            session_info_detail: false,
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::Backspace);
    assert_eq!(input, "see !");
    assert_eq!(cursor, "see ".chars().count());
}

#[test]
fn backspace_falls_through_to_single_char_outside_a_chip() {
    // Mid-word backspace must keep deleting one character at a time.
    let mut input = "hello".to_string();
    let mut cursor = 5;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::None,
            session_info_detail: false,
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::Backspace);
    assert_eq!(input, "hell");
    assert_eq!(cursor, 4);
}

#[test]
fn bang_prefix_dispatches_as_normal_chat() {
    let mut input = "!git status".to_string();
    assert_eq!(
        enter_shell(&mut input),
        InputAction::SendChat("!git status".to_string())
    );
}

#[test]
fn bang_prefix_with_whitespace_dispatches_as_chat() {
    let mut input = "!   ls -la".to_string();
    assert_eq!(
        enter_shell(&mut input),
        InputAction::SendChat("!   ls -la".to_string())
    );
}

#[test]
fn bare_bang_dispatches_as_chat() {
    let mut input = "!".to_string();
    assert_eq!(
        enter_shell(&mut input),
        InputAction::SendChat("!".to_string())
    );
}

// Like `enter`, but with `completion_kind: None` and no suggestions, the
// production state for `!`-prefixed input (slash completion only opens
// when the input starts with `/`).
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    )
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
            active_modal: crate::Modal::Permission,
            session_info_detail: false,
            is_responding: true,
            completion_kind: crate::CompletionKind::None,
            suggestion_count: 0,
            has_exact_suggestion: false,
            suggestion_index: None,
            completion_dismissed: false,
            has_trigger_text: false,
            permission_confirm_always: true,
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::PermissionBack);
}

#[test]
fn plain_ctrl_c_maps_to_semantic_ctrl_c() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::None,
            session_info_detail: false,
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::CtrlC);
}

#[test]
fn star_in_models_modal_toggles_model_favorite() {
    // `*` favorites the highlighted MODEL (favorite is model-level,
    // ADR-0046). It is a Models-only action; in the Connections list `*`
    // falls through to the ordinary char path (inert in browse mode).
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(KeyEvent::new(KeyCode::Char('*'), KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::Models,
            session_info_detail: false,
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::ProviderPickerToggleFavorite);
}

#[test]
fn a_in_connections_modal_opens_template_chooser() {
    // `a` in the Connections modal opens the add-provider template chooser.
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::Connections,
            session_info_detail: false,
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::OpenProviderTemplate);
}

#[test]
fn enter_in_connections_modal_is_inert_no_activate_concept() {
    // Connections is a pure management surface: it has no activate concept
    // (switching the active provider is the Models picker's job), so Enter
    // must not map to `ProviderPickerActivate`. It is inert in browse mode.
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::Connections,
            session_info_detail: false,
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::None);
}

#[test]
fn esc_in_models_browse_closes_the_modal() {
    // The flat Models picker has no back stage: Esc in browse mode closes
    // it (the search sub-layer's first-Esc is `ModelExitSearch`).
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::Models,
            session_info_detail: false,
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::CloseModal);
}

#[test]
fn esc_in_connections_browse_closes_the_modal() {
    // In the Connections list (browse mode), Esc closes the picker.
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::Connections,
            session_info_detail: false,
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::CloseModal);
}

#[test]
fn star_in_connections_modal_is_inert_favorite_is_model_level() {
    // `*` favorites a MODEL — a Models-only action (ADR-0046). In the
    // Connections list it must not map to ToggleFavorite (it falls through
    // to the ordinary char path, which is inert in browse mode).
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(KeyEvent::new(KeyCode::Char('*'), KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::Connections,
            session_info_detail: false,
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    );
    assert_ne!(action, InputAction::ProviderPickerToggleFavorite);
}

#[test]
fn letter_in_models_modal_feeds_the_fuzzy_filter() {
    // In the model picker's search sub-layer every letter feeds the fuzzy
    // filter so users can search for "kimi" or "deepseek". (In browse mode
    // the same letter is inert — see `letter_in_models_browse_mode_is_inert`.)
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE)),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::Models,
            session_info_detail: false,
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
            model_searching: true,
            modal_keymap_open: false,
            editor_field: None,
            custom_provider_field: None,
            question_other_highlighted: false,
            history_clear_confirm: false,
            host_prompting: false,
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::InsertChar('k'));
    assert_eq!(input, "k");
}

#[test]
fn letter_in_models_browse_mode_is_inert_and_slash_enters_search() {
    // Browse mode (no `/` yet): printable letters do not mutate the borrowed
    // composer line; `/` is what enters the search sub-layer.
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let ctx = || InputContext {
        active_modal: crate::Modal::Models,
        session_info_detail: false,
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
        config_custom_editing: false,
        config_websearch_editing: false,
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
fn ctrl_t_opens_todos_modal_when_no_modal_is_open() {
    // Ctrl+T is a declared global binding (registry → OpenTodos). It opens
    // the Todos modal from the top level and is a no-op while another
    // modal owns the surface.
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('t'),
            KeyModifiers::CONTROL,
        )),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::None,
            session_info_detail: false,
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::OpenTodos);
}

#[test]
fn ctrl_m_opens_models_modal_when_no_modal_is_open() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let context = InputContext {
        active_modal: crate::Modal::None,
        session_info_detail: false,
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
        config_custom_editing: false,
        config_websearch_editing: false,
    };
    let action = process_event(
        Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('m'),
            KeyModifiers::CONTROL,
        )),
        &mut input,
        &mut cursor,
        context,
        &mut drag,
    );
    assert_eq!(action, InputAction::OpenModels);

    // While a modal is already open, Ctrl+M is ignored so it cannot yank
    // the user out of another modal mid-interaction.
    let action = process_event(
        Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('m'),
            KeyModifiers::CONTROL,
        )),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::Help,
            session_info_detail: false,
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::None);
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
        config_custom_editing: false,
        config_websearch_editing: false,
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    )
}

#[test]
fn tab_in_compose_without_suggestions_is_noop() {
    // Tab is completion-only: with no suggestion menu open, it does
    // nothing. (Transcript focus uses Ctrl+Up/Ctrl-Down, not Tab.)
    let mut input = String::new();
    assert_eq!(
        key_in_view(KeyCode::Tab, false, &mut input),
        InputAction::None
    );
    let mut input = String::from("draft");
    assert_eq!(
        key_in_view(KeyCode::Tab, false, &mut input),
        InputAction::None
    );
    // Shift+Tab is also a no-op (no zone switching).
    let mut input = String::new();
    assert_eq!(
        key_in_view(KeyCode::BackTab, false, &mut input),
        InputAction::None
    );
}

#[test]
fn tab_is_a_noop_while_busy_and_does_not_edit_the_draft() {
    // While a round runs, Tab toggles between Steer and FollowUp queue target modes.
    let mut input = String::from("follow up");
    let mut cursor = input.chars().count();
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Tab,
            KeyModifiers::NONE,
        )),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::None,
            session_info_detail: false,
            is_responding: true,
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::ToggleComposerSendMode);
    assert_eq!(input, "follow up");
}

#[test]
fn ctrl_b_moves_caret_back_one_char() {
    // Ctrl+B is readline backward-char: it moves the caret left and never
    // touches focus. (Focus navigation is Ctrl+↑/↓.)
    let mut input = String::from("abc");
    let mut cursor = 3;
    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('b'),
        KeyModifiers::CONTROL,
        crate::Modal::None,
        false,
    );
    assert_eq!(action, InputAction::None);
    assert_eq!(cursor, 2, "Ctrl+B moves the caret back one character");
}

#[test]
fn ctrl_arrows_drive_focus() {
    // Ctrl+↑/↓ enter focus from the input box (no focus yet) and keep
    // cycling once a step is focused. Bare Tab stays a no-op.
    let mut input = String::new();
    let mut cursor = 0;
    assert_eq!(
        run_key(
            &mut input,
            &mut cursor,
            KeyCode::Up,
            KeyModifiers::CONTROL,
            crate::Modal::None,
            false,
        ),
        InputAction::FocusPrevTarget
    );
    assert_eq!(
        run_key(
            &mut input,
            &mut cursor,
            KeyCode::Down,
            KeyModifiers::CONTROL,
            crate::Modal::None,
            true,
        ),
        InputAction::FocusNextTarget
    );
    assert_eq!(key_with_focus(KeyCode::Tab), InputAction::None);
}

#[test]
fn arrows_cycle_steps_while_focused() {
    // With a step focused, bare ↑/↓ cycle the focus instead of walking
    // history (history resumes once Esc clears the focus).
    assert_eq!(key_with_focus(KeyCode::Up), InputAction::FocusPrevTarget);
    assert_eq!(key_with_focus(KeyCode::Down), InputAction::FocusNextTarget);
}

#[test]
fn enter_activates_focused_target_space_inserts() {
    // Enter activates the focused step; Space is an ordinary character (it
    // inserts a space — there is no "space activates" anymore).
    assert_eq!(
        key_with_focus(KeyCode::Enter),
        InputAction::ActivateFocusedTarget
    );
    assert_eq!(
        key_with_focus(KeyCode::Char(' ')),
        InputAction::InsertChar(' ')
    );
}

#[test]
fn escape_clears_focus() {
    // Esc is the deliberate exit from a focused step, clearing the focus
    // so every key returns to its ordinary input-box meaning.
    assert_eq!(
        key_with_focus(KeyCode::Esc),
        InputAction::ClearFocusedTarget
    );
}

#[test]
fn typing_while_focused_inserts_and_keeps_focus() {
    // A focused step does not capture typing: printable characters insert
    // into the prompt as usual and leave the focus highlight in place
    // (Esc / Enter, not typing, change the focus).
    let action = key_with_focus(KeyCode::Char('a'));
    assert_eq!(action, InputAction::InsertChar('a'));
}

#[test]
fn q_while_focused_inserts_instead_of_quitting() {
    // 'q' only quits when nothing is focused. With a step focused it is an
    // ordinary character, so navigating never risks an accidental exit.
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
    assert_eq!(action, InputAction::InsertChar('q'));
    assert_eq!(input, "q");
}

#[test]
fn escape_exits_runner_view() {
    let mut input = String::new();
    assert_eq!(
        key_in_view(KeyCode::Esc, true, &mut input),
        InputAction::ExitRunner
    );
    // Outside an runner view, Esc does nothing when idle (no modal).
    assert_eq!(
        key_in_view(KeyCode::Esc, false, &mut input),
        InputAction::None
    );
}

/// Esc inside a `/btw` aside view (ADR-0103 §2): interrupt the viewed
/// aside — NOT exit. Leaving the view is Ctrl+C's job.
#[test]
fn escape_in_side_view_interrupts_the_aside_not_the_view() {
    let mut input = String::new();
    assert_eq!(
        key_in_side_view(KeyCode::Esc, &mut input),
        InputAction::InterruptSide
    );
    // A focused step still loses to the interrupt intent: one Esc inside an
    // aside always means "stop the aside's round".
    let mut input = String::new();
    assert_eq!(
        key_in_side_view_with(KeyCode::Esc, &mut input, |ctx| {
            ctx.has_focused_target = true
        }),
        InputAction::InterruptSide
    );
}

/// Ctrl+C inside a modal is handled by the app loop's `handle_ctrl_c`, so it
/// never reaches the contextual arm; but a Ctrl+C while the asides modal is
/// open closes the modal (not the view). The loop-side chain is covered by
/// the commands tests.
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
            session_info_detail: false,
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::CloseModal);
}

/// Enter inside the asides modal jumps into the highlighted aside.
#[test]
fn enter_in_btw_modal_focuses_the_selected_aside() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::Btw,
            session_info_detail: false,
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::BtwFocusSelected);
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

    // While typing (non-empty input), the brackets insert as characters,
    // not navigation, even inside an runner view.
    let mut typing = "x".to_string();
    key_in_view(KeyCode::Char('['), true, &mut typing);
    assert_eq!(typing, "x[");

    // Outside an runner view, brackets always insert.
    let mut other = String::new();
    key_in_view(KeyCode::Char(']'), false, &mut other);
    assert_eq!(other, "]");
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    )
}

#[test]
fn home_and_end_move_caret_in_compose_zone() {
    // Caret starts mid-string; Home jumps to line start, End to line end.
    // The buffer contents are never modified by these keys.
    let mut input = "hello".to_string();
    let mut cursor = 3;

    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Home,
        KeyModifiers::NONE,
        crate::Modal::None,
        false,
    );
    assert_eq!(action, InputAction::None);
    assert_eq!(input, "hello");
    assert_eq!(cursor, 0);

    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::End,
        KeyModifiers::NONE,
        crate::Modal::None,
        false,
    );
    assert_eq!(action, InputAction::None);
    assert_eq!(input, "hello");
    assert_eq!(cursor, 5);
}

#[test]
fn home_and_end_scroll_in_browse_zone() {
    // In Browse the conversation owns focus, so Home/End drive scrolling
    // instead of moving the (unfocused) input caret.
    let mut input = "hello".to_string();
    let mut cursor = 3;
    assert_eq!(
        run_key(
            &mut input,
            &mut cursor,
            KeyCode::Home,
            KeyModifiers::NONE,
            crate::Modal::None,
            true
        ),
        InputAction::ScrollTop
    );
    assert_eq!(cursor, 3, "Browse Home must not touch the caret");
    assert_eq!(
        run_key(
            &mut input,
            &mut cursor,
            KeyCode::End,
            KeyModifiers::NONE,
            crate::Modal::None,
            true
        ),
        InputAction::ScrollBottom
    );
    assert_eq!(cursor, 3);
}

#[test]
fn home_and_end_scroll_in_permission_modal() {
    let mut input = String::new();
    let mut cursor = 0;
    assert_eq!(
        run_key(
            &mut input,
            &mut cursor,
            KeyCode::Home,
            KeyModifiers::NONE,
            crate::Modal::Permission,
            false
        ),
        InputAction::ScrollTop
    );
    assert_eq!(
        run_key(
            &mut input,
            &mut cursor,
            KeyCode::End,
            KeyModifiers::NONE,
            crate::Modal::Permission,
            false
        ),
        InputAction::ScrollBottom
    );
}

#[test]
fn page_keys_scroll_question_modal_body() {
    let mut input = String::new();
    let mut cursor = 0;
    assert_eq!(
        run_key(
            &mut input,
            &mut cursor,
            KeyCode::PageUp,
            KeyModifiers::NONE,
            crate::Modal::Question,
            false
        ),
        InputAction::ScrollPageUp
    );
    assert_eq!(
        run_key(
            &mut input,
            &mut cursor,
            KeyCode::PageDown,
            KeyModifiers::NONE,
            crate::Modal::Question,
            false
        ),
        InputAction::ScrollPageDown
    );
}

/// Every modal that paints its own scrollable body must route PageUp /
/// PageDown to a body page-scroll action — not just the four modals the
/// old gate covered (None / Permission / Question / OauthPending). This
/// is the regression guard for "any modal should support scroll".
#[test]
fn page_keys_scroll_every_scrollable_modal_body() {
    let scrollable = [
        crate::Modal::Help,
        crate::Modal::Activity,
        crate::Modal::Permissions,
        crate::Modal::Config,
        crate::Modal::TokenReport,
        crate::Modal::OauthPending,
        crate::Modal::ProviderTemplate,
        crate::Modal::CustomProvider,
        crate::Modal::Tools,
        crate::Modal::Mcp,
        crate::Modal::Skills,
        crate::Modal::Sessions,
        crate::Modal::Queue,
        crate::Modal::HistorySearch,
        crate::Modal::Connections,
        crate::Modal::Models,
        crate::Modal::Question,
    ];
    for modal in scrollable {
        let mut input = String::new();
        let mut cursor = 0;
        assert_eq!(
            run_key(
                &mut input,
                &mut cursor,
                KeyCode::PageUp,
                KeyModifiers::NONE,
                modal,
                false
            ),
            InputAction::ScrollPageUp,
            "PageUp should page-scroll the {modal:?} modal body"
        );
        assert_eq!(
            run_key(
                &mut input,
                &mut cursor,
                KeyCode::PageDown,
                KeyModifiers::NONE,
                modal,
                false
            ),
            InputAction::ScrollPageDown,
            "PageDown should page-scroll the {modal:?} modal body"
        );
    }
}

/// Ctrl+↑ / Ctrl+↓ inside any scrollable modal advance the body by a page
/// — the chord a pager/editor binds to a page jump, useful on keyboards
/// without dedicated Page keys and consistent across every modal. Mirrors
/// PageUp / PageDown.
#[test]
fn ctrl_arrows_page_scroll_modal_body() {
    let scrollable = [
        crate::Modal::Help,
        crate::Modal::Activity,
        crate::Modal::Config,
        crate::Modal::TokenReport,
        crate::Modal::Sessions,
        crate::Modal::Queue,
        crate::Modal::HistorySearch,
        crate::Modal::Models,
        crate::Modal::Connections,
        crate::Modal::Question,
        crate::Modal::Skills,
    ];
    for modal in scrollable {
        let mut input = String::new();
        let mut cursor = 0;
        assert_eq!(
            run_key(
                &mut input,
                &mut cursor,
                KeyCode::Up,
                KeyModifiers::CONTROL,
                modal,
                false
            ),
            InputAction::ScrollPageUp,
            "Ctrl+Up should page-scroll the {modal:?} modal body"
        );
        assert_eq!(
            run_key(
                &mut input,
                &mut cursor,
                KeyCode::Down,
                KeyModifiers::CONTROL,
                modal,
                false
            ),
            InputAction::ScrollPageDown,
            "Ctrl+Down should page-scroll the {modal:?} modal body"
        );
    }
}

/// On the no-modal baseline, Ctrl+↑ / Ctrl+↓ still drive transcript item
/// focus (the established gesture), not page-scroll — the modal page-scroll
/// arms are gated on `scrolls_own_body`, so the baseline is untouched.
#[test]
fn ctrl_arrows_keep_transcript_focus_on_no_modal() {
    let mut input = String::new();
    let mut cursor = 0;
    assert_eq!(
        run_key(
            &mut input,
            &mut cursor,
            KeyCode::Up,
            KeyModifiers::CONTROL,
            crate::Modal::None,
            false
        ),
        InputAction::FocusPrevTarget
    );
    assert_eq!(
        run_key(
            &mut input,
            &mut cursor,
            KeyCode::Down,
            KeyModifiers::CONTROL,
            crate::Modal::None,
            false
        ),
        InputAction::FocusNextTarget
    );
}

/// The caret-owning text editors (ModelEditor, InputInjection) have no body
/// scroll, so PageUp / PageDown and Ctrl+↑ / Ctrl+↓ must be inert there
/// (no-op), not a stray page-scroll or transcript focus gesture.
#[test]
fn page_keys_are_inert_in_caret_editors() {
    for modal in [crate::Modal::ModelEditor, crate::Modal::InputInjection] {
        let mut input = String::new();
        let mut cursor = 0;
        assert_eq!(
            run_key(
                &mut input,
                &mut cursor,
                KeyCode::PageUp,
                KeyModifiers::NONE,
                modal,
                false
            ),
            InputAction::None,
            "PageUp should be a no-op in {modal:?}"
        );
        assert_eq!(
            run_key(
                &mut input,
                &mut cursor,
                KeyCode::PageDown,
                KeyModifiers::NONE,
                modal,
                false
            ),
            InputAction::None,
            "PageDown should be a no-op in {modal:?}"
        );
        assert_eq!(
            run_key(
                &mut input,
                &mut cursor,
                KeyCode::Up,
                KeyModifiers::CONTROL,
                modal,
                false
            ),
            InputAction::None,
            "Ctrl+Up should be a no-op in {modal:?}"
        );
    }
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
                active_modal: crate::Modal::Question,
                session_info_detail: false,
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
                config_custom_editing: false,
                config_websearch_editing: false,
            },
            &mut drag,
        )
    };

    assert_eq!(
        mk(crossterm::event::MouseEventKind::ScrollUp),
        InputAction::ScrollUp
    );
    assert_eq!(
        mk(crossterm::event::MouseEventKind::ScrollDown),
        InputAction::ScrollDown
    );
}

#[test]
fn home_and_end_move_caret_in_free_text_modals() {
    // The unified provider editor borrows the input line for one field at a
    // time; Home/End should edit there too, not be swallowed.
    for modal in [crate::Modal::ModelEditor, crate::Modal::HistorySearch] {
        let mut input = "abc".to_string();
        let mut cursor = 2;
        let action = run_key(
            &mut input,
            &mut cursor,
            KeyCode::Home,
            KeyModifiers::NONE,
            modal,
            false,
        );
        assert_eq!(action, InputAction::None);
        assert_eq!(cursor, 0, "Home should reach line start");

        let action = run_key(
            &mut input,
            &mut cursor,
            KeyCode::End,
            KeyModifiers::NONE,
            modal,
            false,
        );
        assert_eq!(action, InputAction::None);
        assert_eq!(cursor, 3, "End should reach line end");
    }
}

#[test]
fn ctrl_a_and_ctrl_e_move_caret_in_compose_zone() {
    let mut input = "hello".to_string();
    let mut cursor = 2;

    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('a'),
        KeyModifiers::CONTROL,
        crate::Modal::None,
        false,
    );
    assert_eq!(action, InputAction::None);
    assert_eq!(cursor, 0);
    assert_eq!(input, "hello", "Ctrl+A must not insert a literal 'a'");

    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('e'),
        KeyModifiers::CONTROL,
        crate::Modal::None,
        false,
    );
    assert_eq!(action, InputAction::None);
    assert_eq!(cursor, 5);
    assert_eq!(input, "hello");
}

#[test]
fn ctrl_a_and_ctrl_e_are_noop_in_browse_zone() {
    // Browse has no input editing; the keys fall through to no-ops rather
    // than scrolling or inserting characters.
    let mut input = String::new();
    let mut cursor = 0;
    assert_eq!(
        run_key(
            &mut input,
            &mut cursor,
            KeyCode::Char('a'),
            KeyModifiers::CONTROL,
            crate::Modal::None,
            true
        ),
        InputAction::None
    );
    assert_eq!(
        run_key(
            &mut input,
            &mut cursor,
            KeyCode::Char('e'),
            KeyModifiers::CONTROL,
            crate::Modal::None,
            true
        ),
        InputAction::None
    );
}

#[test]
fn line_aware_movement_respects_newlines() {
    // Multi-line input: Home/End/Ctrl+A/Ctrl+E operate on the current
    // logical line, not the whole buffer.
    let mut input = "line1\nline2\nline3".to_string();
    // Place the caret in the middle of the second line ("line2").
    // "line1\n" = 6 chars, then 2 more into "line2" -> char index 8.
    let mut cursor = 8;

    // Home -> start of "line2" (char index 6, just past the first '\n').
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Home,
        KeyModifiers::NONE,
        crate::Modal::None,
        false,
    );
    assert_eq!(cursor, 6, "Home should land at start of current line");

    // End -> end of "line2" (char index 11, just before the second '\n').
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::End,
        KeyModifiers::NONE,
        crate::Modal::None,
        false,
    );
    assert_eq!(cursor, 11, "End should land at end of current line");

    // Ctrl+A from the end of line2 should also snap to line start.
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('a'),
        KeyModifiers::CONTROL,
        crate::Modal::None,
        false,
    );
    assert_eq!(cursor, 6);
    // Ctrl+E snaps back to the line end without running off the buffer.
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('e'),
        KeyModifiers::CONTROL,
        crate::Modal::None,
        false,
    );
    assert_eq!(cursor, 11);
}

#[test]
fn ctrl_w_deletes_previous_word() {
    // "hello world" with the caret after "world" (char index 11).
    let mut input = "hello world".to_string();
    let mut cursor = 11;
    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('w'),
        KeyModifiers::CONTROL,
        crate::Modal::None,
        false,
    );
    assert_eq!(action, InputAction::Backspace);
    assert_eq!(input, "hello ");
    assert_eq!(cursor, 6);
}

#[test]
fn ctrl_w_eats_trailing_whitespace_and_previous_word() {
    // Caret sits after the trailing spaces following "world"; Ctrl+W
    // (readline `unix-word-rubout`) eats both the trailing whitespace
    // AND the preceding word in one stroke, leaving "hello ".
    let mut input = "hello world   ".to_string();
    let mut cursor = 14;
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('w'),
        KeyModifiers::CONTROL,
        crate::Modal::None,
        false,
    );
    assert_eq!(input, "hello ");
    assert_eq!(cursor, 6);
}

#[test]
fn ctrl_w_is_noop_at_line_start() {
    let mut input = "hello world".to_string();
    let mut cursor = 0;
    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('w'),
        KeyModifiers::CONTROL,
        crate::Modal::None,
        false,
    );
    assert_eq!(action, InputAction::None);
    assert_eq!(input, "hello world");
    assert_eq!(cursor, 0);
}

#[test]
fn ctrl_w_crosses_newline() {
    // Ctrl+W now crosses newline boundaries. "line1\nworld" with caret
    // at the end → first Ctrl+W deletes "world", second deletes "line1".
    let mut input = "line1\nworld".to_string();
    let mut cursor = 11;
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('w'),
        KeyModifiers::CONTROL,
        crate::Modal::None,
        false,
    );
    assert_eq!(input, "line1\n");
    assert_eq!(cursor, 6);

    // Second Ctrl+W eats the newline and "line1".
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('w'),
        KeyModifiers::CONTROL,
        crate::Modal::None,
        false,
    );
    assert_eq!(input, "");
    assert_eq!(cursor, 0);
}

#[test]
fn ctrl_w_is_noop_in_question_modal() {
    // Ctrl+W must never leak as a literal 'w' or close the modal in the
    // question modal; it should be a silent no-op there.
    let mut input = "abc".to_string();
    let mut cursor = 3;
    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('w'),
        KeyModifiers::CONTROL,
        crate::Modal::Question,
        false,
    );
    assert_eq!(action, InputAction::None);
    assert_eq!(input, "abc");
    assert_eq!(cursor, 3);
}

#[test]
fn shift_tab_returns_to_the_previous_question() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(KeyEvent {
            code: KeyCode::BackTab,
            modifiers: KeyModifiers::SHIFT,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::Question,
            session_info_detail: false,
            ..Default::default()
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::QuestionPrevious);
}

#[test]
fn question_space_toggles_when_other_row_not_highlighted() {
    // On a normal option row, Space toggles the option — it must not be
    // swallowed as free text.
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(KeyEvent {
            code: KeyCode::Char(' '),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::Question,
            session_info_detail: false,
            question_other_highlighted: false,
            history_clear_confirm: false,
            ..Default::default()
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::QuestionToggle);
}

#[test]
fn question_space_inserts_into_other_free_text_row() {
    // When the synthetic "Other" free-text row is highlighted, Space is an
    // ordinary character — it must insert into the field, not toggle.
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(KeyEvent {
            code: KeyCode::Char(' '),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::Question,
            session_info_detail: false,
            question_other_highlighted: true,
            ..Default::default()
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::QuestionInsertChar(' '));
}

#[test]
fn ctrl_u_deletes_to_line_start() {
    let mut input = "hello world".to_string();
    let mut cursor = 7;
    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('u'),
        KeyModifiers::CONTROL,
        crate::Modal::None,
        false,
    );
    assert_eq!(action, InputAction::Backspace);
    assert_eq!(input, "orld");
    assert_eq!(cursor, 0);
}

#[test]
fn ctrl_u_keeps_other_lines_in_multiline_draft() {
    // Multi-line draft: Ctrl+U on line 2 only wipes the part of line 2
    // before the caret, leaving line 1 untouched.
    let mut input = "keep me\nwipe me".to_string();
    let mut cursor = 15;
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('u'),
        KeyModifiers::CONTROL,
        crate::Modal::None,
        false,
    );
    assert_eq!(input, "keep me\n");
    assert_eq!(cursor, 8);
}

#[test]
fn ctrl_k_deletes_to_line_end() {
    let mut input = "hello world".to_string();
    let mut cursor = 5;
    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('k'),
        KeyModifiers::CONTROL,
        crate::Modal::None,
        false,
    );
    assert_eq!(action, InputAction::Backspace);
    assert_eq!(input, "hello");
    assert_eq!(cursor, 5, "Ctrl+K keeps the caret put");
}

#[test]
fn ctrl_k_does_not_eat_next_line() {
    let mut input = "first\nsecond".to_string();
    let mut cursor = 3;
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('k'),
        KeyModifiers::CONTROL,
        crate::Modal::None,
        false,
    );
    assert_eq!(input, "fir\nsecond");
    assert_eq!(cursor, 3);
}

#[test]
fn alt_d_deletes_next_word() {
    // Caret at index 5 (the space); Alt+D should eat "world".
    let mut input = "hello world".to_string();
    let mut cursor = 5;
    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('d'),
        KeyModifiers::ALT,
        crate::Modal::None,
        false,
    );
    assert_eq!(action, InputAction::Backspace);
    assert_eq!(input, "hello");
    assert_eq!(cursor, 5, "Alt+D keeps the caret put");
}

#[test]
fn alt_b_jumps_back_one_word() {
    let mut input = "the quick fox".to_string();
    let mut cursor = 13;
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('b'),
        KeyModifiers::ALT,
        crate::Modal::None,
        false,
    );
    assert_eq!(cursor, 10);
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('b'),
        KeyModifiers::ALT,
        crate::Modal::None,
        false,
    );
    assert_eq!(cursor, 4);
}

#[test]
fn alt_f_jumps_forward_one_word() {
    let mut input = "the quick fox".to_string();
    let mut cursor = 0;
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('f'),
        KeyModifiers::ALT,
        crate::Modal::None,
        false,
    );
    assert_eq!(cursor, 3);
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('f'),
        KeyModifiers::ALT,
        crate::Modal::None,
        false,
    );
    assert_eq!(cursor, 9);
}

#[test]
fn ctrl_left_right_move_word_by_word() {
    // "alpha bravo charlie" — char indices:
    // alpha=0..4, ' '=5, bravo=6..10, ' '=11, charlie=12..18 (len 19).
    let mut input = "alpha bravo charlie".to_string();
    let mut cursor = 19;
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Left,
        KeyModifiers::CONTROL,
        crate::Modal::None,
        false,
    );
    assert_eq!(cursor, 12, "Ctrl+Left snaps to the start of 'charlie'");
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Left,
        KeyModifiers::CONTROL,
        crate::Modal::None,
        false,
    );
    assert_eq!(cursor, 6, "Ctrl+Left snaps to the start of 'bravo'");
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Right,
        KeyModifiers::CONTROL,
        crate::Modal::None,
        false,
    );
    assert_eq!(cursor, 11, "Ctrl+Right snaps to the end of 'bravo'");
}

#[test]
fn alt_backspace_deletes_previous_word() {
    let mut input = "foo bar baz".to_string();
    let mut cursor = 11;
    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Backspace,
        KeyModifiers::ALT,
        crate::Modal::None,
        false,
    );
    assert_eq!(action, InputAction::Backspace);
    assert_eq!(input, "foo bar ");
    assert_eq!(cursor, 8);
}

#[test]
fn ctrl_backspace_deletes_previous_word() {
    // Ctrl+Backspace is the same word-rubout motion on terminals that
    // deliver it; mirror the Alt+Backspace behaviour.
    let mut input = "foo bar baz".to_string();
    let mut cursor = 11;
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Backspace,
        KeyModifiers::CONTROL,
        crate::Modal::None,
        false,
    );
    assert_eq!(input, "foo bar ");
    assert_eq!(cursor, 8);
}

#[test]
fn f1_opens_help() {
    // F1 is a portable help shortcut with no legacy control-byte
    // collision, so it works under multiplexers (tmux) that strip the
    // Kitty keyboard protocol — unlike Ctrl+H, which collapses to the
    // Backspace byte (0x08) there.
    let mut input = String::new();
    let mut cursor = 0;
    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::F(1),
        KeyModifiers::NONE,
        crate::Modal::None,
        false,
    );
    assert_eq!(action, InputAction::OpenHelp);
}

#[test]
fn question_mark_opens_help_when_input_empty() {
    // `?` is the conventional help key. It opens help only from the top
    // level with an empty input box, so typing a literal `?` is never
    // swallowed.
    let mut input = String::new();
    let mut cursor = 0;
    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('?'),
        KeyModifiers::NONE,
        crate::Modal::None,
        false,
    );
    assert_eq!(action, InputAction::OpenHelp);
    assert!(input.is_empty(), "no char inserted when opening help");
}

#[test]
fn question_mark_inserts_when_input_nonempty() {
    // With text already in the box, `?` is a normal character — the help
    // shortcut only fires on an empty prompt.
    let mut input = "what".to_string();
    let mut cursor = 4;
    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('?'),
        KeyModifiers::NONE,
        crate::Modal::None,
        false,
    );
    assert_eq!(action, InputAction::InsertChar('?'));
    assert_eq!(input, "what?");
    assert_eq!(cursor, 5);
}

#[test]
fn ctrl_w_works_in_history_modal() {
    // Free-text modals (history search, models, provider editor) accept the
    // same line-editing vocabulary as the main prompt so the user is
    // never trapped mid-query.
    let mut input = "fuzzy query".to_string();
    let mut cursor = 11;
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('w'),
        KeyModifiers::CONTROL,
        crate::Modal::HistorySearch,
        false,
    );
    assert_eq!(input, "fuzzy ");
    assert_eq!(cursor, 6);
}

#[test]
fn ctrl_keys_do_not_insert_literal_chars() {
    // Regression guard: none of the new Ctrl/Alt shortcuts may fall
    // through to the `Char(c)` insertion path. Each must leave the
    // buffer text untouched when there is nothing to delete.
    let mut input = String::new();
    let mut cursor = 0;
    for (code, mods) in [
        (KeyCode::Char('w'), KeyModifiers::CONTROL),
        (KeyCode::Char('u'), KeyModifiers::CONTROL),
        (KeyCode::Char('k'), KeyModifiers::CONTROL),
        (KeyCode::Char('b'), KeyModifiers::ALT),
        (KeyCode::Char('f'), KeyModifiers::ALT),
        (KeyCode::Char('d'), KeyModifiers::ALT),
    ] {
        let action = run_key(
            &mut input,
            &mut cursor,
            code,
            mods,
            crate::Modal::None,
            false,
        );
        assert_eq!(action, InputAction::None);
        assert!(input.is_empty());
        assert_eq!(cursor, 0);
    }
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    )
}

#[test]
fn typing_in_history_modal_appends_to_query() {
    // The history modal borrows the input line as the fuzzy query, so each
    // printable char must insert into `input` exactly like the ApiKey /
    // Endpoint / ModelName modals do.
    let mut input = String::new();
    let mut cursor = 0;
    run_history_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('g'),
        KeyModifiers::NONE,
    );
    run_history_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('i'),
        KeyModifiers::NONE,
    );
    run_history_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('t'),
        KeyModifiers::NONE,
    );
    assert_eq!(input, "git");
    assert_eq!(cursor, 3);
}

#[test]
fn backspace_in_history_modal_trims_query() {
    let mut input = "rust".to_string();
    let mut cursor = 4;
    run_history_key(
        &mut input,
        &mut cursor,
        KeyCode::Backspace,
        KeyModifiers::NONE,
    );
    assert_eq!(input, "rus");
    assert_eq!(cursor, 3);
}

#[test]
fn esc_in_history_panel_closes_modal_directly() {
    // The history panel floats above a live composer that is permanently
    // the filter field, so there is no browse/search distinction: a single
    // Esc closes the panel outright (and the app loop restores the stashed
    // draft). Whether or not a query is typed, the result is the same.
    let mut input = "git".to_string();
    let mut cursor = 3;
    let action = run_history_key(&mut input, &mut cursor, KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(action, InputAction::CloseModal);
}

#[test]
fn typing_in_history_panel_inserts_into_filter() {
    // The composer is always the live filter: a printable key splices into
    // the query buffer and narrows the list. There is no inert browse mode.
    let mut input = String::new();
    let mut cursor = 0;
    let action = run_history_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('g'),
        KeyModifiers::NONE,
    );
    assert_eq!(action, InputAction::InsertChar('g'));
    assert_eq!(input, "g");
    assert_eq!(cursor, 1);
}

#[test]
fn slash_in_history_panel_inserts_literal() {
    // `/` is just another query character — the composer is always the
    // filter, so it splices into the buffer rather than toggling a mode.
    let mut input = String::new();
    let mut cursor = 0;
    run_history_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('/'),
        KeyModifiers::NONE,
    );
    assert_eq!(input, "/");
    assert_eq!(cursor, 1);
}

#[test]
fn enter_in_history_modal_emits_history_insert() {
    // Enter must NOT send a chat — it inserts the highlighted match into
    // the input box for further editing. The dedicated HistoryInsert
    // action lets the app loop distinguish the two intents.
    let mut input = "go".to_string();
    let mut cursor = 2;
    let action = run_history_key(&mut input, &mut cursor, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(action, InputAction::HistoryInsert);
    assert_eq!(input, "go", "Enter must not consume the query");
    assert_eq!(cursor, 2);
}

#[test]
fn ctrl_r_opens_history_modal_when_no_modal_is_open() {
    // With no modal open, Ctrl+R routes through OpenHistory so the app
    // loop can stash the in-progress draft and show the fuzzy picker.
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('r'),
            KeyModifiers::CONTROL,
        )),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::None,
            session_info_detail: false,
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::OpenHistory);

    // Once any modal is open (including HistorySearch itself), Ctrl+R is
    // a no-op so it cannot yank the user out of the in-progress query.
    let action = process_event(
        Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('r'),
            KeyModifiers::CONTROL,
        )),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::HistorySearch,
            session_info_detail: false,
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::None);
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    )
}

#[test]
fn up_arrow_walks_the_queue_pointer_when_queue_nonempty() {
    // While at least one message is staged in the send queue, ↑ arms the
    // non-destructive queue pointer at the newest item instead of walking
    // input history — the queue is the newer, more urgent surface. Nothing
    // leaves the queue; the composer becomes an editable projection and
    // Enter writes the edit back in place.
    assert_eq!(up_with_queued(true), InputAction::QueuePointerPrev);
}

#[test]
fn up_arrow_walks_history_when_queue_empty() {
    // Once the queue drains (or was never populated), ↑ resumes its
    // normal role of walking the input history.
    assert_eq!(up_with_queued(false), InputAction::HistoryPrev);
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

#[test]
fn arrows_navigate_completion_menu_while_command_is_partial() {
    // A partially-typed `/` command keeps the completion menu interactive:
    // ↑/↓ cycle its candidates (SuggestPrev/SuggestNext) rather than
    // walking history, so the user can keep switching toward the command
    // they want.
    let kind = crate::CompletionKind::Slash;
    assert_eq!(
        compose_key_with_completion(KeyCode::Down, kind, 5, false),
        InputAction::SuggestNext
    );
    assert_eq!(
        compose_key_with_completion(KeyCode::Up, kind, 5, false),
        InputAction::SuggestPrev
    );
}

#[test]
fn arrows_walk_history_once_command_is_fully_typed() {
    // Regression for the reported bug: once ↑/↓ has switched the composer
    // to a fully-typed `/command`, the exact-match completion row used to
    // pin the arrows — every press was consumed as a no-op suggestion
    // cycle and history navigation became unreachable. A resolved command
    // must hand ↑/↓ back to their ordinary history role.
    let kind = crate::CompletionKind::Slash;
    assert_eq!(
        compose_key_with_completion(KeyCode::Down, kind, 1, true),
        InputAction::HistoryNext,
        "↓ on an exact-match command should keep walking history"
    );
    assert_eq!(
        compose_key_with_completion(KeyCode::Up, kind, 1, true),
        InputAction::HistoryPrev,
        "↑ on an exact-match command should keep walking history"
    );
    // Even with several candidates, an exact match (e.g. `/session`
    // alongside the prefix-sibling `/sessions`) is resolved: arrows keep
    // their history role instead of being captured by the popup.
    assert_eq!(
        compose_key_with_completion(KeyCode::Down, kind, 2, true),
        InputAction::HistoryNext
    );
    assert_eq!(
        compose_key_with_completion(KeyCode::Up, kind, 2, true),
        InputAction::HistoryPrev
    );
}

#[test]
fn tab_does_not_cycle_completions_on_exact_command() {
    // A fully-typed command is resolved: its popup is hidden, so Tab must
    // not invisibly commit sibling candidates (e.g. `/session` →
    // `/sessions`).
    let kind = crate::CompletionKind::Slash;
    assert_eq!(
        compose_key_with_completion(KeyCode::Tab, kind, 2, true),
        InputAction::None
    );

    // A partial command commits the highlighted suggestion on Tab — the
    // same gesture as Enter. With the anchor pass keeping the first
    // candidate highlighted, a plain Tab (no prior ↓) commits index 0.
    assert_eq!(
        compose_key_with_completion(KeyCode::Tab, kind, 2, false),
        InputAction::CommitSuggestion("0".to_string())
    );
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
            config_custom_editing: false,
            config_websearch_editing: false,
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    )
}

#[test]
fn queue_modal_enter_recalls_selected_not_newest() {
    // Enter in the Queue modal re-edits the *selected* item (the ↑/↓
    // highlight), so it routes to the dedicated RecallQueuedSelected
    // action rather than the top-level RecallQueued (newest) one.
    assert_eq!(
        queue_modal_key(KeyCode::Enter),
        InputAction::RecallQueuedSelected
    );
}

#[test]
fn queue_modal_shift_d_deletes_selected() {
    // `Shift+D` deletes the highlighted item (the queue modal
    // auto-blocks on open, so the index can't drift mid-delete).
    assert_eq!(queue_modal_char('D'), InputAction::QueueDelete);
}

#[test]
fn queue_modal_k_and_j_reorder() {
    // Vim convention: `K` toward the front (next to pop), `J` toward the
    // tail. Routes through QueueMoveItem with the signed delta.
    assert_eq!(
        queue_modal_char('K'),
        InputAction::QueueMoveItem { delta: -1 }
    );
    assert_eq!(
        queue_modal_char('J'),
        InputAction::QueueMoveItem { delta: 1 }
    );
}

#[test]
fn queue_modal_ctrl_p_toggles_block() {
    // Ctrl+P is NoModal-gated in the registry, so inside the modal it falls
    // through to the contextual arm — which honors it only in the Queue
    // modal so the user can resume without closing the list.
    assert_eq!(
        queue_modal_key_with_modifiers(KeyCode::Char('p'), KeyModifiers::CONTROL),
        InputAction::QueueToggleBlock
    );
}

#[test]
fn up_arrow_in_browse_does_not_recall_queued() {
    // Browse zone owns ↑ for step navigation; the queued-message recall
    // only fires from Compose (where the user can actually edit the
    // recalled draft). In Browse, ↑ keeps walking activatable targets.
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Up,
            KeyModifiers::NONE,
        )),
        &mut input,
        &mut cursor,
        InputContext {
            active_modal: crate::Modal::None,
            session_info_detail: false,
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::FocusPrevTarget);
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    )
}

#[test]
fn ctrl_v_returns_paste_in_free_text_modals() {
    // Ctrl+V routes to InputAction::Paste on the main prompt and in
    // every free-text modal (provider editor, provider picker filter,
    // history search). Other modals drop it so a paste never leaks into
    // a read-only overlay or the permission sheet.
    let free_text_modals = [
        crate::Modal::None,
        crate::Modal::ModelEditor,
        crate::Modal::Models,
        crate::Modal::Connections,
        crate::Modal::HistorySearch,
    ];
    for modal in free_text_modals {
        let mut input = String::new();
        let mut cursor = 0;
        let action = run_key(
            &mut input,
            &mut cursor,
            KeyCode::Char('v'),
            KeyModifiers::CONTROL,
            modal,
            false,
        );
        assert_eq!(
            action,
            InputAction::Paste,
            "Ctrl+V should paste in free-text modal"
        );
        assert!(input.is_empty(), "Ctrl+V must not mutate the buffer itself");
    }

    for modal in [
        crate::Modal::Permission,
        crate::Modal::Question,
        crate::Modal::Help,
        crate::Modal::Sessions,
    ] {
        let mut input = String::new();
        let mut cursor = 0;
        let action = run_key(
            &mut input,
            &mut cursor,
            KeyCode::Char('v'),
            KeyModifiers::CONTROL,
            modal,
            false,
        );
        assert_eq!(
            action,
            InputAction::None,
            "Ctrl+V should be a no-op in non-text modal"
        );
    }
}

#[test]
fn bracketed_paste_routes_in_free_text_modals() {
    // Terminal-level bracketed paste mirrors Ctrl+V: it produces a
    // BracketedPaste action on the main prompt and in the free-text
    // modals, and is dropped elsewhere.
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    );
    (action, cur)
}

#[test]
fn up_arrow_walks_lines_in_multiline_before_history() {
    // In a multi-line draft, ↑ first moves the caret up a line instead
    // of jumping to input history — only at the top line does it fall
    // through to HistoryPrev.
    let seed = "hello\nworld";
    // Caret at end of second line: ↑ should move to the same column on
    // the first line ("hello", col 5) and return None, not HistoryPrev.
    let (action, cur) = multiline_arrow(seed, "hello\nworld".chars().count(), KeyCode::Up);
    assert_eq!(action, InputAction::None);
    assert_eq!(cur, 5, "up should land at col 5 on the first line");

    // Now sitting at the end of the first line: ↑ should hand off to
    // history navigation.
    let (action, _) = multiline_arrow(seed, 5, KeyCode::Up);
    assert_eq!(action, InputAction::HistoryPrev);
}

#[test]
fn down_arrow_walks_lines_in_multiline_before_history() {
    let seed = "hello\nworld";
    // Caret at start of first line: ↓ moves to the same column on the
    // second line and returns None, not HistoryNext.
    let (action, cur) = multiline_arrow(seed, 0, KeyCode::Down);
    assert_eq!(action, InputAction::None);
    assert_eq!(cur, 6, "down should land at col 0 of the second line");

    // Caret at end of the second line: ↓ hands off to history.
    let (action, _) = multiline_arrow(seed, "hello\nworld".chars().count(), KeyCode::Down);
    assert_eq!(action, InputAction::HistoryNext);
}

#[test]
fn up_arrow_clamps_column_to_shorter_line() {
    // Moving up to a shorter line clamps the column to that line's
    // length rather than overshooting into the newline.
    let seed = "hi\nlonger line";
    // Caret at col 7 of the second line ("longer line").
    let start = "hi\n".chars().count() + 7;
    let (action, cur) = multiline_arrow(seed, start, KeyCode::Up);
    assert_eq!(action, InputAction::None);
    assert_eq!(cur, 2, "column should clamp to the first line's length");
}

// --- SgrLeakGuard -------------------------------------------------------

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

#[test]
fn sgr_guard_drops_split_sgr_mouse_sequence() {
    // The exact symptom from the field: a mouse report crossterm split into
    // individual chars. `ESC [ < 0 ; 3 5 ; 4 6 M` — the `[ < … M` payload
    // is dropped, but the leading Esc is *delivered* (it is a real control
    // key, never inserted as text, and is the double-Esc interrupt path).
    let seq: Vec<Event> = [
        leaked_esc(),
        leaked_char('['),
        leaked_char('<'),
        leaked_char('0'),
        leaked_char(';'),
        leaked_char('3'),
        leaked_char('5'),
        leaked_char(';'),
        leaked_char('4'),
        leaked_char('6'),
        leaked_char('M'),
    ]
    .into_iter()
    .collect();
    let (accepted, dropped) = drain_guard(&seq);
    assert_eq!(
        accepted, 1,
        "the leading Esc is delivered, the rest dropped"
    );
    assert_eq!(dropped, seq.len() - 1);
}

#[test]
fn sgr_guard_drops_release_variant_lowercase_m() {
    // SGR release uses lowercase `m`. Same coverage as the press variant:
    // only the leading Esc is delivered.
    let seq: Vec<Event> = [
        leaked_esc(),
        leaked_char('['),
        leaked_char('<'),
        leaked_char('3'),
        leaked_char('5'),
        leaked_char(';'),
        leaked_char('5'),
        leaked_char('6'),
        leaked_char('m'),
    ]
    .into_iter()
    .collect();
    let (accepted, _) = drain_guard(&seq);
    assert_eq!(accepted, 1);
}

#[test]
fn sgr_guard_drops_run_of_split_sequences() {
    // The real complaint showed *many* sequences back to back (resize drag).
    // The guard must resync to idle after each terminating M/m and catch
    // the next one too. Each sequence's leading Esc is delivered; the
    // remaining payload bytes are swallowed.
    let one = |b: char| {
        vec![
            leaked_esc(),
            leaked_char('['),
            leaked_char('<'),
            leaked_char(b),
            leaked_char(';'),
            leaked_char('1'),
            leaked_char('M'),
        ]
    };
    let seq: Vec<Event> = [one('0'), one('3'), one('5')]
        .into_iter()
        .flatten()
        .collect();
    let (accepted, _) = drain_guard(&seq);
    assert_eq!(accepted, 3, "each sequence delivers its leading Esc");
}

#[test]
fn sgr_guard_passes_through_normal_typing() {
    // Ordinary typing must be unaffected — the guard never enters a
    // tracking state and hands every char back as Accept.
    let seq: Vec<Event> = ['h', 'e', 'l', 'l', 'o']
        .into_iter()
        .map(leaked_char)
        .collect();
    let (accepted, dropped) = drain_guard(&seq);
    assert_eq!(accepted, seq.len());
    assert_eq!(dropped, 0);
}

#[test]
fn sgr_guard_delivers_lone_esc() {
    // Regression: a standalone Esc (e.g. the first of a double-Esc
    // interrupt) must reach the app. The previous guard dropped it as a
    // suspected leak prefix, which broke double-Esc interrupt entirely.
    let mut g = SgrLeakGuard::default();
    assert!(matches!(g.feed(&leaked_esc()), Feed::Accept));
    // Not idle: it armed the tracker so a following `[` still opens a leak.
    assert!(!g.is_idle());
    // A subsequent normal char aborts the tracking and is delivered too.
    assert!(matches!(g.feed(&leaked_char('x')), Feed::Accept));
    assert!(g.is_idle());
}

#[test]
fn sgr_guard_delivers_double_esc() {
    // The double-Esc interrupt path: two Escs with nothing between them.
    // Neither is part of an SGR sequence (a real leak has `[` next, never
    // another Esc), so both must be delivered.
    let mut g = SgrLeakGuard::default();
    assert!(matches!(g.feed(&leaked_esc()), Feed::Accept));
    assert!(matches!(g.feed(&leaked_esc()), Feed::Accept));
    assert!(!g.is_idle());
}

#[test]
fn sgr_guard_recovers_from_aborted_prefix() {
    // `ESC [` followed by something other than `<` is a real CSI (e.g. an
    // arrow key's payload). The leading Esc is delivered; `[` is dropped
    // (it can only be leak noise from this state); the aborting char is
    // delivered and the guard returns to idle.
    let mut g = SgrLeakGuard::default();
    // ESC [ A = Up arrow, delivered as separate chars by a broken read.
    assert!(matches!(g.feed(&leaked_esc()), Feed::Accept));
    assert!(matches!(g.feed(&leaked_char('[')), Feed::Drop));
    // 'A' is not the SGR intro: the guard aborts and *this* event is
    // accepted (returned to the caller to deal with), then goes idle.
    assert!(matches!(g.feed(&leaked_char('A')), Feed::Accept));
    assert!(g.is_idle());
    // Subsequent normal typing is accepted.
    assert!(matches!(g.feed(&leaked_char('x')), Feed::Accept));
}

#[test]
fn sgr_guard_resets_on_non_key_events() {
    // A genuine parsed mouse event or a resize resyncs the tracker, so a
    // half-buffered prefix can't poison the next real interaction.
    let mut g = SgrLeakGuard::default();
    assert!(matches!(g.feed(&leaked_esc()), Feed::Accept));
    assert!(matches!(g.feed(&leaked_char('[')), Feed::Drop));
    assert!(matches!(g.feed(&Event::Resize(80, 24)), Feed::Accept));
    assert!(g.is_idle());
    assert!(matches!(g.feed(&leaked_char('x')), Feed::Accept));
}

#[test]
fn sgr_guard_survives_garbage_inside_payload() {
    // A malformed payload (non-digit, non-;) while inside an SGR sequence
    // resyncs to idle instead of swallowing arbitrary following text.
    let mut g = SgrLeakGuard::default();
    assert!(matches!(g.feed(&leaked_esc()), Feed::Accept));
    assert!(matches!(g.feed(&leaked_char('[')), Feed::Drop));
    assert!(matches!(g.feed(&leaked_char('<')), Feed::Drop));
    // A letter that is not the terminator: abort and resync.
    assert!(matches!(g.feed(&leaked_char('Z')), Feed::Accept));
    assert!(g.is_idle());
}

// The OAuth pending sheet shows a device verification code + URL that the
// user must copy. Mouse drag-select does not reach modal body text, so `c`
// / `u` are the only in-app copy path — guard against regressions.
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
            config_custom_editing: false,
            config_websearch_editing: false,
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
            config_custom_editing: false,
            config_websearch_editing: false,
        },
        &mut drag,
    )
}

#[test]
fn oauth_pending_c_copies_user_code() {
    assert_eq!(
        oauth_key('c'),
        InputAction::CopyOauthContent {
            target: OauthCopyTarget::UserCode
        }
    );
}

#[test]
fn oauth_pending_u_copies_url() {
    assert_eq!(
        oauth_key('u'),
        InputAction::CopyOauthContent {
            target: OauthCopyTarget::Url
        }
    );
}

#[test]
fn oauth_pending_enter_and_space_and_y_copy_selected() {
    assert_eq!(
        oauth_keycode(KeyCode::Enter),
        InputAction::CopyOauthContent {
            target: OauthCopyTarget::Selected,
        }
    );
    assert_eq!(
        oauth_key(' '),
        InputAction::CopyOauthContent {
            target: OauthCopyTarget::Selected,
        }
    );
    assert_eq!(
        oauth_key('y'),
        InputAction::CopyOauthContent {
            target: OauthCopyTarget::Selected,
        }
    );
    assert_eq!(
        oauth_keycode(KeyCode::Tab),
        InputAction::CycleOauthSelection,
    );
    assert_eq!(
        oauth_keycode(KeyCode::BackTab),
        InputAction::CycleOauthSelection,
    );
}

#[test]
fn text_modal_commands_resolve_and_consume_composer() {
    // Slash commands that open an interactive modal are intercepted locally:
    // they resolve to a data-less `Open*` action (not `SendSlash`) and consume
    // the composer text. The event loop snapshots the composer before dispatch
    // and relies on `is_text_modal_command` to recover that text for input
    // history + transcript recording — so these must stay in sync with the
    // intercepted set in `process_event`.
    for (cmd, expected) in [
        ("/models", InputAction::OpenModels),
        ("/connections", InputAction::OpenConnections),
        ("/permissions", InputAction::OpenPermissions),
        ("/tools", InputAction::OpenTools),
        ("/mcp", InputAction::OpenMcp),
        ("/skills", InputAction::OpenSkills),
        ("/settings", InputAction::OpenConfig),
        ("/config", InputAction::OpenConfig),
    ] {
        let mut input = cmd.to_string();
        let action = enter(&mut input, true);
        assert_eq!(action, expected, "modal command {cmd}");
        assert!(
            input.is_empty(),
            "composer must be consumed for {cmd}, got {input:?}"
        );
        assert!(
            action.is_text_modal_command(),
            "{cmd} must be flagged as a text modal command so its \
             invocation is recorded in history + transcript"
        );
    }
}

#[test]
fn keybinding_modals_are_not_text_commands() {
    // Ctrl+R / F1 open modals via keybindings, not by typing a slash
    // command, so they must NOT be flagged: they consume no composer text and
    // therefore have nothing to record in input history.
    assert!(!InputAction::OpenHistory.is_text_modal_command());
    assert!(!InputAction::OpenHelp.is_text_modal_command());
    // `/exit` resolves to Quit — it is not a replayable input, so it is
    // deliberately excluded from the recorded set.
    assert!(!InputAction::Quit.is_text_modal_command());
    // Notification-style slash commands carry their text on the action, so the
    // event loop records it from the action itself rather than via this hint.
    assert!(!InputAction::SendSlash("/pursue".to_string()).is_text_modal_command());
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

#[test]
fn mouse_wheel_still_scrolls_when_no_modal_open() {
    // Regression guard: outside the question modal the wheel keeps its original
    // transcript-scroll behavior.
    use crossterm::event::{MouseEvent, MouseEventKind};

    let mk = |kind| {
        let mut input = String::new();
        let mut cursor = 0;
        let mut drag = SelectionDrag::default();
        process_event(
            Event::Mouse(MouseEvent {
                kind,
                column: 5,
                row: 5,
                modifiers: KeyModifiers::NONE,
            }),
            &mut input,
            &mut cursor,
            mouse_ctx_for(crate::Modal::None),
            &mut drag,
        )
    };

    assert_eq!(mk(MouseEventKind::ScrollUp), InputAction::ScrollUp);
    assert_eq!(mk(MouseEventKind::ScrollDown), InputAction::ScrollDown);
}

#[test]
fn ctrl_x_in_history_modal_arms_clear() {
    // Ctrl+X inside the Ctrl+R panel arms the clear-history confirmation.
    // It must never type an `x` into the filter.
    let mut input = "git".to_string();
    let mut cursor = 3;
    let action = run_history_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('x'),
        KeyModifiers::CONTROL,
    );
    assert_eq!(action, InputAction::HistoryClearAll);
    assert_eq!(input, "git", "Ctrl+X must not type into the filter");
    assert_eq!(cursor, 3);
}

#[test]
fn ctrl_x_outside_history_modal_is_a_noop() {
    // Nowhere else does Ctrl+X mean anything: at the top level (no modal) it
    // must not arm the clear — a stray Ctrl+X while composing can never wipe
    // history.
    let mut input = "draft".to_string();
    let mut cursor = 5;
    let mut drag = SelectionDrag::default();
    let action = process_event(
        Event::Key(KeyEvent {
            code: KeyCode::Char('x'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }),
        &mut input,
        &mut cursor,
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
    );
    assert_eq!(action, InputAction::None);
    assert_eq!(input, "draft");
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

#[test]
fn armed_clear_confirm_resolves_on_y_or_enter() {
    let mut input = "filter".to_string();
    let mut cursor = 6;
    assert_eq!(
        run_history_clear_key(
            &mut input,
            &mut cursor,
            KeyCode::Char('y'),
            KeyModifiers::NONE
        ),
        InputAction::HistoryClearConfirm,
        "y confirms the wipe"
    );
    assert_eq!(
        run_history_clear_key(&mut input, &mut cursor, KeyCode::Enter, KeyModifiers::NONE),
        InputAction::HistoryClearConfirm,
        "Enter confirms the wipe"
    );
    assert_eq!(input, "filter", "armed confirm must not edit the filter");
}

#[test]
fn armed_clear_confirm_cancels_on_any_other_key() {
    let mut input = "filter".to_string();
    let mut cursor = 6;
    // Esc, `n`, and a plain filter letter all cancel — and none of them may
    // type into the (soon-to-be-wiped) history.
    for code in [KeyCode::Esc, KeyCode::Char('n'), KeyCode::Char('g')] {
        assert_eq!(
            run_history_clear_key(&mut input, &mut cursor, code, KeyModifiers::NONE),
            InputAction::HistoryClearCancel,
            "{code:?} cancels the armed clear"
        );
    }
    assert_eq!(input, "filter", "cancelling must not edit the filter");
}

#[test]
fn sessions_modal_n_key_triggers_create_new_session() {
    let mut input = String::new();
    let mut cursor = 0;
    let action_n = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('n'),
        KeyModifiers::NONE,
        crate::Modal::Sessions,
        false,
    );
    assert_eq!(action_n, InputAction::CreateNewSession);

    let action_big_n = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('N'),
        KeyModifiers::NONE,
        crate::Modal::Sessions,
        false,
    );
    assert_eq!(action_big_n, InputAction::CreateNewSession);
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

#[test]
fn digit_on_effort_field_jumps_to_that_ladder_rung() {
    // The flat segmented selector makes direct selection natural: `1`..=`7`
    // map to ladder rungs (0-indexed here), and the key never lands in the
    // borrowed input line.
    let mut input = "high".to_string();
    assert_eq!(
        editor_key(KeyCode::Char('1'), 1, &mut input),
        InputAction::ModelEditorEffortJump { index: 0 }
    );
    assert_eq!(
        editor_key(KeyCode::Char('7'), 1, &mut input),
        InputAction::ModelEditorEffortJump { index: 6 }
    );
    assert_eq!(input, "high", "digit must not edit the effort field's line");
}

#[test]
fn digit_jump_is_scoped_to_the_effort_field() {
    // `0` is not a tier (ladder rungs are 1-based), and digits on the API-key
    // field stay ordinary typed characters (a key can contain digits).
    let mut input = "high".to_string();
    assert_eq!(
        editor_key(KeyCode::Char('0'), 1, &mut input),
        InputAction::InsertChar('0'),
        "`0` is not a rung, so it falls through to the borrowed line"
    );

    let mut key = "sk-".to_string();
    assert_eq!(
        editor_key(KeyCode::Char('5'), 0, &mut key),
        InputAction::InsertChar('5'),
        "digits on the API-key field type normally"
    );
    assert_eq!(key, "sk-5");
}

// ─── Forward delete (Del key) ─────────────────────────────────────────────

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

#[test]
fn delete_key_removes_char_after_cursor() {
    // The Del key's defining behaviour: remove the character *after* the
    // caret, leaving the caret unmoved.
    let mut input = "hello".to_string();
    let mut cursor = 2;
    assert_eq!(
        compose_key(KeyCode::Delete, KeyModifiers::NONE, &mut input, &mut cursor),
        InputAction::DeleteForward
    );
    assert_eq!(input, "helo");
    assert_eq!(cursor, 2, "forward delete must not move the caret");
}

#[test]
fn delete_key_at_end_is_inert() {
    let mut input = "hello".to_string();
    let mut cursor = 5;
    assert_eq!(
        compose_key(KeyCode::Delete, KeyModifiers::NONE, &mut input, &mut cursor),
        InputAction::None
    );
    assert_eq!(input, "hello");
    assert_eq!(cursor, 5);
}

#[test]
fn delete_key_removes_full_grapheme_cluster() {
    // A CJK char occupies 3 bytes but must vanish as one user-visible glyph;
    // the byte math must never split what the user sees as one character.
    let mut input = "中abc".to_string();
    let mut cursor = 0;
    assert_eq!(
        compose_key(KeyCode::Delete, KeyModifiers::NONE, &mut input, &mut cursor),
        InputAction::DeleteForward
    );
    assert_eq!(input, "abc");
    assert_eq!(cursor, 0);
}

#[test]
fn horizontal_motion_skips_entire_zwj_grapheme() {
    let family = "👨‍👩‍👧‍👦";
    let mut input = format!("a{family}b");
    let mut cursor = 1 + family.chars().count();

    assert_eq!(
        compose_key(KeyCode::Left, KeyModifiers::NONE, &mut input, &mut cursor),
        InputAction::None
    );
    assert_eq!(cursor, 1, "Left lands before the whole family emoji");

    compose_key(KeyCode::Right, KeyModifiers::NONE, &mut input, &mut cursor);
    assert_eq!(
        cursor,
        1 + family.chars().count(),
        "Right lands after the whole family emoji"
    );
}

#[test]
fn backspace_removes_entire_zwj_grapheme() {
    let family = "👨‍👩‍👧‍👦";
    let mut input = format!("a{family}b");
    let mut cursor = 1 + family.chars().count();

    assert_eq!(
        compose_key(
            KeyCode::Backspace,
            KeyModifiers::NONE,
            &mut input,
            &mut cursor,
        ),
        InputAction::Backspace
    );
    assert_eq!(input, "ab");
    assert_eq!(cursor, 1);
}

#[test]
fn delete_removes_entire_combining_grapheme() {
    let mut input = "ae\u{301}b".to_string();
    let mut cursor = 1;

    assert_eq!(
        compose_key(KeyCode::Delete, KeyModifiers::NONE, &mut input, &mut cursor),
        InputAction::DeleteForward
    );
    assert_eq!(input, "ab");
    assert_eq!(cursor, 1);
}

#[test]
fn delete_key_eats_whole_attachment_chip() {
    // Forward-deleting the `[` of an attachment chip removes the whole chip
    // (plus the trailing space a paste inserts) in one keystroke, mirroring
    // the chip-aware Backspace.
    let chip = crate::composer_attachments::image_chip(1, 42);
    let mut input = format!("hello {chip} world");
    let mut cursor = "hello ".len();
    assert_eq!(
        compose_key(KeyCode::Delete, KeyModifiers::NONE, &mut input, &mut cursor),
        InputAction::DeleteForward
    );
    assert_eq!(input, "hello world", "chip + trailing space both go");
    assert_eq!(cursor, "hello ".len());
}

#[test]
fn delete_key_inert_outside_free_text() {
    // In a read-only modal (Help) the Del key must do nothing — the
    // `edits_input_field` gate keeps it from mutating the borrowed composer.
    let mut input = "hello".to_string();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    assert_eq!(
        process_event(
            Event::Key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE)),
            &mut input,
            &mut cursor,
            InputContext {
                active_modal: crate::Modal::Help,
                ..Default::default()
            },
            &mut drag,
        ),
        InputAction::None
    );
    assert_eq!(input, "hello");
}

#[test]
fn delete_key_closes_selected_view_in_switcher() {
    let mut input = String::new();
    let mut cursor = 0;
    let mut drag = SelectionDrag::default();
    assert_eq!(
        process_event(
            Event::Key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE)),
            &mut input,
            &mut cursor,
            InputContext {
                active_modal: crate::Modal::ViewSwitcher,
                ..Default::default()
            },
            &mut drag,
        ),
        InputAction::ViewCloseSelected
    );
}

#[test]
fn host_prompt_delete_key_removes_forward_char() {
    // The /host dashboard's inline prompt borrows the composer line; Del
    // there deletes forward too (the branch swallows the key and returns
    // None — the mutation is the whole effect).
    let mut input = "abc".to_string();
    let mut cursor = 1;
    let mut drag = SelectionDrag::default();
    assert_eq!(
        process_event(
            Event::Key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE)),
            &mut input,
            &mut cursor,
            InputContext {
                active_modal: crate::Modal::Host,
                host_prompting: true,
                ..Default::default()
            },
            &mut drag,
        ),
        InputAction::None
    );
    assert_eq!(input, "ac");
    assert_eq!(cursor, 1);
}
