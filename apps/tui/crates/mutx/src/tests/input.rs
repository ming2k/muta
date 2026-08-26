//! Composer input state tests: caret ownership, IME, selection, key handling, paste, focus.

use super::*;


#[test]
fn focused_tool_steps_mut_only_touches_focused_runner_children() {
    let mut messages = conversation_with_runners();
    // Focused on task_a: its single child is an assistant message (not a
    // tool step), so the focused stream has 1 message and 0 tool steps.
    let focus = vec![crate::app::ZoomFrame {
        call_id: "task_a".to_string(),
        saved_scroll: crate::app::ScrollSnapshot::default(),
    }];
    let total = focused_messages_mut(&mut messages, &focus).count();
    assert_eq!(total, 1);
    let tool_steps = focused_messages_mut(&mut messages, &focus)
        .filter(|m| m.is_tool_step())
        .count();
    assert_eq!(tool_steps, 0);

    // Root view: 4 messages total, 2 of which are tool steps.
    let focus: Vec<crate::app::ZoomFrame> = Vec::new();
    assert_eq!(focused_messages_mut(&mut messages, &focus).count(), 4);
    let tool_steps = focused_messages_mut(&mut messages, &focus)
        .filter(|m| m.is_tool_step())
        .count();
    assert_eq!(tool_steps, 2);
}


#[test]
fn paste_in_readonly_modal_is_dropped_silently() {
    // Read-only / non-text modals (Help, Sessions, Permission, ...) drop a
    // paste silently — no insertion, no toast, no attachment.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.set_active_modal_for_test(Modal::Help);
    app.input = String::new();
    app.cursor_position = 0;

    clipboard_ops::apply_clipboard_paste(
        &mut app,
        crate::clipboard::ClipboardRead::Text("ignored".to_string()),
    );

    assert!(app.input.is_empty());
    assert!(
        app.copy_toast_until.is_none(),
        "readonly modal paste should not toast"
    );
    assert!(app.pending_text_pastes.is_empty());
}


// ── Caret ownership / visibility (IME anchor) ─────────────────────────────
// `App::caret_owner` / `App::caret_visible` are the single source of truth for
// which surface holds the terminal cursor. The IME anchors its composition
// window to that cursor, so any state that owns no caret must hide it —
// otherwise the IME binds to a stale coordinate (the "drift" when a disclosure
// is clicked mid-composition). These lock the contract for every state.

#[test]
fn caret_owner_composer_by_default() {
    let (app, _tmp) = app_in_tempdir(&[], &[]);
    assert_eq!(app.caret_owner(), CaretOwner::Composer);
    assert!(
        app.caret_visible(),
        "no modal, no focus, no selection → visible"
    );
}


#[test]
fn caret_owner_none_when_step_focused() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.focused_target = Some(InteractiveTarget::tool_step(0));
    assert_eq!(app.caret_owner(), CaretOwner::None);
    assert!(
        !app.caret_visible(),
        "a focused transcript step owns no caret → hidden, IME unanchored"
    );
}


#[test]
fn caret_owner_none_in_runner_view() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.enter_runner("call-1".to_string());
    assert_eq!(app.caret_owner(), CaretOwner::None);
    assert!(
        !app.caret_visible(),
        "runner zoom has no input line → cursor hidden, IME unanchored"
    );
}


#[test]
fn caret_owner_modal_for_caret_modals() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    for modal in [Modal::CustomProvider, Modal::InputInjection] {
        app.set_active_modal_for_test(modal);
        assert_eq!(
            app.caret_owner(),
            CaretOwner::Modal,
            "{modal:?} borrows the input line and renders its own caret",
        );
        assert!(
            app.caret_visible(),
            "{modal:?} must keep the cursor visible so the IME anchors to its field",
        );
    }
}


#[test]
fn caret_owner_none_for_read_only_and_decision_modals() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    for modal in [
        Modal::Help,
        Modal::Sessions,
        Modal::Tools,
        Modal::Mcp,
        Modal::Permissions,
        Modal::Activity,
        // `Question` is listed here to cover the *default* state — any option
        // but "Other" highlighted (or no question model at all). Its caret
        // ownership is conditional: see `caret_owner_question_owns_caret_only_on_other`.
        Modal::Question,
        Modal::Permission,
        Modal::Config,
    ] {
        app.set_active_modal_for_test(modal);
        assert_eq!(
            app.caret_owner(),
            CaretOwner::None,
            "{modal:?} renders no caret → cursor must hide so the IME has no stale anchor",
        );
        assert!(
            !app.caret_visible(),
            "{modal:?} must hide the terminal cursor",
        );
    }
}


#[test]
fn caret_owner_question_owns_caret_only_on_other() {
    // The Question modal is a decision sheet (no caret) EXCEPT while the
    // synthetic "Other" free-text row is highlighted — then it is a real
    // text-input surface and must own the terminal cursor so the host IME can
    // anchor its composition window. Navigating to/from "Other" flips
    // ownership, so the IME anchor appears exactly when there is a field to
    // type into and never when there is not.
    use crate::question_model::{QuestionAction, QuestionModel};
    use muta_contracts::{UserQuestion, UserQuestionOption, UserQuestionRequest};

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let req = UserQuestionRequest {
        id: "q".into(),
        questions: vec![UserQuestion {
            header: None,
            question: "pick".into(),
            options: vec![
                UserQuestionOption {
                    label: "a".into(),
                    description: None,
                },
                UserQuestionOption {
                    label: "b".into(),
                    description: None,
                },
            ],
            multi_select: false,
        }],
        origin: None,
    };
    // Open: highlight on row 0 (a real option) → no caret, cursor hidden.
    let model = QuestionModel::open(req);
    app.set_active_modal_for_test(Modal::Question);
    app.question = Some(model.clone());
    assert_eq!(
        app.caret_owner(),
        CaretOwner::None,
        "real option → no caret"
    );
    assert!(
        !app.caret_visible(),
        "a non-Other option must hide the cursor so the IME has no stale anchor",
    );

    // Navigate down to "Other" (index 2) → caret owned, cursor visible.
    let model = model.update(QuestionAction::Down).0; // -> b (1)
    let model = model.update(QuestionAction::Down).0; // -> Other (2)
    app.question = Some(model);
    assert_eq!(
        app.caret_owner(),
        CaretOwner::Modal,
        "Other highlighted → modal owns the caret for the IME",
    );
    assert!(
        app.caret_visible(),
        "the Other field must keep the cursor visible so the IME anchors to it",
    );

    // Navigate back to a real option → ownership reverts to None.
    let model = app.question.take().unwrap().update(QuestionAction::Up).0;
    app.question = Some(model);
    assert_eq!(
        app.caret_owner(),
        CaretOwner::None,
        "leaving Other must drop caret ownership again",
    );
}


#[test]
fn caret_hidden_while_selection_active_even_for_composer() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    // Composer owns the caret, but an active selection hides the block cursor
    // so it does not clash with the selection background. Ownership is
    // unaffected; only visibility folds in the selection.
    assert_eq!(app.caret_owner(), CaretOwner::Composer);
    app.selection = SelectionState::Range {
        anchor: crate::model::layout::SemanticCursor::new(0, 0, 0),
        head: crate::model::layout::SemanticCursor::new(0, 0, 3),
    };
    assert_eq!(app.caret_owner(), CaretOwner::Composer);
    assert!(
        !app.caret_visible(),
        "an active selection hides the cursor regardless of ownership",
    );
}


#[test]
fn has_input_selection_detects_both_block_and_range() {
    let mut app = app_with_input_selection("hello");
    assert!(app.has_input_selection());

    // A transcript selection never binds the composer.
    app.selection = SelectionState::Block {
        message_idx: 0,
        block_idx: 0,
    };
    assert!(
        !app.has_input_selection(),
        "transcript selections must not trigger the input caret relay"
    );

    // An active Range on INPUT_MSG_IDX is an input selection.
    app.selection = SelectionState::Range {
        anchor: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 0),
        head: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 2),
    };
    assert!(app.has_input_selection());

    // A collapsed Range (anchor == head) is not active and does not count.
    app.selection = SelectionState::Range {
        anchor: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 0),
        head: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 0),
    };
    assert!(!app.has_input_selection());
}


#[test]
fn input_selection_relays_arrows_only_when_composer_owns_caret() {
    let mut app = app_with_input_selection("hello");
    assert_eq!(app.caret_owner(), CaretOwner::Composer);
    assert!(app.input_selection_relays_arrows());

    // A transcript step holding focus means the composer no longer owns the
    // caret: arrows mean step navigation, so the relay must stand down even
    // though a selection is technically active.
    app.focused_target = Some(crate::model::layout::InteractiveTarget::tool_step(0));
    assert!(
        !app.input_selection_relays_arrows(),
        "arrows belong to step navigation while a step holds focus"
    );
}
