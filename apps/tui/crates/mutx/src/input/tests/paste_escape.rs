//! Paste and escape tests: bracketed paste, images, Esc handling and leaked-Esc guards.

use super::*;

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
            connection_info_detail: false,
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
            config_focus: crate::overlays::ConfigFocus::Categories,
            leader_chord: crate::app::LeaderChord::None,
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
            connection_info_detail: false,
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
            config_focus: crate::overlays::ConfigFocus::Categories,
            leader_chord: crate::app::LeaderChord::None,
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
            config_focus: crate::overlays::ConfigFocus::Categories,
            leader_chord: crate::app::LeaderChord::None,
        },
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
            active_modal: crate::Modal::Permission,
            session_info_detail: false,
            connection_info_detail: false,
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
            config_focus: crate::overlays::ConfigFocus::Categories,
            leader_chord: crate::app::LeaderChord::None,
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::PermissionBack);
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
            config_focus: crate::overlays::ConfigFocus::Categories,
            leader_chord: crate::app::LeaderChord::None,
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
            config_focus: crate::overlays::ConfigFocus::Categories,
            leader_chord: crate::app::LeaderChord::None,
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::CloseModal);
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
            config_focus: crate::overlays::ConfigFocus::Categories,
            leader_chord: crate::app::LeaderChord::None,
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

    // Modal::Config is a navigation modal, so bracketed paste is dropped
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
    assert_eq!(action, InputAction::None);
}
