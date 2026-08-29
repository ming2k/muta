//! Enter/Tab submission tests: exact and prefix slash commands, highlighted suggestions, path completion, menu reopen.

use super::*;

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
            history_recall_active: false,
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
            leader_chord: crate::app::LeaderChord::None,
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::None);
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
            history_recall_active: false,
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
            leader_chord: crate::app::LeaderChord::None,
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::ToggleComposerSendMode);
    assert_eq!(input, "follow up");
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
            history_recall_active: false,
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
            leader_chord: crate::app::LeaderChord::None,
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::BtwFocusSelected);
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
