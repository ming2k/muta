//! neenee-editor — application shell.
//!
//! iris owns the Wayland window + event loop. Each frame:
//!   build(frame, input) → lens chrome (filename, status bar) + key dispatch
//!   paint(host)         → flux/flux-text document surface under the chrome
//!
//! The document is painted by `render::paint` inside iris's canvas envelope.
//! The `flux_text::Text` context is built once from iris's device (the only
//! flux device in the process — see `PaintHost::device`).

#![cfg(feature = "gui")]

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use flux::Arena;
use flux_text::{Family, Style, Text};
use iris::{Align, Application, Config, Cursor, Frame, Input, PaintHost, key, mods};

use neenee_editor::display;
use neenee_editor::editor::{Dir, Editor};
use neenee_editor::render::{Metrics, Palette, paint};

struct App {
    editor: Editor,
    path: Option<PathBuf>,
    text_ctx: Option<Text>,
    arena: Option<Arena>,
    scroll_y: f32,
    view_w: f32,
    view_h: f32,
    blink_phase: f32,
    dirty: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os().nth(1).map(PathBuf::from);
    let initial = path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_else(|| {
            "# neenee editor\n\n\
             A Zed-influenced editor rendered through optics.\n\
             Start typing…\n\n\
             Ctrl-S save · Ctrl-O open · Ctrl-Z undo · Ctrl-Y redo\n"
                .to_string()
        });
    let editor = Editor::from_text(&initial);

    let app = Rc::new(RefCell::new(App {
        editor,
        path,
        text_ctx: None,
        arena: None,
        scroll_y: 0.0,
        view_w: 960.0,
        view_h: 640.0,
        blink_phase: 0.0,
        dirty: true,
    }));

    let app_paint = Rc::clone(&app);
    let app_build = Rc::clone(&app);

    let cfg = Config::new("neenee editor")?.size(960, 640);
    Application::run(
        cfg,
        move |frame: &mut Frame, input: &Input| build_ui(frame, input, &app_build),
        Some(move |host: PaintHost| paint_frame(&host, &app_paint)),
    )?;
    Ok(())
}

fn build_ui(frame: &mut Frame, input: &Input, app: &Rc<RefCell<App>>) {
    // 1. Dispatch input + advance blink + read status-bar state, all in one
    //    mutable borrow so we can call editor mutators and point_of_offset.
    let (name, dirty, len, caret_pos, view_h, cursor_y) = {
        let mut a = app.borrow_mut();
        a.view_w = input.as_raw().display_size.x;
        a.view_h = input.as_raw().display_size.y;
        handle_input(input, &mut a);
        a.blink_phase += input.as_raw().dt_seconds;
        let head = a.editor.selections.primary().head;
        let p = a.editor.point_of_offset(head);
        let name = a
            .path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "untitled".to_string());
        (
            name,
            a.dirty,
            a.editor.len(),
            (p.row + 1, p.column + 1),
            a.view_h,
            input.as_raw().cursor.y,
        )
    };

    let theme = frame.theme();

    frame.column_ex(
        &LayoutOpts {
            flex: 1.0,
            cross: Align::Stretch,
            bg: theme.bg(),
            ..LayoutOpts::default()
        },
        |f| {
            // Top bar: filename + modified dot + byte count.
            f.row_ex(
                &LayoutOpts {
                    gap: 8.0,
                    pad: 6.0,
                    cross: Align::Center,
                    ..LayoutOpts::default()
                },
                |f| {
                    f.label(&name);
                    if dirty {
                        f.label("●");
                    }
                    f.flex(1.0);
                    f.label(&format!("{}B", len));
                },
            );
            f.separator();
            // Middle: the document area is painted by the paint callback, not a
            // lens widget. Reserve it with a flex spacer.
            f.flex(1.0);
            f.separator();
            // Bottom status bar: caret position + hints.
            f.row_ex(
                &LayoutOpts {
                    gap: 10.0,
                    pad: 4.0,
                    cross: Align::Center,
                    ..LayoutOpts::default()
                },
                |f| {
                    f.label(&format!("{}:{}", caret_pos.0, caret_pos.1));
                    f.flex(1.0);
                    f.label("Ctrl-S save · Ctrl-O open · Ctrl-Z undo · Ctrl-Y redo");
                },
            );
        },
    );

    // I-beam cursor over the document area.
    if cursor_y > 28.0 && cursor_y < view_h - 28.0 {
        iris::set_cursor(Cursor::Text);
    } else {
        iris::set_cursor(Cursor::Default);
    }
}

#[allow(clippy::unwrap_used)] // both options are checked non-empty just above
fn paint_frame(host: &PaintHost, app: &Rc<RefCell<App>>) {
    let mut a = app.borrow_mut();

    // Lazily build the text context + arena from iris's device (the only flux
    // device in the process). SAFETY: PaintHost::device is a live flux_device
    // retained by iris for the app's lifetime; we borrow, never release.
    if a.text_ctx.is_none() {
        let device =
            unsafe { flux::Device::borrow_raw(host.device() as *mut flux::sys::flux_device) };
        a.text_ctx = Text::new(&device).ok();
        a.arena = Arena::with_capacity(4 * 1024 * 1024).ok();
    }

    // Snapshot the scalars we need so the paint call doesn't alias-borrow `a`.
    let (view_w, view_h, scroll_y, blink_phase) = (a.view_w, a.view_h, a.scroll_y, a.blink_phase);
    if a.text_ctx.is_none() || a.arena.is_none() {
        return;
    }

    let metrics = Metrics::default_for(15.0);
    let palette = Palette::dark();

    // Wrap to the text column width. Build the display map and full text from
    // the editor up front (immutable reads), then hand a fresh &mut to paint.
    let text_w = (view_w - metrics.gutter_w - 2.0 * metrics.pad_x).max(50.0);
    let style =
        Style::new(metrics.font_size, palette.text).with_family(Family::FLUX_TEXT_FAMILY_MONO);
    let rows: Vec<(u32, String)> = a
        .editor
        .text()
        .split('\n')
        .enumerate()
        .map(|(i, s)| (i as u32, s.to_string()))
        .collect();
    // display::build needs &Text — borrow it from the option just for this call.
    let display = {
        let text_ctx = a.text_ctx.as_ref().unwrap();
        display::build(text_ctx, &style, rows, text_w)
    };

    // Blink: ~530 ms visible in a ~1060 ms cycle.
    let blink = (blink_phase % 1.06) < 0.53;

    // Borrow the three disjoint fields together. The borrow checker can't see
    // through `Option::as_ref()`, so take raw pointers from the fields (a field
    // access is provably disjoint from `&mut a.editor`) and reborrow inside
    // `paint_with_refs`.
    let text_ptr = a.text_ctx.as_ref().unwrap() as *const Text;
    let arena_ptr = a.arena.as_ref().unwrap() as *const Arena;
    // SAFETY: text_ptr/arena_ptr come from valid `&a.text_ctx`/`&a.arena` field
    // accesses and `App` outlives this frame; `&mut a.editor` is disjoint.
    unsafe {
        paint_with_refs(
            &mut a.editor,
            text_ptr,
            arena_ptr,
            host,
            view_w,
            view_h,
            scroll_y,
            blink,
            &metrics,
            &palette,
            &display,
        );
    }
    a.arena.as_ref().unwrap().reset();
    a.dirty = false;
}

/// See `paint_frame`: takes raw pointers to the disjoint `text_ctx`/`arena`
/// fields so the caller can borrow `&mut editor` alongside them. Both pointers
/// are reborrowed as shared refs for the duration of the paint call.
///
/// # Safety
/// `text_ptr` / `arena_ptr` must point into the caller's `App` and outlive this
/// call. They do — `App` outlives the frame.
#[allow(clippy::too_many_arguments)]
unsafe fn paint_with_refs(
    editor: &mut Editor,
    text_ptr: *const Text,
    arena_ptr: *const Arena,
    host: &PaintHost,
    view_w: f32,
    view_h: f32,
    scroll_y: f32,
    blink: bool,
    metrics: &Metrics,
    palette: &Palette,
    display: &display::DisplayMap,
) {
    // SAFETY: pointers come from valid `&a.text_ctx` / `&a.arena` (see caller);
    // they remain valid for this call because `App` outlives the frame.
    let text_ctx = unsafe { &*text_ptr };
    let arena = unsafe { &*arena_ptr };
    text_ctx.set_scale(host.scale());
    // SAFETY: host.canvas() is the live flux_canvas for this paint call.
    unsafe {
        paint(
            editor,
            text_ctx,
            host.canvas(),
            arena,
            host.scale(),
            view_w,
            view_h,
            scroll_y,
            blink,
            metrics,
            palette,
            display,
        );
    }
}

fn handle_input(input: &Input, a: &mut App) {
    let raw = input.as_raw();
    let m = raw.mods;
    let ctrl = (m & mods::CTRL) != 0;
    let shift = (m & mods::SHIFT) != 0;

    let nkeys = raw.key_count as usize;
    let keys = &raw.keys[..nkeys.min(raw.keys.len())];

    // Control-key shortcuts first. On Linux/xkb the keysym for a printable
    // key with Ctrl held is the lowercase ASCII code of the character.
    if ctrl {
        for k in keys {
            if !k.pressed {
                continue;
            }
            match k.key {
                115 => save(a),              // Ctrl-S
                111 => open(a),              // Ctrl-O
                122 => a.editor.undo(),      // Ctrl-Z
                121 => a.editor.redo(),      // Ctrl-Y
                97 => a.editor.select_all(), // Ctrl-A
                _ => {}
            }
        }
        // Ctrl combos suppress ordinary text insertion.
        return;
    }

    let extend = shift;
    for k in keys {
        if !k.pressed || k.repeat {
            continue;
        }
        match k.key {
            x if x == key::LEFT => a
                .editor
                .move_carets(if ctrl { Dir::WordLeft } else { Dir::Left }, extend),
            x if x == key::RIGHT => a
                .editor
                .move_carets(if ctrl { Dir::WordRight } else { Dir::Right }, extend),
            x if x == key::UP => a.editor.move_carets(Dir::Up, extend),
            x if x == key::DOWN => a.editor.move_carets(Dir::Down, extend),
            x if x == key::HOME => a
                .editor
                .move_carets(if ctrl { Dir::DocStart } else { Dir::LineStart }, extend),
            x if x == key::END => a
                .editor
                .move_carets(if ctrl { Dir::DocEnd } else { Dir::LineEnd }, extend),
            x if x == key::BACKSPACE => a.editor.backspace(),
            x if x == key::DELETE => a.editor.delete(),
            x if x == key::RETURN => a.editor.insert("\n"),
            x if x == key::TAB => a.editor.insert("    "),
            _ => {}
        }
    }

    // Commit repeated movement keys (held arrows) even when repeat-flagged.
    if ctrl {
        return;
    }
    for k in keys {
        if !k.pressed || !k.repeat {
            continue;
        }
        match k.key {
            x if x == key::LEFT => a.editor.move_carets(Dir::Left, extend),
            x if x == key::RIGHT => a.editor.move_carets(Dir::Right, extend),
            x if x == key::UP => a.editor.move_carets(Dir::Up, extend),
            x if x == key::DOWN => a.editor.move_carets(Dir::Down, extend),
            x if x == key::BACKSPACE => a.editor.backspace(),
            x if x == key::DELETE => a.editor.delete(),
            _ => {}
        }
    }

    // Typed text (UTF-8). lens delivers committed characters here.
    let txt = c_str_to_str(&raw.text_utf8);
    if !txt.is_empty() {
        a.editor.insert(txt);
    }
}

fn c_str_to_str(buf: &[std::os::raw::c_char]) -> &str {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let bytes: &[u8] = unsafe { std::slice::from_raw_parts(buf.as_ptr() as *const u8, end) };
    std::str::from_utf8(bytes).unwrap_or("")
}

fn save(a: &mut App) {
    let text = a.editor.text();
    if let Some(p) = &a.path {
        if std::fs::write(p, &text).is_ok() {
            a.dirty = false;
        }
    } else {
        let p = PathBuf::from("untitled.txt");
        if std::fs::write(&p, &text).is_ok() {
            a.path = Some(p);
            a.dirty = false;
        }
    }
}

fn open(a: &mut App) {
    if let Some(uri) = iris::pick_file(Some("Open file")) {
        let path_str = uri.strip_prefix("file://").unwrap_or(&uri);
        if let Ok(text) = std::fs::read_to_string(path_str) {
            let path = PathBuf::from(path_str);
            // Preserve the (lazily-created) text ctx + arena across the reload.
            let text_ctx = a.text_ctx.take();
            let arena = a.arena.take();
            *a = App {
                editor: Editor::from_text(&text),
                path: Some(path),
                text_ctx,
                arena,
                scroll_y: 0.0,
                view_w: a.view_w,
                view_h: a.view_h,
                blink_phase: a.blink_phase,
                dirty: false,
            };
        }
    }
}

// lens re-exports LayoutOpts; bring it into scope for the column_ex/row_ex calls.
use iris::LayoutOpts;
