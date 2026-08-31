//! Runtime-surface tests: overview, console/relay monitors, daemon status, activity state, tab surfaces.

use super::*;

#[test]
fn activity_modal_renders_provider_retry_status() {
    let now = std::time::Instant::now();
    let retry = ProviderRetryState {
        attempt: 3,
        max_attempts: 30,
        retry_at: now + std::time::Duration::from_millis(4_000),
        failure: "HTTP 429: rate limit exceeded".to_string(),
    };
    let mut grid = mutx_engine::Grid::new(80, 24);
    let mut frame = mutx_engine::Frame::new(&mut grid);
    let mut scroll = 0;
    let theme = Theme::default();
    let rect = crate::overlays::draw_activity_modal(
        &mut frame,
        crate::overlays::ActivityModalView {
            active_tab: crate::modal::ActivityTab::Activity,
            todos: None,
            user_prompt: Some("Fix issue in parser"),
            round_count: 1,
            current_turn: 1,
            current_model: "claude-sonnet",
            round_started_at: Some(now),
            activity: "waiting to retry",
            provider_retry: Some(&retry),
        },
        &mut scroll,
        &theme,
        &crate::model::selection::SelectionState::None,
        &mut crate::model::layout::LayoutMap::new(),
    );
    assert!(rect.width > 0 && rect.height > 0);
}

#[test]
fn activity_modal_todos_align_with_header() {
    let mut todos = muta_contracts::TodoList::new();
    todos.items.push(muta_contracts::TodoItem {
        id: muta_contracts::TodoId(1),
        content: "First todo task".to_string(),
        status: muta_contracts::TodoStatus::InProgress,
        created_at: 0,
        updated_at: 0,
    });
    let mut terminal = mutx_engine::TestTerminal::new(80, 24);
    let mut scroll = 0;
    let theme = Theme::default();
    let mut layout_map = crate::model::layout::LayoutMap::new();
    let mut rect = mutx_engine::Rect::default();
    terminal.draw(|frame| {
        rect = crate::overlays::draw_activity_modal(
            frame,
            crate::overlays::ActivityModalView {
                active_tab: crate::modal::ActivityTab::Todos,
                todos: Some(&todos),
                user_prompt: None,
                round_count: 0,
                current_turn: 0,
                current_model: "",
                round_started_at: None,
                activity: "",
                provider_retry: None,
            },
            &mut scroll,
            &theme,
            &crate::model::selection::SelectionState::None,
            &mut layout_map,
        );
    });
    let buffer = terminal.buffer();
    let inner_x = rect.x + crate::design::MODAL_INNER_H_PADDING;
    let header_y = rect.y + crate::design::MODAL_INNER_V_PADDING;
    // Header title "Todos" starts at inner_x
    assert_eq!(buffer.get(inner_x, header_y).unwrap().symbol(), "T");
    // Todo item status glyph "●" starts at the exact same column inner_x, aligning with header title
    let body_y = header_y + 2;
    assert_eq!(buffer.get(inner_x, body_y).unwrap().symbol(), "●");
    assert_eq!(buffer.get(inner_x + 1, body_y).unwrap().symbol(), " ");
    assert_eq!(buffer.get(inner_x + 2, body_y).unwrap().symbol(), "F");
}

#[test]
fn activity_modal_expands_to_fit_multiline_prompt_without_scrolling() {
    let long_prompt = "This is a very long prompt submitted by the user that will wrap across multiple visual lines when displayed inside the modal body in an eighty column terminal viewport.";
    let mut terminal = mutx_engine::TestTerminal::new(80, 40);
    let mut scroll = 0;
    let theme = Theme::default();
    let mut layout_map = crate::model::layout::LayoutMap::new();
    let mut rect = mutx_engine::Rect::default();
    terminal.draw(|frame| {
        rect = crate::overlays::draw_activity_modal(
            frame,
            crate::overlays::ActivityModalView {
                active_tab: crate::modal::ActivityTab::Activity,
                todos: None,
                user_prompt: Some(long_prompt),
                round_count: 1,
                current_turn: 1,
                current_model: "claude-sonnet",
                round_started_at: None,
                activity: "idle",
                provider_retry: None,
            },
            &mut scroll,
            &theme,
            &crate::model::selection::SelectionState::None,
            &mut layout_map,
        );
    });

    // In an 80-column terminal, modal width is 72% (56 cols) and body width is 54 cols.
    // The prompt wraps to 3 visual lines.
    // Total visual rows: 1 (Prompt heading) + 3 (prompt) + 1 (blank) + 1 (Status heading) + 1 (detail) + 1 (idle) = 8 rows.
    // With 6 chrome rows, desired is 14 rows.
    assert!(rect.height >= 14);

    // Ensure all visual lines fit in the body without triggering scroll
    assert_eq!(scroll, 0);

    // Ensure no scrollbar arrow is drawn because max_scroll is 0
    let buffer = terminal.buffer();
    let track_x = rect.x + rect.width - crate::design::MODAL_INNER_H_PADDING;
    let track_y = rect.y + crate::design::MODAL_INNER_V_PADDING + 2;
    // The top scrollbar cap is not "▲"
    assert_ne!(buffer.get(track_x, track_y).map(|c| c.symbol()), Some("▲"));
}

/// Regression for the wiring itself: the event loop feeds the input layer the
/// **unsuppressed** `completion_kind` (the dismissal latch travels as its own
/// `completion_dismissed` flag). Suppressing the kind while the latch is set
/// would make Tab's re-open branch unreachable — `completion_kind` would be
/// `None` exactly when the user pressed Tab after Esc — so this pins the
/// contract end to end through the real mapper.
#[test]
fn tab_after_esc_reopens_through_the_event_loop_context_shape() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/se".to_string();
    app.cursor_position = app.input.chars().count();

    // Esc's arm: latch the dismissal, drop the highlight.
    app.suggestion_index = None;
    app.completion_dismissed = true;

    // Build the context exactly as `run_app_loop` does: the candidate list
    // is suppressed (empty) while the latch is set, but the classification
    // is NOT — that distinction is what makes the re-open gesture visible
    // to the input layer.
    let suppress_completions = app.completion_dismissed;
    let completions = if suppress_completions {
        Vec::new()
    } else {
        app.completions()
    };
    let completion_kind = app.completion_kind();
    let has_trigger_text = app.completion_trigger_text_present();
    let mut input = app.input.clone();
    let mut cursor = app.cursor_position;
    let mut drag = crate::model::selection::SelectionDrag::default();
    let action = crate::input::process_event(
        crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Tab,
            crossterm::event::KeyModifiers::NONE,
        )),
        &mut input,
        &mut cursor,
        crate::input::InputContext {
            active_modal: crate::Modal::None,
            completion_kind,
            suggestion_count: completions.len(),
            has_exact_suggestion: false,
            suggestion_index: app.suggestion_index,
            completion_dismissed: app.completion_dismissed,
            has_trigger_text,
            ..Default::default()
        },
        &mut drag,
    );
    assert_eq!(
        action,
        crate::input::InputAction::ReopenCompletion,
        "Tab after Esc must re-open the dismissed slash menu"
    );
    // And the ReopenCompletion arm's state change restores a selected menu
    // once the loop's post-dispatch anchor runs.
    app.completion_dismissed = false;
    let completions = app.completions();
    app.anchor_completion_selection(&completions);
    assert_eq!(app.suggestion_index, Some(0));
    assert!(!app.completion_dismissed);
}

#[test]
fn relay_left_arrow_breaks_selection_at_head_then_steps() {
    let mut app = app_with_input_selection("hello world");
    // Hidden caret at the release point: end of "hello world" (char 11).
    // ← must break the selection there and step one left: 10.
    let action = relay_probe(&mut app, crossterm::event::KeyCode::Left);
    assert!(matches!(action, Some(crate::input::InputAction::None)));
    assert_eq!(
        app.selection,
        SelectionState::None,
        "← must break selection"
    );
    assert_eq!(
        app.cursor_position, 10,
        "first ← lands one past the release point"
    );
}

#[test]
fn relay_right_arrow_clamps_at_buffer_end() {
    let mut app = app_with_input_selection("abc");
    app.cursor_position = 3; // released at the end
    relay_probe(&mut app, crossterm::event::KeyCode::Right);
    assert_eq!(app.cursor_position, 3, "→ past the end clamps");
    assert_eq!(app.selection, SelectionState::None);
}

#[test]
fn relay_up_and_down_restore_hidden_caret() {
    // The hidden caret's position for a whole-input selection is defined as
    // the head edge (the buffer end) — ↑/↓ restore the caret there and
    // consume the press, rather than leaving the stale pre-selection
    // position in place.
    let mut app = app_with_input_selection("hello");
    app.cursor_position = 1; // stale visible caret from before the drag
    relay_probe(&mut app, crossterm::event::KeyCode::Up);
    assert_eq!(
        app.cursor_position, 5,
        "↑ must restore the caret at the head edge, not the stale position"
    );
    assert_eq!(app.selection, SelectionState::None);

    // ↓ behaves identically: adopt the head edge and consume the press. The
    // press itself does not walk lines or history — that resumes from the
    // restored position on the next key.
    let mut app = app_with_input_selection("hello");
    app.cursor_position = 1;
    relay_probe(&mut app, crossterm::event::KeyCode::Down);
    assert_eq!(app.cursor_position, 5);
    assert_eq!(app.selection, SelectionState::None);
}

#[test]
fn relay_backspace_and_delete_replace_selection() {
    for code in [
        crossterm::event::KeyCode::Backspace,
        crossterm::event::KeyCode::Delete,
    ] {
        let mut app = app_with_input_selection("keep this");
        app.cursor_position = 1; // stale visible caret
        let action = relay_probe(&mut app, code);
        assert!(
            matches!(action, Some(crate::input::InputAction::Backspace)),
            "delete-family must return Backspace's post-edit signal"
        );
        assert_eq!(app.input, "", "the whole selection goes in one stroke");
        assert_eq!(app.cursor_position, 0);
        assert_eq!(app.selection, SelectionState::None);
    }
}

#[test]
fn relay_ignores_keys_without_selection_or_outside_family() {
    // No selection: the probe must miss so ordinary input handling runs.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = "hi".to_string();
    assert!(relay_probe(&mut app, crossterm::event::KeyCode::Left).is_none());

    // With a selection, an uninvolved key (e.g. `x`) must NOT be swallowed:
    // typing over a selection is out of scope for the relay (the TUI has no
    // replace-selection-on-type), so the key keeps its normal meaning.
    let mut app = app_with_input_selection("hi");
    assert!(relay_probe(&mut app, crossterm::event::KeyCode::Char('x')).is_none());
    assert!(
        app.has_input_selection(),
        "an uninvolved key must leave the selection intact"
    );
}

#[tokio::test]
async fn console_bare_text_prompts_the_selection() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    console_host_rows(&mut app);
    console_dispatch(&mut app, "fix the flaky test", false).await;
    match &app.host_console_log[..] {
        [
            crate::overlays::ConsoleLine::Dispatch {
                targets, action, ..
            },
        ] => {
            assert_eq!(targets, &[1], "bare text routes to the selection (#1)");
            assert_eq!(*action, "prompt");
        }
        other => panic!("expected one dispatch line, got {other:?}"),
    }
}

#[tokio::test]
async fn console_bare_text_from_n_creates_instead() {
    // The `n`-opened prompt's default role is create: an explicit address
    // overrides it, but plain text must not silently prompt another
    // session.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    console_host_rows(&mut app);
    console_dispatch(&mut app, "refactor the retry loop", true).await;
    match &app.host_console_log[..] {
        [
            crate::overlays::ConsoleLine::Dispatch {
                targets, action, ..
            },
        ] => {
            assert!(targets.is_empty(), "create targets nobody");
            assert_eq!(*action, "new session");
        }
        other => panic!("expected one dispatch line, got {other:?}"),
    }
}

#[tokio::test]
async fn console_unknown_address_is_a_notice_not_a_dispatch() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    console_host_rows(&mut app);
    console_dispatch(&mut app, "@9 do the thing", false).await;
    match &app.host_console_log[..] {
        [crate::overlays::ConsoleLine::Notice(text)] => {
            assert!(text.contains("#9"), "notice names the address: {text}");
        }
        other => panic!("expected one notice, got {other:?}"),
    }
}

#[tokio::test]
async fn console_verb_without_selection_is_a_notice() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    console_host_rows(&mut app);
    app.host_sessions.clear();
    console_dispatch(&mut app, "/interrupt", false).await;
    match &app.host_console_log[..] {
        [crate::overlays::ConsoleLine::Notice(text)] => {
            assert!(text.contains("no session"), "notice explains: {text}");
        }
        other => panic!("expected one notice, got {other:?}"),
    }
}

#[tokio::test]
async fn console_help_lists_the_grammar() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    console_host_rows(&mut app);
    console_dispatch(&mut app, "/help", false).await;
    let text: Vec<String> = app
        .host_console_log
        .iter()
        .filter_map(|l| match l {
            crate::overlays::ConsoleLine::Notice(t) => Some(t.clone()),
            _ => None,
        })
        .collect();
    let joined = text.join("\n");
    for verb in ["/interrupt", "/suspend", "/kill", "/new", "@3 text"] {
        assert!(joined.contains(verb), "help must mention {verb}: {joined}");
    }
}

#[tokio::test]
async fn console_kill_key_arms_then_confirms() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    console_host_rows(&mut app);
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();
    crate::event_loop::host_test_shims::kill(&mut app, &runtime);
    // First press: armed, with a notice naming the target.
    assert!(app.host_kill_confirm.is_some(), "first k arms");
    assert!(matches!(
        app.host_console_log.last(),
        Some(crate::overlays::ConsoleLine::Notice(t)) if t.contains("#1")
    ));
    // Second press: confirmed — the arm clears and a kill dispatch logs.
    crate::event_loop::host_test_shims::kill(&mut app, &runtime);
    assert!(app.host_kill_confirm.is_none(), "second k fires");
    assert!(matches!(
        app.host_console_log.last(),
        Some(crate::overlays::ConsoleLine::Dispatch { action, .. }) if *action == "kill"
    ));
}

#[tokio::test]
async fn console_kill_arm_cancels_on_selection_move() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    console_host_rows(&mut app);
    let runtime = crate::event_loop::UiRuntime::minimal_for_test();
    crate::event_loop::host_test_shims::kill(&mut app, &runtime);
    assert!(app.host_kill_confirm.is_some());
    // Moving the dock selection (the ModalUp path) cancels the arm.
    crate::event_loop::host_test_shims::kill_cancel(&mut app);
    assert!(app.host_kill_confirm.is_none());
    // A `k` after the cancel arms afresh rather than firing.
    crate::event_loop::host_test_shims::kill(&mut app, &runtime);
    assert!(app.host_kill_confirm.is_some(), "re-arm, not fire");
    assert_eq!(app.host_console_log.len(), 2, "no kill dispatched yet");
}

#[test]
fn websearch_provider_dropdown_builds_and_selects() {
    let ws = muta_contracts::WebSearchConfigView {
        provider: "exa".to_string(),
        reader: "none".to_string(),
        proxy: None,
        timeout_secs: 20,
        searxng_url: None,
        exa_api_key_set: true,
        parallel_api_key_set: false,
        tavily_api_key_set: true,
        bocha_api_key_set: false,
        jina_api_key_set: false,
        search_connections: Vec::new(),
        reader_connections: Vec::new(),
    };
    let dropdown = crate::overlays::build_websearch_provider_dropdown("tavily", Some(&ws));
    assert_eq!(dropdown.context.as_deref(), Some("websearch_provider"));
    assert_eq!(dropdown.selected_payload().map(|s| s.as_str()), Some("tavily"));
    assert_eq!(dropdown.items.len(), 8);
}

#[test]
fn websearch_reader_dropdown_builds_and_selects() {
    let mut ws = muta_contracts::WebSearchConfigView {
        provider: "exa".to_string(),
        reader: "my-jina".to_string(),
        proxy: None,
        timeout_secs: 20,
        searxng_url: None,
        exa_api_key_set: false,
        parallel_api_key_set: false,
        tavily_api_key_set: false,
        bocha_api_key_set: false,
        jina_api_key_set: false,
        search_connections: Vec::new(),
        reader_connections: vec![muta_contracts::WebReaderConnection {
            id: "my-jina".to_string(),
            name: Some("Custom Jina".to_string()),
            preset_id: Some("jina".to_string()),
            api_key_env: None,
            base_url: None,
            custom_headers: None,
            enabled: true,
        }],
    };
    let dropdown = crate::overlays::build_websearch_reader_dropdown("my-jina", Some(&ws));
    assert_eq!(dropdown.context.as_deref(), Some("websearch_reader"));
    assert_eq!(dropdown.selected_payload().map(|s| s.as_str()), Some("my-jina"));
    assert_eq!(dropdown.items.len(), 3); // my-jina + disabled + add_new

    ws.reader_connections.clear();
    let empty_dropdown = crate::overlays::build_websearch_reader_dropdown("none", Some(&ws));
    assert_eq!(empty_dropdown.items.len(), 2); // disabled + add_new
}
