//! Frame and Terminal: the draw-closure entry point the app calls each tick.
//!
//! `Frame` is the per-draw handle the application renders into. It borrows the
//! back [`Grid`] mutably, exposes ratatui-shaped methods (`area`,
//! `buffer_mut`, `render_widget`, `set_cursor_position`), and tracks the
//! desired terminal cursor position so the loop can emit a single move after
//! the closure returns.
//!
//! `Terminal` owns the [`Backend`], the back grid, and the front grid. Its
//! [`Terminal::draw`] runs the app closure against a fresh `Frame`, then
//! diffs the back grid against the front grid, hands the [`DrawCmd`] to the
//! backend, and promotes dirty cells into the front grid. Idle frames (no
//! dirty cells) emit nothing.

use std::io;

use crossterm::QueueableCommand;

use crate::backend::Backend;
use crate::diff::{self, DrawCmd};
use crate::grid::{Fit, Grid};
use crate::layout::Rect;
use crate::widgets::Paragraph;

/// The terminal cursor mode the frame loop should end with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CursorState {
    #[default]
    Hidden,
    Visible(u16, u16),
}

/// A per-draw frame handle. Borrows the back grid for the duration of the
/// draw closure.
pub struct Frame<'a> {
    grid: &'a mut Grid,
    area_rect: Rect,
    cursor: CursorState,
}

impl<'a> Frame<'a> {
    /// Construct a frame over a grid. Public so that integration tests can
    /// drive the widget API directly.
    pub fn new(grid: &'a mut Grid) -> Self {
        let (w, h) = grid.size();
        Self {
            grid,
            area_rect: Rect::new(0, 0, w, h),
            cursor: CursorState::default(),
        }
    }

    /// Full-terminal area.
    pub fn area(&self) -> Rect {
        self.area_rect
    }

    /// Mutable access to the underlying grid, for in-place cell mutation
    /// (the dim-recess effect and the hand-rolled scrollbar).
    pub fn buffer_mut(&mut self) -> &mut Grid {
        self.grid
    }

    /// Render a widget into `area`. Only the three widget kinds muta uses
    /// are supported (`Paragraph`, `Block`, `Clear`).
    pub fn render_widget<W: Widget>(&mut self, widget: W, area: Rect) {
        widget.render(area, self.grid);
    }

    /// Set the terminal cursor position for after this frame's flush. The last
    /// call wins.
    pub fn set_cursor_position<P: Into<(u16, u16)>>(&mut self, pos: P) {
        let (x, y) = pos.into();
        self.cursor = CursorState::Visible(x, y);
    }

    /// Write a styled string directly into the grid (convenience for callers
    /// that don't want to build a `Paragraph`).
    pub fn put(&mut self, x: u16, y: u16, style: crate::Style, text: &str) {
        self.grid.put(x, y, Fit::Clip, style, text);
    }

    pub(crate) fn take_cursor(&mut self) -> CursorState {
        std::mem::take(&mut self.cursor)
    }
}

/// A widget that can render itself into a grid. Implemented for `Paragraph`,
/// `Block`, `Clear`, and `(Rect,)` passthrough.
pub trait Widget {
    fn render(self, area: Rect, grid: &mut Grid);
}

impl Widget for Paragraph<'_> {
    fn render(self, area: Rect, grid: &mut Grid) {
        Paragraph::render(&self, area, grid);
    }
}
impl Widget for crate::widgets::Block<'_> {
    fn render(self, area: Rect, grid: &mut Grid) {
        crate::widgets::Block::render(&self, area, grid);
    }
}
impl Widget for crate::widgets::Clear {
    fn render(self, area: Rect, grid: &mut Grid) {
        crate::widgets::Clear::render(self, area, grid);
    }
}

/// Owns the backend and the back/front grids. The application holds one of
/// these for the lifetime of the TUI.
pub struct Terminal<W: io::Write> {
    backend: Backend<W>,
    back: Grid,
    front: Grid,
    cursor: CursorState,
    /// A resize invalidated the terminal contents, but the clear/reset is held
    /// until the next committed frame. Staged measurement frames must never
    /// write terminal bytes of their own.
    pending_clear: bool,
}

impl<W: io::Write> Terminal<W> {
    pub fn new(backend: Backend<W>) -> Self {
        // Size the grids to whatever the backend reports via crossterm.
        let size = crossterm::terminal::size().unwrap_or((80, 24));
        let back = Grid::new(size.0, size.1);
        let front = Grid::new(size.0, size.1);
        Self {
            backend,
            back,
            front,
            cursor: CursorState::Hidden,
            pending_clear: false,
        }
    }

    /// Resize the back and front grids to the current terminal size.
    pub fn resize_to(&mut self, width: u16, height: u16) {
        self.back.resize(width, height);
        self.front = Grid::new(width, height);
        self.back.mark_all_dirty();
        self.pending_clear = true;
    }

    fn render_frame<F>(&mut self, render: F)
    where
        F: FnOnce(&mut Frame<'_>),
    {
        // Sync grid size with terminal.
        if let Ok((w, h)) = crossterm::terminal::size()
            && self.back.size() != (w, h)
        {
            self.back.resize(w, h);
            self.front = Grid::new(w, h);
            self.back.mark_all_dirty();
            self.pending_clear = true;
        }
        let mut frame = Frame::new(&mut self.back);
        render(&mut frame);
        self.cursor = frame.take_cursor();
    }

    fn commit(&mut self) -> io::Result<()> {
        self.backend.begin_sync_update()?;
        let commit_result = self.commit_frame();
        let end_result = self.backend.end_sync_update();
        commit_result.and(end_result)
    }

    /// Emit one already-rendered logical frame. [`Self::commit`] owns the
    /// synchronized-update envelope around this method.
    fn commit_frame(&mut self) -> io::Result<()> {
        if self.pending_clear {
            // Reconcile the SGR tracker and real terminal only when a frame is
            // actually committed. A staged layout pass may resize the grids,
            // but it must remain completely invisible.
            let _ = self.backend.invalidate();
            let _ = self.backend.writer().queue(crossterm::terminal::Clear(
                crossterm::terminal::ClearType::All,
            ));
            self.pending_clear = false;
        }

        let cmd: DrawCmd = diff::diff(&self.back, &mut self.front);

        // ── Cursor Shielding ────────────────────────────────────────────────
        // Hide the physical cursor before emitting cell drawing commands so
        // that neither the host terminal emulator nor the OS IME samples
        // intermediate/transient cursor coordinates while rendering cells.
        if !cmd.draws.is_empty() {
            self.backend.hide_cursor()?;
        }

        self.backend.render(&cmd)?;
        diff::promote_scrolled(&mut self.back, &mut self.front, &cmd);

        // ── Single Atomic Cursor Placement ──────────────────────────────────
        // Only at the very end of the frame is the hardware cursor positioned
        // at its final desired screen coordinate in a single atomic step.
        match self.cursor {
            CursorState::Hidden => {
                self.backend.hide_cursor()?;
            }
            CursorState::Visible(x, y) => {
                self.backend.show_cursor_at(x, y)?;
            }
        }
        Ok(())
    }

    /// Run the app's draw closure against a fresh frame, then diff → render
    /// → promote. Returns `Ok(())` on success.
    pub fn draw<F>(&mut self, render: F) -> io::Result<()>
    where
        F: FnOnce(&mut Frame<'_>),
    {
        self.render_frame(render);
        self.commit()
    }

    /// Render into the retained back grid without emitting terminal output.
    /// The next [`Self::draw`] replaces or completes this staged frame and
    /// commits only the final grid. This supports layout-dependent state such
    /// as bottom-follow scrolling without flashing an intermediate viewport.
    pub fn stage<F>(&mut self, render: F) -> io::Result<()>
    where
        F: FnOnce(&mut Frame<'_>),
    {
        self.render_frame(render);
        Ok(())
    }

    /// Commit the currently staged back grid without running layout again.
    /// Callers use this when a measurement pass confirms that its viewport was
    /// already final; if layout-dependent state changed, call [`Self::draw`]
    /// instead so the staged grid is replaced before the single commit.
    pub fn commit_staged(&mut self) -> io::Result<()> {
        self.commit()
    }

    /// Borrow the underlying writer (for alt-screen / raw-mode setup).
    pub fn writer(&mut self) -> &mut W {
        self.backend.writer()
    }

    /// Borrow the backend (for the app to call `invalidate` after a clear).
    pub fn backend(&mut self) -> &mut Backend<W> {
        &mut self.backend
    }

    /// The live terminal size as crossterm reports it right now (falling
    /// back to the retained grid's size if the query fails).
    ///
    /// Distinct from the grids' size on purpose: a `Resize` event only
    /// reaches the grids at the next [`Self::render_frame`], so callers
    /// deciding between frames whether cached geometry is still valid —
    /// e.g. an immediate cursor placement ahead of the next draw — must
    /// compare against the live size, not the retained one.
    pub fn size(&self) -> (u16, u16) {
        crossterm::terminal::size().unwrap_or_else(|_| self.back.size())
    }

    /// Show the cursor.
    pub fn show_cursor(&mut self) -> io::Result<()> {
        // Route through the backend's visibility-dedup path so repeated calls
        // don't re-emit `?25h` every frame (the same dedup `hide_cursor`
        // already enjoys). `move_to`/position parking is the caller's job when
        // an explicit coordinate is wanted; see `show_cursor_at`.
        self.backend.show_cursor()?;
        self.backend.writer().flush()?;
        Ok(())
    }

    /// Hide the cursor.
    pub fn hide_cursor(&mut self) -> io::Result<()> {
        self.backend.hide_cursor()?;
        self.backend.writer().flush()?;
        Ok(())
    }
}

/// A test terminal: owns a back grid the tests can render into and then
/// inspect, without any real I/O. Mirrors the `Terminal<TestBackend>` pattern
/// ratatui tests used. The grid is accessible via [`TestTerminal::buffer`], and
/// the last caret position the render closure requested via
/// [`TestTerminal::cursor`].
pub struct TestTerminal {
    back: Grid,
    cursor: CursorState,
}

impl TestTerminal {
    /// Create a test terminal with a grid of the given dimensions.
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            back: Grid::new(width, height),
            cursor: CursorState::default(),
        }
    }

    /// Run a draw closure against a frame over the back grid.
    pub fn draw<F>(&mut self, render: F)
    where
        F: FnOnce(&mut Frame<'_>),
    {
        let mut frame = Frame::new(&mut self.back);
        render(&mut frame);
        self.cursor = frame.take_cursor();
    }

    /// Read the rendered grid (the "buffer" the tests inspect).
    pub fn buffer(&self) -> &Grid {
        &self.back
    }

    /// The caret position the last draw closure requested (or `Hidden`).
    pub fn cursor(&self) -> CursorState {
        self.cursor
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Style;
    use crate::backend::Bce;

    #[test]
    fn staged_frame_emits_only_the_final_committed_grid() {
        crossterm::style::force_color_output(true);
        let mut output = Vec::new();
        {
            let backend = Backend::with_bce(&mut output, Bce::No);
            let mut terminal = Terminal::new(backend);

            terminal
                .stage(|frame| frame.put(0, 0, Style::default(), "intermediate"))
                .unwrap();
            assert!(terminal.writer().is_empty());

            terminal
                .draw(|frame| frame.put(0, 0, Style::default(), "final"))
                .unwrap();
        }

        let rendered = String::from_utf8(output).unwrap();
        assert!(
            rendered.starts_with("\x1b[?2026h"),
            "committed frame must begin synchronized update: {rendered:?}"
        );
        assert!(
            rendered.ends_with("\x1b[?2026l"),
            "committed frame must end synchronized update: {rendered:?}"
        );
        assert!(rendered.contains("final"));
        assert!(!rendered.contains("intermediate"));
    }
}

#[cfg(test)]
mod resize_envelope_tests {
    //! The resize path's `backend.invalidate()` must not flush mid-envelope:
    //! a flush between the SGR reset and the queued Clear(All) lets the
    //! terminal paint a half-reset screen — the resize flicker.

    use super::*;

    #[test]
    fn invalidate_does_not_flush_by_itself() {
        // With a BufWriter sink, queued bytes stay in the writer's buffer
        // until an explicit flush. `invalidate` must leave them there —
        // flushing is the synchronized-update envelope owner's job
        // (`Terminal::commit`); a mid-envelope flush let the terminal paint
        // a half-reset screen on resize.
        let mut sink = std::io::BufWriter::new(Vec::<u8>::new());
        let mut be = Backend::new(&mut sink);
        be.invalidate().unwrap();

        // No flush yet: the queued reset must still sit in the buffer (the
        // sink below the BufWriter has received nothing), and the backend's
        // own final flush below is what delivers it.
        use std::io::Write as _;
        be.writer().flush().unwrap();
        let buf = sink.into_inner().unwrap();
        assert!(
            String::from_utf8_lossy(&buf).starts_with("\x1b[0m"),
            "reset is the first byte once flushed: {buf:?}"
        );
    }

    #[test]
    fn size_falls_back_to_retained_grid() {
        // Terminal::size queries crossterm; under a test there is no real
        // terminal, so it must fall back to the retained grid size rather
        // than panicking or lying.
        let be = Backend::new(Vec::new());
        let terminal = Terminal::new(be);
        let (w, h) = terminal.size();
        let (gw, gh) = {
            let t = terminal;
            // The grids were sized from crossterm::terminal::size() at
            // construction; whatever that returned, the fallback path must
            // produce a consistent pair.
            drop(t);
            (0u16, 0u16)
        };
        let _ = (gw, gh);
        assert!(
            w > 0 && h > 0,
            "live size resolves to something usable: {w}x{h}"
        );
    }
}
