//! Snapshot baselines for tool-step rendering.
//!
//! These lock the text/layout of the tool step so the rendering
//! refactor (RenderCtx extraction, and later redesign steps) can be verified
//! behavior-preserving. Snapshots capture the painted grid (cell symbols per
//! row, trailing whitespace trimmed) at a fixed terminal size.
//!
//! Regenerate baselines after an intentional visual change:
//!
//! ```sh
//! INSTA_UPDATE=always cargo test -p neenee-tui-engine paint::snapshot_tests
//! ```

#![cfg(test)]

use neenee_tui_engine::Rect;

use crate::model::document::{MessageKind, TranscriptMessage};
use crate::model::layout::LayoutMap;
use crate::model::selection::SelectionState;
use neenee_contracts::Role;

use super::Theme;
use super::disclosure::draw_tool_step;

/// Build a finished tool-step message with optional output and expand state.
fn tool_step(
    name: &str,
    arguments: &str,
    output: Option<&str>,
    expanded: bool,
) -> TranscriptMessage {
    let mut m = TranscriptMessage::tool_step("call_test", name, arguments);
    if let MessageKind::ToolStep {
        output: out,
        expanded: exp,
        ..
    } = &mut m.kind
    {
        *out = output.map(str::to_string);
        *exp = expanded;
    }
    m
}

/// Like [`tool_step`] but also sets the structured payload + terminal status,
/// mirroring what `finish_tool_step` produces in production.
fn tool_step_structured(
    name: &str,
    arguments: &str,
    structured: neenee_contracts::ToolOutput,
    expanded: bool,
) -> TranscriptMessage {
    let text = structured.to_text();
    let mut m = TranscriptMessage::tool_step("call_test", name, arguments);
    m.finish_tool_step("call_test", text, structured, 0);
    if let MessageKind::ToolStep { expanded: exp, .. } = &mut m.kind {
        *exp = expanded;
    }
    m
}

/// A still-running tool step carrying a partial structured payload, mirroring
/// what `push_tool_stream` produces mid-stream (status stays `Running`).
fn tool_step_streaming(
    name: &str,
    arguments: &str,
    structured: neenee_contracts::ToolOutput,
    expanded: bool,
) -> TranscriptMessage {
    let mut m = TranscriptMessage::tool_step("call_test", name, arguments);
    if let MessageKind::ToolStep {
        structured: s,
        expanded: exp,
        ..
    } = &mut m.kind
    {
        *s = Some(Box::new(structured));
        *exp = expanded;
    }
    m
}

/// Render `msg` as a tool step into a fresh `width x height` buffer and
/// return the painted grid as trimmed text rows joined by newlines.
fn render_grid(msg: &TranscriptMessage, width: u16, height: u16) -> String {
    let mut terminal = neenee_tui_engine::TestTerminal::new(width, height);
    terminal.draw(|f| {
        let area: Rect = f.area();
        let mut layout_map = LayoutMap::default();
        let selection = SelectionState::default();
        let theme = Theme::default();
        let mut diff_cache = crate::tools::DiffCache::default();
        let mut skip_rows = 0usize;
        let mut current_y = area.y;
        let mut content_lines = 0usize;
        let mut sticky = Vec::new();
        draw_tool_step(
            f,
            area,
            msg,
            0,
            &selection,
            None,
            &theme,
            &mut diff_cache,
            &mut layout_map,
            &mut skip_rows,
            &mut current_y,
            &mut content_lines,
            &mut sticky,
            false,
            false,
        );
    });

    let buf = terminal.buffer();
    let bw = buf.area().width as usize;
    let mut rows: Vec<String> = Vec::with_capacity(height as usize);
    for y in 0..height as usize {
        let mut row = String::new();
        for x in 0..width as usize {
            let cell = &buf.content[y * bw + x];
            let sym: &str = cell.symbol();
            row.push_str(sym);
        }
        rows.push(row.trim_end().to_string());
    }
    while rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
    }
    let grid = rows.join("\n");

    // Style layer: a compact run-length map of the background per row, plus a
    // legend. Text snapshots can't see color, so this makes palette/banding
    // changes (Step 2) visible and reviewable. Symbols are assigned per-frame
    // in first-appearance order; `.` is the terminal default.
    let (bgmap, legend) = background_map(buf);
    if bgmap.is_empty() {
        grid
    } else {
        format!("{grid}\n\nbackgrounds:\n{legend}\n{bgmap}")
    }
}

/// Compact per-row background run-length map + legend for a rendered buffer.
/// Skips rows that are entirely the terminal default so the output stays
/// focused on the painted step.
fn background_map(buf: &neenee_tui_engine::Grid) -> (String, String) {
    use neenee_tui_engine::Color;
    type Bg = Color;

    let bw = buf.area().width as usize;
    let bh = buf.area().height as usize;

    let is_default = |bg: Bg| bg == Color::Reset;

    // Distinct bg colors in first-appearance order.
    let mut order: Vec<Bg> = Vec::new();
    for y in 0..bh {
        for x in 0..bw {
            let bg = buf.content[y * bw + x].style().bg;
            if !order.contains(&bg) {
                order.push(bg);
            }
        }
    }
    let sym_of = |bg: Bg| -> char {
        if is_default(bg) {
            '.'
        } else {
            let i = order
                .iter()
                .position(|x| is_default(*x) == is_default(bg) && x == &bg)
                .unwrap_or(0);
            (b'A' + i as u8) as char
        }
    };
    let fmt_color = |bg: Bg| -> String {
        match bg {
            Color::Reset => "reset".to_string(),
            Color::Rgb(r, g, b) => format!("#{:02X}{:02X}{:02X}", r, g, b),
            other => format!("{:?}", other),
        }
    };
    let legend = order
        .iter()
        .map(|&bg| format!("{}={}", sym_of(bg), fmt_color(bg)))
        .collect::<Vec<_>>()
        .join("  ");

    let mut lines: Vec<String> = Vec::new();
    for y in 0..bh {
        let mut runs: Vec<(char, usize)> = Vec::new();
        for x in 0..bw {
            let s = sym_of(buf.content[y * bw + x].style().bg);
            match runs.last_mut() {
                Some((last, n)) if *last == s => *n += 1,
                _ => runs.push((s, 1)),
            }
        }
        while matches!(runs.last(), Some(('.', _))) {
            runs.pop();
        }
        if runs.is_empty() {
            continue;
        }
        let line = runs
            .iter()
            .map(|(s, n)| format!("{}{}", s, n))
            .collect::<Vec<_>>()
            .join(" ");
        lines.push(format!("{}: {}", y, line));
    }
    (lines.join("\n"), legend)
}

#[test]
fn read_text_expanded_renders_code_block() {
    let m = tool_step(
        "read_text",
        r#"{"path":"src/lib.rs"}"#,
        Some("fn main() {\n    let x = 1;\n}\n"),
        true,
    );
    insta::assert_snapshot!(render_grid(&m, 80, 40));
}

#[test]
fn read_text_with_offset_numbers_from_start_line() {
    // A structured `Code` carrying `start_line: 100` (as `read_text` emits
    // when called with `offset: 100`) must number the gutter 100, 101, … —
    // not restart at 1. Also locks that the gutter column widens to fit
    // three-digit line numbers instead of overflowing.
    let m = tool_step_structured(
        "read_text",
        r#"{"path":"src/lib.rs","offset":100}"#,
        neenee_contracts::ToolOutput::Code {
            lang: Some("rs".into()),
            text: "fn a() {}\nfn b() {}\nfn c() {}\n".into(),
            start_line: 100,
            prefix: None,
            suffix: None,
        },
        true,
    );
    insta::assert_snapshot!(render_grid(&m, 80, 40));
}

#[test]
fn bash_expanded_renders_markers_and_output() {
    let m = tool_step(
        "bash",
        r#"{"command":"cargo test"}"#,
        Some("running 3 tests\n.\n.\n.\ntest result: ok. 3 passed\nSTDOUT:\nbuilt ok\nExit 0\n"),
        true,
    );
    insta::assert_snapshot!(render_grid(&m, 80, 40));
}

#[test]
fn bash_expanded_renders_structured_shell() {
    // Structured Shell: stdout and stderr render as separate color bands and a
    // non-zero exit produces an `exit N` footer (the failure is also reflected
    // in the header status glyph). This is the path bash takes in production
    // post-ADR-0001; the marker-sniffing test above covers the legacy fallback.
    let m = tool_step_structured(
        "bash",
        r#"{"command":"cargo test"}"#,
        neenee_contracts::ToolOutput::Shell {
            command: "cargo test".into(),
            stdout: "running 3 tests\n...\ntest result: ok. 3 passed".into(),
            stderr: "warning: unused import".into(),
            lines: Vec::new(),
            exit: Some(1),
            truncated: false,
            termination: neenee_contracts::tool_output::ShellTermination::Exited,
        },
        true,
    );
    insta::assert_snapshot!(render_grid(&m, 80, 40));
}

#[test]
fn bash_expanded_preserves_stdout_stderr_interleaving() {
    // Regression for the "all-stdout-then-all-stderr" reorder symptom: when
    // `lines` is populated, the renderer must emit them in arrival order, not
    // bucketed by stream. Here a stderr line sits *between* two stdout lines,
    // so a correct render shows `warning: b` between `Compiling a` and
    // `Compiling c` — not pinned to the bottom.
    use neenee_contracts::tool_output::{ShellLine, ShellStream};
    let m = tool_step_structured(
        "bash",
        r#"{"command":"cargo build"}"#,
        neenee_contracts::ToolOutput::Shell {
            command: "cargo build".into(),
            stdout: "Compiling a\nCompiling c".into(),
            stderr: "warning: b".into(),
            lines: vec![
                ShellLine {
                    stream: ShellStream::Out,
                    text: "Compiling a".into(),
                },
                ShellLine {
                    stream: ShellStream::Err,
                    text: "warning: b".into(),
                },
                ShellLine {
                    stream: ShellStream::Out,
                    text: "Compiling c".into(),
                },
            ],
            exit: Some(0),
            truncated: false,
            termination: neenee_contracts::tool_output::ShellTermination::Exited,
        },
        true,
    );
    insta::assert_snapshot!(render_grid(&m, 80, 40));
}

#[test]
fn bash_expanded_folds_long_output_keeping_tail_events() {
    // A long stdout (well past HEAD + TAIL + 1 = 7 lines) folds its middle
    // into a single `⋯ N lines hidden` row, while the head, the tail, and the
    // trailing `exit 1` event footer all stay visible. This is the fix for the
    // "verbose stdout buries the exit code" symptom on an expanded bash step.
    let stdout = (1..=20)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let m = tool_step_structured(
        "bash",
        r#"{"command":"cargo build"}"#,
        neenee_contracts::ToolOutput::Shell {
            command: "cargo build".into(),
            stdout,
            stderr: String::new(),
            lines: Vec::new(),
            exit: Some(1),
            truncated: false,
            termination: neenee_contracts::tool_output::ShellTermination::Exited,
        },
        true,
    );
    insta::assert_snapshot!(render_grid(&m, 80, 40));
}

#[test]
fn grep_expanded_renders_grouped_matches() {
    let m = tool_step(
        "grep",
        r#"{"pattern":"foo","path":"src"}"#,
        Some("src/a.rs:10:let foo = 1;\nsrc/a.rs:22:foo();\nsrc/b.rs:5:foo,"),
        true,
    );
    insta::assert_snapshot!(render_grid(&m, 80, 40));
}

#[test]
fn edit_file_expanded_renders_diff() {
    let m = tool_step(
        "edit_file",
        r#"{"path":"a.rs","old_string":"let x = 1;","new_string":"let x = 2;"}"#,
        Some("Edited a.rs"),
        true,
    );
    insta::assert_snapshot!(render_grid(&m, 80, 30));
}

#[test]
fn edit_file_prose_diff_suppresses_noisy_word_highlights() {
    let m = tool_step(
        "edit_file",
        r#"{"path":"docs/explanation/tui.md","old_string":"Because the frame is a pure function of state, anything that changes state — a streamed token, a permission request, a mouse drag — shows up on the very next frame with no manual invalidation.","new_string":"Because the frame is a pure function of state, diff compares the back grid against the front grid and walks only the dirty rows from each row's dirty column."}"#,
        Some("Edited docs/explanation/tui.md"),
        true,
    );
    insta::assert_snapshot!(render_grid(&m, 100, 30));
}

#[test]
fn edit_file_multihunk_interleaves_changes() {
    // Two separated single-token edits: the LCS diff must interleave
    // context/remove/add per hunk rather than all-remove-then-all-add.
    let m = tool_step(
        "edit_file",
        r#"{"path":"a.rs","old_string":"fn one() {\n    return 1;\n}\n\nfn two() {\n    return 2;\n}\n","new_string":"fn one() {\n    return 10;\n}\n\nfn two() {\n    return 20;\n}\n"}"#,
        Some("Edited a.rs"),
        true,
    );
    insta::assert_snapshot!(render_grid(&m, 80, 40));
}

#[test]
fn edit_file_distant_changes_render_explicit_hunks() {
    // Two changes separated by 10 context lines become two explicit hunks.
    // Each hunk owns a standard @@ range header; omitted source is represented
    // by the gap between hunks rather than a synthetic ellipsis row.
    let old = concat!(
        "line  1\nline  2\nCHANGE\n",
        "line  4\nline  5\nline  6\nline  7\nline  8\nline  9\n",
        "line 10\nline 11\nline 12\nline 13\n",
        "CHANGE\nline 15\nline 16\n",
    );
    let new = concat!(
        "line  1\nline  2\nchange\n",
        "line  4\nline  5\nline  6\nline  7\nline  8\nline  9\n",
        "line 10\nline 11\nline 12\nline 13\n",
        "change\nline 15\nline 16\n",
    );
    let args = format!(
        r#"{{"path":"a.rs","old_string":{},"new_string":{}}}"#,
        serde_json::to_string(old).unwrap(),
        serde_json::to_string(new).unwrap(),
    );
    let m = tool_step("edit_file", &args, Some("Edited a.rs"), true);
    insta::assert_snapshot!(render_grid(&m, 80, 40));
}

#[test]
fn list_dir_expanded_renders_listing() {
    let m = tool_step(
        "list_dir",
        r#"{"path":"."}"#,
        Some("src/\ntests/\nCargo.toml\nREADME.md"),
        true,
    );
    insta::assert_snapshot!(render_grid(&m, 80, 30));
}

#[test]
fn bash_running_streams_live_preview() {
    // A long-running bash command mid-stream: status is still Running (header
    // shows a steady `info` accent) but partial stdout already shows under the
    // header via the structured Shell, instead of freezing on a spinner.
    let m = tool_step_streaming(
        "bash",
        r#"{"command":"cargo build"}"#,
        neenee_contracts::ToolOutput::Shell {
            command: "cargo build".into(),
            stdout: "Compiling neenee-contracts v0.1.0\nCompiling neenee-tui-engine v0.1.0".into(),
            stderr: String::new(),
            lines: Vec::new(),
            exit: None,
            truncated: false,
            termination: neenee_contracts::tool_output::ShellTermination::Exited,
        },
        false,
    );
    insta::assert_snapshot!(render_grid(&m, 80, 24));
}

#[test]
fn edit_file_diff_renders_from_structured_patch() {
    // The diff now comes from the ToolOutput::Patch payload (old/new), not
    // from re-parsing the tool arguments.
    let m = tool_step_structured(
        "edit_file",
        r#"{"path":"a.rs","old_string":"let x = 1;","new_string":"let x = 2;"}"#,
        neenee_contracts::ToolOutput::Patch {
            path: "a.rs".into(),
            op: neenee_contracts::PatchOp::Edit,
            old: "let x = 1;".into(),
            new: "let x = 2;".into(),
            start_line: 0,
        },
        true,
    );
    insta::assert_snapshot!(render_grid(&m, 80, 30));
}

#[test]
fn failed_edit_renders_error_instead_of_intended_diff() {
    let m = tool_step_structured(
        "edit_file",
        r#"{"path":"a.rs","old_string":"let x = 1;","new_string":"let x = 2;"}"#,
        neenee_contracts::ToolOutput::Error {
            message: "old_string was not found".into(),
            detail: Some("The file changed before the edit ran.".into()),
        },
        true,
    );
    let rendered = render_grid(&m, 80, 30);

    assert!(rendered.contains("Error: old_string was not found"));
    assert!(rendered.contains("The file changed before the edit ran."));
    assert!(!rendered.contains("- let x = 1;"));
    assert!(!rendered.contains("+ let x = 2;"));
}

// ── Tool-step batch spacing (ADR-0001, layout-owned boundaries) ──
//
// Known same-turn tool steps stack flush regardless of disclosure state. The
// first tests also lock the compatibility behavior for legacy messages whose
// turn is unknown: adjacent collapsed headers remain compact, while an
// expanded legacy body keeps one separator before the next header. These tests
// render the full transcript (`draw_transcript`, which owns the boundary) so
// the single-step `render_grid` helper cannot mask layout behavior.

/// Render `steps` (already finalized tool-step messages) through the full
/// transcript pipeline and return the painted grid as trimmed rows. Unlike
/// [`render_grid`], this exercises `draw_transcript` so the message-level
/// spacing between consecutive steps is captured. Backgrounds are omitted:
/// these tests are about row counts, not palette.
fn render_transcript_grid(messages: &[TranscriptMessage], width: u16, height: u16) -> String {
    use super::{EmptyStateGuidance, QueueBarView, Theme, TranscriptView, draw_transcript};
    use crate::model::layout::LayoutMap;

    let theme = Theme::default();
    let selection = SelectionState::default();
    let mut terminal = neenee_tui_engine::TestTerminal::new(width, height);
    let mut layout_map = LayoutMap::new();
    terminal.draw(|f| {
        let _ = draw_transcript(
            f,
            &mut layout_map,
            TranscriptView {
                messages,
                scroll: 0,
                selection: &selection,
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
                envoy_bar: None,
                side_banner: None,
                page_hints: None,
                session_head: None,
                todos: None,
                review_alert: String::new(),
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

    let buf = terminal.buffer();
    let bw = buf.area().width as usize;
    let mut rows: Vec<String> = Vec::with_capacity(height as usize);
    for y in 0..height as usize {
        let mut row = String::new();
        for x in 0..width as usize {
            row.push_str(buf.content[y * bw + x].symbol());
        }
        rows.push(row.trim_end().to_string());
    }
    while rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
    }
    rows.join("\n")
}

/// A batch of collapsed tool steps renders with no blank rows between headers.
#[test]
fn collapsed_tool_steps_stack_flush() {
    let steps = vec![
        tool_step_structured(
            "read_text",
            r#"{"path":"a.rs"}"#,
            neenee_contracts::ToolOutput::Code {
                lang: None,
                text: "x".into(),
                start_line: 1,
                prefix: None,
                suffix: None,
            },
            false,
        ),
        tool_step_structured(
            "read_text",
            r#"{"path":"b.rs"}"#,
            neenee_contracts::ToolOutput::Code {
                lang: None,
                text: "y".into(),
                start_line: 1,
                prefix: None,
                suffix: None,
            },
            false,
        ),
        tool_step_structured(
            "read_text",
            r#"{"path":"c.rs"}"#,
            neenee_contracts::ToolOutput::Code {
                lang: None,
                text: "z".into(),
                start_line: 1,
                prefix: None,
                suffix: None,
            },
            false,
        ),
    ];
    let grid = render_transcript_grid(&steps, 60, 14);
    // The three headers must be adjacent: no blank row between any pair. Each
    // header carries the disclosure marker (`+` collapsed) somewhere in the
    // line, so locate their row indices and assert they are consecutive.
    let header_idx: Vec<usize> = grid
        .lines()
        .enumerate()
        .filter(|(_, l)| l.contains("Read ") && (l.contains('+') || l.contains('-')))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(header_idx.len(), 3, "expected three Read headers:\n{grid}");
    assert_eq!(
        header_idx[1] - header_idx[0],
        1,
        "first two collapsed headers must be flush (no blank row):\n{grid}"
    );
    assert_eq!(
        header_idx[2] - header_idx[1],
        1,
        "last two collapsed headers must be flush (no blank row):\n{grid}"
    );
}

/// For legacy messages without turn stamps, an expanded body sits flush
/// against its own header but retains one compatibility separator before the
/// next header; collapsed neighbors remain flush.
#[test]
fn expanded_body_flush_to_header_neighbours_stay_flush() {
    let steps = vec![
        tool_step_structured(
            "read_text",
            r#"{"path":"a.rs"}"#,
            neenee_contracts::ToolOutput::Code {
                lang: None,
                text: "x".into(),
                start_line: 1,
                prefix: None,
                suffix: None,
            },
            false, // collapsed — flush against the next step's header
        ),
        tool_step_structured(
            "grep",
            r#"{"pattern":"foo","path":"src"}"#,
            neenee_contracts::ToolOutput::Matches {
                pattern: "foo".into(),
                lines: vec!["src/a.rs:10:1:foo".into(), "src/b.rs:5:1:foo".into()],
            },
            true, // expanded legacy step — compatibility separator follows
        ),
        tool_step_structured(
            "read_text",
            r#"{"path":"c.rs"}"#,
            neenee_contracts::ToolOutput::Code {
                lang: None,
                text: "z".into(),
                start_line: 1,
                prefix: None,
                suffix: None,
            },
            false, // collapsed legacy step — starts below that separator
        ),
    ];
    insta::assert_snapshot!(render_transcript_grid(&steps, 60, 16));
}

/// A user panel followed by a tool step keeps one explicit blank row between the
/// panel transition and the step header, so the two shapes never visually stick
/// together.
#[test]
fn user_message_before_tool_step_has_single_separator_row() {
    let messages = vec![
        TranscriptMessage::new(Role::User, "inspect files").with_round(1),
        tool_step_structured(
            "read_text",
            r#"{"path":"src/lib.rs"}"#,
            neenee_contracts::ToolOutput::Code {
                lang: None,
                text: "pub fn main() {}".into(),
                start_line: 1,
                prefix: None,
                suffix: None,
            },
            false,
        ),
    ];

    let grid = render_transcript_grid(&messages, 60, 16);
    let rows: Vec<&str> = grid.lines().collect();
    let user_text_idx = rows
        .iter()
        .position(|row| row.contains("inspect files"))
        .expect("user text row must render");
    let tool_idx = rows
        .iter()
        .position(|row| row.contains("Read ") && row.contains('+'))
        .expect("collapsed tool header must render");

    // User row + one bottom transition row + one blank separator row + header.
    assert_eq!(
        tool_idx - user_text_idx,
        3,
        "user panel should have exactly one blank row before a following tool step:\n{grid}"
    );
    assert!(
        rows[tool_idx - 1].trim().is_empty(),
        "row immediately before tool header should be the separator blank row:\n{grid}"
    );
}

/// The metadata component owns separators, so a sent user header with both a
/// round and timestamp renders exactly one separator between the two chips.
#[test]
fn sent_user_header_has_one_metadata_separator() {
    let message = TranscriptMessage::new(Role::User, "inspect files")
        .with_round(5)
        .with_sent_at_ms(1_700_000_000_000);

    let grid = render_transcript_grid(&[message], 60, 14);
    let header = grid
        .lines()
        .find(|row| row.contains("round 5"))
        .expect("sent user header must render");

    assert_eq!(
        header.matches('·').count(),
        1,
        "round and timestamp should have one separator:\n{grid}"
    );
}

/// ADR-0106: command rows render by shape. A short single-line reply joins
/// inline (` · `, no marker), a result-less record renders plain, and a
/// multi-line reply keeps the `+`/`-` disclosure — no row shows `⚙`, and no
/// row shows `+` unless a body exists to expand into.
#[test]
fn command_rows_render_by_shape_without_false_markers() {
    let messages = vec![
        // Inline: `/new`'s single-line confirmation.
        TranscriptMessage::command_result(
            "new",
            "",
            Some(neenee_contracts::CommandResult::Text(
                "Started new session: a1b2c3".to_string(),
            )),
        ),
        // Plain: shell passthrough, no persisted result.
        TranscriptMessage::command_result("shell", "!ls -la", None),
        // Disclose: multi-line permission list.
        TranscriptMessage::command_result(
            "permissions",
            "",
            Some(neenee_contracts::CommandResult::PermissionList {
                allowed: vec!["bash".to_string()],
            }),
        ),
    ];

    let grid = render_transcript_grid(&messages, 72, 18);
    let rows: Vec<&str> = grid.lines().collect();

    let inline_idx = rows
        .iter()
        .position(|row| row.contains("/new ·"))
        .unwrap_or_else(|| panic!("inline reply must join with the R1 dot:\n{grid}"));
    assert!(
        rows[inline_idx].contains("Started new session: a1b2c3"),
        "the inline reply text must be on the same row:\n{grid}"
    );
    assert!(
        !rows[inline_idx].trim_start().starts_with('+'),
        "an inline row must not carry the disclosure marker:\n{grid}"
    );

    let plain_idx = rows
        .iter()
        .position(|row| row.contains("!ls -la"))
        .expect("shell passthrough must render its invocation");
    assert!(
        !rows[plain_idx].contains('·') || plain_idx == inline_idx,
        "a plain row carries no join:\n{grid}"
    );

    let disclose_idx = rows
        .iter()
        .position(|row| row.contains("/permissions"))
        .unwrap_or_else(|| panic!("multi-line result must keep its header:\n{grid}"));
    assert!(
        rows[disclose_idx].trim_start().starts_with('+'),
        "a multi-line result keeps the disclosure affordance:\n{grid}"
    );

    for (i, row) in rows.iter().enumerate() {
        assert!(
            !row.contains('⚙'),
            "no command row may show the gear glyph (row {i}):\n{grid}"
        );
    }
}

/// ADR-0106: expanding a Disclose command row reveals the typed result body
/// through the shared block renderer, and pinning is respected.
#[test]
fn command_row_disclose_expands_to_result_body() {
    let mut message = TranscriptMessage::command_result(
        "permissions",
        "",
        Some(neenee_contracts::CommandResult::PermissionList {
            allowed: vec!["bash".to_string()],
        }),
    );
    message.pin_command_result_expanded(true);

    let grid = render_transcript_grid(&[message], 72, 18);
    assert!(
        grid.contains("- /permissions"),
        "an expanded row shows the open marker:\n{grid}"
    );
    assert!(
        grid.contains("Always-allowed tools:"),
        "the expanded body must render:\n{grid}"
    );
    assert!(
        grid.contains("• bash"),
        "the body's list must render through the block renderer:\n{grid}"
    );
}

/// ADR-0106: the inline layout is width-aware — a reply that cannot fit
/// beside its invocation without truncation must fall back to the disclosure
/// layout rather than render a fragment.
#[test]
fn command_row_inline_falls_back_to_disclose_when_narrow() {
    // A reply long enough that `invocation · reply` overflows even a
    // conversational band.
    let message = TranscriptMessage::command_result(
        "search",
        "the integration flag",
        Some(neenee_contracts::CommandResult::Text(
            "Relevant history (most similar first): the integration flag was introduced in round 7"
                .to_string(),
        )),
    );

    // Wide: inline join.
    let wide = render_transcript_grid(std::slice::from_ref(&message), 120, 18);
    assert!(
        wide.contains("/search the integration flag ·"),
        "a fitting reply joins inline:\n{wide}"
    );

    // Narrow: the reply cannot fit, so the row must disclose instead.
    let narrow = render_transcript_grid(&[message], 40, 18);
    assert!(
        narrow.contains("+ /search"),
        "a non-fitting reply discloses rather than truncating inline:\n{narrow}"
    );
}

/// Expanded reasoning traces keep block gaps but do not add a blank row before
/// the first body line or append a trailing bottom gap. Layout-level spacing
/// owns the boundary before the following assistant text.
#[test]
fn reasoning_trace_spacing_has_internal_gaps_and_single_trailing_separator() {
    let mut reasoning = TranscriptMessage::thinking("first thought\n\nsecond thought").with_turn(1);
    reasoning.pin_thinking_expanded(true);
    let assistant = TranscriptMessage::new(Role::Assistant, "final answer").with_turn(2);
    let messages = vec![reasoning, assistant];

    let grid = render_transcript_grid(&messages, 72, 18);
    let rows: Vec<&str> = grid.lines().collect();
    let summary_idx = rows
        .iter()
        .position(|row| row.contains("Thinking ·"))
        .expect("reasoning summary must render");
    let first_idx = rows
        .iter()
        .position(|row| row.contains("first thought"))
        .expect("first reasoning block must render");
    let second_idx = rows
        .iter()
        .position(|row| row.contains("second thought"))
        .expect("second reasoning block must render");
    let answer_idx = rows
        .iter()
        .position(|row| row.contains("final answer"))
        .expect("assistant text must render");

    assert_eq!(
        first_idx - summary_idx,
        1,
        "reasoning body should start directly below its summary:\n{grid}"
    );
    assert_eq!(
        second_idx - first_idx,
        2,
        "reasoning blocks should be separated by one blank row:\n{grid}"
    );
    assert_eq!(
        answer_idx - second_idx,
        2,
        "reasoning trace should have exactly one trailing layout separator before the next message:\n{grid}"
    );
}

/// The turn header labels the group and keeps one row before its first
/// component.
#[test]
fn default_turn_header_has_one_gap_before_first_tool() {
    let step = tool_step_structured(
        "read_text",
        r#"{"path":"a.rs"}"#,
        neenee_contracts::ToolOutput::Code {
            lang: None,
            text: "x".into(),
            start_line: 1,
            prefix: None,
            suffix: None,
        },
        false,
    )
    .with_turn(7)
    .with_attribution("anthropic", "claude-sonnet");

    let grid = render_transcript_grid(&[step], 72, 14);
    let rows: Vec<&str> = grid.lines().collect();
    let turn_idx = rows
        .iter()
        .position(|row| row.contains("◆ turn 7"))
        .expect("turn header must render");
    let tool_idx = rows
        .iter()
        .position(|row| row.contains("Read ") && row.contains('+'))
        .expect("tool header must render");

    assert_eq!(
        turn_idx, 1,
        "turn header should only inherit the viewport's one top-margin row:\n{grid}"
    );
    assert_eq!(
        tool_idx - turn_idx,
        2,
        "turn header and first tool should have one blank row:\n{grid}"
    );
}

/// Thinking and the tool batch are separate visual segments, while parallel
/// tools inside the batch stay flush.
#[test]
fn same_turn_segments_have_gaps_but_parallel_tools_stay_flush() {
    let mut thinking = TranscriptMessage::thinking("inspect the files").with_turn(7);
    thinking.set_thinking_duration(10);
    let first = tool_step_structured(
        "read_text",
        r#"{"path":"a.rs"}"#,
        neenee_contracts::ToolOutput::Code {
            lang: None,
            text: "a".into(),
            start_line: 1,
            prefix: None,
            suffix: None,
        },
        false,
    )
    .with_turn(7);
    let second = tool_step_structured(
        "read_text",
        r#"{"path":"b.rs"}"#,
        neenee_contracts::ToolOutput::Code {
            lang: None,
            text: "b".into(),
            start_line: 1,
            prefix: None,
            suffix: None,
        },
        false,
    )
    .with_turn(7);

    let grid = render_transcript_grid(&[thinking, first, second], 72, 16);
    let rows: Vec<&str> = grid.lines().collect();
    let turn_idx = rows
        .iter()
        .position(|row| row.contains("◆ turn 7"))
        .expect("turn header must render");
    let thinking_idx = rows
        .iter()
        .position(|row| row.contains("Thinking ·"))
        .expect("thinking summary must render");
    let tool_idx: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.contains("Read ") && row.contains('+'))
        .map(|(index, _)| index)
        .collect();

    assert_eq!(
        thinking_idx,
        turn_idx + 2,
        "header → thinking needs one blank row:\n{grid}"
    );
    assert_eq!(
        tool_idx,
        vec![thinking_idx + 2, thinking_idx + 3],
        "thinking → tools needs one row; parallel tools must be flush:\n{grid}"
    );
}

/// A turn boundary, unlike a component boundary inside a turn, keeps the
/// standard single separator row.
#[test]
fn different_tool_turns_have_one_vertical_gap() {
    let make_step = |turn: u64| {
        tool_step_structured(
            "read_text",
            format!(r#"{{"path":"{turn}.rs"}}"#).as_str(),
            neenee_contracts::ToolOutput::Code {
                lang: None,
                text: turn.to_string(),
                start_line: 1,
                prefix: None,
                suffix: None,
            },
            false,
        )
        .with_turn(turn)
    };
    let grid = render_transcript_grid(&[make_step(1), make_step(2)], 72, 16);
    let rows: Vec<&str> = grid.lines().collect();
    let turn_rows: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.contains("◆ turn"))
        .map(|(index, _)| index)
        .collect();
    let tool_rows: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.contains("Read ") && row.contains('+'))
        .map(|(index, _)| index)
        .collect();

    assert_eq!(turn_rows.len(), 2, "expected two turn headers:\n{grid}");
    assert_eq!(tool_rows.len(), 2, "expected two tool headers:\n{grid}");
    assert_eq!(
        turn_rows[1] - tool_rows[0],
        2,
        "turns need exactly one blank separator row:\n{grid}"
    );
    assert_eq!(
        tool_rows[1] - turn_rows[1],
        2,
        "turn header → tool needs one blank row:\n{grid}"
    );
}
