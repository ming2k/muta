use super::*;
use crate::markdown_table::{build_table_render, shrink_column_widths};
use crate::text_layout::wrap_text;
use unicode_width::UnicodeWidthStr;

    /// Smoke-render every redesigned component into a buffer to catch panics
    /// (border math, rect underflows, empty content) without a live terminal.
    #[test]
    fn redesigned_components_render_without_panicking() {
        let theme = Theme::default();
        let mut terminal = mutx_engine::TestTerminal::new(80, 30);

        terminal
            .draw(|f| {
                let mut layout_map = LayoutMap::new();
                let mut thinking = TranscriptMessage::thinking("Reasoning about the task step by step.");
                thinking.set_thinking_expanded(true);
                let mut tool = TranscriptMessage::tool_step("call_1", "list_dir", r#"{"path":"."}"#);
                tool.set_tool_step_expanded(true);
                tool.finish_tool_step("call_1", "file_a\nfile_b", muta_contracts::ToolOutput::text("file_a\nfile_b"), 12);
                let messages = vec![
                    TranscriptMessage::new(muta_contracts::Role::User, "hi"),
                    TranscriptMessage::new(
                        muta_contracts::Role::Assistant,
                        "Here is a table:\n\n| Tool | Count |\n| --- | ---: |\n| read | 1 |\n| webfetch | 250 |",
                    ),
                    thinking,
                    tool,
                ];
                let _ = draw_transcript(
                    f,
                    &mut layout_map,
                    TranscriptView {
                        messages: &messages,
                        scroll: 0,
                        selection: &SelectionState::None,
                        cell_selection: None,
                        activity: "waiting for model",
                        awaiting_permission: false,                        spinner_phase: 0,
                        input: "hello",
                        byte_cursor: 5,
                        chrome_hidden: false,
                        queue_bar: QueueBarView {
                            items: &[],
                            paused: false,
                            blocked: false,
                        },
                        runner_bar: None,
                        side_banner: None,
                        page_hints: None,
                    session_head: None,
                        todos: None,
                                        round_started_at: None,
                        hovered_step: None,
                        focused_target: None,
                        logo: None,
                        guidance: EmptyStateGuidance::Tour,
                        carousel_index: 0,
                        theme: &theme,
                        layout: crate::layout::Strategy::default(),
                        height_cache: None,
                    },
                );
                draw_composer(
                    f,
                    Rect::new(0, 21, 80, 3),
                    "hello",
                    5,
                    true,
                    true,
                    &theme,
                    &mut LayoutMap::new(),
                    true,
                    &mut 0,
                    &SelectionState::None,
                    0,
                    0,
                );
                draw_completion_menu(
                    f,
                    &mut layout_map,
                    &[
                        crate::completion::Completion {
                            label: "/new".to_string(),
                            description: "New".to_string(),
                            insert_text: "/new".to_string(),
                            replace_start: 0,
                            replace_end: 0,
                            kind: crate::completion::CompletionItemKind::Slash,
                            doc: None,
                        },
                    ],
                    Some(0),
                    Rect::new(0, 20, 80, 3),
                    2,
                    &theme,
                );
                draw_copy_toast(f, "copied to clipboard", false, &theme);
                draw_armed_toast(f, "press Ctrl+C again to exit", &theme);
            });

        // Modals + permission sheet on a fresh frame.
        terminal.draw(|f| {
            draw_connections_modal(
                f,
                &mut LayoutMap::new(),
                &[],
                "mock",
                0,
                "",
                0,
                &mut 0,
                true,
                false,
                false,
                &theme,
                &crate::model::selection::SelectionState::None,
            );
            draw_models_modal(
                f,
                &mut LayoutMap::new(),
                &[],
                "mock",
                "mock-model",
                0,
                "",
                0,
                &mut 0,
                true,
                false,
                false,
                &theme,
                &crate::model::selection::SelectionState::None,
            );
            let history_roster: Vec<muta_contracts::HistoryEntry> =
                [muta_contracts::HistoryEntry::new(
                    "a".to_string(),
                    None,
                    None,
                    0,
                )]
                .into_iter()
                .collect();
            let ranked: Vec<(usize, crate::fuzzy::FuzzyMatch)> = crate::fuzzy::rank(&["a"], "");
            let input_rect = mutx_engine::Rect::new(0, 20, 80, 3);
            let selection = crate::model::selection::SelectionState::None;
            let mut layout_map = crate::model::layout::LayoutMap::new();
            let _ = draw_history_panel(
                f,
                &history_roster,
                &ranked,
                0,
                &mut 0,
                true,
                false,
                false,
                input_rect,
                0,
                &theme,
                &selection,
                &mut layout_map,
            );
            draw_model_editor(f, "OpenAI", "", 0, true, 0, None, &[], None, None, &theme);
            // Provider-template chooser.
            let mut template_scroll = 0;
            draw_provider_template_chooser(0, f, &theme, &mut template_scroll);
            // Provider editor on the Model filter field.
            use crate::providers::CustomField;
            let mut scroll = 0;
            draw_custom_provider_editor(
                CustomEditorView {
                    fields: &[
                        CustomField::Name,
                        CustomField::BaseUrl,
                        CustomField::Token,
                        CustomField::Model,
                    ],
                    field: 3,
                    editing: false,
                    title: "Custom OpenAI",
                    name_buf: "My Relay",
                    base_url_buf: "https://relay/v1/chat/completions",
                    token_buf: "tok",
                    model_display: "GPT-4o",
                    url_hint: "https://relay.example.com/v1/chat/completions",
                    suggestions: &["GPT-4o".to_string(), "GPT-4o mini".to_string()],
                    suggest_index: 0,
                    input: "gpt",
                    cursor_position: 3,
                },
                f,
                &theme,
                &mut scroll,
            );
            {
                let mut scroll = 0;
                let bindings: &[HelpBinding] = &[];
                let selection = crate::model::selection::SelectionState::None;
                let mut layout_map = crate::model::layout::LayoutMap::new();
                draw_help_modal(
                    f,
                    &mut scroll,
                    bindings,
                    &theme,
                    &selection,
                    &mut layout_map,
                );
            }
            let selection = crate::model::selection::SelectionState::None;
            let mut layout_map = crate::model::layout::LayoutMap::new();
            draw_sessions_modal(
                f,
                &[
                    muta_contracts::SessionOverview {
                        id: "abc123".to_string(),
                        overview: "Refactor the renderer".to_string(),
                        created_at: 0,
                        updated_at: 0,
                        message_count: 12,
                        active: true,
                        parent_id: None,
                        fork_kind: muta_contracts::SessionForkKind::Trunk,
                    },
                    muta_contracts::SessionOverview {
                        id: "def456".to_string(),
                        overview: "Fix the tool_call_id bug".to_string(),
                        created_at: 0,
                        updated_at: 0,
                        message_count: 4,
                        active: false,
                        parent_id: None,
                        fork_kind: muta_contracts::SessionForkKind::Trunk,
                    },
                ],
                0,
                false,
                &mut scroll,
                true,
                &theme,
                false,
                0,
                false,
                None,
                &mut 0,
                &selection,
                &mut layout_map,
            );
            let question_request = UserQuestionRequest {
                id: "q1".to_string(),
                questions: vec![muta_contracts::UserQuestion {
                    header: Some("Style".to_string()),
                    question: "Which error handling crate?".to_string(),
                    options: vec![
                        muta_contracts::UserQuestionOption {
                            label: "anyhow (Recommended)".to_string(),
                            description: Some("Simple".to_string()),
                        },
                        muta_contracts::UserQuestionOption {
                            label: "thiserror".to_string(),
                            description: Some("Structured".to_string()),
                        },
                    ],
                    multi_select: false,
                }],
                origin: None,
            };
            let mut hit_map = crate::model::layout::ModalHitMap::new();
            draw_question_modal(
                f,
                &mut hit_map,
                &question_request,
                0,
                &[vec![1]],
                &[String::new()],
                1,
                &mut 0,
                true,
                &theme,
            );
        });

        terminal.draw(|f| {
            let request = PermissionRequest {
                id: "p1".to_string(),
                tool: "execute_command".to_string(),
                label: "execute_command".to_string(),
                description: "run a command".to_string(),
                arguments: r#"{"command":"ls"}"#.to_string(),
                scope: "*".to_string(),
                elevation: false,
                one_off: false,
                origin: None,
                ..Default::default()
            };
            let rect = mutx_engine::Rect::new(0, 0, 60, 3);
            let mut hit_map = crate::model::layout::ModalHitMap::new();
            let _ = draw_permission_sheet(
                f,
                &mut hit_map,
                &request,
                0,
                false,
                false,
                0,
                rect,
                &theme,
                &crate::model::selection::SelectionState::None,
                &mut crate::model::layout::LayoutMap::new(),
            );
        });
    }

    #[test]
    fn config_appearance_pages_render_at_minimum_terminal_size() {
        let theme = Theme::default();
        let custom = muta_contracts::ColorSchemeConfig::default();
        let mut terminal = mutx_engine::TestTerminal::new(80, 24);

        terminal.draw(|frame| {
            draw_config_view(
                frame,
                ConfigViewProps {
                    category_index: 0,
                    detail_index: 0,
                    focus: ConfigFocus::Categories,
                    color_scheme: "zen",
                    custom_color_scheme: &custom,
                    custom_color_draft: &custom,
                    custom_editing: false,
                    input: "",
                    cursor_position: 0,
                    transcript_layout: crate::layout::Strategy::TurnBand,
                    expand_auto_scroll: false,
                    click_outside_dismiss: true,
                    websearch: None,
                    websearch_editing: None,
                    workspace: "~/workspace",
                    category_scroll: &mut 0,
                    detail_scroll: &mut 0,
                    theme: &theme,
                },
            );
        });
        assert!(grid_row(&terminal, 0).contains("SETTINGS"));
        assert!(grid_row(&terminal, 0).contains("Appearance"));
        assert!(
            grid_row(&terminal, 1).trim().is_empty(),
            "Row 1 must be an empty spacer line"
        );
        assert!(
            !grid_row(&terminal, 2).contains("CATEGORIES"),
            "Panel title row must be removed"
        );

        terminal.draw(|frame| {
            draw_config_view(
                frame,
                ConfigViewProps {
                    category_index: 0,
                    detail_index: 5,
                    focus: ConfigFocus::Detail,
                    color_scheme: "custom",
                    custom_color_scheme: &custom,
                    custom_color_draft: &custom,
                    custom_editing: true,
                    input: "#8ea191",
                    cursor_position: 7,
                    transcript_layout: crate::layout::Strategy::TurnBand,
                    expand_auto_scroll: false,
                    click_outside_dismiss: true,
                    websearch: None,
                    websearch_editing: None,
                    workspace: "~/workspace",
                    category_scroll: &mut 0,
                    detail_scroll: &mut 0,
                    theme: &theme,
                },
            );
        });
        assert!(grid_row(&terminal, 0).contains("SETTINGS"));
    }

    /// Render both the compact Runner step (root view) and the zoomed-in
    /// Runner view with its page header, ensuring no layout panics.
    /// Visual verification (run with MUTA_VISUAL=1 --nocapture): an runner
    /// zoom view with two ReAct turns, each emitting a concurrent tool-call
    /// batch, groups into turn bands with flush same-turn calls and a blank
    /// line between turns — exactly like the main session.
    #[test]
    fn runner_view_groups_children_into_turn_bands() {
        let theme = Theme::default();
        let mut terminal = mutx_engine::TestTerminal::new(80, 30);
        let mut task = TranscriptMessage::tool_step(
            "task_1",
            "runner",
            r#"{"description":"explore the codebase","prompt":"..."}"#,
        );
        let call =
            |id: &str, name: &str, round: u64, turn: usize| muta_contracts::RunnerEvent::ToolCall {
                id: id.into(),
                name: name.into(),
                arguments: r#"{"p":"x"}"#.into(),
                round,
                turn,
            };
        let result = |id: &str, name: &str| muta_contracts::RunnerEvent::ToolResult {
            id: id.into(),
            name: name.into(),
            output: "done".into(),
            duration_ms: 5,
        };
        // Turn 1: a 3-call concurrent batch.
        for (id, name) in [("a", "read_text"), ("b", "search_text"), ("c", "list_dir")] {
            task.push_runner_event(&call(id, name, 1, 0));
            task.push_runner_event(&result(id, name));
        }
        // Turn 2: a 2-call concurrent batch.
        for (id, name) in [("d", "websearch"), ("e", "webfetch")] {
            task.push_runner_event(&call(id, name, 1, 1));
            task.push_runner_event(&result(id, name));
        }
        let children = task.runner_children().unwrap().to_vec();
        terminal.draw(|f| {
            let mut layout_map = LayoutMap::new();
            let _ = draw_transcript(
                f,
                &mut layout_map,
                TranscriptView {
                    messages: &children,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    runner_bar: Some(RunnerBarInfo {
                        role: Some("explore".to_string()),
                        label: "the codebase".to_string(),
                        index: 1,
                        total: 1,
                    }),
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            );
        });
        let width = terminal.buffer().area().width as usize;
        let rows: Vec<String> = (0..terminal.buffer().area().height as usize)
            .map(|row| {
                terminal.buffer().content[row * width..(row + 1) * width]
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect()
            })
            .collect();
        if std::env::var("MUTA_VISUAL").is_ok() {
            eprintln!("\n┌─ Runner zoom (turn-banded) ─");
            for r in &rows {
                eprintln!("│{r}");
            }
            eprintln!("└────\n");
        }
        // Two turn headers appear (turn 1 and turn 2 of the runner's round 1).
        let body = rows.join("\n");
        assert!(body.contains("turn 1"), "expected a `turn 1` band: {body}");
        assert!(body.contains("turn 2"), "expected a `turn 2` band: {body}");
        // Same-turn sibling calls are flush (no blank row between `read_text`
        // and `search_text` inside turn 1); the two turns are separated by a blank.
        let line_of = |needle: &str| {
            rows.iter()
                .position(|r| r.contains(needle))
                .unwrap_or_else(|| panic!("no row containing {needle}"))
        };
        let t1_first = line_of("Read");
        let t1_second = line_of("Search");
        assert_eq!(t1_second, t1_first + 1, "same-turn calls stay flush");
        // turn 2's header sits at least one blank row after turn 1's batch.
        let t2_header = line_of("turn 2");
        assert!(t2_header > t1_second + 1, "turns are separated");
    }

    #[test]
    fn runner_step_and_view_render_without_panicking() {
        let theme = Theme::default();
        let mut terminal = mutx_engine::TestTerminal::new(80, 30);

        // Root view: a completed runner task renders as a compact step.
        let mut task = TranscriptMessage::tool_step(
            "task_1",
            "runner",
            r#"{"description":"explore the codebase","prompt":"..."}"#,
        );
        task.push_runner_event(&muta_contracts::RunnerEvent::ToolCall {
            id: "inner".into(),
            name: "search_text".into(),
            arguments: r#"{"pattern":"foo"}"#.into(),
            round: 1,
            turn: 0,
        });
        task.finish_tool_step(
            "task_1",
            "found 3 matches",
            muta_contracts::ToolOutput::text("found 3 matches"),
            1200,
        );
        let root_messages = vec![
            TranscriptMessage::new(muta_contracts::Role::User, "explore please"),
            task,
        ];

        terminal.draw(|f| {
            let mut layout_map = LayoutMap::new();
            let _ = draw_transcript(
                f,
                &mut layout_map,
                TranscriptView {
                    messages: &root_messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "running runner",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    runner_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            );
        });

        // Zoomed-in Runner view: the task's children are the message stream
        // and the contextual header is shown on the first row.
        let children = root_messages[1].runner_children().unwrap().to_vec();
        terminal.draw(|f| {
            let mut layout_map = LayoutMap::new();
            let _ = draw_transcript(
                f,
                &mut layout_map,
                TranscriptView {
                    messages: &children,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    runner_bar: Some(RunnerBarInfo {
                        role: Some("explore".to_string()),
                        label: "the codebase".to_string(),
                        index: 1,
                        total: 2,
                    }),
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            );
        });

        let width = terminal.buffer().area().width as usize;
        let row_text = |row: usize| -> String {
            terminal.buffer().content[row * width..(row + 1) * width]
                .iter()
                .map(|cell| cell.symbol())
                .collect()
        };
        let head_row = row_text(0);
        // The symbol row is unchanged by the full-width band (the pads and the
        // old inset columns were all spaces); only the background differs —
        // the whole row now paints `body`, asserted in page_header's tests.
        assert_eq!(
            head_row,
            "   ENVOY [EXPLORE] the codebase                                         (1/2)   ",
            "Runner identity, role tag, title and sibling index on the head row"
        );
        // The permanent key legend occupies the last three terminal rows,
        // with the shortcuts on its middle row.
        let legend = row_text(28);
        assert!(
            legend.contains("Esc back") && legend.contains("[ prev") && legend.contains("] next"),
            "Runner shortcuts pinned on the footer's middle row: {legend:?}"
        );
        assert!(
            row_text(27).trim().is_empty() && row_text(29).trim().is_empty(),
            "The footer's top and bottom rows are blank padding"
        );
    }

    #[test]
    fn height_cache_skip_path_matches_full_layout() {
        // Stage 2 invariant: a warm height cache (which lets the transcript
        // pass *skip* re-wrapping off-screen messages) must produce byte-for-
        // byte the same frame — and the same total `content_lines` — as a cold
        // render that lays every message out in full. If the skip arithmetic
        // (`skip_rows` / `current_y` / `content_lines`) drifted, this fails.
        use crate::model::layout::LayoutMap;
        let theme = Theme::default();

        // A tall transcript: enough wrapped plain-text messages to overflow an
        // 80x24 viewport several times, so both skip branches are exercised —
        // messages scrolled above the viewport (fully_above) and messages below
        // its bottom (fully_below).
        let messages: Vec<TranscriptMessage> = (0..40)
            .map(|i| {
                TranscriptMessage::new(
                    muta_contracts::Role::Assistant,
                    format!(
                        "Message number {i} with enough words to wrap across a \
                         couple of lines in an eighty column terminal so the \
                         per-message heights are non-trivial and varied."
                    ),
                )
            })
            .collect();
        let (width, height, scroll) = (80u16, 24u16, 30u16);

        let dump = |cache: &mut HeightCache| -> (String, usize) {
            let mut terminal = mutx_engine::TestTerminal::new(width, height);
            let mut layout_map = LayoutMap::new();
            let mut content_lines = 0usize;
            terminal.draw(|f| {
                let r = draw_transcript(
                    f,
                    &mut layout_map,
                    TranscriptView {
                        messages: &messages,
                        scroll,
                        selection: &SelectionState::None,
                        cell_selection: None,
                        activity: "",
                        awaiting_permission: false,
                        spinner_phase: 0,
                        input: "",
                        byte_cursor: 0,
                        chrome_hidden: false,
                        queue_bar: QueueBarView {
                            items: &[],
                            paused: false,
                            blocked: false,
                        },
                        runner_bar: None,
                        side_banner: None,
                        page_hints: None,
                        session_head: None,
                        todos: None,
                        round_started_at: None,
                        hovered_step: None,
                        focused_target: None,
                        logo: None,
                        guidance: EmptyStateGuidance::Tour,
                        carousel_index: 0,
                        theme: &theme,
                        layout: crate::layout::Strategy::default(),
                        height_cache: Some(cache),
                    },
                );
                content_lines = r.content_lines;
            });
            let buf = terminal.buffer();
            let bw = buf.area().width as usize;
            let mut s = String::new();
            for y in 0..height as usize {
                for x in 0..width as usize {
                    s.push_str(buf.content[y * bw + x].symbol());
                }
                s.push('\n');
            }
            (s, content_lines)
        };

        let mut cache = HeightCache::default();
        // Cold: cache empty, every message laid out in full (and measured).
        let (cold_grid, cold_lines) = dump(&mut cache);
        // Warm: off-screen messages now take the skip path.
        let (warm_grid, warm_lines) = dump(&mut cache);

        assert_eq!(
            cold_lines, warm_lines,
            "content_lines must match between full and skip layout"
        );
        assert_eq!(
            cold_grid, warm_grid,
            "rendered frame must be identical between full and skip layout"
        );
        // The skip path must actually have been reachable (cache populated).
        assert!(cache.get(messages[0].id).is_some());
    }

    #[test]
    fn expanded_edit_diff_height_is_scroll_independent() {
        // Regression: the expanded edit-diff renderer must account every
        // logical row in `content_lines` even when the viewport clips the
        // body mid-hunk. An early return once the viewport filled made the
        // measured height depend on the scroll offset; the app loop derives
        // `max_scroll` from it, so the scroll position oscillated and the
        // frame flickered during the animation heartbeat.
        let theme = Theme::default();

        // A completed edit whose diff body is several times taller than the
        // viewport, so mid-range scroll offsets clip inside the hunk rows.
        let old: String = (1..=60).map(|i| format!("let v{i} = {i};\n")).collect();
        let new: String = (1..=60)
            .map(|i| format!("let v{i} = {};\n", i * 10))
            .collect();
        let mut m = TranscriptMessage::tool_step(
            "call_test",
            "edit_file",
            r#"{"path":"a.rs","old_string":"…","new_string":"…"}"#,
        );
        let structured = muta_contracts::ToolOutput::Patch {
            path: "a.rs".into(),
            op: muta_contracts::PatchOp::Edit,
            old,
            new,
            start_line: 0,
        };
        m.finish_tool_step("call_test", structured.to_text(), structured, 0);
        if let crate::model::document::MessageKind::ToolStep { expanded, .. } = &mut m.kind {
            *expanded = true;
        }
        let messages = vec![m];

        let (width, height) = (80u16, 24u16);
        let measure = |scroll: u16, cache: &mut HeightCache| -> usize {
            let mut terminal = mutx_engine::TestTerminal::new(width, height);
            let mut layout_map = LayoutMap::new();
            let mut lines = 0usize;
            terminal.draw(|f| {
                let r = draw_transcript(
                    f,
                    &mut layout_map,
                    TranscriptView {
                        messages: &messages,
                        scroll,
                        selection: &SelectionState::None,
                        cell_selection: None,
                        activity: "",
                        awaiting_permission: false,
                        spinner_phase: 0,
                        input: "",
                        byte_cursor: 0,
                        chrome_hidden: false,
                        queue_bar: QueueBarView {
                            items: &[],
                            paused: false,
                            blocked: false,
                        },
                        runner_bar: None,
                        side_banner: None,
                        page_hints: None,
                        session_head: None,
                        todos: None,
                        round_started_at: None,
                        hovered_step: None,
                        focused_target: None,
                        logo: None,
                        guidance: EmptyStateGuidance::Tour,
                        carousel_index: 0,
                        theme: &theme,
                        layout: crate::layout::Strategy::default(),
                        height_cache: Some(cache),
                    },
                );
                lines = r.content_lines;
            });
            lines
        };

        let mut cache = HeightCache::default();
        let at_top = measure(0, &mut cache);
        assert!(
            at_top > height as usize,
            "the diff must overflow the viewport for this test to mean anything"
        );
        // Every offset that clips into the diff body must report the same
        // total height, through both cold and warm height-cache paths.
        for scroll in [1u16, 7, 20, 40, 60] {
            assert_eq!(
                measure(scroll, &mut cache),
                at_top,
                "content_lines must not depend on the scroll offset (scroll = {scroll})"
            );
        }
        let mut fresh_cache = HeightCache::default();
        assert_eq!(
            measure(20, &mut fresh_cache),
            at_top,
            "a cold height cache must measure the same height as a warm one"
        );
    }

    #[test]
    fn completed_diff_cache_survives_height_invalidation_and_resize() {
        let mut cache = HeightCache::default();
        let first = cache.diff_cache.patch(42, "old", "new", 10);

        cache.clear();
        cache.prepare(120);

        let second = cache.diff_cache.patch(42, "old", "new", 10);
        assert!(
            std::sync::Arc::ptr_eq(&first, &second),
            "width-dependent height invalidation must retain semantic diff rows"
        );
    }

    #[test]
    fn virtual_index_selects_only_chunks_intersecting_the_viewport() {
        let messages = (0..4)
            .map(|i| TranscriptMessage::new(muta_contracts::Role::Assistant, format!("m{i}")))
            .collect::<Vec<_>>();
        let mut cache = HeightCache::default();
        cache.prepare(80);
        // Four-line bodies plus one boundary row owned by each following
        // message: chunks begin at 0, 4, 9, and 14.
        for message in &messages {
            cache.set(message.id, 4);
        }

        let window = cache
            .virtual_window(&messages, crate::layout::Strategy::TurnBand, 6, 3)
            .expect("all message heights are cached");
        assert_eq!(window.message_start, 1);
        assert_eq!(window.message_end, 2);
        assert_eq!(window.prefix_lines, 4);
        assert_eq!(window.skip_rows, 2);
        assert_eq!(window.total_lines, 19);
    }

    #[test]
    fn virtual_index_uses_segmented_same_turn_geometry() {
        let mut thinking = TranscriptMessage::thinking("reasoning").with_turn(3);
        thinking.set_thinking_duration(1);
        let first = TranscriptMessage::tool_step("a", "read_text", r#"{"path":"a"}"#).with_turn(3);
        let second = TranscriptMessage::tool_step("b", "read_text", r#"{"path":"b"}"#).with_turn(3);
        let messages = vec![thinking, first, second];
        let mut cache = HeightCache::default();
        cache.prepare(80);
        for message in &messages {
            cache.set(message.id, 2);
        }

        let window = cache
            .virtual_window(&messages, crate::layout::Strategy::TurnBand, 0, 20)
            .expect("all message heights are cached");
        assert_eq!(window.message_start, 0);
        assert_eq!(window.message_end, 3);
        assert_eq!(
            window.total_lines, 9,
            "header + header gap + thinking + segment gap + flush tool batch"
        );
    }

    #[test]
    fn line_selection_intersects_wrapped_lines() {
        use crate::model::layout::SemanticCursor;
        let sel = SelectionState::Range {
            anchor: SemanticCursor::new(0, 0, 2),
            head: SemanticCursor::new(0, 0, 8),
        };
        let range = block_selection_range(&sel, 0, 0);

        // Line covering bytes 0..5 ("hello"): selected from 2 to end.
        let first = WrappedLine {
            text: "hello".to_string(),
            start_byte: 0,
            end_byte: 5,
        };
        assert_eq!(line_selection(range, &first), Some((2, 5)));

        // Line covering bytes 5..10 ("world"): selected up to head char (8 → rel 3, inclusive → 4).
        let second = WrappedLine {
            text: "world".to_string(),
            start_byte: 5,
            end_byte: 10,
        };
        assert_eq!(line_selection(range, &second), Some((0, 4)));

        // A line after the selection has no overlap.
        let third = WrappedLine {
            text: "after".to_string(),
            start_byte: 10,
            end_byte: 15,
        };
        assert_eq!(line_selection(range, &third), None);
    }

    #[test]
    fn block_selection_covers_middle_blocks_fully() {
        use crate::model::layout::SemanticCursor;
        let sel = SelectionState::Range {
            anchor: SemanticCursor::new(0, 0, 3),
            head: SemanticCursor::new(0, 2, 1),
        };
        assert_eq!(block_selection_range(&sel, 0, 0), Some((3, None)));
        assert_eq!(block_selection_range(&sel, 0, 1), Some((0, None)));
        assert_eq!(block_selection_range(&sel, 0, 2), Some((0, Some(1))));
        assert_eq!(block_selection_range(&sel, 0, 3), None);
        assert_eq!(block_selection_range(&sel, 1, 0), None);
    }

    #[test]
    fn test_wrap_text() {
        let lines = wrap_text("hello world", 5);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].text, "hello");
        assert_eq!(lines[1].text, " worl");
        assert_eq!(lines[2].text, "d");
    }

    #[test]
    fn test_wrap_with_newlines() {
        let lines = wrap_text("hi\nthere", 10);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "hi");
        assert_eq!(lines[1].text, "there");
    }

    #[test]
    fn wrap_avoids_cjk_punctuation_at_line_start() {
        let lines = wrap_text("人生需要坚持，才能前进。", 12);
        assert!(lines.len() > 1);
        assert!(lines.iter().skip(1).all(|line| {
            line.text
                .chars()
                .next()
                .is_none_or(|ch| !prohibited_line_start(ch))
        }));
        assert!(lines.iter().all(|line| {
            line.text
                .chars()
                .last()
                .is_none_or(|ch| !prohibited_line_end(ch))
        }));
    }

    /// The input box must reserve only a single content row for a short input
    /// but grow to fit wrapped text when the input is long.
    #[test]
    fn input_box_grows_with_wrapped_content() {
        let theme = Theme::default();
        let messages: Vec<TranscriptMessage> = Vec::new();

        fn render_with(theme: &Theme, messages: &[TranscriptMessage], input: &str) -> Rect {
            let mut terminal = mutx_engine::TestTerminal::new(40, 24);
            let mut rect = Rect::default();
            terminal.draw(|f| {
                let mut layout_map = LayoutMap::new();
                let r = draw_transcript(
                    f,
                    &mut layout_map,
                    TranscriptView {
                        messages,
                        scroll: 0,
                        selection: &SelectionState::None,
                        cell_selection: None,
                        activity: "",
                        awaiting_permission: false,
                        spinner_phase: 0,
                        input,
                        byte_cursor: input.len(),
                        chrome_hidden: false,
                        queue_bar: QueueBarView {
                            items: &[],
                            paused: false,
                            blocked: false,
                        },
                        runner_bar: None,
                        side_banner: None,
                        page_hints: None,
                        session_head: None,
                        todos: None,
                        round_started_at: None,
                        hovered_step: None,
                        focused_target: None,
                        logo: None,
                        guidance: EmptyStateGuidance::Tour,
                        carousel_index: 0,
                        theme,
                        layout: crate::layout::Strategy::default(),
                        height_cache: None,
                    },
                );
                rect = r.input_rect;
            });
            rect
        }

        // Short input: one content line + two padding rows = 3.
        let short = render_with(&theme, &messages, "hi");
        assert_eq!(short.height, 3);

        // Long input wraps across many lines on a 40-wide terminal; the box
        // must grow beyond the single-line baseline.
        let long_input = "word ".repeat(40);
        let tall = render_with(&theme, &messages, &long_input);
        assert!(
            tall.height > 3,
            "wrapped input should grow the box, got height {}",
            tall.height
        );
        // ...but never more than half the terminal.
        assert!(tall.height <= 12);
    }

    #[test]
    fn footer_keeps_one_blank_row_below_transcript_when_active_or_idle() {
        fn assert_gap(activity: &str) {
            let theme = Theme::default();
            let messages = vec![TranscriptMessage::new(
                muta_contracts::Role::Assistant,
                "A finished response above the footer.",
            )];
            let mut terminal = mutx_engine::TestTerminal::new(60, 20);
            let mut footer_anchor_y = 0;
            let mut transcript_height = 0;

            terminal.draw(|frame| {
                let mut layout_map = LayoutMap::new();
                let rendered = draw_transcript(
                    frame,
                    &mut layout_map,
                    TranscriptView {
                        messages: &messages,
                        scroll: 0,
                        selection: &SelectionState::None,
                        cell_selection: None,
                        activity,
                        awaiting_permission: false,
                        spinner_phase: 0,
                        input: "",
                        byte_cursor: 0,
                        chrome_hidden: false,
                        queue_bar: QueueBarView {
                            items: &[],
                            paused: false,
                            blocked: false,
                        },
                        runner_bar: None,
                        side_banner: None,
                        page_hints: None,
                        session_head: None,
                        todos: None,
                        round_started_at: None,
                        hovered_step: None,
                        focused_target: None,
                        logo: None,
                        guidance: EmptyStateGuidance::Tour,
                        carousel_index: 0,
                        theme: &theme,
                        layout: crate::layout::Strategy::default(),
                        height_cache: None,
                    },
                );
                footer_anchor_y = footer_stack::rect_of(&rendered.footer, FooterRowId::Activity)
                    .map(|rect| rect.y)
                    .unwrap_or(rendered.input_rect.y);
                transcript_height = rendered.view_height;
            });

            // The footer always begins after a permanent one-row gap below the
            // transcript. The queue bar in this fixture is empty, so it is
            // hidden; the anchor is whichever region leads the footer — the
            // activity bar when responding, the input box when idle — both of
            // which sit directly under the gap.
            let expected_anchor = 1 + transcript_height + FOOTER_TOP_GAP_ROWS;
            assert_eq!(footer_anchor_y, expected_anchor);
            // The permanent one-row gap sits directly below the transcript,
            // above whichever footer region leads (activity bar when
            // responding, queue bar when idle).
            let separator_y = 1 + transcript_height;
            let width = terminal.buffer().area().width as usize;
            let row_start = separator_y as usize * width;
            let separator = &terminal.buffer().content[row_start..row_start + width];
            assert!(
                separator.iter().all(|cell| cell.symbol() == " "),
                "separator row must stay blank while activity={activity:?}"
            );
        }

        assert_gap("responding");
        assert_gap("idle");
    }

    /// The declarative footer stack must place every row exactly where the
    /// old hand-rolled offset arithmetic did. This test keeps the legacy
    /// formula as an oracle: with a full chrome (todo + queue + activity +
    /// composer + hint all visible) each bar's rect must equal the
    /// `status_y + Σ(prior heights)` it replaced, so the refactor is provably
    /// behavior-preserving.
    #[test]
    fn footer_stack_places_rows_where_the_legacy_offsets_did() {
        let theme = Theme::default();
        let messages = vec![TranscriptMessage::new(muta_contracts::Role::User, "hello")];
        let todos = muta_contracts::TodoList {
            items: vec![muta_contracts::TodoItem {
                id: muta_contracts::TodoId(1),
                content: "one".into(),
                status: muta_contracts::TodoStatus::InProgress,
                created_at: 0,
                updated_at: 0,
            }],
            next_id: 2,
            updated_at_round: 0,
        };
        let queue_items = [crate::chrome::QueueItemView {
            queued_at_ms: 1_700_000_000_000,
            text: "next".into(),
            steering: false,
        }];

        let mut terminal = mutx_engine::TestTerminal::new(80, 30);
        let mut render_opt: Option<TranscriptRender> = None;
        terminal.draw(|f| {
            render_opt = Some(draw_transcript(
                f,
                &mut LayoutMap::new(),
                TranscriptView {
                    messages: &messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "responding",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: crate::chrome::QueueBarView {
                        items: &queue_items,
                        paused: false,
                        blocked: false,
                    },
                    runner_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: Some(&todos),
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            ));
        });
        let rendered = render_opt.expect("render result");

        // Legacy oracle, verbatim from the pre-stack code: footer_x/w from the
        // shared inset, status_y after the top gap, then each row's y is the
        // cumulative sum of the rows above it.
        let footer_h = crate::design::FOOTER_TOP_GAP_ROWS
            + crate::design::TODO_BAR_ROWS
            + crate::design::QUEUE_BAR_ROWS
            + crate::design::ACTIVITY_BAR_ROWS
            + rendered.input_rect.height // composer
            + crate::design::HINT_BAR_ROWS;
        // The terminal is 30 rows; the head is absent here, so the footer
        // band starts at 30 - footer_h.
        let band_y = 30 - footer_h;
        let footer_x = crate::design::FOOTER_H_INSET;
        let footer_w = 80 - 2 * crate::design::FOOTER_H_INSET;
        let status_y = band_y + crate::design::FOOTER_TOP_GAP_ROWS;

        let expect = |y: u16, h: u16| mutx_engine::Rect::new(footer_x, y, footer_w, h);
        assert_eq!(
            footer_stack::rect_of(&rendered.footer, FooterRowId::Todos),
            Some(expect(status_y, TODO_BAR_ROWS)),
            "todos bar rect"
        );
        assert_eq!(
            footer_stack::rect_of(&rendered.footer, FooterRowId::Queue),
            Some(expect(status_y + TODO_BAR_ROWS, QUEUE_BAR_ROWS)),
            "queue bar rect"
        );
        assert_eq!(
            footer_stack::rect_of(&rendered.footer, FooterRowId::Activity),
            Some(expect(
                status_y + TODO_BAR_ROWS + QUEUE_BAR_ROWS,
                ACTIVITY_BAR_ROWS
            )),
            "activity bar rect"
        );
        assert_eq!(
            Some(rendered.input_rect),
            footer_stack::rect_of(&rendered.footer, FooterRowId::Composer),
            "composer rect appears in the registry exactly as returned"
        );
        assert_eq!(
            rendered.input_rect,
            expect(
                status_y + TODO_BAR_ROWS + QUEUE_BAR_ROWS + ACTIVITY_BAR_ROWS,
                rendered.input_rect.height
            ),
            "composer rect matches the legacy offset"
        );
        assert_eq!(
            Some(rendered.hint_rect),
            footer_stack::rect_of(&rendered.footer, FooterRowId::Hint),
            "hint bar rect appears in the registry exactly as returned"
        );
        assert_eq!(
            rendered.hint_rect,
            expect(
                status_y
                    + TODO_BAR_ROWS
                    + QUEUE_BAR_ROWS
                    + ACTIVITY_BAR_ROWS
                    + rendered.input_rect.height,
                HINT_BAR_ROWS
            ),
            "hint bar rect matches the legacy offset"
        );
        // The registry is complete: gap + the five interactive rows.
        assert_eq!(rendered.footer.rows.len(), 6, "registry completeness");
        assert_eq!(
            footer_stack::rect_of(&rendered.footer, FooterRowId::TopGap),
            Some(expect(band_y, crate::design::FOOTER_TOP_GAP_ROWS)),
            "the top gap is part of the stack's geometry"
        );
    }

    /// When the terminal is resized below the usable minimum,
    /// `draw_transcript` must not render the normal UI (which would underflow
    /// the footer layout math). Instead it hides everything, shows a centered
    /// "terminal too small" notice, and returns a zeroed `TranscriptRender` so
    /// the app loop draws no chrome over it.
    #[test]
    fn too_small_terminal_shows_notice_and_zeroed_render() {
        let theme = Theme::default();
        let messages = vec![TranscriptMessage::new(muta_contracts::Role::User, "hello")];

        let mut terminal = mutx_engine::TestTerminal::new(20, 8);
        let mut render_opt: Option<TranscriptRender> = None;
        terminal.draw(|f| {
            render_opt = Some(draw_transcript(
                f,
                &mut LayoutMap::new(),
                TranscriptView {
                    messages: &messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    runner_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            ));
        });

        let render = render_opt.expect("draw_transcript must return a render");
        // The guard suppresses all chrome geometry.
        assert_eq!(render.input_rect, Rect::default());
        assert_eq!(render.hint_rect, Rect::default());
        assert_eq!(render.view_height, 0);
        assert_eq!(render.content_lines, 0);

        // The notice text must be present somewhere in the rendered buffer.
        let buffer = terminal.buffer();
        let rendered: String = (0..buffer.area().height)
            .flat_map(|y| {
                (0..buffer.area().width).map(move |x| buffer[(x, y)].symbol().to_string())
            })
            .collect::<String>();
        assert!(
            rendered.contains("Terminal too small"),
            "expected the too-small notice in the rendered buffer"
        );
    }

    /// An empty composer must still record a layout-map region for its single
    /// text row. Without it a click inside the empty box can't resolve to a
    /// cursor, so the click handler can't clear a focused step to hand typing
    /// back to the prompt. See `draw_composer` / `composer_wrapped`.
    #[test]
    fn draw_composer_records_region_for_empty_input() {
        let theme = Theme::default();
        let mut terminal = mutx_engine::TestTerminal::new(30, 5);
        let mut layout_map = LayoutMap::new();
        let input_rect = Rect::new(0, 0, 30, 3);
        terminal.draw(|f| {
            draw_composer(
                f,
                input_rect,
                "",
                0,
                true,
                true,
                &theme,
                &mut layout_map,
                true,
                &mut 0,
                &SelectionState::None,
                0,
                0,
            );
        });

        // The empty text row sits one line below the box's top edge.
        let cursor = layout_map
            .cursor_at(
                input_rect.x + COMPOSER_PROMPT_PREFIX_COLS as u16,
                input_rect.y + 1,
            )
            .expect("click inside empty input box must resolve to a cursor");
        assert_eq!(cursor.message_idx, INPUT_MSG_IDX);
        assert_eq!(cursor.byte_offset, 0);
    }

    /// `draw_composer` must not panic for tricky inputs and should place the caret
    /// on the second wrapped line when the cursor sits past the first wrap.
    #[test]
    fn draw_composer_wraps_and_positions_caret() {
        let theme = Theme::default();
        let mut terminal = mutx_engine::TestTerminal::new(20, 12);
        // "aaaa bbbb cccc" wraps within the ~17-wide inner area; cursor at the
        // very end should be on a later line, not off the box.
        let input = "aaaa bbbb cccc dddd eeee";
        terminal.draw(|f| {
            draw_composer(
                f,
                Rect::new(0, 0, 20, 8),
                input,
                input.len(),
                true,
                true,
                &theme,
                &mut LayoutMap::new(),
                true,
                &mut 0,
                &SelectionState::None,
                0,
                0,
            );
        });
    }

    /// The caret must land flush against the final glyph at the end of the
    /// input, measured in display columns — i.e. exactly where the grid painted
    /// the text. This is the CJK regression: a buggy grapheme-floor returned the
    /// last grapheme *start*, leaving the caret two columns short of a wide
    /// glyph (one for ASCII). The caret column must equal the rendered width of
    /// the text, for both wide and narrow glyphs.
    #[test]
    fn draw_composer_caret_flush_against_final_grapheme() {
        let theme = Theme::default();

        for (label, input, expected_cols) in [
            ("cjk", "中文", 4usize),
            ("ascii", "ab", 2),
            ("mixed", "a中", 3),
        ] {
            let mut terminal = mutx_engine::TestTerminal::new(20, 5);
            terminal.draw(|f| {
                draw_composer(
                    f,
                    Rect::new(0, 0, 20, 4),
                    input,
                    input.len(),
                    true,
                    true,
                    &theme,
                    &mut LayoutMap::new(),
                    false,
                    &mut 0,
                    &SelectionState::None,
                    0,
                    0,
                );
            });
            let cursor = match terminal.cursor() {
                mutx_engine::CursorState::Visible(x, y) => (x, y),
                other => panic!("{label}: caret should be visible, got {other:?}"),
            };
            // The text row sits one line below the box's top padding row, and
            // the caret follows the `› ` prefix plus the full rendered width.
            assert_eq!(
                cursor,
                (
                    (COMPOSER_PROMPT_PREFIX_COLS + expected_cols) as u16,
                    crate::design::COMPOSER_TEXT_ROW_OFFSET,
                ),
                "{label}: caret not flush with end of {input:?}"
            );
        }
    }

    /// A resolved `/command` token renders in bold + the theme accent color,
    /// and the accent stops at the token boundary — the argument tail keeps
    /// the normal text color so the two read as command + payload.
    #[test]
    fn draw_composer_highlighted_accents_only_the_command_token() {
        let theme = Theme::default();
        let mut terminal = mutx_engine::TestTerminal::new(30, 4);
        let input = "/repeat every minute";
        terminal.draw(|f| {
            draw_composer_highlighted(
                f,
                Rect::new(0, 0, 30, 3),
                input,
                input.len(),
                true,
                true,
                &theme,
                &mut LayoutMap::new(),
                false,
                &mut 0,
                &SelectionState::None,
                "/repeat".len(),
                0,
                0,
            );
        });
        let buf = terminal.buffer();
        let text_y = crate::design::COMPOSER_TEXT_ROW_OFFSET;
        let text_x = COMPOSER_PROMPT_PREFIX_COLS as u16;
        // Every glyph of `/repeat` is bold + brand-colored on the panel bg.
        for (i, ch) in "/repeat".chars().enumerate() {
            let cell = buf.get(text_x + i as u16, text_y).expect("command cell");
            assert_eq!(cell.symbol(), ch.to_string());
            assert_eq!(cell.fg, theme.brand(), "command glyph {ch} lost the accent");
            assert!(
                cell.style.add.contains(mutx_engine::Modifier::BOLD),
                "command glyph {ch} lost bold"
            );
        }
        // The argument tail (`every minute`) keeps the default text color.
        let arg_start = text_x + "/repeat ".len() as u16;
        let cell = buf.get(arg_start, text_y).expect("argument cell");
        assert_eq!(cell.symbol(), "e");
        assert_eq!(cell.fg, theme.fg(), "argument text must not be accented");
    }

    /// The accent must not bleed past the first wrapped row: when the command
    /// token itself fits but the highlight length would cover the wrap
    /// boundary, the continuation row renders in the normal text color.
    #[test]
    fn draw_composer_highlight_clamps_at_wrap_boundary() {
        let theme = Theme::default();
        let mut terminal = mutx_engine::TestTerminal::new(13, 6);
        // 10-column text area (13 - 2 prefix - 2 right pad + 1): `/sessions`
        // fills row 0 exactly; ` abc` wraps to row 1.
        let input = "/sessions abc";
        terminal.draw(|f| {
            draw_composer_highlighted(
                f,
                Rect::new(0, 0, 13, 5),
                input,
                input.len(),
                true,
                true,
                &theme,
                &mut LayoutMap::new(),
                false,
                &mut 0,
                &SelectionState::None,
                "/sessions".len(),
                0,
                0,
            );
        });
        let buf = terminal.buffer();
        let row1_y = crate::design::COMPOSER_TEXT_ROW_OFFSET + 1;
        // The continuation row keeps the two-column prompt indent before the
        // wrapped text (`/sessions` fills row 0 exactly).
        let cell = buf
            .get(COMPOSER_PROMPT_PREFIX_COLS as u16 + 1, row1_y)
            .expect("continuation cell");
        assert_eq!(cell.symbol(), "a", "continuation row should start with 'a'");
        assert_eq!(
            cell.fg,
            theme.fg(),
            "accent must not bleed onto the wrapped argument row"
        );
    }

    /// Attachment chips render as distinct colored "pills": a pasted-text
    /// chip in the calm text-block blue and an image chip in the warm amber,
    /// each bold on a tinted band, while the surrounding prose keeps the
    /// normal text color. The color is the identifier's second channel —
    /// kind at a glance, payload size in the label.
    #[test]
    fn draw_composer_paints_paste_and_image_chips_distinctly() {
        let theme = Theme::default();
        let paste_chip = crate::composer_attachments::paste_chip(1, 3, 2048);
        let image_chip = crate::composer_attachments::image_chip(1, 1536);
        let input = format!("see {paste_chip} plus {image_chip} end");
        let mut terminal = mutx_engine::TestTerminal::new(120, 5);
        terminal.draw(|f| {
            draw_composer(
                f,
                Rect::new(0, 0, 120, 3),
                &input,
                input.len(),
                true,
                true,
                &theme,
                &mut LayoutMap::new(),
                false,
                &mut 0,
                &SelectionState::None,
                1,
                1,
            );
        });
        let buf = terminal.buffer();
        let text_y = crate::design::COMPOSER_TEXT_ROW_OFFSET;
        let text_x = COMPOSER_PROMPT_PREFIX_COLS as u16;
        let panel_bg = theme.input_surface();

        // Chip labels use ASCII metadata; display columns
        // come from `str_len`, never from the raw byte length.
        let paste_width = mutx_engine::text::str_len(&paste_chip);
        let paste_start = text_x + "see ".len() as u16;
        let paste_end = paste_start + paste_width as u16;
        for col in paste_start..paste_end {
            let cell = buf.get(col, text_y).expect("paste chip cell");
            assert_eq!(
                cell.fg,
                theme.chip_paste_fg(),
                "paste chip glyph lost its blue"
            );
            assert_eq!(
                cell.bg,
                theme.chip_paste_bg(panel_bg),
                "paste chip lost its pill band"
            );
            assert!(
                cell.style.add.contains(mutx_engine::Modifier::BOLD),
                "paste chip glyph lost bold"
            );
        }

        let image_width = mutx_engine::text::str_len(&image_chip);
        let image_start = text_x + ("see ".len() + paste_width + " plus ".len()) as u16;
        let image_end = image_start + image_width as u16;
        for col in image_start..image_end {
            let cell = buf.get(col, text_y).expect("image chip cell");
            assert_eq!(
                cell.fg,
                theme.chip_image_fg(),
                "image chip glyph lost its amber"
            );
            assert_eq!(
                cell.bg,
                theme.chip_image_bg(panel_bg),
                "image chip lost its pill band"
            );
            assert!(
                cell.style.add.contains(mutx_engine::Modifier::BOLD),
                "image chip glyph lost bold"
            );
        }

        // The prose around the chips keeps the normal text color on the panel.
        for col in [
            text_x,
            text_x + 2,
            text_x + ("see ".len() + paste_width) as u16,
        ] {
            let cell = buf.get(col, text_y).expect("prose cell");
            assert_eq!(cell.fg, theme.fg(), "prose next to a chip must stay plain");
            assert_eq!(cell.bg, panel_bg, "prose must not pick up a chip band");
        }
    }

    /// A chip label with **no staged payload** — typed by hand, or left over
    /// after the paste was undone — must render as ordinary text, never as a
    /// colored pill. The color marks a real attachment; a literal
    /// `[Image #1]` that the user merely typed must not pretend one exists.
    #[test]
    fn draw_composer_leaves_orphan_chip_labels_as_plain_text() {
        let theme = Theme::default();
        // No payload staged at all: `image_count = 0`, `paste_count = 0`.
        let orphan_image = "[Image #1]".to_string();
        let orphan_paste = "[Pasted text #1 +5 lines]".to_string();
        let input = format!("typed {orphan_image} and {orphan_paste} here");
        let mut terminal = mutx_engine::TestTerminal::new(100, 5);
        terminal.draw(|f| {
            draw_composer(
                f,
                Rect::new(0, 0, 100, 3),
                &input,
                input.len(),
                true,
                true,
                &theme,
                &mut LayoutMap::new(),
                false,
                &mut 0,
                &SelectionState::None,
                0,
                0,
            );
        });
        let buf = terminal.buffer();
        let text_y = crate::design::COMPOSER_TEXT_ROW_OFFSET;
        let text_x = COMPOSER_PROMPT_PREFIX_COLS as u16;
        let panel_bg = theme.input_surface();

        // Every glyph of both orphan labels keeps the plain text color on the
        // plain panel background — no pill band, no kind color, no bold.
        for (offset, label) in [
            ("typed ".len(), &orphan_image),
            ("typed [Image #1] and ".len(), &orphan_paste),
        ] {
            let start = text_x + offset as u16;
            let end = start + mutx_engine::text::str_len(label) as u16;
            for col in start..end {
                let cell = buf.get(col, text_y).expect("orphan chip cell");
                assert_eq!(
                    cell.fg,
                    theme.fg(),
                    "orphan label {label:?} must keep plain text color at col {col}"
                );
                assert_eq!(
                    cell.bg, panel_bg,
                    "orphan label {label:?} must not get a pill band at col {col}"
                );
                assert!(
                    !cell.style.add.contains(mutx_engine::Modifier::BOLD),
                    "orphan label {label:?} must not be bold at col {col}"
                );
            }
        }
    }

    /// A real chip (payload staged) is colored while an orphan label typed
    /// next to it stays plain — the pill reflects the actual staged state of
    /// each block, so one never masks the other.
    #[test]
    fn draw_composer_colors_only_backed_chips_when_mixed() {
        let theme = Theme::default();
        let real_paste = crate::composer_attachments::paste_chip(1, 3, 2048);
        let orphan_image = "[Image #1]".to_string();
        // One paste payload staged; the image chip is a typed orphan.
        let input = format!("{real_paste} then {orphan_image} end");
        let mut terminal = mutx_engine::TestTerminal::new(100, 5);
        terminal.draw(|f| {
            draw_composer(
                f,
                Rect::new(0, 0, 100, 3),
                &input,
                input.len(),
                true,
                true,
                &theme,
                &mut LayoutMap::new(),
                false,
                &mut 0,
                &SelectionState::None,
                0, // image_count: no image payload staged
                1, // paste_count: one paste payload staged
            );
        });
        let buf = terminal.buffer();
        let text_y = crate::design::COMPOSER_TEXT_ROW_OFFSET;
        let text_x = COMPOSER_PROMPT_PREFIX_COLS as u16;
        let panel_bg = theme.input_surface();

        // The backed paste chip gets the blue pill.
        let paste_width = mutx_engine::text::str_len(&real_paste);
        let paste_end = text_x + paste_width as u16;
        for col in text_x..paste_end {
            let cell = buf.get(col, text_y).expect("backed paste cell");
            assert_eq!(
                cell.fg,
                theme.chip_paste_fg(),
                "backed paste chip lost its blue"
            );
            assert_eq!(
                cell.bg,
                theme.chip_paste_bg(panel_bg),
                "backed paste chip lost its band"
            );
        }

        // The orphan image label stays plain text.
        let orphan_start = text_x + ("".len() + paste_width + " then ".len()) as u16;
        let orphan_end = orphan_start + mutx_engine::text::str_len(&orphan_image) as u16;
        for col in orphan_start..orphan_end {
            let cell = buf.get(col, text_y).expect("orphan image cell");
            assert_eq!(
                cell.fg,
                theme.fg(),
                "orphan image label must stay plain text"
            );
            assert_eq!(
                cell.bg, panel_bg,
                "orphan image label must not get a pill band"
            );
        }
    }

    /// Selecting a chip keeps its identity color (so the user can still tell
    /// which pasted block is selected) but the selection wins on background —
    /// the highlighted slice stays a uniform `selected_bg`.
    #[test]
    fn draw_composer_chip_keeps_identity_color_under_selection() {
        let theme = Theme::default();
        let paste_chip = crate::composer_attachments::paste_chip(1, 3, 2048);
        let input = format!("see {paste_chip} end");
        let mut terminal = mutx_engine::TestTerminal::new(80, 5);
        // Select exactly the chip bytes (absolute offsets into `input`).
        let sel_lo = "see ".len();
        let sel_hi = sel_lo + paste_chip.len();
        use crate::model::layout::SemanticCursor;
        let selection = SelectionState::Range {
            anchor: SemanticCursor::new(crate::composer::INPUT_MSG_IDX, 0, sel_lo),
            head: SemanticCursor::new(crate::composer::INPUT_MSG_IDX, 0, sel_hi),
        };
        terminal.draw(|f| {
            draw_composer(
                f,
                Rect::new(0, 0, 80, 3),
                &input,
                input.len(),
                true,
                false,
                &theme,
                &mut LayoutMap::new(),
                false,
                &mut 0,
                &selection,
                0,
                1,
            );
        });
        let buf = terminal.buffer();
        let text_y = crate::design::COMPOSER_TEXT_ROW_OFFSET;
        let text_x = COMPOSER_PROMPT_PREFIX_COLS as u16;
        let chip_start = text_x + sel_lo as u16;
        let chip_end = chip_start + mutx_engine::text::str_len(&paste_chip) as u16;
        for col in chip_start..chip_end {
            let cell = buf.get(col, text_y).expect("selected chip cell");
            assert_eq!(
                cell.fg,
                theme.chip_paste_fg(),
                "selected chip must keep its identity color"
            );
            assert_eq!(
                cell.bg,
                theme.selected(),
                "selection must win the background"
            );
        }
    }

    /// A chip split across a wrap boundary paints both fragments with the
    /// same pill, so a pasted block stays visually contiguous as it wraps
    /// inside the input box.
    #[test]
    fn draw_composer_chip_pill_continues_across_wrap() {
        let theme = Theme::default();
        let image_chip = crate::composer_attachments::image_chip(1, 1536);
        // Narrow text area (16 - 2 prefix - 2 pad = 12 cols) forces the
        // `[Image #1 (1.5 KB)]` label onto its own wrapped fragment.
        let input = format!("xx {image_chip} yy");
        let mut terminal = mutx_engine::TestTerminal::new(16, 6);
        terminal.draw(|f| {
            draw_composer(
                f,
                Rect::new(0, 0, 16, 5),
                &input,
                input.len(),
                true,
                true,
                &theme,
                &mut LayoutMap::new(),
                false,
                &mut 0,
                &SelectionState::None,
                1,
                0,
            );
        });
        let buf = terminal.buffer();
        let panel_bg = theme.input_surface();
        // Scan every rendered row: every glyph that belongs to the chip label
        // (ignoring spaces, which also appear in the prompt indent and the
        // panel padding) must carry the chip band, proving the pill survives
        // the wrap instead of reverting to plain text on the continuation row.
        let chip_glyphs: Vec<char> = image_chip.chars().filter(|c| *c != ' ').collect();
        for row in 0..5u16 {
            for col in 0..16u16 {
                let cell = buf.get(col, row).expect("row cell");
                if chip_glyphs.contains(&cell.symbol().chars().next().unwrap_or('\0')) {
                    assert_eq!(
                        cell.bg,
                        theme.chip_image_bg(panel_bg),
                        "wrapped chip fragment at ({col},{row}) lost its band"
                    );
                    assert_eq!(
                        cell.fg,
                        theme.chip_image_fg(),
                        "wrapped chip fragment at ({col},{row}) lost its amber"
                    );
                }
            }
        }
    }

    /// The composer must forward its resolved cursor coordinate unchanged to
    /// the frame for ASCII, CJK, empty, and wrapped inputs.
    #[test]
    fn cursor_screen_pos_matches_drawn_caret() {
        use super::composer::cursor_screen_pos;

        let theme = Theme::default();
        // Composer rect must fit inside the test terminal (24×8): a 4-row box
        // at y=0..4, x=0..20.
        let rect = Rect::new(0, 0, 20, 4);

        // (label, input, byte cursor) spanning ASCII, CJK (wide), mid-string,
        // empty, and a cursor that rests past the last wrapped line.
        let cases: &[(&str, &str, usize)] = &[
            ("ascii end", "hello", 5),
            ("ascii mid", "hello", 2),
            ("empty", "", 0),
            ("cjk end", "中文测试", 12),
            ("cjk mid", "中文测试", 6),
            ("mixed", "a中b文", 5),
            ("past wrap", "aaaa bbbb cccc dd", 16),
        ];

        for (label, input, byte_cursor) in cases {
            let byte_cursor = *byte_cursor;
            // What the draw path places.
            let mut terminal = mutx_engine::TestTerminal::new(24, 8);
            terminal.draw(|f| {
                draw_composer(
                    f,
                    rect,
                    input,
                    byte_cursor,
                    true,
                    true,
                    &theme,
                    &mut LayoutMap::new(),
                    false,
                    &mut 0,
                    &SelectionState::None,
                    0,
                    0,
                );
            });
            let drawn = match terminal.cursor() {
                mutx_engine::CursorState::Visible(x, y) => (x, y),
                other => panic!("{label}: caret should be visible, got {other:?}"),
            };

            // What the authoritative geometry function resolves.
            let mut scroll = 0usize;
            let resolved = cursor_screen_pos(rect, input, byte_cursor, &mut scroll)
                .unwrap_or_else(|| panic!("{label}: cursor_screen_pos returned None"));

            assert_eq!(
                drawn, resolved,
                "{label} (input={input:?}, byte={byte_cursor}): \
                 draw path did not forward the resolved caret"
            );
        }
    }

    /// Cursor resolution updates `input_scroll` to keep the final caret inside
    /// the visible composer rows.
    #[test]
    fn cursor_screen_pos_clamps_scroll_like_draw() {
        use super::composer::cursor_screen_pos;

        // A 20-wide box (text width ~16) with a long input; the box shows only
        // a couple of rows, so a caret near the end forces a scroll.
        let rect = Rect::new(0, 0, 20, 4);
        let input = "word ".repeat(20); // ~100 chars, wraps many times
        let byte_cursor = input.len();

        let mut scroll = 0usize;
        let resolved = cursor_screen_pos(rect, &input, byte_cursor, &mut scroll)
            .expect("caret position resolves");

        // The resolved caret must sit on a visible row (within the box's text
        // rows), proving scroll advanced to track it.
        let visible_rows = (rect.height as usize)
            .saturating_sub(crate::design::COMPOSER_VERTICAL_CHROME_ROWS as usize)
            .max(1);
        let caret_row = (resolved.1 - rect.y - crate::design::COMPOSER_TEXT_ROW_OFFSET) as usize;
        assert!(
            caret_row < visible_rows,
            "resolved caret row {caret_row} outside the {visible_rows} visible rows"
        );
        assert!(scroll > 0, "scroll should have advanced to track the caret");
    }

    /// (head + continuation), cover exactly the selected glyphs, and leave the
    /// trailing pad on the panel background — no extra glyph, no half-highlighted
    /// wide char. Exercises the full-3-CJK selection the live bug report used.
    #[test]
    fn composer_cjk_selection_covers_full_width_glyphs() {
        use crate::model::layout::SemanticCursor;
        let theme = Theme::default();
        let panel_bg = theme.input_surface();
        let sel_bg = theme.selected();
        let input = "中文测"; // 3 wide glyphs = 6 cols (cols 2..8)
        // Select all three. Head points AT 测 (byte 6); the inclusive-head model
        // includes the glyph under the head, so the range is [0, 9) = "中文测".
        let sel = SelectionState::Range {
            anchor: SemanticCursor::new(INPUT_MSG_IDX, 0, 0),
            head: SemanticCursor::new(INPUT_MSG_IDX, 0, 6),
        };
        let mut terminal = mutx_engine::TestTerminal::new(20, 5);
        terminal.draw(|f| {
            draw_composer(
                f,
                Rect::new(0, 0, 20, 4),
                input,
                input.len(),
                true,
                false,
                &theme,
                &mut LayoutMap::new(),
                false,
                &mut 0,
                &sel,
                0,
                0,
            );
        });
        let g = terminal.buffer();
        let y = crate::design::COMPOSER_TEXT_ROW_OFFSET;
        // Cols: 0='›', 1=gap, 2-7='中文测'(sel), 8+=panel tail.
        for (col, label, expect_sel) in [
            (2usize, "中 head", true),
            (3, "中 cont", true),
            (4, "文 head", true),
            (5, "文 cont", true),
            (6, "测 head", true),
            (7, "测 cont", true),
            (8, "tail 0", false),
            (9, "tail 1", false),
        ] {
            let cell = g.get(col as u16, y).unwrap();
            let want = if expect_sel { sel_bg } else { panel_bg };
            assert_eq!(
                cell.bg, want,
                "{label} at col {col}: bg {:?} expected {:?}",
                cell.bg, want
            );
        }
        // While a selection is active the caller passes `show_caret = false`
        // (see the event loop), so no terminal caret is placed on top of the
        // highlighted glyphs — the "appended flickering character" symptom.
        assert!(
            matches!(terminal.cursor(), mutx_engine::CursorState::Hidden),
            "caret must be hidden while a selection is active"
        );
    }

    #[test]
    fn composer_two_cjk_select_all_has_no_extra_glyph_or_tail_highlight() {
        use crate::model::layout::SemanticCursor;

        let theme = Theme::default();
        let panel_bg = theme.input_surface();
        let sel_bg = theme.selected();
        let input = "你好";
        let sel = SelectionState::Range {
            anchor: SemanticCursor::new(INPUT_MSG_IDX, 0, 0),
            head: SemanticCursor::new(INPUT_MSG_IDX, 0, input.len()),
        };
        let mut terminal = mutx_engine::TestTerminal::new(16, 5);

        terminal.draw(|f| {
            draw_composer(
                f,
                Rect::new(0, 0, 16, 4),
                input,
                input.len(),
                true,
                false,
                &theme,
                &mut LayoutMap::new(),
                false,
                &mut 0,
                &sel,
                0,
                0,
            );
        });

        let y = crate::design::COMPOSER_TEXT_ROW_OFFSET;
        let buffer = terminal.buffer();

        assert_eq!(buffer.get(2, y).unwrap().symbol(), "你");
        assert_eq!(buffer.get(2, y).unwrap().width, 2);
        assert_eq!(buffer.get(3, y).unwrap().symbol(), " ");
        assert_eq!(buffer.get(3, y).unwrap().width, 0);
        assert_eq!(buffer.get(4, y).unwrap().symbol(), "好");
        assert_eq!(buffer.get(4, y).unwrap().width, 2);
        assert_eq!(buffer.get(5, y).unwrap().symbol(), " ");
        assert_eq!(buffer.get(5, y).unwrap().width, 0);
        assert_eq!(
            buffer.get(6, y).unwrap().symbol(),
            " ",
            "tail cell must not contain a duplicate glyph"
        );

        for col in 2..=5 {
            assert_eq!(
                buffer.get(col, y).unwrap().bg,
                sel_bg,
                "col {col} should be selected"
            );
        }
        assert_eq!(
            buffer.get(6, y).unwrap().bg,
            panel_bg,
            "tail cell must remain on input panel background"
        );
        assert!(
            matches!(terminal.cursor(), mutx_engine::CursorState::Hidden),
            "caret must be hidden while a selection is active"
        );
    }

    /// Regression for the input-select bug: a click that starts a selection
    /// (anchor == head, a collapsed range) must highlight NOTHING, and a drag
    /// through the real click pipeline (layout_map → cursor_at) must highlight
    /// exactly the dragged glyphs with the correct background. The prior
    /// `inclusive_grapheme_end`-on-a-point logic lit up one glyph on every
    /// click and flickered as the drag moved — "an extra changing character
    /// appears and the selection background misbehaves".
    #[test]
    fn composer_collapsed_click_highlights_nothing_drag_highlights_cleanly() {
        let theme = Theme::default();
        let panel_bg = theme.input_surface();
        let sel_bg = theme.selected();
        let input = "中文测";
        let rect = Rect::new(0, 0, 20, 4);
        let text_row = crate::design::COMPOSER_TEXT_ROW_OFFSET;

        // Record input regions so cursor_at can resolve real drag positions.
        let mut layout_map = LayoutMap::new();
        let mut rec = mutx_engine::TestTerminal::new(20, 5);
        rec.draw(|f| {
            draw_composer(
                f,
                rect,
                input,
                input.len(),
                true,
                false,
                &theme,
                &mut layout_map,
                true,
                &mut 0,
                &SelectionState::None,
                0,
                0,
            );
        });
        let anchor = layout_map.cursor_at(rect.x + 2, rect.y + text_row).unwrap();
        assert_eq!(anchor.byte_offset, 0);

        fn row_bgs(
            input: &str,
            rect: Rect,
            text_row: u16,
            theme: &Theme,
            sel: &SelectionState,
        ) -> Vec<mutx_engine::Color> {
            let mut t = mutx_engine::TestTerminal::new(20, 5);
            t.draw(|f| {
                draw_composer(
                    f,
                    rect,
                    input,
                    input.len(),
                    true,
                    false,
                    theme,
                    &mut LayoutMap::new(),
                    false,
                    &mut 0,
                    sel,
                    0,
                    0,
                );
            });
            (0..10u16)
                .map(|c| t.buffer().get(c, text_row).unwrap().bg)
                .collect()
        }

        // 1) Collapsed click (anchor == head): no glyph may carry the selection bg.
        let collapsed = SelectionState::Range {
            anchor,
            head: anchor,
        };
        for (col, bg) in row_bgs(input, rect, text_row, &theme, &collapsed)
            .into_iter()
            .enumerate()
        {
            assert_ne!(bg, sel_bg, "collapsed click lit up col {col}");
            let _ = panel_bg;
        }

        // 2) Drag onto 测's first column (byte 6): inclusive head selects all
        //    three glyphs; the trailing pad stays on the panel bg.
        let head = layout_map.cursor_at(rect.x + 6, rect.y + text_row).unwrap();
        assert_eq!(head.byte_offset, 6);
        let drag = SelectionState::Range { anchor, head };
        let bgs = row_bgs(input, rect, text_row, &theme, &drag);
        // cols 0,1 = prefix; 2..8 = "中文测" (selected); 8,9 = tail (panel).
        for (col, &bg) in bgs[2..8].iter().enumerate() {
            assert_eq!(bg, sel_bg, "col {} should be selected", col + 2);
        }
        for (col, &bg) in bgs[8..10].iter().enumerate() {
            assert_eq!(bg, panel_bg, "col {} should be panel tail", col + 8);
        }

        // 3) Drag to the second visual column of 中. The hit-test cursor maps
        // both columns of a wide glyph to that glyph's byte start; with an
        // inclusive head this selects 中 only, not the next glyph.
        let head = layout_map.cursor_at(rect.x + 3, rect.y + text_row).unwrap();
        assert_eq!(head.byte_offset, 1);
        let drag = SelectionState::Range { anchor, head };
        let bgs = row_bgs(input, rect, text_row, &theme, &drag);
        for (col, &bg) in bgs[2..4].iter().enumerate() {
            assert_eq!(bg, sel_bg, "col {} should select 中", col + 2);
        }
        for (col, &bg) in bgs[4..8].iter().enumerate() {
            assert_eq!(bg, panel_bg, "col {} should remain unselected", col + 4);
        }
    }

    #[test]
    fn user_message_and_composer_keep_symmetric_panel_padding() {
        let theme = Theme::default();
        let user_bg = theme.user_surface();
        let input_bg = theme.input_surface();
        let app_bg = theme.surface();
        let width = 60u16;
        let mut terminal = mutx_engine::TestTerminal::new(width, 24);

        // A long user message fills the first wrapped line edge to edge, so the
        // right-side padding is only present if the wrap width reserves it.
        let messages = vec![TranscriptMessage::new(
            muta_contracts::Role::User,
            "x".repeat(200),
        )];
        let long_input = "y".repeat(200);

        terminal.draw(|f| {
            let mut layout_map = LayoutMap::new();
            // draw_transcript only computes the input box geometry; the composer
            // itself is drawn separately (as the live app does), using the
            // returned input_rect.
            let render = draw_transcript(
                f,
                &mut layout_map,
                TranscriptView {
                    messages: &messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: &long_input,
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    runner_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            );
            let mut input_scroll = 0;
            draw_composer(
                f,
                render.input_rect,
                &long_input,
                0,
                true,
                true,
                &theme,
                &mut layout_map,
                false,
                &mut input_scroll,
                &SelectionState::None,
                0,
                0,
            );
        });

        let buffer = terminal.buffer();

        // Find the first user-message text row. Layout (60-col terminal):
        //   cols 0,1  = global app_bg (viewport margin)
        //   cols 2,3  = user_panel_bg inner pad (USER_MESSAGE_TEXT_GAP_COLS)
        //   col  4+   = text
        let user_row = (0..buffer.area().height)
            .find(|&y| {
                let c4 = &buffer[(4, y)];
                c4.symbol() == "x" && c4.bg == user_bg
            })
            .expect("user message row exists");

        // Left: 2-col app_bg outer gutter (viewport margin + entry inset),
        // then 2-col user_panel_bg inner pad.
        assert_eq!(buffer[(0, user_row)].bg, app_bg, "left outer gutter");
        assert_eq!(buffer[(1, user_row)].bg, app_bg, "left outer gutter");
        assert_eq!(
            buffer[(2, user_row)].bg,
            user_bg,
            "left inner padding must be user_panel_bg"
        );
        assert_eq!(
            buffer[(3, user_row)].bg,
            user_bg,
            "left inner padding is 2 cols, not 1"
        );
        assert_eq!(buffer[(4, user_row)].symbol(), "x", "text starts at col 4");

        // Right: 2-col user_panel_bg inner pad, then 2-col app_bg outer gutter.
        // user_text_width = (band_w) - (TEXT_GAP + RIGHT_PAD) = (60-4) - 4 = 52
        // -> text fills cols 4..56.
        assert_eq!(
            buffer[(56, user_row)].symbol(),
            " ",
            "right inner padding must stay clear of wrapped text"
        );
        assert_eq!(buffer[(56, user_row)].bg, user_bg, "right inner padding");
        assert_eq!(buffer[(57, user_row)].bg, user_bg, "right inner padding");
        assert_eq!(buffer[(58, user_row)].bg, app_bg, "right outer gutter");
        assert_eq!(buffer[(59, user_row)].bg, app_bg, "right outer gutter");

        // Composer: the input panel starts at x = FOOTER_H_INSET (2). `›` at
        // x=2, text from x=4, and a 2-col right pad in the input box's active
        // background before the app_bg gutter at the far right.
        let composer_row = (0..buffer.area().height)
            .find(|&y| {
                let c4 = &buffer[(4, y)];
                c4.symbol() == "y" && c4.bg == input_bg
            })
            .expect("composer row exists");
        assert_eq!(buffer[(2, composer_row)].symbol(), "›", "composer prompt");
        assert_eq!(
            buffer[(4, composer_row)].symbol(),
            "y",
            "composer text starts at col 4"
        );
        // full_w (composer panel) = 60 - 2*FOOTER_H_INSET = 56, panel spans
        // x=2..58. Right pad at x=56,57 (input_bg), gutter x=58,59 (app_bg).
        assert_eq!(
            buffer[(56, composer_row)].bg,
            input_bg,
            "composer right inner padding"
        );
        assert_eq!(
            buffer[(57, composer_row)].bg,
            input_bg,
            "composer right inner padding"
        );
        assert_eq!(
            buffer[(58, composer_row)].bg,
            app_bg,
            "composer right outer gutter"
        );
        assert_eq!(
            buffer[(59, composer_row)].bg,
            app_bg,
            "composer right outer gutter"
        );
    }

    /// The input box owns two dedicated background tokens — active (the box
    /// owns the keyboard) and inactive (a transcript step owns it). Both must
    /// render as full panels and the two states must be visibly different
    /// colors, so "where does typing land" is legible from luminance alone
    /// and neither state melts into the app background. Regression guard for
    /// the activated/deactivated input being indistinguishable.
    #[test]
    fn composer_focused_and_unfocused_panels_render_distinct_backgrounds() {
        let theme = Theme::default();
        let active_bg = theme.input_surface();
        let inactive_bg = theme.input_surface_inactive();
        let app_bg = theme.surface();
        assert_ne!(active_bg, inactive_bg, "pair must be distinct colors");

        let panel_bg_at = |focused: bool| -> mutx_engine::Color {
            let mut terminal = mutx_engine::TestTerminal::new(30, 5);
            terminal.draw(|f| {
                draw_composer(
                    f,
                    Rect::new(0, 0, 30, 3),
                    "hello",
                    5,
                    focused,
                    false,
                    &theme,
                    &mut LayoutMap::new(),
                    false,
                    &mut 0,
                    &SelectionState::None,
                    0,
                    0,
                );
            });
            let buffer = terminal.buffer();
            // A point inside the panel: the top padding row is painted
            // unconditionally, so it carries the panel background.
            let cell = &buffer[(0, 0)];
            assert_eq!(cell.symbol(), " ", "top padding row must be blank");
            cell.bg
        };

        let rendered_active = panel_bg_at(true);
        let rendered_inactive = panel_bg_at(false);
        assert_eq!(
            rendered_active, active_bg,
            "focused box must paint the input-active background"
        );
        assert_eq!(
            rendered_inactive, inactive_bg,
            "unfocused box must paint the input-inactive background"
        );
        assert_ne!(
            rendered_active, app_bg,
            "focused box must not melt into the app background"
        );
        assert_ne!(
            rendered_inactive, app_bg,
            "unfocused box must not melt into the app background"
        );
        assert_ne!(
            rendered_inactive,
            theme.user_surface(),
            "the inactive input is its own token, not the sent-user-message panel"
        );
    }

    /// A queued user message (one staged in the send queue waiting for the
    /// in-flight turn to finish) must render with the dimmer
    /// `user_panel_bg_queued` band and a visible "⏸ Queued" badge so the user
    /// can tell their message is pending, not delivered.
    #[test]
    fn queued_user_message_renders_badge_and_dimmer_bg() {
        let theme = Theme::default();
        let _queued_bg = theme.user_surface_queued();
        let delivered_bg = theme.user_surface();
        let width = 40u16;
        let mut terminal = mutx_engine::TestTerminal::new(width, 20);

        let messages = vec![
            TranscriptMessage::new(muta_contracts::Role::User, "first queued").queued(),
            TranscriptMessage::new(muta_contracts::Role::User, "second queued").queued(),
        ];
        terminal.draw(|f| {
            let mut layout_map = LayoutMap::new();
            let _ = draw_transcript(
                f,
                &mut layout_map,
                TranscriptView {
                    messages: &messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    runner_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            );
        });

        let buffer = terminal.buffer();

        // Both queued panels must carry the queued bg, never the delivered bg.
        // Scan the inner-pad columns (2,3) of every row for any cell painted
        // with the delivered bg — that would mean a queued message leaked the
        // wrong surface.
        for y in 0..buffer.area().height {
            for x in 2..4 {
                let bg = buffer[(x, y)].bg;
                assert_ne!(
                    bg, delivered_bg,
                    "queued panels must never carry the delivered bg, found at ({},{})",
                    x, y
                );
            }
        }

        // Each queued user message renders one "⏸ Queued" badge row OUTSIDE
        // the panel (on plain `surface`, above the panel's top transition).
        // The badge is the paused glyph at the text column, on a surface row.
        let badge_count = (0..buffer.area().height)
            .filter(|&y| buffer[(4, y)].symbol() == "⏸")
            .count();
        assert_eq!(
            badge_count, 2,
            "each queued user message must render one badge row, got {}",
            badge_count
        );
    }

    /// ADR-0126: a *held* insert — one whose round ended (naturally or by an
    /// Esc Esc interrupt) before admission — renders the same pending panel
    /// as a queued message, with a label that spells out the different fate:
    /// `⏸ Held for next round`, not the plain `⏸ Queued`.
    #[test]
    fn held_insert_renders_the_held_label_and_dimmer_bg() {
        use crate::model::document::DeliveryStatus;
        let theme = Theme::default();
        let delivered_bg = theme.user_surface();
        let width = 56u16;
        let mut terminal = mutx_engine::TestTerminal::new(width, 16);

        let mut held = TranscriptMessage::new(muta_contracts::Role::User, "held steer");
        held.delivery = DeliveryStatus::HeldNextRound;
        held.origin = crate::model::document::UserMessageOrigin::Steer;
        let messages = vec![held];

        terminal.draw(|f| {
            let mut layout_map = LayoutMap::new();
            let _ = draw_transcript(
                f,
                &mut layout_map,
                TranscriptView {
                    messages: &messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    runner_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            );
        });

        let buffer = terminal.buffer();
        // The held panel carries the dimmer pending band, never the delivered
        // one.
        for y in 0..buffer.area().height {
            for x in 2..4 {
                assert_ne!(
                    buffer[(x, y)].bg,
                    delivered_bg,
                    "a held panel must never carry the delivered bg, found at ({},{})",
                    x,
                    y
                );
            }
        }
        // The full label renders (spelled out, unlike the compact `⏸ Queued`).
        let row_text = |y: u16| -> String {
            (0..buffer.area().width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect()
        };
        let rendered = (0..buffer.area().height)
            .map(row_text)
            .any(|row| row.contains("Held for next round"));
        assert!(
            rendered,
            "the held entry must spell out its fate (⏸ Held for next round)"
        );
    }

    /// The transcript content rect must be recorded after rendering so that
    /// clicks on gap rows (which carry no region) still switch keyboard focus
    /// to Browse. It must span the horizontal band inside the outer gutters
    /// (clicks in the gutters are not transcript clicks) and the vertical
    /// extent of drawn content, including the inter-message gap row.
    #[test]
    fn transcript_content_rect_spans_band_and_gap_rows() {
        let theme = Theme::default();
        let width = 40u16;
        let mut terminal = mutx_engine::TestTerminal::new(width, 24);
        // Two assistant text messages so a `MESSAGE_GAP_ROWS` blank row is
        // emitted between them — that row is rendered but never registered.
        let messages = vec![
            TranscriptMessage::new(muta_contracts::Role::Assistant, "first".to_string()),
            TranscriptMessage::new(muta_contracts::Role::Assistant, "second".to_string()),
        ];
        let mut layout_map = LayoutMap::new();
        terminal.draw(|f| {
            draw_transcript(
                f,
                &mut layout_map,
                TranscriptView {
                    messages: &messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    runner_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            );
        });

        let rect = layout_map
            .transcript_content_rect()
            .expect("content rect must be recorded when messages are drawn");
        // Horizontal band excludes the outer `TRANSCRIPT_H_INSET` gutters.
        assert_eq!(rect.x, TRANSCRIPT_H_INSET);
        assert_eq!(rect.width, width - 2 * TRANSCRIPT_H_INSET);

        // The whole point of the rect: a gap row between the two messages is
        // rendered but carries no region (clicking it does not resolve to a
        // cursor). It must still fall inside the content rect so the click
        // handler can switch focus to Browse.
        let gap_y = (rect.y..rect.y + rect.height)
            .find(|&y| layout_map.region_at(rect.x, y).is_none())
            .expect("there must be at least one unregistered gap row between the two messages");
        assert!(rect.y <= gap_y && gap_y < rect.y + rect.height);
    }

    /// Wide tables (including CJK content) must keep borders intact and never
    /// overflow the viewport: columns shrink to fit, cell text wraps, and
    /// every rendered line stays within the available width.
    #[test]
    fn wide_table_shrinks_columns_and_keeps_borders_intact() {
        use crate::model::document::TableAlignment;

        let headers = vec![
            "Tool".to_string(),
            "Type".to_string(),
            "Implementation".to_string(),
            "Key Feature".to_string(),
        ];
        let rows = vec![
            vec![
                "execute_command".to_string(),
                "Write".to_string(),
                "std::process::Command (sh -c / cmd /C)".to_string(),
                "execute shell command, supports timeout, truncates output".to_string(),
            ],
            vec![
                "read_text".to_string(),
                "Read".to_string(),
                "std::fs::read_to_string".to_string(),
                "supports offset/limit".to_string(),
            ],
        ];
        let aligns = vec![
            TableAlignment::None,
            TableAlignment::None,
            TableAlignment::None,
            TableAlignment::None,
        ];

        // ── Narrow terminal (34 cols): table is far wider, must shrink ──
        let lines = build_table_render(&headers, &rows, &aligns, 34).lines;
        assert!(!lines.is_empty(), "table must produce output");

        for (i, line) in lines.iter().enumerate() {
            assert!(
                line.width() <= 34,
                "line {i} overflows: {} cols: {}",
                line.width(),
                line
            );
        }
        assert!(lines.first().unwrap().starts_with('┌'));
        assert!(lines.last().unwrap().starts_with('└'));
        assert!(
            lines.iter().any(|l| l.starts_with('├')),
            "missing header/body separator"
        );
        // Two body rows → one separator between them (plus one after header).
        let sep_count = lines.iter().filter(|l| l.starts_with('├')).count();
        assert_eq!(
            sep_count, 2,
            "expected 2 separators (header→body + row→row), got {sep_count}"
        );
        let pipe_counts: Vec<usize> = lines
            .iter()
            .filter(|l| l.starts_with('│'))
            .map(|l| l.matches('│').count())
            .collect();
        assert!(!pipe_counts.is_empty(), "must have data lines");
        assert!(
            pipe_counts.iter().all(|&c| c == pipe_counts[0]),
            "all data lines must have the same number of column separators"
        );

        // ── Wide terminal (80 cols): table fits without shrinking ──
        let wide_lines = build_table_render(&headers, &rows, &aligns, 76).lines;
        for (i, line) in wide_lines.iter().enumerate() {
            assert!(
                line.width() <= 76,
                "wide line {i} overflows: {} cols",
                line.width()
            );
        }
        // When it fits, the table should be shorter (no wrapping needed).
        assert!(
            wide_lines.len() <= lines.len(),
            "wide table should have fewer lines than shrunk table"
        );
    }

    /// Ragged body rows (fewer cells than the header, and more) must not panic
    /// the adaptive renderer and must still produce a rectangular grid: every
    /// data line carries the same number of `│` column separators. Regression
    /// test for the `index out of bounds: the len is 1 but the index is 1`
    /// panic at `markdown_table.rs` (`cell_styles[i]`) caused by a body row
    /// with a single cell in a two-column table.
    #[test]
    fn table_render_handles_ragged_rows_without_panicking() {
        use crate::model::document::TableAlignment;

        let headers = vec!["A".to_string(), "B".to_string()];
        // 0, 1, 2, and 3 cells — exercises both the under- and over-wide paths.
        let rows = vec![
            vec![],
            vec!["only".to_string()],
            vec!["x".to_string(), "y".to_string()],
            vec!["p".to_string(), "q".to_string(), "r".to_string()],
        ];
        let aligns = vec![TableAlignment::None, TableAlignment::None];

        let table = build_table_render(&headers, &rows, &aligns, 40);
        assert!(!table.lines.is_empty(), "ragged table must still render");

        // Every data line must have the same number of column separators, i.e.
        // the grid stays rectangular regardless of input raggedness.
        let pipe_counts: Vec<usize> = table
            .lines
            .iter()
            .filter(|l| l.starts_with('│'))
            .map(|l| l.matches('│').count())
            .collect();
        assert!(!pipe_counts.is_empty(), "must have data lines");
        assert!(
            pipe_counts.iter().all(|&c| c == pipe_counts[0]),
            "ragged rows produced uneven column counts: {pipe_counts:?}"
        );

        // Every data line carries per-cell geometry for exactly `ncols` cells,
        // so hit-testing / selection never indexes out of bounds.
        for info in table.line_info.iter().flatten() {
            assert_eq!(
                info.col_spans.len(),
                2,
                "each data line must describe exactly 2 cells"
            );
        }
    }

    /// Inline-code / bold markup delimiters (`` ` ``, `**`) are rendered at zero
    /// width, so a column holding markup must be sized and wrapped by its
    /// *visible* width — otherwise the column is inflated, the wrapped text can
    /// split a `` `…` ``/`**…**` pair across lines, and data-row `│` separators
    /// drift out of line with the border grid. A plain table and a markup table
    /// carrying the same visible content must therefore share identical borders
    /// and the same line count (no spurious wrap).
    #[test]
    fn table_markup_columns_size_to_visible_width() {
        use crate::model::document::TableAlignment;

        let plain = build_table_render(
            &["a".to_string(), "b".to_string()],
            &[vec!["bold".to_string(), "code".to_string()]],
            &[TableAlignment::None, TableAlignment::None],
            80,
        );
        let markup = build_table_render(
            &["a".to_string(), "b".to_string()],
            &[vec!["**bold**".to_string(), "`code`".to_string()]],
            &[TableAlignment::None, TableAlignment::None],
            80,
        );

        // Borders are markup-free, so plain and markup grids must match exactly
        // once columns are sized to visible width.
        let plain_borders: Vec<&String> =
            plain.lines.iter().filter(|l| !l.starts_with('│')).collect();
        let markup_borders: Vec<&String> = markup
            .lines
            .iter()
            .filter(|l| !l.starts_with('│'))
            .collect();
        assert_eq!(
            plain_borders, markup_borders,
            "markup must not inflate column width"
        );

        // The markup cell fits its column on a single line (no delimiter split):
        // same number of data lines as the plain version.
        let plain_data = plain.lines.iter().filter(|l| l.starts_with('│')).count();
        let markup_data = markup.lines.iter().filter(|l| l.starts_with('│')).count();
        assert_eq!(
            plain_data, markup_data,
            "markup must not introduce extra wrapped lines"
        );
    }

    #[test]
    fn shrink_columns_preserves_minimum_and_proportions() {
        // Intrinsic [10, 5, 20], target 24, min 3.
        // total_min = 9, shrinkable = 26, available = 15.
        // col0: 3 + 7*15/26 = 3 + 4 = 7
        // col1: 3 + 2*15/26 = 3 + 1 = 4
        // col2: 3 + 17*15/26 = 3 + 9 = 12
        let result = shrink_column_widths(&[10, 5, 20], 24, 3);
        assert_eq!(result.len(), 3);
        assert!(result.iter().all(|&w| w >= 3), "must respect minimum");
        assert!(
            result.iter().sum::<usize>() <= 24,
            "must fit within target, got {}",
            result.iter().sum::<usize>()
        );
        // Largest intrinsic column stays largest after shrinking.
        let max_val = *result.iter().max().unwrap();
        let max_idx = result.iter().position(|&v| v == max_val).unwrap();
        assert_eq!(max_idx, 2);
    }

    #[test]
    fn shrink_columns_with_tiny_target_returns_all_minimum() {
        let result = shrink_column_widths(&[10, 20, 30], 5, 3);
        assert_eq!(result, vec![3, 3, 3]);
    }

    /// Drive `draw_history_panel` against a real buffer across every input
    /// state the Ctrl+R picker can land in. The assertions are deliberately
    /// structural ("does not panic, produces a non-empty frame") because the
    /// fuzzy highlight math is already covered by `fuzzy::tests`; here we
    /// only need to prove the renderer consumes each state without exploding.
    #[test]
    fn history_panel_renders_every_query_state() {
        let selection = crate::model::selection::SelectionState::None;
        let mut layout_map = crate::model::layout::LayoutMap::new();

        let theme = Theme::default();
        let history: Vec<muta_contracts::HistoryEntry> = [
            "git status",
            "git commit -am 'ship it'",
            "cargo test",
            "review the diff before sending",
        ]
        .into_iter()
        .enumerate()
        .map(|(i, text)| {
            muta_contracts::HistoryEntry::new(
                text.to_string(),
                Some(format!("s{i}")),
                Some("~/p".to_string()),
                (i as u64) * 1_000,
            )
        })
        .collect();
        let texts: Vec<&str> = history.iter().map(|e| e.text.as_str()).collect();

        let cases: &[(&str, usize)] = &[
            ("", history.len()), // empty query → everything surfaces
            ("git", 2),          // partial match → subset with highlights
            ("zzz", 0),          // no subsequence → empty placeholder
        ];

        let input_rect = mutx_engine::Rect::new(0, 22, 80, 2);
        for (query, expected_matches) in cases {
            let mut terminal = mutx_engine::TestTerminal::new(80, 24);
            let mut ranked = crate::fuzzy::rank(&texts, query);
            crate::fuzzy::sort_by_score(&mut ranked);
            assert_eq!(
                ranked.len(),
                *expected_matches,
                "query {:?} should surface {} entries",
                query,
                expected_matches
            );
            terminal.draw(|f| {
                let selection = crate::model::selection::SelectionState::None;
                let mut layout_map = crate::model::layout::LayoutMap::new();
                let _ = draw_history_panel(
                    f,
                    &history,
                    &ranked,
                    0,
                    &mut 0,
                    true,
                    false,
                    false,
                    input_rect,
                    0,
                    &theme,
                    &selection,
                    &mut layout_map,
                );
            });
        }

        // Empty history must render the "(no history yet)" placeholder rather
        // than indexing into an empty slice.
        let mut terminal = mutx_engine::TestTerminal::new(80, 24);
        let empty: Vec<muta_contracts::HistoryEntry> = Vec::new();
        let ranked: Vec<(usize, crate::fuzzy::FuzzyMatch)> = crate::fuzzy::rank::<&str>(&[], "");
        terminal.draw(|f| {
            let _ = draw_history_panel(
                f,
                &empty,
                &ranked,
                0,
                &mut 0,
                true,
                false,
                false,
                input_rect,
                0,
                &theme,
                &selection,
                &mut layout_map,
            );
        });
    }

    /// A multi-line history entry collapses to its first line in the fuzzy
    /// list (so a long prompt never breaks the single-row grid), and the
    /// preview mode renders the full text verbatim. Both modes must consume a
    /// real buffer without panicking.
    #[test]
    fn history_panel_folds_multiline_and_previews_full_text() {
        let selection = crate::model::selection::SelectionState::None;
        let mut layout_map = crate::model::layout::LayoutMap::new();

        let theme = Theme::default();
        let history: Vec<muta_contracts::HistoryEntry> =
            ["first line\nsecond line\nthird line", "single line"]
                .into_iter()
                .enumerate()
                .map(|(i, text)| {
                    muta_contracts::HistoryEntry::new(
                        text.to_string(),
                        Some(format!("s{i}")),
                        None,
                        0,
                    )
                })
                .collect();
        let texts: Vec<&str> = history.iter().map(|e| e.text.as_str()).collect();

        let mut terminal = mutx_engine::TestTerminal::new(80, 24);
        let ranked = crate::fuzzy::rank(&texts, "");
        let input_rect = mutx_engine::Rect::new(0, 22, 80, 2);

        // List mode: the multi-line entry must render as one row.
        terminal.draw(|f| {
            let _ = draw_history_panel(
                f,
                &history,
                &ranked,
                0,
                &mut 0,
                true,
                false,
                false,
                input_rect,
                0,
                &theme,
                &selection,
                &mut layout_map,
            );
        });
        let buf = terminal.buffer();
        let has_marker = buf.content.iter().any(|c| c.symbol() == "↵");
        assert!(has_marker, "multi-line entry should show the ↵ fold marker");

        // Preview mode: the full multi-line text renders without panic.
        terminal.draw(|f| {
            let _ = draw_history_panel(
                f,
                &history,
                &ranked,
                0,
                &mut 0,
                true,
                true,
                false,
                input_rect,
                0,
                &theme,
                &selection,
                &mut layout_map,
            );
        });
    }

    /// The dropdown is an extension of the composer, not a fixed-size window:
    /// it collapses to the actual row count rather than reserving a fixed
    /// minimum. Two entries must produce a 4-row panel (2 rows + header +
    /// footer), not the old 6-row floor.
    #[test]
    fn history_panel_collapses_to_actual_row_count() {
        let selection = crate::model::selection::SelectionState::None;
        let mut layout_map = crate::model::layout::LayoutMap::new();

        let theme = Theme::default();
        let history: Vec<muta_contracts::HistoryEntry> = ["one", "two"]
            .into_iter()
            .enumerate()
            .map(|(i, text)| {
                muta_contracts::HistoryEntry::new(
                    text.to_string(),
                    Some(format!("s{i}")),
                    None,
                    i as u64,
                )
            })
            .collect();
        let texts: Vec<&str> = history.iter().map(|e| e.text.as_str()).collect();
        let ranked = crate::fuzzy::rank(&texts, "");
        // Composer near the bottom of a tall terminal so room-above is not the
        // binding constraint — the row count is.
        let input_rect = mutx_engine::Rect::new(0, 40, 80, 2);
        let mut terminal = mutx_engine::TestTerminal::new(80, 42);
        let mut panel: Option<mutx_engine::Rect> = None;
        terminal.draw(|f| {
            panel = draw_history_panel(
                f,
                &history,
                &ranked,
                0,
                &mut 0,
                true,
                false,
                false,
                input_rect,
                0,
                &theme,
                &selection,
                &mut layout_map,
            )
        });
        let panel = panel.expect("panel should render with ample room above");
        // 2 entries + 4 chrome rows (top padding, header, footer, bottom
        // padding) = 6 rows. The panel still collapses to the actual row
        // count — a fixed minimum would have forced 8+ regardless of entries.
        assert_eq!(
            panel.height, 6,
            "panel must collapse to actual row count + chrome (6), not a fixed minimum"
        );
    }

    /// The dropdown shares the composer's surface language, not the permission
    /// sheet's: it opens and closes with full panel-bg padding rows and never
    /// paints a full-height brand-colored left column (which would read as
    /// selection/severity). The top and bottom rows must be solid panel
    /// background (no half-block `▄`/`▀` glyphs), and the left column must NOT
    /// be brand-colored.
    #[test]
    fn history_panel_uses_composer_padding_not_brand_column() {
        let selection = crate::model::selection::SelectionState::None;
        let mut layout_map = crate::model::layout::LayoutMap::new();

        let theme = Theme::default();
        let history: Vec<muta_contracts::HistoryEntry> = ["one", "two", "three"]
            .into_iter()
            .enumerate()
            .map(|(i, text)| {
                muta_contracts::HistoryEntry::new(
                    text.to_string(),
                    Some(format!("s{i}")),
                    None,
                    i as u64,
                )
            })
            .collect();
        let texts: Vec<&str> = history.iter().map(|e| e.text.as_str()).collect();
        let ranked = crate::fuzzy::rank(&texts, "");
        let input_rect = mutx_engine::Rect::new(0, 40, 30, 2);
        let mut terminal = mutx_engine::TestTerminal::new(30, 42);
        let mut panel: Option<mutx_engine::Rect> = None;
        terminal.draw(|f| {
            panel = draw_history_panel(
                f,
                &history,
                &ranked,
                0,
                &mut 0,
                true,
                false,
                false,
                input_rect,
                0,
                &theme,
                &selection,
                &mut layout_map,
            )
        });
        let panel = panel.expect("panel should render");
        let buf = terminal.buffer();

        // Top row is a full panel-bg padding row (no half-block glyph).
        let top_left = buf.get(panel.x, panel.y).expect("top-left cell");
        assert_eq!(
            top_left.bg,
            theme.panel(),
            "top edge must be a solid panel-bg row, matching the composer's padding"
        );
        assert_eq!(
            top_left.symbol(),
            " ",
            "top edge must be blank (no ▄ transition glyph)"
        );
        // Bottom row is likewise a solid panel-bg padding row.
        let bottom_left = buf
            .get(panel.x, panel.y + panel.height - 1)
            .expect("bottom-left cell");
        assert_eq!(
            bottom_left.bg,
            theme.panel(),
            "bottom edge must be a solid panel-bg row, matching the composer's padding"
        );
        assert_eq!(
            bottom_left.symbol(),
            " ",
            "bottom edge must be blank (no ▀ transition glyph)"
        );

        // No full-height brand column: the background of the left column on the
        // header row (which is never selection-tinted) must NOT be the brand
        // color. A brand column would paint every left-edge cell, including the
        // header's, with brand as its background. The header sits one row below
        // the top transition edge.
        let header_left = buf.get(panel.x, panel.y + 1).expect("header left cell");
        assert_ne!(
            header_left.bg,
            theme.brand(),
            "no full-height brand left column — the composer edge language has none"
        );
    }
    /// never grows into the activity bar's rows, so the live status surface
    /// above the composer always stays visible and always reads as above the
    /// history dropdown.
    #[test]
    fn history_panel_reserves_activity_bar_rows() {
        let theme = Theme::default();
        // Enough entries that, absent the reservation, the panel would want to
        // grow tall and run past the activity bar.
        let history: Vec<muta_contracts::HistoryEntry> = (0..25)
            .map(|i| {
                muta_contracts::HistoryEntry::new(
                    format!("entry {i}"),
                    Some(format!("s{i}")),
                    None,
                    i,
                )
            })
            .collect();
        let texts: Vec<&str> = history.iter().map(|e| e.text.as_str()).collect();
        let ranked = crate::fuzzy::rank(&texts, "");
        // Composer at row 15; the activity bar occupies the single row above it
        // (row 14), so `activity_height = 1`.
        let input_rect = mutx_engine::Rect::new(0, 15, 80, 2);
        let mut terminal = mutx_engine::TestTerminal::new(80, 17);
        let mut panel: Option<mutx_engine::Rect> = None;
        terminal.draw(|f| {
            let selection = crate::model::selection::SelectionState::None;
            let mut layout_map = crate::model::layout::LayoutMap::new();
            panel = draw_history_panel(
                f,
                &history,
                &ranked,
                0,
                &mut 0,
                true,
                false,
                false,
                input_rect,
                1,
                &theme,
                &selection,
                &mut layout_map,
            )
        });
        let panel = panel.expect("panel should render");
        // The activity bar occupies the single row above the composer
        // (input_rect.y - 1 = 14). The panel must never cover it: its bottom
        // edge (panel.y + panel.height) must sit at or above row 14.
        assert!(
            panel.y + panel.height <= 14,
            "panel footprint [y={}, h={}] must not cover the activity bar row (14)",
            panel.y,
            panel.height
        );
    }

    /// With no messages, `draw_transcript` renders the empty-state hero in
    /// place of the stream: `content_lines` is non-zero (so the app loop does
    /// not treat it as a zero-height stream) and the call does not panic.
    #[test]
    fn empty_session_renders_empty_state_with_nonzero_height() {
        let theme = Theme::default();
        let mut terminal = mutx_engine::TestTerminal::new(80, 24);
        let messages: Vec<TranscriptMessage> = Vec::new();

        let mut render_opt: Option<TranscriptRender> = None;
        terminal.draw(|f| {
            let mut layout_map = LayoutMap::new();
            render_opt = Some(draw_transcript(
                f,
                &mut layout_map,
                TranscriptView {
                    messages: &messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "idle",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    runner_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            ));
        });
        let render = render_opt.expect("draw_transcript must return a render");

        // The empty-state hero replaces the transcript; it occupies the logo
        // rows plus a gap, never zero, so scroll-follow logic stays honest.
        assert!(
            render.content_lines > 0,
            "empty state should report non-zero content_lines"
        );
        assert!(render.sticky.is_none(), "no sticky header on empty state");
        assert!(
            render.view_height > 0,
            "view_height should reflect the viewport, not be zero"
        );
    }

    /// A non-empty session skips the empty-state branch entirely — the hero
    /// never competes with real content.
    #[test]
    fn nonempty_session_does_not_render_empty_state() {
        let theme = Theme::default();
        let mut terminal = mutx_engine::TestTerminal::new(80, 24);
        let messages = vec![TranscriptMessage::new(muta_contracts::Role::User, "hello")];

        let mut render_opt: Option<TranscriptRender> = None;
        terminal.draw(|f| {
            let mut layout_map = LayoutMap::new();
            render_opt = Some(draw_transcript(
                f,
                &mut layout_map,
                TranscriptView {
                    messages: &messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "idle",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    runner_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            ));
        });
        let render = render_opt.expect("draw_transcript must return a render");

        // With a real message the stream is rendered normally — content_lines
        // reflects at least one rendered message rather than the fixed
        // empty-state height.
        assert!(
            render.content_lines > 0,
            "non-empty session should render its messages"
        );
    }

    /// A user-supplied logo (from `logo.txt`) replaces the built-in wordmark
    /// on the empty state, and `content_lines` tracks its (clamped) height so
    /// scroll accounting stays honest. A four-line user logo yields seven
    /// reported lines (4 + blank gap + carousel page), distinct from the
    /// built-in wordmark's height.
    #[test]
    fn empty_session_uses_user_logo_and_reports_its_height() {
        let theme = Theme::default();
        let mut terminal = mutx_engine::TestTerminal::new(80, 24);
        let messages: Vec<TranscriptMessage> = Vec::new();
        // Four lines → reported content is 4 + 2 (gap + carousel page) = 7.
        let logo: Vec<String> = vec![
            "  N N  ".to_string(),
            " N N N ".to_string(),
            "  N N  ".to_string(),
            "       ".to_string(),
        ]
        .into_iter()
        .chain(std::iter::repeat_n("xxxxx".to_string(), 0))
        .collect();

        let mut render_opt: Option<TranscriptRender> = None;
        terminal.draw(|f| {
            let mut layout_map = LayoutMap::new();
            render_opt = Some(draw_transcript(
                f,
                &mut layout_map,
                TranscriptView {
                    messages: &messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "idle",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    runner_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: Some(&logo),
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            ));
        });
        let render = render_opt.expect("draw_transcript must return a render");

        // 4 logo lines + 2 blank gap + 1 carousel page = 7.
        assert_eq!(
            render.content_lines, 7,
            "user-logo content_lines must be logo rows + gap + guidance rows"
        );
    }

    /// A shared harness for full-transcript renders that need to inspect the
    /// painted grid. Returns the terminal so callers can read its buffer.
    fn render_full_view(
        width: u16,
        height: u16,
        messages: &[TranscriptMessage],
        page_hints: Option<PageHints<'_>>,
    ) -> mutx_engine::TestTerminal {
        let theme = Theme::default();
        let mut terminal = mutx_engine::TestTerminal::new(width, height);
        let hints = page_hints;
        terminal.draw(|f| {
            let _ = draw_transcript(
                f,
                &mut LayoutMap::new(),
                TranscriptView {
                    messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    runner_bar: None,
                    side_banner: None,
                    page_hints: hints,
                    session_head: Some(SessionHead {
                        session_id: "sess-01a2b3c4",
                        workspace: "~/projects/xx",
                        yolo: false,
                    }),
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            );
        });
        terminal
    }

    fn grid_row(terminal: &mutx_engine::TestTerminal, y: u16) -> String {
        let buffer = terminal.buffer();
        let width = buffer.area().width;
        (0..width).map(|x| buffer[(x, y)].symbol()).collect()
    }

    /// ADR-0104: the head band's row 2 is demand-driven. On the main view
    /// with no live asides (the common idle case) the band is a single row —
    /// the legend line stays blank and the empty-state hero moves up one row.
    #[test]
    fn main_view_without_asides_renders_a_single_row_head_band() {
        let terminal = render_full_view(
            80,
            24,
            &[],
            Some(PageHints {
                kind: PageKind::Main,
                asides: None,
                interruptible: true,
                parent_note: "",
            }),
        );
        assert!(grid_row(&terminal, 0).contains("SESSION"));
        let row1 = grid_row(&terminal, 1);
        assert!(
            row1.trim().is_empty(),
            "row 2 must stay blank without asides: {row1:?}"
        );
    }

    /// ADR-0104: with live asides the row-2 legend appears (chip + `F5
    /// asides`), and it never carries an interrupt pair — the activity bar's
    /// `Esc Esc interrupt` is the authoritative copy.
    #[test]
    fn main_view_with_asides_shows_the_legend_row() {
        let terminal = render_full_view(
            80,
            24,
            &[],
            Some(PageHints {
                kind: PageKind::Main,
                asides: Some(AsidesChip {
                    total: 2,
                    running: 1,
                }),
                interruptible: true,
                parent_note: "",
            }),
        );
        let row1 = grid_row(&terminal, 1);
        assert!(row1.contains("btw: 2 total (1 active)"), "chip: {row1:?}");
        assert!(row1.contains("F5"), "aside jump pair: {row1:?}");
        assert!(!row1.contains("Esc"), "no interrupt pair: {row1:?}");
        assert!(!row1.contains("F1"), "no global help pair: {row1:?}");
    }

    /// The Runner page's row 2 never renders — its permanent footer already
    /// carries the same legend (ADR-0104), so a second copy one screen apart
    /// would be pure duplication.
    #[test]
    fn runner_view_omits_row2_entirely() {
        let hints = PageHints {
            kind: PageKind::Runner,
            asides: None,
            interruptible: true,
            parent_note: "",
        };
        assert!(!hints.has_content());
        let terminal = render_full_view(80, 24, &[], Some(hints));
        let row1 = grid_row(&terminal, 1);
        assert!(row1.trim().is_empty(), "row 2 blank on runner: {row1:?}");
    }

    /// The empty-state tour renders the current carousel page beneath the
    /// logo (ADR-0104) — no static tagline, no dot indicator.
    #[test]
    fn empty_state_tour_renders_the_current_carousel_page() {
        let theme = Theme::default();
        let mut terminal = mutx_engine::TestTerminal::new(80, 24);
        let messages: Vec<TranscriptMessage> = Vec::new();
        terminal.draw(|f| {
            let _ = draw_transcript(
                f,
                &mut LayoutMap::new(),
                TranscriptView {
                    messages: &messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    runner_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 2,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            );
        });
        let buffer = terminal.buffer();
        let width = buffer.area().width as usize;
        let all: Vec<String> = (0..buffer.area().height)
            .map(|y| (0..width).map(|x| buffer[(x as u16, y)].symbol()).collect())
            .collect();
        let joined = all.join("\n");
        // The static tagline is retired (ADR-0104): the carousel's first
        // page already answers "how do I start", so no duplicate line.
        assert!(
            !joined.contains("Type a message below to begin."),
            "no static tagline: {joined}"
        );
        // Page 2 of the tour is the /btw page.
        assert!(joined.contains("/btw"), "page 2 visible: {joined}");
        // No dot indicator row (ADR-0104): the carousel is a single line and
        // the rotation is self-explaining.
        assert!(!joined.contains('●'), "no dot indicator anywhere: {joined}");
    }

    /// An H1 heading renders with an UNDERLINED modifier. The underline must
    /// cover only the prefix + text cells and must not bleed into the trailing
    /// whitespace of the heading row. Inspects the rendered grid cells
    /// directly to pin the clamp in `draw_message_body`.
    #[test]
    fn h1_underline_clamps_to_text_extent() {
        let theme = Theme::default();
        let mut terminal = mutx_engine::TestTerminal::new(60, 12);
        let messages = vec![TranscriptMessage::new(
            muta_contracts::Role::Assistant,
            "# QQ_H1_TEST\n\nbody text here\n",
        )];
        terminal.draw(|f| {
            let _ = draw_transcript(
                f,
                &mut LayoutMap::new(),
                TranscriptView {
                    messages: &messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    runner_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            );
        });
        let buffer = terminal.buffer();
        let width = buffer.area().width;
        let underline = mutx_engine::Modifier::UNDERLINE;

        let mut head = None;
        'outer: for y in 0..buffer.area().height {
            for x in 0..width {
                if buffer[(x, y)].symbol() == "Q" {
                    head = Some((x, y));
                    break 'outer;
                }
            }
        }
        let (hx, hy) = head.expect("heading 'Q' cell exists");

        // "QQ_H1_TEST" is 10 cells; prefix is 3 cells. All 13 are underlined.
        for x in hx..hx + 10 {
            assert!(
                buffer[(x, hy)].style.add.contains(underline),
                "heading text cell at x={x} must be UNDERLINED"
            );
        }
        let trailing = hx + 10;
        assert!(trailing < width, "trailing cell within grid");
        assert!(
            !buffer[(trailing, hy)].style.add.contains(underline),
            "underline must not bleed into trailing whitespace at x={trailing}"
        );
        assert!(
            !buffer[(width - 1, hy)].style.add.contains(underline),
            "underline must not reach the right edge"
        );
    }

    /// Same clamp check with a multi-codepoint emoji grapheme (ZWJ family) in
    /// the heading: `wrap_text` measures per-char (overcounting the sequence)
    /// while the grid renders per-grapheme, so this guards the underline width
    /// against the char-vs-grapheme measurement split.
    #[test]
    fn h1_underline_clamps_with_emoji_grapheme() {
        let theme = Theme::default();
        let mut terminal = mutx_engine::TestTerminal::new(60, 12);
        let messages = vec![TranscriptMessage::new(
            muta_contracts::Role::Assistant,
            "# 👨‍👩‍👧 OKX\n\nbody\n",
        )];
        terminal.draw(|f| {
            let _ = draw_transcript(
                f,
                &mut LayoutMap::new(),
                TranscriptView {
                    messages: &messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    runner_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            );
        });
        let buffer = terminal.buffer();
        let width = buffer.area().width;
        let underline = mutx_engine::Modifier::UNDERLINE;

        let mut x_pos = None;
        'outer: for y in 0..buffer.area().height {
            for x in 0..width {
                if buffer[(x, y)].symbol() == "X" {
                    x_pos = Some((x, y));
                    break 'outer;
                }
            }
        }
        let (xx, xy) = x_pos.expect("heading 'X' cell exists");

        assert!(
            buffer[(xx, xy)].style.add.contains(underline),
            "heading 'X' text cell must be UNDERLINED"
        );
        let trailing = xx + 1;
        assert!(trailing < width, "trailing cell within grid");
        assert!(
            !buffer[(trailing, xy)].style.add.contains(underline),
            "underline must not bleed past emoji heading at x={trailing}"
        );
    }

    /// A wide (emoji) glyph in an H1 heading occupies a head cell plus a
    /// wide-continuation cell. The grid stores the continuation without the
    /// `add` modifiers (it is a non-emitted placeholder), but the diff skips
    /// continuations and emits the head's run style — so the backend prints
    /// the wide glyph while the UNDERLINED SGR is active, underlining both
    /// columns. This pins that emitted behavior at the `Draw`-command layer.
    #[test]
    fn h1_underline_emits_wide_glyph_in_underlined_run() {
        let theme = Theme::default();
        let width = 60u16;
        let mut terminal = mutx_engine::TestTerminal::new(width, 12);
        let messages = vec![TranscriptMessage::new(
            muta_contracts::Role::Assistant,
            "# Hello😀\n\nbody\n",
        )];
        terminal.draw(|f| {
            let _ = draw_transcript(
                f,
                &mut LayoutMap::new(),
                TranscriptView {
                    messages: &messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    runner_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            );
        });
        let back = terminal.buffer();
        let front = mutx_engine::Grid::new(width, 12);
        let cmd = mutx_engine::diff::diff(back, &front);
        let underline = mutx_engine::Modifier::UNDERLINE;

        let wide_run_style = cmd.draws.iter().find_map(|d| match d {
            mutx_engine::Draw::Cells { style, cells, .. } => cells
                .iter()
                .any(|(sym, w)| sym == "😀" && *w == 2)
                .then_some(*style),
            _ => None,
        });
        let style =
            wide_run_style.expect("a Draw::Cells run containing wide glyph '😀' must be emitted");
        assert!(
            style.add.contains(underline),
            "wide glyph '😀' must be emitted in an UNDERLINED run so the terminal \
             underlines both columns, got add={:?}",
            style.add,
        );
    }

    /// Regression: a long H1 heading that wraps to multiple lines. The heading
    /// *prefix* (the leading indent on row 0 and the continuation indent
    /// on rows 1+) is decoration, not heading text, so it must NOT carry the
    /// UNDERLINED modifier. Previously the prefix shared the UNDERLINED style,
    /// which underlined the leading whitespace of every wrapped row — the
    /// underline appeared to "cross the line head" and cover the blank indent.
    ///
    /// We render a heading that wraps to ≥2 rows and assert that, on every
    /// row, the underline begins exactly at the text column (prefix width) and
    /// that the indent columns themselves are never underlined. The trailing
    /// blank columns must also stay un-underlined (the existing clamp).
    #[test]
    fn h1_underline_excludes_prefix_indent_on_wrapped_rows() {
        let theme = Theme::default();
        // Use a terminal at/above the render minimum so `draw_transcript` does
        // not trip its too-small guard. A 76-column transcript band still
        // wraps this ~95-char heading to ≥2 rows.
        let mut terminal = mutx_engine::TestTerminal::new(80, 24);
        let messages = vec![TranscriptMessage::new(
            muta_contracts::Role::Assistant,
            "# This is a very long heading that intentionally wraps to multiple rows for the underline-prefix test\n\nbody\n",
        )];
        terminal.draw(|f| {
            let _ = draw_transcript(
                f,
                &mut LayoutMap::new(),
                TranscriptView {
                    messages: &messages,
                    scroll: 0,
                    selection: &SelectionState::None,
                    cell_selection: None,
                    activity: "",
                    awaiting_permission: false,
                    spinner_phase: 0,
                    input: "",
                    byte_cursor: 0,
                    chrome_hidden: false,
                    queue_bar: QueueBarView {
                        items: &[],
                        paused: false,
                        blocked: false,
                    },
                    runner_bar: None,
                    side_banner: None,
                    page_hints: None,
                    session_head: None,
                    todos: None,
                    round_started_at: None,
                    hovered_step: None,
                    focused_target: None,
                    logo: None,
                    guidance: EmptyStateGuidance::Tour,
                    carousel_index: 0,
                    theme: &theme,
                    layout: crate::layout::Strategy::default(),
                    height_cache: None,
                },
            );
        });
        let buffer = terminal.buffer();
        let width = buffer.area().width;
        let underline = mutx_engine::Modifier::UNDERLINE;

        // The heading prefix is "   " (3 columns); locate the heading's rows
        // as the contiguous non-blank rows at the top (before the blank gap +
        // body). The heading "This is a very long heading that wraps to
        // multiple lines" wraps to several rows here.
        let mut heading_rows: Vec<u16> = Vec::new();
        let mut found_body = false;
        for y in 0..buffer.area().height {
            let row_has_text = (0..width).any(|x| buffer[(x, y)].symbol() != " ");
            if !row_has_text {
                if !heading_rows.is_empty() {
                    found_body = true;
                }
                continue;
            }
            if found_body {
                break;
            }
            heading_rows.push(y);
        }
        assert!(
            heading_rows.len() >= 2,
            "heading must wrap to at least 2 rows, got {}",
            heading_rows.len()
        );

        for &y in &heading_rows {
            // Indent columns [0, text_start) must never be underlined.
            // The heading prefix is `TRANSCRIPT_BODY_LEADING_INDENT` cols
            // (matching body prose — see the `Block::Heading` arm), applied
            // inside the already-inset band: entry inset (TRANSCRIPT_H_INSET)
            // + heading prefix (TRANSCRIPT_BODY_LEADING_INDENT). Text starts
            // at col `TRANSCRIPT_H_INSET + TRANSCRIPT_BODY_LEADING_INDENT`.
            let text_start = super::TRANSCRIPT_H_INSET + super::TRANSCRIPT_BODY_LEADING_INDENT;
            for x in 0..text_start {
                let cell = &buffer[(x, y)];
                assert!(
                    !cell.style.add.contains(underline),
                    "indent cell at (x={x}, y={y}) must NOT be underlined \
                     (it is heading decoration, not text), symbol={:?}",
                    cell.symbol(),
                );
            }
            // The trailing blank tail (rightmost column) must not be underlined.
            let last = width - 1;
            assert!(
                !buffer[(last, y)].style.add.contains(underline),
                "trailing cell at (x={last}, y={y}) must NOT be underlined"
            );
            // And at least the first text column must be underlined (the
            // heading text itself is still underlined).
            let first_text_cell = &buffer[(text_start, y)];
            assert!(
                first_text_cell.style.add.contains(underline),
                "first heading-text cell at (x={text_start}, y={y}) must be UNDERLINED, \
                 symbol={:?}",
                first_text_cell.symbol(),
            );
        }
    }
