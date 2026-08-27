//! Composer/input-box rendering tests: wrapping, caret positioning, grapheme/emoji handling, chips, scroll-block, held/queued rows.

use super::*;

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
                    backoff_clause: None,
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
            ComposerView {
                frame: f,
                input_rect,
                theme: &theme,
                layout_map: &mut layout_map,
                input_scroll: &mut 0,
                selection: &SelectionState::None,
            },
            ComposerText {
                input: "",
                byte_cursor: 0,
            },
            true,
            true,
            true,
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
            ComposerView {
                frame: f,
                input_rect: Rect::new(0, 0, 20, 8),
                theme: &theme,
                layout_map: &mut LayoutMap::new(),
                input_scroll: &mut 0,
                selection: &SelectionState::None,
            },
            ComposerText {
                input,
                byte_cursor: input.len(),
            },
            true,
            true,
            true,
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
                ComposerView {
                    frame: f,
                    input_rect: Rect::new(0, 0, 20, 4),
                    theme: &theme,
                    layout_map: &mut LayoutMap::new(),
                    input_scroll: &mut 0,
                    selection: &SelectionState::None,
                },
                ComposerText {
                    input,
                    byte_cursor: input.len(),
                },
                true,
                true,
                false,
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
            ComposerView {
                frame: f,
                input_rect: Rect::new(0, 0, 30, 3),
                theme: &theme,
                layout_map: &mut LayoutMap::new(),
                input_scroll: &mut 0,
                selection: &SelectionState::None,
            },
            ComposerText {
                input,
                byte_cursor: input.len(),
            },
            ComposerDrawOptions {
                focused: true,
                show_caret: true,
                record: false,
                image_count: 0,
                paste_count: 0,
            },
            "/repeat".len(),
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
            ComposerView {
                frame: f,
                input_rect: Rect::new(0, 0, 13, 5),
                theme: &theme,
                layout_map: &mut LayoutMap::new(),
                input_scroll: &mut 0,
                selection: &SelectionState::None,
            },
            ComposerText {
                input,
                byte_cursor: input.len(),
            },
            ComposerDrawOptions {
                focused: true,
                show_caret: true,
                record: false,
                image_count: 0,
                paste_count: 0,
            },
            "/sessions".len(),
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
            ComposerView {
                frame: f,
                input_rect: Rect::new(0, 0, 120, 3),
                theme: &theme,
                layout_map: &mut LayoutMap::new(),
                input_scroll: &mut 0,
                selection: &SelectionState::None,
            },
            ComposerText {
                input: &input,
                byte_cursor: input.len(),
            },
            true,
            true,
            false,
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
            ComposerView {
                frame: f,
                input_rect: Rect::new(0, 0, 100, 3),
                theme: &theme,
                layout_map: &mut LayoutMap::new(),
                input_scroll: &mut 0,
                selection: &SelectionState::None,
            },
            ComposerText {
                input: &input,
                byte_cursor: input.len(),
            },
            true,
            true,
            false,
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
            ComposerView {
                frame: f,
                input_rect: Rect::new(0, 0, 100, 3),
                theme: &theme,
                layout_map: &mut LayoutMap::new(),
                input_scroll: &mut 0,
                selection: &SelectionState::None,
            },
            ComposerText {
                input: &input,
                byte_cursor: input.len(),
            },
            true,
            true,
            false,
            0, // image_count: no image payload staged,
            1, // paste_count: one paste payload staged,
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
            ComposerView {
                frame: f,
                input_rect: Rect::new(0, 0, 80, 3),
                theme: &theme,
                layout_map: &mut LayoutMap::new(),
                input_scroll: &mut 0,
                selection: &selection,
            },
            ComposerText {
                input: &input,
                byte_cursor: input.len(),
            },
            true,
            false,
            false,
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
            ComposerView {
                frame: f,
                input_rect: Rect::new(0, 0, 16, 5),
                theme: &theme,
                layout_map: &mut LayoutMap::new(),
                input_scroll: &mut 0,
                selection: &SelectionState::None,
            },
            ComposerText {
                input: &input,
                byte_cursor: input.len(),
            },
            true,
            true,
            false,
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
    use crate::composer::cursor_screen_pos;

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
                ComposerView {
                    frame: f,
                    input_rect: rect,
                    theme: &theme,
                    layout_map: &mut LayoutMap::new(),
                    input_scroll: &mut 0,
                    selection: &SelectionState::None,
                },
                ComposerText { input, byte_cursor },
                true,
                true,
                false,
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
    use crate::composer::cursor_screen_pos;

    // A 20-wide box (text width ~16) with a long input; the box shows only
    // a couple of rows, so a caret near the end forces a scroll.
    let rect = Rect::new(0, 0, 20, 4);
    let input = "word ".repeat(20); // ~100 chars, wraps many times
    let byte_cursor = input.len();

    let mut scroll = 0usize;
    let resolved =
        cursor_screen_pos(rect, &input, byte_cursor, &mut scroll).expect("caret position resolves");

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
            ComposerView {
                frame: f,
                input_rect: Rect::new(0, 0, 20, 4),
                theme: &theme,
                layout_map: &mut LayoutMap::new(),
                input_scroll: &mut 0,
                selection: &sel,
            },
            ComposerText {
                input,
                byte_cursor: input.len(),
            },
            true,
            false,
            false,
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
            ComposerView {
                frame: f,
                input_rect: Rect::new(0, 0, 16, 4),
                theme: &theme,
                layout_map: &mut LayoutMap::new(),
                input_scroll: &mut 0,
                selection: &sel,
            },
            ComposerText {
                input,
                byte_cursor: input.len(),
            },
            true,
            false,
            false,
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
            ComposerView {
                frame: f,
                input_rect: rect,
                theme: &theme,
                layout_map: &mut layout_map,
                input_scroll: &mut 0,
                selection: &SelectionState::None,
            },
            ComposerText {
                input,
                byte_cursor: input.len(),
            },
            true,
            false,
            true,
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
                ComposerView {
                    frame: f,
                    input_rect: rect,
                    theme,
                    layout_map: &mut LayoutMap::new(),
                    input_scroll: &mut 0,
                    selection: sel,
                },
                ComposerText {
                    input,
                    byte_cursor: input.len(),
                },
                true,
                false,
                false,
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
                backoff_clause: None,
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
            ComposerView {
                frame: f,
                input_rect: render.input_rect,
                theme: &theme,
                layout_map: &mut layout_map,
                input_scroll: &mut input_scroll,
                selection: &SelectionState::None,
            },
            ComposerText {
                input: &long_input,
                byte_cursor: 0,
            },
            true,
            true,
            false,
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
                ComposerView {
                    frame: f,
                    input_rect: Rect::new(0, 0, 30, 3),
                    theme: &theme,
                    layout_map: &mut LayoutMap::new(),
                    input_scroll: &mut 0,
                    selection: &SelectionState::None,
                },
                ComposerText {
                    input: "hello",
                    byte_cursor: 5,
                },
                focused,
                false,
                false,
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
                backoff_clause: None,
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
                backoff_clause: None,
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
                backoff_clause: None,
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
