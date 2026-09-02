//! Modal input routing tests: models/connections modals, fuzzy filters, question modals, queue/oauth editors, focus.

use super::*;

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
    assert_eq!(action, InputAction::ProviderPickerToggleFavorite);
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
            history_searching: false,
            model_searching: true,
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
fn q_while_focused_does_not_quit_or_insert() {
    // With a step focused, 'q' does not quit and is isolated from typing
    // into the composer.
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
    assert_eq!(action, InputAction::None);
    assert_eq!(input, "");
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
    };

    assert_eq!(
        mk(crossterm::event::MouseEventKind::ScrollUp),
        InputAction::Wheel {
            up: true,
            x: 5,
            y: 5
        }
    );
    assert_eq!(
        mk(crossterm::event::MouseEventKind::ScrollDown),
        InputAction::Wheel {
            up: false,
            x: 5,
            y: 5
        }
    );
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
            connection_info_detail: false,
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
            connection_info_detail: false,
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
            connection_info_detail: false,
            question_other_highlighted: true,
            ..Default::default()
        },
        &mut drag,
    );
    assert_eq!(action, InputAction::QuestionInsertChar(' '));
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
fn composer_mouse_drag_emits_complete_selection_lifecycle() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

    let mut input = "first\nsecond".to_string();
    let mut cursor = input.chars().count();
    let mut drag = SelectionDrag::default();
    let event = |kind, column, row| {
        Event::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    };

    assert_eq!(
        process_event(
            event(MouseEventKind::Down(MouseButton::Left), 5, 20),
            &mut input,
            &mut cursor,
            mouse_ctx_for(crate::Modal::None),
            &mut drag,
        ),
        InputAction::SelectionStart { x: 5, y: 20 }
    );
    assert!(drag.active);

    assert_eq!(
        process_event(
            event(MouseEventKind::Drag(MouseButton::Left), 5, 19),
            &mut input,
            &mut cursor,
            mouse_ctx_for(crate::Modal::None),
            &mut drag,
        ),
        InputAction::SelectionUpdate { x: 5, y: 19 }
    );

    assert_eq!(
        process_event(
            event(MouseEventKind::Up(MouseButton::Left), 5, 19),
            &mut input,
            &mut cursor,
            mouse_ctx_for(crate::Modal::None),
            &mut drag,
        ),
        InputAction::SelectionEnd
    );
    assert!(!drag.active);
}
