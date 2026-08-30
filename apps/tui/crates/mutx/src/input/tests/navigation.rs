//! Cursor movement tests: arrows, word jumps, line-aware movement, Home/End/PageUp/PageDown.

use super::*;

#[test]
fn arrows_cycle_steps_while_focused() {
    // With a step focused, bare ↑/↓ cycle the focus instead of walking
    // history (history resumes once Esc clears the focus).
    assert_eq!(key_with_focus(KeyCode::Up), InputAction::FocusPrevTarget);
    assert_eq!(key_with_focus(KeyCode::Down), InputAction::FocusNextTarget);
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
    assert_eq!(action, InputAction::FocusPrevTarget);
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
