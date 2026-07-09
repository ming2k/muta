//! Rendering the document surface through flux + flux-text, inside iris's
//! paint callback (under the lens chrome). Responsible for:
//!
//!  - gutter (line numbers)
//!  - visible wrapped lines (`flux_text::Text::draw` per visual line)
//!  - selection rects (`flux_text::Text::selection_rects` per line)
//!  - blinking caret (`flux_text::Text::x_for_byte`)
//!
//! The paint callback receives a raw canvas pointer from iris; we wrap it in a
//! borrowed view whose `Drop` is a no-op (iris owns the canvas and destroys it
//! itself). The arena is owned by the app and reset after each frame.

use flux::{Arena, Canvas};
use flux_text::{Style, Text};

use crate::display::DisplayMap;
use crate::editor::Editor;

/// Visual metrics, all in logical px.
pub struct Metrics {
    pub font_size: f32,
    pub line_h: f32,
    pub gutter_w: f32,
    pub pad_x: f32,
    pub pad_y: f32,
}

impl Metrics {
    pub fn default_for(size: f32) -> Self {
        Self {
            font_size: size,
            line_h: size * 1.45,
            gutter_w: size * 4.5,
            pad_x: 8.0,
            pad_y: 6.0,
        }
    }
}

/// Theme-driven palette. Packed premultiplied RGBA (flux's colour packing).
pub struct Palette {
    pub bg: u32,
    pub text: u32,
    pub gutter: u32,
    pub selection: u32,
    pub caret: u32,
    pub current_line: u32,
}

impl Palette {
    pub fn dark() -> Self {
        Self {
            bg: 0xff1a1d23,
            text: 0xffe6e6e6,
            gutter: 0xff6b7280,
            selection: 0x555b9bd6,
            caret: 0xffe6e6e6,
            current_line: 0x0affffff,
        }
    }
    pub fn light() -> Self {
        Self {
            bg: 0xffffffff,
            text: 0xff222222,
            gutter: 0xff9aa0a6,
            selection: 0x55bad6ff,
            caret: 0xff222222,
            current_line: 0x0a000000,
        }
    }
}

/// Paint one frame of the editor surface.
///
/// `canvas_ptr` is iris's borrowed canvas (a live `flux_canvas*` already inside
/// an open `flux_canvas_begin/end` pair); `arena` is the app's owned per-frame
/// scratch (reset by the caller after this returns). `full` is the buffer's
/// full text (the caller materialises it once per frame).
///
/// # Safety
/// `canvas_ptr` must be the live `flux_canvas*` iris handed the paint callback
#[allow(clippy::too_many_arguments)] // the per-frame paint entry point; params are all distinct
pub unsafe fn paint(
    editor: &mut Editor,
    text_ctx: &Text,
    canvas_ptr: *mut std::ffi::c_void,
    arena: &Arena,
    scale: f32,
    view_w: f32,
    view_h: f32,
    scroll_y: f32,
    show_caret: bool,
    metrics: &Metrics,
    palette: &Palette,
    display: &DisplayMap,
) {
    // SAFETY: caller guarantees canvas_ptr is a live flux_canvas for this frame.
    // Build a non-owning Canvas view whose Drop is a no-op (iris owns the real
    // canvas and destroys it at teardown).
    let canvas = unsafe { Canvas::borrow_raw(canvas_ptr as *mut flux::sys::flux_canvas) };
    text_ctx.set_scale(scale);
    canvas.set_scale(scale);

    let style = Style::new(metrics.font_size, palette.text);
    let gutter_style = Style::new(metrics.font_size, palette.gutter);

    // 1. Background.
    canvas.fill_rect(0.0, 0.0, view_w, view_h, palette.bg);

    let full = editor.text();

    // 2. Visible line range from the scroll offset.
    let text_w = (view_w - metrics.gutter_w - 2.0 * metrics.pad_x).max(0.0);
    let _ = text_w;
    let first_line = (scroll_y / metrics.line_h).floor().max(0.0) as usize;
    let visible_count = (view_h / metrics.line_h).ceil() as usize + 1;
    let last_line = (first_line + visible_count).min(display.lines.len());

    // 3. Primary caret's logical row (for current-line highlight).
    let primary_row = editor.point_of_offset(editor.selections.primary().head).row;

    for vi in first_line..last_line {
        let dl = &display.lines[vi];
        let y = metrics.pad_y + (vi as f32) * metrics.line_h - scroll_y;
        if y + metrics.line_h < 0.0 || y > view_h {
            continue;
        }
        let slice = &full[dl.lo..dl.hi];

        // Current-line band.
        if dl.row == primary_row {
            canvas.fill_rect(
                metrics.gutter_w,
                y,
                view_w - metrics.gutter_w,
                metrics.line_h,
                palette.current_line,
            );
        }

        // Selection rects on this visual line.
        for sel in &editor.selections.all {
            let s_lo = sel.start().0;
            let s_hi = sel.end().0;
            if s_hi <= dl.lo || s_lo >= dl.hi {
                continue;
            }
            // Clamp the selection to this line's byte range, rebase to local.
            let local_lo = s_lo.max(dl.lo) - dl.lo;
            let local_hi = s_hi.min(dl.hi) - dl.lo;
            let rects = text_ctx.selection_rects(slice, local_lo, local_hi, &style);
            for r in rects {
                let x = metrics.gutter_w + metrics.pad_x + r.x0;
                let w = (r.x1 - r.x0).max(2.0);
                canvas.fill_rect(x, y + 2.0, w, metrics.line_h - 4.0, palette.selection);
            }
        }

        // The text run itself.
        if !slice.is_empty() {
            text_ctx.draw(
                &canvas,
                arena,
                metrics.gutter_w + metrics.pad_x,
                y,
                slice,
                &style,
            );
        }

        // Gutter line number, on the first visual line of each logical row.
        if dl.lo
            == display
                .row_starts
                .get(dl.row as usize)
                .copied()
                .unwrap_or(usize::MAX)
        {
            let label = format!("{}", dl.row + 1);
            canvas.fill_rect(0.0, y, metrics.gutter_w, metrics.line_h, palette.bg);
            text_ctx.draw(&canvas, arena, metrics.pad_x, y, &label, &gutter_style);
        }
    }

    // 4. Caret(s): one per caret selection.
    if show_caret {
        for sel in &editor.selections.all {
            if !sel.is_caret() {
                continue;
            }
            let off = sel.head.0;
            // Which visual line contains this offset?
            let Some(vi) = display
                .lines
                .iter()
                .position(|dl| off >= dl.lo && off <= dl.hi)
            else {
                continue;
            };
            let dl = &display.lines[vi];
            let slice = &full[dl.lo..dl.hi];
            let local = off - dl.lo;
            let x = text_ctx.x_for_byte(slice, local, &style);
            let y = metrics.pad_y + (vi as f32) * metrics.line_h - scroll_y;
            canvas.fill_rect(
                metrics.gutter_w + metrics.pad_x + x,
                y,
                2.0,
                metrics.line_h,
                palette.caret,
            );
        }
    }
}
