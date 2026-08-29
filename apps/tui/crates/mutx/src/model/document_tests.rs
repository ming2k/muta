//! Tests for the transcript document model and markdown parser.

use super::*;

#[test]
fn test_parse_simple_text() {
    let blocks = parse_blocks("Hello world");
    assert_eq!(blocks.len(), 1);
    assert!(matches!(&blocks[0], Block::Text(inline) if inline.content == "Hello world"));
}

#[test]
fn test_parse_code_block() {
    let text = "Some text\n\n```rust\nfn main() {}\n```\n\nMore text";
    let blocks = parse_blocks(text);
    assert_eq!(blocks.len(), 5);
    assert!(matches!(&blocks[0], Block::Text(inline) if inline.content == "Some text"));
    assert!(
        matches!(&blocks[2], Block::Code { language, content } if language.as_deref() == Some("rust") && content == "fn main() {}")
    );
    assert!(matches!(&blocks[4], Block::Text(inline) if inline.content == "More text"));
}

#[test]
fn inline_code_keeps_its_backtick_quotes_in_prose() {
    // Inline code keeps its backtick delimiters in the flattened content
    // so the rendered/copied paragraph still shows the quotes, and the
    // renderer can paint the span on the code surface. This holds across
    // paragraph / heading / list item / quote contexts.
    let blocks = parse_blocks("Call the `read_text` tool.");
    assert!(matches!(
        &blocks[0],
        Block::Text(inline) if inline.content == "Call the `read_text` tool."
    ));

    // Heading.
    let blocks = parse_blocks("# Use `list_dir` for directories");
    assert!(matches!(
        &blocks[0],
        Block::Heading { level: 1, inline } if inline.content == "Use `list_dir` for directories"
    ));

    // List item.
    let blocks = parse_blocks("- item with `code` inside");
    assert!(matches!(
        &blocks[0],
        Block::ListItem { inline, .. } if inline.content == "item with `code` inside"
    ));

    // Blockquote.
    let blocks = parse_blocks("> quoted `code` span");
    assert!(matches!(
        &blocks[0],
        Block::Quote(inline) if inline.content == "quoted `code` span"
    ));

    // Multiple inline spans in one paragraph, mixed with emphasis.
    let blocks = parse_blocks("Mix `a` and `b` and plain.");
    assert!(matches!(
        &blocks[0],
        Block::Text(inline) if inline.content == "Mix `a` and `b` and plain."
    ));
}

/// Helper: find the byte range of the first `` `…` `` run in `s`, matching
/// what the parser records, so the `code_ranges` assertions below can be
/// written against the literal content rather than hand-counted offsets.
fn code_ranges_of(s: &str) -> Vec<CodeRange> {
    let mut ranges = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            // find the closing backtick
            if let Some(rel) = s[i + 1..].find('`') {
                ranges.push((i, i + 1 + rel + 1));
                i = i + 1 + rel + 1;
                continue;
            }
        }
        i += 1;
    }
    ranges
}

#[test]
fn parses_inline_math_and_http_links_outside_code() {
    let text = "Use $x^2$ and [Rust](https://www.rust-lang.org), not `https://ignored.test`.";
    let blocks = parse_blocks(text);
    let Block::Text(inline) = &blocks[0] else {
        panic!("expected text block");
    };
    assert_eq!(inline.math_ranges, vec![(4, 9)]);
    assert_eq!(inline.code_ranges.len(), 1);
    assert_eq!(inline.link_ranges.len(), 1);
    assert_eq!(inline.link_ranges[0].label_range, (15, 19));
    assert_eq!(inline.link_ranges[0].url, "https://www.rust-lang.org");
}

#[test]
fn parses_display_math_blocks() {
    let blocks = parse_blocks("Before\n\n$$\n\\int_0^\\infty e^{-x} dx = 1\n$$\n\nAfter");
    assert!(matches!(&blocks[0], Block::Text(inline) if inline.content == "Before"));
    assert!(matches!(&blocks[2], Block::Math { content } if content.contains("\\int_0^\\infty")));
    assert!(matches!(&blocks[4], Block::Text(inline) if inline.content == "After"));
}

#[test]
fn inline_code_records_byte_ranges_for_every_prose_context() {
    // Paragraph: the run is `read_text` including both backticks.
    let text = "Call the `read_text` tool.";
    let expected = code_ranges_of(text);
    let blocks = parse_blocks(text);
    let Block::Text(inline) = &blocks[0] else {
        panic!("expected Text block, got {:?}", blocks[0]);
    };
    assert_eq!(inline.content, text);
    assert_eq!(inline.code_ranges, expected);

    // Heading.
    let text = "Use `list_dir` for directories";
    let expected = code_ranges_of(text);
    let blocks = parse_blocks(&format!("# {text}"));
    let Block::Heading { inline, .. } = &blocks[0] else {
        panic!("expected Heading block, got {:?}", blocks[0]);
    };
    assert_eq!(inline.content, text);
    assert_eq!(inline.code_ranges, expected);

    // List item.
    let text = "item with `code` inside";
    let expected = code_ranges_of(text);
    let blocks = parse_blocks(&format!("- {text}"));
    let Block::ListItem { inline, .. } = &blocks[0] else {
        panic!("expected ListItem block, got {:?}", blocks[0]);
    };
    assert_eq!(inline.content, text);
    assert_eq!(inline.code_ranges, expected);

    // Blockquote.
    let text = "quoted `code` span";
    let expected = code_ranges_of(text);
    let blocks = parse_blocks(&format!("> {text}"));
    let Block::Quote(inline) = &blocks[0] else {
        panic!("expected Quote block, got {:?}", blocks[0]);
    };
    assert_eq!(inline.content, text);
    assert_eq!(inline.code_ranges, expected);

    // Multiple spans → multiple, non-overlapping, ordered ranges.
    let text = "Mix `a` and `b` and plain.";
    let expected = code_ranges_of(text);
    let blocks = parse_blocks(text);
    let Block::Text(inline) = &blocks[0] else {
        panic!("expected Text block");
    };
    assert_eq!(inline.code_ranges, expected);
}

#[test]
fn test_push_stream() {
    let mut streamed = TranscriptMessage::new(Role::Assistant, "");
    for chunk in [
        "# Result\n\n",
        "First paragraph.\n\n",
        "- one\n",
        "- two\n\n",
        "```rust\nfn main() {}\n```",
    ] {
        streamed.push_stream(chunk);
    }

    let completed = TranscriptMessage::new(Role::Assistant, streamed.raw.clone());
    assert_eq!(streamed.blocks, completed.blocks);
}

#[test]
fn parses_block_boundaries_without_collapsing_the_document() {
    let blocks = parse_blocks(
        "# Result\n\nFirst paragraph.\n\nSecond paragraph.\n\n1. one\n2. two\n\n> quoted",
    );

    assert!(matches!(
        &blocks[0],
        Block::Heading { level: 1, inline } if inline.content == "Result"
    ));
    assert!(blocks.iter().any(|block| matches!(block, Block::Break)));
    assert!(
        blocks.iter().any(
            |block| matches!(block, Block::Text(inline) if inline.content == "First paragraph.")
        )
    );
    assert!(blocks.iter().any(
        |block| matches!(block, Block::Text(inline) if inline.content == "Second paragraph.")
    ));
    assert!(blocks.iter().any(|block| matches!(
        block,
        Block::ListItem {
            inline,
            ordered: Some(1),
            ..
        } if inline.content == "one"
    )));
    assert!(
        blocks
            .iter()
            .any(|block| matches!(block, Block::Quote(inline) if inline.content == "quoted"))
    );
}

#[test]
fn headings_are_visually_separated_from_following_body_text() {
    let blocks = parse_blocks("# Result\nFirst paragraph.");

    assert!(matches!(&blocks[0], Block::Heading { inline, .. } if inline.content == "Result"));
    assert!(
        matches!(&blocks[1], Block::Break),
        "heading-to-text boundaries should render with a blank row"
    );
    assert!(matches!(&blocks[2], Block::Text(inline) if inline.content == "First paragraph."));
}

#[test]
fn markdown_soft_breaks_flow_but_hard_breaks_are_preserved() {
    let soft = parse_blocks("alpha bravo\ncharlie delta");
    assert!(matches!(
        &soft[0],
        Block::Text(inline) if inline.content == "alpha bravo charlie delta"
    ));

    let hard = parse_blocks("alpha bravo  \ncharlie delta");
    assert!(matches!(
        &hard[0],
        Block::Text(inline) if inline.content == "alpha bravo\ncharlie delta"
    ));
}

#[test]
fn parses_task_lists_and_tables() {
    let blocks =
        parse_blocks("- [x] done\n- [ ] next\n\n| Name | State |\n| --- | --- |\n| muta | ready |");

    assert!(blocks.iter().any(|block| matches!(
        block,
        Block::ListItem {
            checked: Some(true),
            inline,
            ..
        } if inline.content == "done"
    )));
    assert!(blocks.iter().any(|block| matches!(
        block,
        Block::ListItem {
            checked: Some(false),
            inline,
            ..
        } if inline.content == "next"
    )));
    let table = blocks.iter().find_map(|block| match block {
        Block::Table { headers, rows, .. } => Some((headers, rows)),
        _ => None,
    });
    let (headers, rows) = table.expect("table block present");
    assert_eq!(headers, &["Name".to_string(), "State".to_string()]);
    assert_eq!(rows, &[vec!["muta".to_string(), "ready".to_string()]]);

    // The rendered grid must align columns and separate the header from
    // the body, the regression that motivated reintroducing Block::Table.
    let rendered = blocks
        .iter()
        .find_map(|block| match block {
            Block::Table { rendered, .. } => Some(rendered.as_str()),
            _ => None,
        })
        .expect("rendered table text");
    assert!(rendered.contains("┌"), "missing top border: {rendered}");
    assert!(
        rendered.contains("├"),
        "missing header/body separator: {rendered}"
    );
    // Pipes must line up: the header and data rows share the same `│`
    // positions, so splitting on `│` yields the same number of pieces.
    let pipes = |line: &str| line.matches('│').count();
    let header_line = rendered.lines().nth(1).unwrap();
    let data_line = rendered.lines().nth(3).unwrap();
    assert_eq!(
        pipes(header_line),
        pipes(data_line),
        "header and body rows must align: {rendered}"
    );
}

#[test]
fn table_alignment_and_uneven_cells_line_up() {
    let blocks =
        parse_blocks("| Tool | Count |\n| :--- | ---: |\n| read | 1 |\n| webfetch | 250 |");
    let rendered = blocks
        .iter()
        .find_map(|block| match block {
            Block::Table {
                rendered, aligns, ..
            } => Some((rendered.as_str(), aligns.clone())),
            _ => None,
        })
        .expect("table block");
    let (rendered, aligns) = rendered;
    assert_eq!(
        aligns,
        vec![TableAlignment::Left, TableAlignment::Right],
        "alignment must be captured: {rendered}"
    );
    // Right-aligned numeric column: digits hug the right border, so the
    // single-digit "1" gets more left padding than "250" does.
    let data_lines: Vec<&str> = rendered.lines().skip(3).take(2).collect();
    assert!(
        data_lines[0].ends_with("│     1 │"),
        "got: {}",
        data_lines[0]
    );
    assert!(
        data_lines[1].ends_with("│   250 │"),
        "got: {}",
        data_lines[1]
    );
}

/// GFM fixes the table column count from the header, so every body row in
/// a `Block::Table` must be normalized to exactly `headers.len()` cells:
/// short rows padded with empty strings, over-wide rows truncated. This is
/// the invariant the live renderer indexes against; a ragged row used to
/// panic `build_table_render` with an out-of-bounds index.
#[test]
fn table_normalizes_ragged_body_rows_to_header_width() {
    // 2-column header; body rows have 2, 1, and 3 cells respectively.
    let blocks = parse_blocks("| A | B |\n|---|---|\n| 1 | 2 |\n| 3 |\n| 4 | 5 | 6 |");
    let (headers, rows) = blocks
        .iter()
        .find_map(|block| match block {
            Block::Table { headers, rows, .. } => Some((headers.clone(), rows.clone())),
            _ => None,
        })
        .expect("table block present");
    let ncols = headers.len();
    assert_eq!(ncols, 2, "header defines 2 columns");
    assert!(
        rows.iter().all(|row| row.len() == ncols),
        "every body row must be normalized to {ncols} cells, got {rows:?}"
    );
    // Short rows are padded with empty cells, the over-wide row truncated.
    assert_eq!(rows[0], vec!["1".to_string(), "2".to_string()]);
    assert_eq!(rows[1], vec!["3".to_string(), String::new()]);
    assert_eq!(rows[2], vec!["4".to_string(), "5".to_string()]);
}

#[test]
fn tool_step_collapses_and_restores_full_semantic_detail() {
    let mut message =
        TranscriptMessage::tool_step("call_1", "read_text", r#"{"path":"README.md"}"#);
    // Collapsed running: human-readable summary only — no tool name.
    assert!(message.raw.contains("Read README.md"));
    assert!(!message.raw.contains("read_text"));

    assert!(message.finish_tool_step(
        "call_1",
        "contents",
        muta_contracts::ToolOutput::text("contents"),
        1234
    ));
    // Collapsed completed: summary + duration suffix.
    assert!(message.raw.contains("Read README.md"));
    assert!(message.raw.contains("1.2s"));
    message.set_tool_step_expanded(true);

    // Expanded: arguments as compact key-value text + output verbatim.
    assert!(message.raw.contains("path: README.md"));
    assert!(message.raw.contains("contents"));
}

#[test]
fn runner_task_is_detected_and_addressable() {
    let task = TranscriptMessage::tool_step(
        "call_42",
        "runner",
        r#"{"description":"explore src","prompt":"..."}"#,
    );
    assert!(task.is_runner_task());
    assert_eq!(task.tool_step_call_id(), Some("call_42"));
    assert_eq!(task.runner_children().map(|c| c.len()), Some(0));
    assert_eq!(task.runner_description(), "explore src");
    assert_eq!(task.runner_role(), None);

    // A regular tool step is not an runner task.
    let read = TranscriptMessage::tool_step("call_1", "read_text", r#"{"path":"a"}"#);
    assert!(!read.is_runner_task());
    assert!(read.runner_status_line().is_none());
}

#[test]
fn runner_started_event_labels_step_by_role() {
    // A `Started` event stamps the bound profile name on the step so the
    // page header can read the role out as its `[ROLE]` tag.
    let mut task = TranscriptMessage::tool_step(
        "call_7",
        "runner",
        r#"{"description":"write the plan","prompt":"..."}"#,
    );
    assert_eq!(task.runner_description(), "write the plan");
    assert_eq!(task.runner_role(), None);
    assert!(
        task.push_runner_event(&muta_contracts::RunnerEvent::Started {
            profile: "explore".to_string()
        })
    );
    assert_eq!(task.runner_role().as_deref(), Some("explore"));
    assert_eq!(task.runner_description(), "write the plan");
    // The collapsed header carries only the description — the role is
    // shown by the renderer's `[PROFILE]` badge in front of it.
    let header = task.tool_step_summary().expect("summary");
    assert_eq!(header, "write the plan");
}

#[test]
fn runner_status_reflects_children_and_completion() {
    let mut task =
        TranscriptMessage::tool_step("call_9", "runner", r#"{"description":"d","prompt":"p"}"#);

    // No children yet, still running — the peek row opens with the
    // generic `running` state until the runner reports more.
    let running = task.runner_status_line().expect("running status");
    assert!(running.starts_with("running"), "got: {running}");

    // A reported activity line (e.g. during the first model call) is
    // surfaced so the row reads as alive, not stuck on a bare state.
    task.push_runner_event(&RunnerEvent::Activity("waiting for model".into()));
    let waiting = task.runner_status_line().expect("waiting status");
    assert!(
        waiting.starts_with("running waiting for model"),
        "got: {waiting}"
    );

    // Streaming assistant text => the peek row reports `thinking`.
    task.push_runner_event(&RunnerEvent::StreamStart { round: 1, turn: 0 });
    task.push_runner_event(&RunnerEvent::StreamDelta("partial".into()));
    let thinking = task.runner_status_line().expect("thinking status");
    assert!(thinking.starts_with("running thinking"), "got: {thinking}");

    // An in-flight child tool call surfaces the tool's header.
    task.push_runner_event(&RunnerEvent::ToolCall {
        id: "inner".into(),
        name: "search_text".into(),
        arguments: r#"{"query":"foo"}"#.into(),
        round: 1,
        turn: 0,
    });
    let running = task.runner_status_line().expect("running status");
    assert!(running.contains("Search"), "got: {running}");

    // Completing the parent hides the peek row; the outcome row takes over
    // with the runner's one-line conclusion.
    assert!(task.finish_tool_step(
        "call_9",
        "final answer",
        muta_contracts::ToolOutput::text("final answer"),
        1500
    ));
    assert!(
        task.runner_status_line().is_none(),
        "the peek row must disappear once the runner terminates"
    );
    assert_eq!(
        task.runner_outcome_line().as_deref(),
        Some("final answer"),
        "the outcome row carries the runner's conclusion"
    );

    // Children are accessible for the dedicated runner view.
    assert_eq!(task.runner_children().map(|c| c.len()), Some(2));
}

#[test]
fn runner_failed_status_reports_failure() {
    let mut task =
        TranscriptMessage::tool_step("c", "runner", r#"{"description":"d","prompt":"p"}"#);
    task.push_runner_event(&RunnerEvent::ToolCall {
        id: "i".into(),
        name: "execute_command".into(),
        arguments: "{}".into(),
        round: 1,
        turn: 0,
    });
    // The runner failure is now signalled by the structured `failed`
    // flag on `ToolOutput::Runner`, not by an "Error:" text prefix.
    let structured = muta_contracts::ToolOutput::Runner {
        summary: "Error: boom".into(),
        messages: Vec::new(),
        usage: muta_contracts::TokenUsage::default(),
        generation_ms: 0,
        failed: true,
        interrupted: false,
    };
    assert!(task.finish_tool_step("c", structured.to_text(), structured, 100));
    assert!(
        task.runner_status_line().is_none(),
        "a terminal runner hides the peek row"
    );
    // The outcome row surfaces the error summary's first line.
    assert_eq!(task.runner_outcome_line().as_deref(), Some("Error: boom"));
}

#[test]
fn runner_peek_reports_awaiting_approval_while_parked() {
    let mut task =
        TranscriptMessage::tool_step("c", "runner", r#"{"description":"d","prompt":"p"}"#);
    task.push_runner_event(&RunnerEvent::ToolCall {
        id: "i".into(),
        name: "execute_command".into(),
        arguments: r#"{"command":"rm -rf x"}"#.into(),
        round: 1,
        turn: 0,
    });
    // The in-flight tool normally drives the peek row…
    let peek = task.runner_status_line().unwrap();
    assert!(peek.starts_with("running Run rm"), "got: {peek}");

    // …but a parked permission request takes over the row: the runner is
    // blocked on a human, not making progress.
    task.push_runner_event(&RunnerEvent::PermissionRequest(
        muta_contracts::PermissionRequest {
            id: "p1".into(),
            tool: "execute_command".into(),
            label: "Run rm".into(),
            description: String::new(),
            arguments: "{}".into(),
            scope: "workspace".into(),
            elevation: false,
            one_off: false,
            origin: None,
            ..Default::default()
        },
    ));
    let peek = task.runner_status_line().unwrap();
    assert!(peek.starts_with("awaiting approval"), "got: {peek}");

    // The next progress event from the runner clears the parked wait.
    task.push_runner_event(&RunnerEvent::ToolResult {
        id: "i".into(),
        name: "execute_command".into(),
        output: "done".into(),
        duration_ms: 3,
    });
    task.push_runner_event(&RunnerEvent::StreamStart { round: 1, turn: 0 });
    task.push_runner_event(&RunnerEvent::StreamDelta("…".into()));
    let peek = task.runner_status_line().unwrap();
    assert!(peek.starts_with("running thinking"), "got: {peek}");
}

#[test]
fn interrupted_runner_status_reports_interrupted_not_failed() {
    let mut task =
        TranscriptMessage::tool_step("c", "runner", r#"{"description":"d","prompt":"p"}"#);
    task.push_runner_event(&RunnerEvent::ToolCall {
        id: "i".into(),
        name: "read_text".into(),
        arguments: "{}".into(),
        round: 1,
        turn: 0,
    });
    task.push_runner_event(&RunnerEvent::ToolResult {
        id: "i".into(),
        name: "read_text".into(),
        output: "found 1 of 3 handlers".into(),
        duration_ms: 5,
    });
    // An interrupted runner carries `interrupted: true, failed: false`:
    // the partial work was preserved, so it must classify as Interrupted
    // — never as Failed (it did not error) and never as Ok (it did not
    // finish).
    let structured = muta_contracts::ToolOutput::Runner {
        summary: "Interrupted: stopped by the user".into(),
        messages: Vec::new(),
        usage: muta_contracts::TokenUsage::default(),
        generation_ms: 0,
        failed: false,
        interrupted: true,
    };
    assert!(task.finish_tool_step("c", structured.to_text(), structured, 100));
    assert_eq!(
        task.tool_step_status(),
        Some(ToolStepStatus::Interrupted),
        "an interrupted runner classifies as Interrupted"
    );
    assert!(
        task.runner_status_line().is_none(),
        "a terminal runner hides the peek row"
    );
    assert_eq!(
        task.runner_outcome_line().as_deref(),
        Some("Interrupted: stopped by the user"),
        "the outcome row carries the interruption summary"
    );
}

#[test]
fn bash_failure_is_classified_failed_from_structured_exit_code() {
    // Regression: a bash failure emits `Exit N …` which does NOT start with
    // "Error", so the legacy text sniff misclassified it as `Ok`. With
    // structured `ToolOutput::Shell { exit: Some(1) }`, `is_error()` now
    // drives the classification and the step correctly reads `Failed`.
    let mut step = TranscriptMessage::tool_step("c", "execute_command", r#"{"command":"false"}"#);
    let structured = muta_contracts::ToolOutput::Shell {
        command: "false".into(),
        stdout: String::new(),
        stderr: "boom".into(),
        lines: Vec::new(),
        exit: Some(1),
        truncated: false,
        termination: muta_contracts::tool_output::ShellTermination::Exited,
    };
    let text = structured.to_text();
    assert!(
        !text.starts_with("Error"),
        "precondition: text is not Error-prefixed"
    );
    assert!(step.finish_tool_step("c", text, structured, 50));
    assert_eq!(step.tool_step_status(), Some(ToolStepStatus::Failed));
}

#[test]
fn bash_success_is_classified_ok() {
    let mut step = TranscriptMessage::tool_step("c", "execute_command", r#"{"command":"true"}"#);
    let structured = muta_contracts::ToolOutput::Shell {
        command: "true".into(),
        stdout: "ok\n".into(),
        stderr: String::new(),
        lines: Vec::new(),
        exit: Some(0),
        truncated: false,
        termination: muta_contracts::tool_output::ShellTermination::Exited,
    };
    let text = structured.to_text();
    assert!(step.finish_tool_step("c", text, structured, 5));
    assert_eq!(step.tool_step_status(), Some(ToolStepStatus::Ok));
}

#[test]
fn push_tool_stream_builds_interleaved_lines_for_live_view() {
    // L5: the streaming seed must populate `lines` (with the right stream
    // tag each) so the live view renders arrival-ordered, stderr-tinted,
    // interleaved output — not the all-stdout-then-all-stderr degraded
    // band the empty-`lines` fallback forced.
    use muta_contracts::{ToolStream, tool_output::ShellStream};
    let mut step = TranscriptMessage::tool_step("c", "execute_command", r#"{"command":"x"}"#);
    assert!(step.push_tool_stream("c", &ToolStream::Stdout("Compiling a\n".into())));
    assert!(step.push_tool_stream("c", &ToolStream::Stderr("warning: b\n".into())));
    assert!(step.push_tool_stream("c", &ToolStream::Stdout("Compiling c\n".into())));

    let lines = match &step.kind {
        MessageKind::ToolStep {
            structured: Some(b),
            ..
        } => match b.as_ref() {
            muta_contracts::ToolOutput::Shell { lines, .. } => lines,
            _ => panic!("expected Shell"),
        },
        _ => panic!("expected ToolStep"),
    };
    assert_eq!(
        lines
            .iter()
            .map(|l| (l.stream, l.text.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (ShellStream::Out, "Compiling a"),
            (ShellStream::Err, "warning: b"),
            (ShellStream::Out, "Compiling c"),
        ],
        "streaming seed must preserve arrival order + stream tags"
    );
    // The flat strings stay populated too (model-facing path).
    match step.kind {
        MessageKind::ToolStep {
            structured: Some(b),
            ..
        } => match b.as_ref() {
            muta_contracts::ToolOutput::Shell { stdout, stderr, .. } => {
                assert!(stdout.contains("Compiling a"));
                assert!(stdout.contains("Compiling c"));
                assert!(stderr.contains("warning: b"));
            }
            _ => unreachable!(),
        },
        _ => unreachable!(),
    }
}

#[test]
fn cancel_tool_step_transitions_to_a_terminal_state() {
    let mut step = TranscriptMessage::tool_step("call_1", "websearch", r#"{"query":"rust"}"#);
    // Running -> Cancelled is a real terminal transition.
    assert_eq!(step.tool_step_status(), Some(ToolStepStatus::Running));
    assert!(step.cancel_tool_step("call_1"));
    assert_eq!(step.tool_step_status(), Some(ToolStepStatus::Cancelled));

    // The summary advertises the cancelled state instead of staying blank.
    let summary = step.tool_step_summary().expect("summary");
    assert!(summary.contains("cancelled"), "got: {summary}");
    // The raw (collapsed) transcript line mirrors the summary.
    assert!(step.raw.contains("cancelled"), "got: {}", step.raw);

    // Cancelled is terminal: a late result or another cancel is ignored.
    assert!(!step.finish_tool_step(
        "call_1",
        "late result",
        muta_contracts::ToolOutput::text("late result"),
        10
    ));
    assert!(!step.cancel_tool_step("call_1"));
    assert_eq!(step.tool_step_status(), Some(ToolStepStatus::Cancelled));
}

#[test]
fn cancel_only_acts_on_the_matching_call_id() {
    let mut step = TranscriptMessage::tool_step("call_1", "websearch", "{}");
    // A different id does nothing and leaves the step running.
    assert!(!step.cancel_tool_step("call_9"));
    assert_eq!(step.tool_step_status(), Some(ToolStepStatus::Running));
}

#[test]
fn cancelling_a_runner_also_cancels_its_running_children() {
    let mut task =
        TranscriptMessage::tool_step("task_1", "runner", r#"{"description":"d","prompt":"p"}"#);
    // A nested tool call still in flight.
    task.push_runner_event(&RunnerEvent::ToolCall {
        id: "inner".into(),
        name: "search_text".into(),
        arguments: r#"{"query":"foo"}"#.into(),
        round: 1,
        turn: 0,
    });
    let children = task.runner_children().expect("has children");
    assert_eq!(
        children[0].tool_step_status(),
        Some(ToolStepStatus::Running)
    );

    // Interrupting the parent task cancels it AND the nested running child,
    // so the runner view never shows a stuck "running" step.
    assert!(task.cancel_tool_step("task_1"));
    assert_eq!(task.tool_step_status(), Some(ToolStepStatus::Cancelled));
    let children = task.runner_children().expect("has children");
    assert_eq!(
        children[0].tool_step_status(),
        Some(ToolStepStatus::Cancelled),
        "nested child must converge with the parent"
    );

    // A cancelled runner is terminal: the peek row disappears and the
    // outcome row falls back to the legacy output text (none was recorded
    // here, so the row hides entirely).
    assert!(task.runner_status_line().is_none());
    assert!(task.runner_outcome_line().is_none());
}

#[test]
fn cancel_all_running_is_a_defensive_sweep_that_skips_terminal_steps() {
    let mut a = TranscriptMessage::tool_step("a", "read_text", "{}");
    let mut b = TranscriptMessage::tool_step("b", "read_text", "{}");
    // `b` already finished successfully; the sweep must not clobber it.
    assert!(b.finish_tool_step(
        "b",
        "contents",
        muta_contracts::ToolOutput::text("contents"),
        5
    ));
    assert_eq!(b.tool_step_status(), Some(ToolStepStatus::Ok));

    // The sweep cancels a running step and is then a no-op on it.
    assert!(a.cancel_all_running());
    assert!(!a.cancel_all_running());
    assert_eq!(a.tool_step_status(), Some(ToolStepStatus::Cancelled));
    // A finished step is untouched by the sweep.
    assert!(!b.cancel_all_running());
    assert_eq!(b.tool_step_status(), Some(ToolStepStatus::Ok));
}

#[test]
fn notice_carries_severity_and_is_classified_as_notice() {
    let n = TranscriptMessage::notice(NoticeSeverity::Error, "boom");
    assert!(n.is_notice());
    assert!(matches!(
        n.kind,
        MessageKind::Notice {
            severity: NoticeSeverity::Error,
            ..
        }
    ));
    // The raw text is preserved verbatim for the renderer (no "Error: "
    // prefix injection — the glyph is the renderer's job).
    assert_eq!(n.raw, "boom");
    assert_eq!(n.notice_expanded(), Some(false));

    let mut mut_n = n.clone();
    mut_n.pin_notice_expanded(true);
    assert_eq!(mut_n.notice_expanded(), Some(true));

    // A text message is not a notice.
    let plain = TranscriptMessage::new(Role::Assistant, "hi");
    assert!(!plain.is_notice());
}

#[test]
fn notice_from_core_preserves_the_topic_and_detail_split() {
    // The two-part split agreed at the contract layer (topic vocabulary +
    // title/body detail) must survive the boundary into the transcript
    // model instead of being flattened to `raw` and re-parsed at render
    // time. `raw` still carries the flattened form for copy fidelity.
    let core = muta_contracts::AgentNotice::new(
        muta_contracts::NoticeKind::ProviderRetry,
        muta_contracts::NoticeSeverity::Error,
        "Retrying provider request (2/3)",
        muta_contracts::NoticeSource::Harness,
    )
    .with_body("Google HTTP 429 Too Many Requests: {\"error\":{\"code\":429}}");

    let msg = TranscriptMessage::notice_from_core(&core);

    let parts = msg.notice_parts().expect("core notices carry parts");
    assert_eq!(parts.topic.as_deref(), Some("provider"));
    assert_eq!(parts.title, "Retrying provider request (2/3)");
    assert_eq!(
        parts.detail.as_deref(),
        Some("Google HTTP 429 Too Many Requests: {\"error\":{\"code\":429}}")
    );
    // Flattened fallback stays byte-identical to `render_text()`.
    assert_eq!(msg.raw, core.render_text());

    // Local notices keep the parts-free legacy shape (renderer falls back to
    // its heuristic parse).
    let local = TranscriptMessage::notice(NoticeSeverity::Info, "compacted 12 messages");
    assert!(local.notice_parts().is_none());
}

#[test]
fn notice_topic_labels_cover_the_contract_vocabulary() {
    // Every wire `NoticeKind` maps to a predictable user-facing topic label;
    // the match must not grow stale as the contract evolves.
    for (kind, label) in [
        (muta_contracts::NoticeKind::ProviderRetry, "provider"),
        (muta_contracts::NoticeKind::NudgeInjected, "turn guard"),
        (muta_contracts::NoticeKind::ReviewAlert, "review"),
        (muta_contracts::NoticeKind::TrustChanged, "trust"),
        (muta_contracts::NoticeKind::CommandAck, "command"),
    ] {
        assert_eq!(notice_topic_label(kind), label);
    }
}

#[test]
fn user_message_origin_defaults_to_chat_and_can_be_overridden() {
    // A plain user message is a genuine chat prompt by default.
    let chat = TranscriptMessage::new(Role::User, "fix the bug");
    assert_eq!(chat.origin, UserMessageOrigin::Chat);

    // Slash commands and shell passthroughs tag themselves so the
    // Activity modal does not mistake them for the driving prompt.
    let slash = TranscriptMessage::new(Role::User, "/review working-tree")
        .with_origin(UserMessageOrigin::Slash);
    assert_eq!(slash.origin, UserMessageOrigin::Slash);

    let shell = TranscriptMessage::new(Role::User, "!ls -la").with_origin(UserMessageOrigin::Shell);
    assert_eq!(shell.origin, UserMessageOrigin::Shell);

    // with_origin is idempotent and does not depend on the text: a
    // genuine chat prompt that happens to start with '/' stays Slash only
    // when explicitly tagged, never inferred from text here.
    let explicit_chat =
        TranscriptMessage::new(Role::User, "/etc is a path").with_origin(UserMessageOrigin::Chat);
    assert_eq!(explicit_chat.origin, UserMessageOrigin::Chat);
}

#[test]
fn round_interrupt_creates_structured_notice() {
    use muta_contracts::{RoundInterrupt, RoundInterruptReason};
    let marker = TranscriptMessage::round_interrupted(RoundInterrupt {
        reason: RoundInterruptReason::User,
        round: Some(3),
        at_ms: 1_000,
    });
    assert!(marker.is_notice());
    assert!(marker.is_round_interrupt());
    assert_eq!(marker.raw, "Round 3 — cancelled via [Esc Esc]");
    let MessageKind::Notice { ref parts, .. } = marker.kind else {
        panic!("must be notice");
    };
    let parts = parts.as_ref().expect("must have parts");
    assert_eq!(parts.topic.as_deref(), Some("interrupted"));
    assert_eq!(
        parts.origin,
        Some(crate::model::document::NoticeOrigin::System {
            topic: crate::model::document::SystemNoticeTopic::Interrupted
        })
    );
}

#[test]
fn command_result_populates_command_kind() {
    let harness_cmd = TranscriptMessage::pending_command("review", "HEAD~1");
    assert!(matches!(
        harness_cmd.command_kind(),
        Some(crate::model::document::CommandKind::Harness { name, args }) if name == "review" && args == "HEAD~1"
    ));
    assert_eq!(harness_cmd.raw, "/review HEAD~1");

    let shell_cmd = TranscriptMessage::pending_command("shell", "!cargo test");
    assert!(matches!(
        shell_cmd.command_kind(),
        Some(crate::model::document::CommandKind::Shell { command }) if command == "!cargo test"
    ));
    assert_eq!(shell_cmd.raw, "!cargo test");
}

#[test]
fn notice_strips_terminal_controls_from_crlf_http_errors() {
    let n = TranscriptMessage::notice(
        NoticeSeverity::Error,
        "OpenAI HTTP 504: <html>\r\n<head>timeout</head>\x1b[2J\r\n</html>",
    );

    assert_eq!(
        n.raw,
        "OpenAI HTTP 504: <html>\n<head>timeout</head>[2J\n</html>"
    );
    assert!(!n.raw.chars().any(|c| c.is_control() && c != '\n'));
}

/// Streaming thinking rows read as a live token count (`Thinking · N
/// tokens`), not an estimate; finished traces settle to the final line with
/// the duration.
#[test]
fn thinking_summary_sprays_tokens_then_settles() {
    // Short stream: exact per-token count.
    let streaming = TranscriptMessage::thinking("one two three four five");
    let summary = streaming.thinking_summary().unwrap();
    assert!(summary.starts_with("Thinking · "), "got: {summary}");
    assert!(summary.ends_with(" tokens"), "got: {summary}");
    assert!(!summary.contains('~'), "no estimate tilde: {summary}");

    // Deep into a trace the count floors to the quantum so the number
    // climbs in steps instead of strobing every heartbeat.
    let filler = "lorem ipsum ".repeat(600); // ≈ 1 800 tokens
    let deep = TranscriptMessage::thinking(&filler);
    let shown = deep
        .thinking_summary()
        .unwrap()
        .trim_start_matches("Thinking · ")
        .trim_end_matches(" tokens")
        .replace(' ', "")
        .parse::<usize>()
        .expect("numeric count");
    let actual = muta_contracts::tokenizer::count_tokens(&filler);
    assert!(
        shown <= actual && actual - shown < 25,
        "shown {shown} vs {actual}"
    );

    // Finished trace: exact count, humanized duration.
    let mut done = TranscriptMessage::thinking(filler);
    done.set_thinking_duration(2_400);
    let settled = done.thinking_summary().unwrap();
    assert!(
        settled.starts_with(&format!("Thinking · {actual} tokens")),
        "exact count when finished: {settled}"
    );
    assert!(settled.ends_with("2.4s"), "humanized duration: {settled}");
}
