//! Cursor movement tests: arrows, word jumps, line-aware movement, Home/End/PageUp/PageDown.

use super::*;

#[test]
fn bare_arrows_walk_steps_when_focused() {
    // ADR-0176: When a target is focused, bare ↑/↓ navigate targets.
    // When composer is active, bare ↑/↓ hand off to history recall.
    assert_eq!(key_with_focus(KeyCode::Up), InputAction::FocusPrevTarget);
    assert_eq!(key_with_focus(KeyCode::Down), InputAction::FocusNextTarget);
}

#[test]
fn home_and_end_navigate_line_in_composer_and_scroll_in_focus() {
    // ADR-0176: Home/End in the composer move caret to line-start/end (readline convention).
    // In target focus or browse focus, Home/End scroll the transcript to top/bottom.
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
    assert_eq!(cursor, 0, "Home moves caret to start of line in composer");

    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::End,
        KeyModifiers::NONE,
        crate::Modal::None,
        false,
    );
    assert_eq!(action, InputAction::None);
    assert_eq!(cursor, 5, "End moves caret to end of line in composer");

    // When target or browse focus is active, Home/End scroll transcript
    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::Home,
        KeyModifiers::NONE,
        crate::Modal::None,
        true,
    );
    assert_eq!(action, InputAction::ScrollTop);

    let action = run_key(
        &mut input,
        &mut cursor,
        KeyCode::End,
        KeyModifiers::NONE,
        crate::Modal::None,
        true,
    );
    assert_eq!(action, InputAction::ScrollBottom);
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
        run_sheet_key(
            &mut input,
            &mut cursor,
            KeyCode::Home,
            KeyModifiers::NONE,
            crate::sheet::SheetKind::Permission,
            false
        ),
        InputAction::ScrollTop
    );
    assert_eq!(
        run_sheet_key(
            &mut input,
            &mut cursor,
            KeyCode::End,
            KeyModifiers::NONE,
            crate::sheet::SheetKind::Permission,
            false
        ),
        InputAction::ScrollBottom
    );
}

#[test]
fn ctrl_home_and_end_scroll_regardless_of_focus() {
    let mut input = "hello".to_string();
    let mut cursor = 3;
    assert_eq!(
        run_key(
            &mut input,
            &mut cursor,
            KeyCode::Home,
            KeyModifiers::CONTROL,
            crate::Modal::None,
            false
        ),
        InputAction::ScrollTop
    );
    assert_eq!(cursor, 3, "Ctrl+Home must not move the caret");
    assert_eq!(
        run_key(
            &mut input,
            &mut cursor,
            KeyCode::End,
            KeyModifiers::CONTROL,
            crate::Modal::None,
            false
        ),
        InputAction::ScrollBottom
    );
    assert_eq!(cursor, 3, "Ctrl+End must not move the caret");
}

#[test]
fn page_keys_scroll_question_modal_body() {
    let mut input = String::new();
    let mut cursor = 0;
    assert_eq!(
        run_sheet_key(
            &mut input,
            &mut cursor,
            KeyCode::PageUp,
            KeyModifiers::NONE,
            crate::sheet::SheetKind::Question,
            false
        ),
        InputAction::ScrollPageUp
    );
    assert_eq!(
        run_sheet_key(
            &mut input,
            &mut cursor,
            KeyCode::PageDown,
            KeyModifiers::NONE,
            crate::sheet::SheetKind::Question,
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
        crate::Modal::Todos,
        crate::Modal::Permissions,
        crate::Modal::Config,
        crate::Modal::Telemetry,
        crate::Modal::OauthPending,
        crate::Modal::ProviderPreset,
        crate::Modal::CustomProvider,
        crate::Modal::Tools,
        crate::Modal::Mcp,
        crate::Modal::Skills,
        crate::Modal::Sessions,
        crate::Modal::Queue,
        crate::Modal::HistorySearch,
        crate::Modal::Connections,
        crate::Modal::Models,
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

/// The caret-owning text editors (ModelEditor, InputInjection) have no body
/// scroll, so PageUp / PageDown and Ctrl+↑ / Ctrl+↓ must be inert there
/// (no-op), not a stray page-scroll or transcript focus gesture.
#[test]
fn page_keys_are_inert_in_caret_editors() {
    {
        let mut input = String::new();
        let mut cursor = 0;
        assert_eq!(
            run_key(
                &mut input,
                &mut cursor,
                KeyCode::PageUp,
                KeyModifiers::NONE,
                crate::Modal::ModelEditor,
                false
            ),
            InputAction::None,
            "PageUp should be a no-op in ModelEditor"
        );
        assert_eq!(
            run_key(
                &mut input,
                &mut cursor,
                KeyCode::PageDown,
                KeyModifiers::NONE,
                crate::Modal::ModelEditor,
                false
            ),
            InputAction::None,
            "PageDown should be a no-op in ModelEditor"
        );
        assert_eq!(
            run_key(
                &mut input,
                &mut cursor,
                KeyCode::Up,
                KeyModifiers::CONTROL,
                crate::Modal::ModelEditor,
                false
            ),
            InputAction::None,
            "Ctrl+Up should be a no-op in ModelEditor"
        );
    }
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
fn line_aware_movement_respects_newlines() {
    // Multi-line input: Home/End/Ctrl+A/Ctrl+E operate on the current
    // logical line, not the whole buffer.
    let mut input = "line1\nline2\nline3".to_string();
    // Place the caret in the middle of the second line ("line2").
    // "line1\n" = 6 chars, then 2 more into "line2" -> char index 8.
    let mut cursor = 8;

    // Ctrl+A -> start of "line2" (char index 6, just past the first '\n').
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('a'),
        KeyModifiers::CONTROL,
        crate::Modal::None,
        false,
    );
    assert_eq!(cursor, 6, "Ctrl+A should land at start of current line");

    // Ctrl+E -> end of "line2" (char index 11, just before the second '\n').
    run_key(
        &mut input,
        &mut cursor,
        KeyCode::Char('e'),
        KeyModifiers::CONTROL,
        crate::Modal::None,
        false,
    );
    assert_eq!(cursor, 11, "Ctrl+E should land at end of current line");

    // Ctrl+A snaps back to the line start.
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
fn pageup_scrolls_transcript_page() {
    assert_eq!(pageup_key(), InputAction::ScrollPageUp);
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
fn arrows_recall_history_once_command_is_fully_typed() {
    // Once a command is resolved (exact match), completion popup closes and
    // arrows hand off at the draft's edges to inline history recall
    // (ADR-0174): a single-line command draft is all edge.
    let kind = crate::CompletionKind::Slash;
    assert_eq!(
        compose_key_with_completion(KeyCode::Down, kind, 1, true),
        InputAction::HistoryNext,
        "↓ on an exact-match command recalls the next history entry"
    );
    assert_eq!(
        compose_key_with_completion(KeyCode::Up, kind, 1, true),
        InputAction::HistoryPrev,
        "↑ on an exact-match command recalls the previous history entry"
    );
}

#[test]
fn up_arrow_in_browse_hands_off_to_history() {
    // The queued-message recall only fires from Compose (where the user can
    // actually edit the recalled draft). In Browse (a step selected, no
    // completion), a single-line draft's ↑ still hands off to inline
    // history recall at the edge (ADR-0174); step walking stays verb-owned
    // (Alt+↑, ADR-0173).
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
            connection_info_detail: false,
            is_responding: false,
            completion_kind: crate::CompletionKind::None,
            suggestion_count: 0,
            has_exact_suggestion: false,
            suggestion_index: None,
            completion_dismissed: false,
            has_trigger_text: false,
            permission_confirm_always: false,
            has_focused_target: false,
            ..Default::default()
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::HistoryPrev);
}

#[test]
fn up_arrow_walks_lines_in_multiline_and_hands_off_at_top_line() {
    // In a multi-line draft, ↑ moves the caret up a line. From the top
    // line, it hands off to inline history recall (ADR-0174).
    let seed = "hello\nworld";
    // Caret at end of second line: ↑ should move to the same column on
    // the first line ("hello", col 5) and stay a caret motion.
    let (action, cur) = multiline_arrow(seed, "hello\nworld".chars().count(), KeyCode::Up);
    assert_eq!(action, InputAction::None);
    assert_eq!(cur, 5, "up should land at col 5 on the first line");

    // Sitting on the first line: ↑ hands off to history recall.
    let (action, _) = multiline_arrow(seed, 5, KeyCode::Up);
    assert_eq!(action, InputAction::HistoryPrev);
}

#[test]
fn down_arrow_walks_lines_in_multiline_and_hands_off_at_bottom_line() {
    let seed = "hello\nworld";
    // Caret at start of first line: ↓ moves to the same column on the
    // second line and stays a caret motion.
    let (action, cur) = multiline_arrow(seed, 0, KeyCode::Down);
    assert_eq!(action, InputAction::None);
    assert_eq!(cur, 6, "down should land at col 0 of the second line");

    // Caret at end of the second line: ↓ hands off to history recall
    // (ADR-0174) — walking forward, or restoring the stashed draft.
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
