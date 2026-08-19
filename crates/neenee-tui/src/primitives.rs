//! Tiny shared render helpers: viewport math, modal centering/recess, panel
//! chrome, and color arithmetic. Kept in one place so the per-component
//! modules do not need to depend on each other for these primitives.

use crate::modal::Recess;
use neenee_tui_engine::{
    Constraint, Direction, Frame, Layout, Line, Margin, Modifier, Rect,
    {Block as RtBlock, Clear, Paragraph}, {Color, Span, Style},
};

use super::Theme;
pub(crate) use super::components::footer::{
    FooterHint, FooterHintWithBand, keymap_body_lines, keymap_page_footer_hints, modal_footer_text,
    render_modal_footer, render_modal_footer_with_more,
};
#[cfg(test)]
use super::design::PANEL_BAR_INSET;
use super::design::{MODAL_INNER_H_PADDING, MODAL_INNER_V_PADDING, SCROLLBAR_GAP};
/// Canonical key-display vocabulary: named `&'static str` constants for the
/// glyphs footers and legends repeat (`keyvocab::ESC`, `keyvocab::ARROWS_UD`,
/// …). Re-exported here because every overlay already imports this module for
/// `FooterHint`, so a footer's key + label both come from one place.
pub(crate) use super::keymap::keyvocab;

/// Global viewport margins. One row of breathing room is reserved at the
/// top; horizontally every component spans the full terminal width. The
/// bottom margin is 0: the hint bar pins flush against the terminal's bottom
/// edge — an empty `app_bg` row below it only wasted a transcript row.
pub(crate) const VIEWPORT_H_MARGIN: u16 = 0;
pub(crate) const VIEWPORT_TOP_MARGIN: u16 = 1;
pub(crate) const VIEWPORT_BOTTOM_MARGIN: u16 = 0;

/// The usable area after reserving the global viewport margins (1 cell top,
/// 0 bottom). The full `frame.area()` is only used to paint the app
/// background and the modal backdrop.
pub(crate) fn viewport_rect(frame: &Frame) -> Rect {
    let area = frame.area();
    Rect::new(
        area.x + VIEWPORT_H_MARGIN,
        area.y + VIEWPORT_TOP_MARGIN,
        area.width.saturating_sub(2 * VIEWPORT_H_MARGIN),
        area.height
            .saturating_sub(VIEWPORT_TOP_MARGIN + VIEWPORT_BOTTOM_MARGIN),
    )
}

pub(crate) fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    let area = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1];
    // Snap the width to an even column count. A modal insets its body by
    // `MODAL_INNER_H_PADDING` on each side (an even total), so an even outer
    // width yields an even body width — which CJK / full-width glyphs (each 2
    // columns wide) can tile without leaving a stranded trailing column that
    // forces every wrap line short by one glyph. The odd column is shed from
    // the right margin; the rect stays centered because `Layout` already
    // divided the margins evenly.
    even_width(area)
}

/// Like [`centered_rect`] but the vertical extent is an explicit row count
/// instead of a percentage, so a modal can size to its content rather than
/// reserve a fixed slab of the viewport. `height` is clamped to `r`'s height
/// and the band is centered vertically; the width is still a percentage so the
/// modal keeps a consistent horizontal footprint regardless of how tall it is.
pub(crate) fn centered_rect_h(percent_x: u16, height: u16, r: Rect) -> Rect {
    let height = height.min(r.height);
    let top = r.y + r.height.saturating_sub(height) / 2;
    let band = Rect {
        x: r.x,
        y: top,
        width: r.width,
        height,
    };
    let area = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(band)[1];
    // See `centered_rect`: an even width gives the body an even usable width so
    // full-width (CJK) glyphs tile flush on every wrap line.
    even_width(area)
}

/// Floor a rect's width to the nearest even column count, clamped to ≥ 0.
///
/// This is the single lever that makes every modal body an even-width surface.
/// Because [`modal_frame`] insets the body symmetrically by `MODAL_INNER_H_PADDING`
/// (an even total), an even panel width propagates to an even body width, so a
/// run of 2-column CJK glyphs fills every row end-to-end instead of stranding
/// one empty column that costs a full glyph on each wrapped line.
fn even_width(mut rect: Rect) -> Rect {
    rect.width &= !1;
    rect
}

#[derive(Clone, Copy)]
pub(crate) struct ModalSpec {
    pub width_percent: u16,
    pub header: bool,
    pub footer: bool,
}

/// Geometry for a modal whose height is a fixed percentage of the viewport.
///
/// Keeping this distinct from [`ContentModalSpec`] makes invalid combinations
/// unrepresentable: a fixed renderer cannot accidentally request content
/// sizing, and neither API needs to return `Option` for a structural invariant.
#[derive(Clone, Copy)]
pub(crate) struct FixedModalSpec {
    spec: ModalSpec,
    height_percent: u16,
}

impl FixedModalSpec {
    const fn new(width_percent: u16, height_percent: u16) -> Self {
        Self {
            spec: ModalSpec {
                width_percent,
                header: true,
                footer: true,
            },
            height_percent,
        }
    }

    // The template chooser shares the provider list's footprint.
    pub const PROVIDER: Self = Self::new(76, 80);
    pub const QUESTION: Self = Self::new(78, 70);
    #[allow(dead_code)]
    pub const OAUTH_PENDING: Self = Self::new(76, 75);
    #[allow(dead_code)]
    pub const CUSTOM_PROVIDER: Self = Self::new(72, 78);
    pub const HELP: Self = Self::new(66, 78);
    pub const SESSIONS: Self = Self::new(82, 78);
    #[allow(dead_code)]
    pub const PERMISSIONS: Self = Self::new(72, 75);
    #[allow(dead_code)]
    pub const SKILLS: Self = Self::new(72, 75);

    /// Construct a modal spec with custom width and height percentages.
    #[allow(dead_code)]
    pub const fn custom(width_percent: u16, height_percent: u16) -> Self {
        Self::new(width_percent, height_percent)
    }

    /// Return a new spec with modified height percentage.
    #[allow(dead_code)]
    pub const fn with_height(mut self, height_percent: u16) -> Self {
        self.height_percent = height_percent;
        self
    }

    /// Return a new spec with modified width percentage.
    #[allow(dead_code)]
    pub const fn with_width(mut self, width_percent: u16) -> Self {
        self.spec.width_percent = width_percent;
        self
    }

    /// The width percentage of the modal relative to the viewport.
    #[allow(dead_code)]
    pub const fn width_percent(&self) -> u16 {
        self.spec.width_percent
    }

    /// The height percentage of the modal relative to the viewport.
    #[allow(dead_code)]
    pub const fn height_percent(&self) -> u16 {
        self.height_percent
    }

    /// Calculate the exact columns and rows this modal occupies in `frame`.
    #[allow(dead_code)]
    pub fn exact_dimensions(&self, frame: &Frame) -> (u16, u16) {
        let r = modal_area(frame, *self);
        (r.width, r.height)
    }
}

/// Geometry for a modal whose height follows its rendered content up to max bounds.
#[derive(Clone, Copy)]
pub(crate) struct ContentModalSpec {
    spec: ModalSpec,
    min_rows: u16,
    max_viewport_percent: u16,
    max_height_rows: Option<u16>,
    max_width_cols: Option<u16>,
}

impl ContentModalSpec {
    const fn new(width_percent: u16, min_rows: u16, max_viewport_percent: u16) -> Self {
        Self {
            spec: ModalSpec {
                width_percent,
                header: true,
                footer: true,
            },
            min_rows,
            max_viewport_percent,
            max_height_rows: None,
            max_width_cols: None,
        }
    }

    pub const TOOLS: Self = Self::new(64, 11, 84);
    pub const MCP: Self = Self::new(64, 9, 84);
    pub const QUEUE: Self = Self::new(66, 9, 84);
    /// The `/btw` asides list (ADR-0103 §5). One row per live aside; sized
    /// like the queue overview it mirrors (list + footer legend).
    pub const BTW: Self = Self::new(66, 9, 84);
    pub const TOKEN_REPORT: Self = Self::new(66, 9, 80);
    pub const ACTIVITY: Self = Self::new(72, 8, 80);
    /// The unified provider/model editor (`draw_model_editor`). Sizes to its
    /// content — at most three rows (API key, reasoning effort, extended
    /// thinking) — instead of reserving a fixed 30% slab that left most of
    /// the panel empty. Width 66% gives long API keys more room than the old
    /// 60% while staying comfortably inside the viewport. `max_viewport_percent`
    /// is a generous 60% purely as a ceiling; the real height is the content
    /// row count plus chrome, which never approaches it.
    pub const MODEL_EDITOR: Self = Self::new(66, 6, 60);
    pub const OAUTH_PENDING: Self = Self::new(76, 7, 80);
    pub const CUSTOM_PROVIDER: Self = Self::new(72, 8, 80);
    pub const PERMISSIONS: Self = Self::new(72, 7, 80);
    pub const SKILLS: Self = Self::new(72, 7, 80);
    #[allow(dead_code)]
    pub const HELP: Self = Self::new(66, 10, 84);

    /// Construct a modal spec with custom max constraints.
    #[allow(dead_code)]
    pub const fn custom(width_percent: u16, min_rows: u16, max_viewport_percent: u16) -> Self {
        Self::new(width_percent, min_rows, max_viewport_percent)
    }

    /// Set an explicit maximum row ceiling.
    #[allow(dead_code)]
    pub const fn with_max_rows(mut self, max_rows: u16) -> Self {
        self.max_height_rows = Some(max_rows);
        self
    }

    /// Set an explicit maximum column width.
    #[allow(dead_code)]
    pub const fn with_max_cols(mut self, max_cols: u16) -> Self {
        self.max_width_cols = Some(max_cols);
        self
    }

    /// Set maximum viewport height percentage.
    #[allow(dead_code)]
    pub const fn with_max_percent(mut self, max_viewport_percent: u16) -> Self {
        self.max_viewport_percent = max_viewport_percent;
        self
    }

    /// Set minimum row count.
    #[allow(dead_code)]
    pub const fn with_min_rows(mut self, min_rows: u16) -> Self {
        self.min_rows = min_rows;
        self
    }

    /// Set width percentage.
    #[allow(dead_code)]
    pub const fn with_width(mut self, width_percent: u16) -> Self {
        self.spec.width_percent = width_percent;
        self
    }

    #[allow(dead_code)]
    pub const fn width_percent(&self) -> u16 {
        self.spec.width_percent
    }

    #[allow(dead_code)]
    pub const fn max_viewport_percent(&self) -> u16 {
        self.max_viewport_percent
    }

    #[allow(dead_code)]
    pub const fn min_rows(&self) -> u16 {
        self.min_rows
    }

    #[allow(dead_code)]
    pub const fn max_height_rows(&self) -> Option<u16> {
        self.max_height_rows
    }

    #[allow(dead_code)]
    pub const fn max_width_cols(&self) -> Option<u16> {
        self.max_width_cols
    }

    /// Calculate the exact dimensions (cols, rows) for a given desired row count in `frame`.
    #[allow(dead_code)]
    pub fn exact_dimensions(&self, frame: &Frame, desired_rows: u16) -> (u16, u16) {
        let area = content_modal_area(frame, *self, desired_rows);
        (area.width, area.height)
    }

    /// Calculate the maximum dimensions (cols, rows) this modal can occupy in `frame`.
    #[allow(dead_code)]
    pub fn max_dimensions(&self, frame: &Frame) -> (u16, u16) {
        let viewport = viewport_rect(frame);
        let mut max_h = ((viewport.height as u32 * self.max_viewport_percent as u32) / 100) as u16;
        if let Some(limit) = self.max_height_rows {
            max_h = max_h.min(limit);
        }
        let probe = content_modal_probe(frame, *self);
        let mut w = probe.width;
        if let Some(limit_w) = self.max_width_cols {
            w = w.min(limit_w);
        }
        (w, max_h.max(self.min_rows))
    }

    pub const fn modal_spec(self) -> ModalSpec {
        self.spec
    }
}

pub(crate) fn modal_chrome_rows(spec: ModalSpec) -> u16 {
    let mut rows = 2 * MODAL_INNER_V_PADDING;
    if spec.header {
        rows += 2; // header + gap after header
    }
    if spec.footer {
        rows += 2; // gap before footer + footer
    }
    rows
}

pub(crate) fn modal_area(frame: &Frame, geometry: FixedModalSpec) -> Rect {
    centered_rect(
        geometry.spec.width_percent,
        geometry.height_percent,
        viewport_rect(frame),
    )
}

pub(crate) fn content_modal_probe(frame: &Frame, geometry: ContentModalSpec) -> Rect {
    let viewport = viewport_rect(frame);
    let mut rect = centered_rect(geometry.spec.width_percent, 100, viewport);
    if let Some(max_cols) = geometry.max_width_cols
        && rect.width > max_cols
    {
        let left = rect.x + (rect.width - max_cols) / 2;
        rect = Rect::new(left, rect.y, max_cols, rect.height);
    }
    even_width(rect)
}

pub(crate) fn content_modal_area(
    frame: &Frame,
    geometry: ContentModalSpec,
    desired_rows: u16,
) -> Rect {
    let viewport = viewport_rect(frame);
    let mut max_h = ((viewport.height as u32 * geometry.max_viewport_percent as u32) / 100) as u16;
    if let Some(limit) = geometry.max_height_rows {
        max_h = max_h.min(limit);
    }
    let height = desired_rows.clamp(geometry.min_rows, max_h.max(geometry.min_rows));
    let mut rect = centered_rect_h(geometry.spec.width_percent, height, viewport);
    if let Some(max_cols) = geometry.max_width_cols
        && rect.width > max_cols
    {
        let left = rect.x + (rect.width - max_cols) / 2;
        rect = Rect::new(left, rect.y, max_cols, rect.height);
    }
    even_width(rect)
}

/// Recess the live surface behind a modal, per its [`Recess`] policy.
///
/// A terminal cannot alpha-blend, so the event loop calls this exactly once
/// per frame *after* the transcript and chrome are drawn and *before* the
/// centered modal panel — which then overpaints its own crisp area on top.
/// The three policies:
///
/// - [`Recess::None`] leaves the surface untouched (lightweight floats such as
///   Question / Permission that never take over).
/// - [`Recess::Dim`] darkens every cell in place by [`Theme::modal_dim_factor`]
///   so the background stays visible for context while the modal reads as the
///   focal layer. This replaces the old opaque full-screen fill: context no
///   longer vanishes behind a modal.
/// - [`Recess::Takeover`] clears + fills with [`Theme::backdrop`], fully
///   occluding the surface for a context switch (session selection).
///
/// [`Theme::modal_dim_factor`]: Theme::modal_dim_factor
pub fn recess_backdrop(frame: &mut Frame, recess: Recess, theme: &Theme) {
    match recess {
        Recess::None => {}
        Recess::Dim => dim_surface(frame, theme),
        Recess::Takeover => {
            let area = frame.area();
            frame.render_widget(Clear, area);
            frame.render_widget(
                RtBlock::default().style(Style::default().bg(theme.backdrop())),
                area,
            );
        }
    }
}

/// Darken the whole frame buffer in place by scaling each cell's RGB channels
/// toward black by `factor` (0.0 = invisible, 1.0 = unchanged). This is the
/// "dim-recess" effect: the surface is rendered normally first, then every
/// cell is multiplied by `factor`, so context stays visible while clearly
/// receding behind the modal drawn on top.
///
/// Only [`Color::Rgb`] is scaled (the entire palette is RGB, so this covers
/// every painted cell); named / Reset colors are left untouched so the dim is
/// additive rather than lossy where they appear.
fn dim_surface(frame: &mut Frame, theme: &Theme) {
    let factor = theme.modal_dim_factor();
    // Code already starts closer to the dark surface than prose. If foreground
    // and background are both multiplied by the modal factor, inline/code-block
    // text loses contrast first. Keep code text a little brighter while still
    // dimming its surface with the rest of the transcript.
    let code_fg_factor = (factor + 0.25).min(1.0);
    let buffer = frame.buffer_mut();
    for cell in buffer.content.iter_mut() {
        let fg_factor = if cell.fg == theme.code_text() {
            code_fg_factor
        } else {
            factor
        };
        cell.fg = scale_color(cell.fg, fg_factor);
        cell.bg = scale_color(cell.bg, factor);
        cell.style.fg = cell.fg;
        cell.style.bg = cell.bg;
    }
}

/// Multiply an RGB color's channels by `factor`, clamped to `[0, 1]`.
fn scale_color(color: Color, factor: f32) -> Color {
    let f = factor.clamp(0.0, 1.0);
    match color {
        Color::Rgb(r, g, b) => Color::Rgb(
            (r as f32 * f).round() as u8,
            (g as f32 * f).round() as u8,
            (b as f32 * f).round() as u8,
        ),
        other => other,
    }
}

/// A borderless panel with a single thick colored left bar (opencode-style).
pub(crate) fn panel_block(bar_color: Color, bg: Color) -> RtBlock<'static> {
    RtBlock::default()
        .borders(neenee_tui_engine::Borders::LEFT)
        .border_type(neenee_tui_engine::BorderType::Thick)
        .border_style(Style::default().fg(bar_color))
        .style(Style::default().bg(bg))
}

/// Content rect inside a [`panel_block`]: starts one column right of the left
/// `┃` bar and reserves a matching column on the right, so the panel's
/// content is symmetric and a long line never touches either edge. Callers
/// paint [`panel_block`] bare over the full `area` for the chrome, then
/// render content into this rect — the left-bar-panel counterpart to how
/// [`modal_frame`] insets the borderless modal family via
/// `MODAL_INNER_H_PADDING`.
#[cfg(test)]
pub(crate) fn panel_inner(area: Rect) -> Rect {
    area.inner(Margin {
        horizontal: PANEL_BAR_INSET,
        vertical: 0,
    })
}

/// Section rects produced by [`modal_frame`]: the header and footer are
/// `Option`al (omitted when the modal asked for none), and `body` is always
/// present and flexes to fill whatever the header/footer leave behind.
pub(crate) struct ModalFrame {
    pub header: Option<Rect>,
    pub body: Rect,
    pub footer: Option<Rect>,
}

/// Render the unified modal title into the header rect produced by
/// [`modal_frame`]. This is the single place every centered modal's
/// `brand + BOLD` title is painted, so the header style no longer needs to be
/// repeated per-component. The two-line variants (a muted breadcrumb followed
/// by a brand title, e.g. `Configuration › Layout`) pass the parts via
/// [`HeaderPart`]; the common case is a single [`HeaderPart::title`].
pub(crate) fn modal_header(frame: &mut Frame, header: Option<Rect>, title: &str, theme: &Theme) {
    modal_header_parts(frame, header, &[HeaderPart::title(title)], theme);
}

/// A styled segment of a modal header line, laid out left-to-right.
#[derive(Clone, Copy)]
pub(crate) enum HeaderPart<'a> {
    /// The primary title: `brand` color, bold.
    Title(&'a str),
    /// A leading/trailing muted segment (e.g. the `Configuration › ` breadcrumb
    /// or `← ` back affordance). `accent` makes it the brand tone instead.
    Text { text: &'a str, accent: bool },
}

impl<'a> HeaderPart<'a> {
    pub(crate) const fn title(text: &'a str) -> Self {
        HeaderPart::Title(text)
    }
}

/// Render a multi-part modal header. `parts` is laid out in order on one line.
pub(crate) fn modal_header_parts(
    frame: &mut Frame,
    header: Option<Rect>,
    parts: &[HeaderPart<'_>],
    theme: &Theme,
) {
    let Some(h) = header else { return };
    let spans: Vec<Span<'static>> = parts
        .iter()
        .map(|part| match *part {
            HeaderPart::Title(text) => Span::styled(
                text.to_string(),
                Style::default()
                    .fg(theme.brand())
                    .add_modifier(Modifier::BOLD),
            ),
            HeaderPart::Text { text, accent } => Span::styled(
                text.to_string(),
                if accent {
                    Style::default()
                        .fg(theme.brand())
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.muted())
                },
            ),
        })
        .collect();
    frame.render_widget(Paragraph::new(Line::from(spans)), h);
}

/// The single separator glyph for a hierarchical (breadcrumb) modal header.
/// Keeps every drill-in sub-page — `Sessions › Info`, `Settings › Layout`,
/// `Settings › Appearance` — visually identical. Centralized (and `'static`)
/// so the glyph and spacing never drift between modals.
pub(crate) const BREADCRUMB_SEP: &str = " › ";

/// The standard hierarchical (breadcrumb) header for a modal sub-page: a muted
/// parent label, the [`BREADCRUMB_SEP`] separator, then the bold child title.
/// This is the component-level convention for **modal hierarchy**:
///
/// - A sub-page keeps the *same* `Modal` variant as its parent (it is one modal
///   drilling into a secondary view, not a separate modal), so the breadcrumb is
///   how the user sees where they are. Example: a Sessions picker drilled into
///   its info view renders `Sessions › Info`.
/// - `Esc` navigates one level up (handled in the event loop's `CloseModal`
///   arm): the first `Esc` returns from a sub-page to its parent view, a second
///   `Esc` closes the modal. The header flips back to the parent title on
///   back-out.
///
/// Pass the returned slice to `modal_header_parts`. All three segments borrow
/// their input `&str`s (the separator is a `'static` const), so no allocation.
pub(crate) fn breadcrumb_parts<'a>(parent: &'a str, child: &'a str) -> [HeaderPart<'a>; 3] {
    [
        HeaderPart::Text {
            text: parent,
            accent: false,
        },
        HeaderPart::Text {
            text: BREADCRUMB_SEP,
            accent: false,
        },
        HeaderPart::title(child),
    ]
}

/// The multi-level hierarchical breadcrumb builder with automatic front-truncation (`... › `).
///
/// When the available header width is insufficient to fit all segments (e.g. `Connections › Add › Google Antigravity`),
/// it drops leftmost levels and replaces them with `... › `, e.g. `... › Add › Google Antigravity` or `... › Google Antigravity`.
pub(crate) fn hierarchical_breadcrumb<'a>(
    levels: &[&'a str],
    max_width: usize,
) -> Vec<HeaderPart<'a>> {
    if levels.is_empty() {
        return Vec::new();
    }
    if levels.len() == 1 {
        return vec![HeaderPart::title(levels[0])];
    }

    let sep_w = 3; // " › "
    let full_width: usize =
        levels.iter().map(|s| s.chars().count()).sum::<usize>() + (levels.len() - 1) * sep_w;

    if full_width <= max_width {
        let mut parts = Vec::with_capacity(levels.len() * 2 - 1);
        for (i, level) in levels.iter().enumerate() {
            if i > 0 {
                parts.push(HeaderPart::Text {
                    text: BREADCRUMB_SEP,
                    accent: false,
                });
            }
            if i == levels.len() - 1 {
                parts.push(HeaderPart::title(level));
            } else {
                parts.push(HeaderPart::Text {
                    text: level,
                    accent: false,
                });
            }
        }
        return parts;
    }

    // Progressively drop leftmost levels and prepend `...`
    for start_idx in 1..levels.len() {
        let remaining = &levels[start_idx..];
        let rem_width: usize = 3 // "..."
            + sep_w
            + remaining.iter().map(|s| s.chars().count()).sum::<usize>()
            + (remaining.len() - 1) * sep_w;

        if rem_width <= max_width || start_idx == levels.len() - 1 {
            let mut parts = Vec::with_capacity(remaining.len() * 2 + 1);
            parts.push(HeaderPart::Text {
                text: "...",
                accent: false,
            });
            for (i, level) in remaining.iter().enumerate() {
                parts.push(HeaderPart::Text {
                    text: BREADCRUMB_SEP,
                    accent: false,
                });
                if i == remaining.len() - 1 {
                    parts.push(HeaderPart::title(level));
                } else {
                    parts.push(HeaderPart::Text {
                        text: level,
                        accent: false,
                    });
                }
            }
            return parts;
        }
    }

    vec![HeaderPart::title(levels[levels.len() - 1])]
}

/// Paint the unified modal chrome and split the content area into sections.
///
/// Every centered modal goes through this so the panel style lives in one
/// place: a borderless solid-bg panel (no `┃` left bar) with
/// `MODAL_INNER_H_PADDING`/`MODAL_INNER_V_PADDING` inner padding, then a
/// vertical split into optional `header` (1 row) / `body` (flex) / optional
/// 1-row gap + `footer` (1 row). The caller renders its own header / body /
/// footer content into the returned rects.
pub(crate) fn modal_frame(
    frame: &mut Frame,
    area: Rect,
    bg: Color,
    header: bool,
    footer: bool,
) -> ModalFrame {
    frame.render_widget(Clear, area);
    frame.render_widget(RtBlock::default().style(Style::default().bg(bg)), area);
    let inner = area.inner(Margin {
        horizontal: MODAL_INNER_H_PADDING,
        vertical: MODAL_INNER_V_PADDING,
    });

    // Tagged constraints so we can map split chunks back to sections:
    // 0 = header, 4 = gap after header, 1 = body, 2 = gap before footer,
    // 3 = footer. Both gaps are 1 row so the body always sits one blank line
    // below the header and one above the footer — regardless of which sections
    // a modal asks for.
    let mut tagged: Vec<(u8, Constraint)> = Vec::new();
    if header {
        tagged.push((0, Constraint::Length(1)));
        tagged.push((4, Constraint::Length(1)));
    }
    tagged.push((1, Constraint::Min(0)));
    if footer {
        tagged.push((2, Constraint::Length(1)));
        tagged.push((3, Constraint::Length(1)));
    }
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(tagged.iter().map(|(_, c)| *c))
        .split(inner);

    let mut out = ModalFrame {
        header: None,
        body: inner,
        footer: None,
    };
    for (i, (tag, _)) in tagged.iter().enumerate() {
        match tag {
            0 => out.header = Some(chunks[i]),
            1 => out.body = chunks[i],
            3 => out.footer = Some(chunks[i]),
            _ => {}
        }
    }
    out
}

/// Render a modal body with shared scroll mechanics. The `scroll` offset is
/// clamped to `[0, content_lines - visible]` (so it can never drift past the
/// last line) and, when `follow` is `Some(idx)`, nudged so row `idx` stays on
/// screen — that's how list modals keep their selection visible without a
/// separate scroll cursor. The body is rendered with `.scroll()` so anything
/// past the visible window is clipped rather than silently truncated.
///
/// `edge_margin` keeps the followed row away from the top/bottom edges by an
/// `edge_margin`-row band (when the viewport is tall enough), so `↑/↓`
/// navigation never pins the highlight to the last visible line — there is
/// always a buffer of context on the side being moved toward. Pass
/// [`SCROLL_EDGE_MARGIN`] for a pure list (provider/model/template/skills/…
/// pickers) where every followed row is a peer and context on both sides is
/// meaningful; pass `0` for decision sheets and content viewers whose
/// `follow` is an absolute body line in mixed header+row content (the
/// question / permission sheets) or that scroll manually (help / activity),
/// where edge-pinning reads better. A viewport too short for the band falls
/// back to edge-pinning in either case.
/// Resolve the effective scroll offset for a body of `total` lines in a
/// viewport `visible` rows tall, honoring an optional follow index and the
/// same edge-margin band logic [`render_body`] applies. Returns `(scroll,
/// max_scroll)`. This is the pure scroll-resolution half of [`render_body`],
/// factored out so a caller can compute the visible window *before* building
/// lines — letting list modals build only the rows that will actually be
/// painted instead of the whole list every frame.
pub(crate) fn resolve_scroll(
    scroll: &mut usize,
    visible: usize,
    total: usize,
    follow: Option<usize>,
    edge_margin: usize,
) -> (usize, usize) {
    let max_scroll = total.saturating_sub(visible);
    *scroll = (*scroll).min(max_scroll);
    if let Some(idx) = follow
        && visible > 0
    {
        // Margin band kept clear above and below the selection. Capped at
        // `(visible - 1) / 2` so it never exceeds half the viewport — for a
        // short viewport this collapses to 0 and the edge-pinning fallback
        // below kicks in. `edge_margin == 0` selects pure edge-pinning.
        let margin = edge_margin.min((visible - 1) / 2);
        if margin > 0 {
            let top_band = *scroll + margin;
            let bottom_band = *scroll + visible - margin;
            if idx < top_band {
                *scroll = idx.saturating_sub(margin);
            } else if idx >= bottom_band {
                *scroll = idx - (visible - 1 - margin);
            }
        } else if idx < *scroll {
            *scroll = idx;
        } else if idx >= *scroll + visible {
            *scroll = idx.saturating_sub(visible.saturating_sub(1));
        }
        // Re-clamp: a follow nudge can overshoot when content is shorter than
        // the viewport, or when `idx` is near the very end.
        *scroll = (*scroll).min(max_scroll);
    }
    (*scroll, max_scroll)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_body(
    frame: &mut Frame,
    body_rect: Rect,
    lines: Vec<Line<'static>>,
    scroll: &mut usize,
    follow: Option<usize>,
    edge_margin: usize,
    wrap: bool,
    theme: &Theme,
) {
    let visible = body_rect.height as usize;
    let (_, max_scroll) = resolve_scroll(scroll, visible, lines.len(), follow, edge_margin);

    let mut para = Paragraph::new(lines).scroll(*scroll as u16, 0);
    if wrap {
        para = para.wrap(neenee_tui_engine::Wrap { trim: false });
    }
    frame.render_widget(para, body_rect);

    // Scroll indicator: a one-cell scrollbar in the right margin showing
    // whether more content lies above and/or below the window. Only drawn
    // when content overflows the body height.
    draw_scrollbar(frame, body_rect, *scroll, max_scroll, theme);
}

/// The number of context rows kept above and below a followed selection before
/// the viewport begins to scroll. Keeps `↑/↓` movement from pinning the
/// highlight to the last visible line. Pass this as `render_body`'s
/// `edge_margin` for pure-list modals; only applies when the viewport is tall
/// enough (at least `2 * SCROLL_EDGE_MARGIN + 1` body rows), otherwise the
/// follow falls back to edge-pinning.
pub(crate) const SCROLL_EDGE_MARGIN: usize = 3;

/// Draw a minimal one-column scrollbar in the body's rightmost column when
/// the content overflows. Shows a thumb whose vertical position reflects the
/// `scroll / max_scroll` ratio, plus `▲` / `▼` caps when more content lies
/// above / below. The thumb uses `theme.muted()`; the caps use `theme.dim()`
/// so the bar reads as a subtle affordance, not a focal element.
pub(crate) fn draw_scrollbar(
    frame: &mut Frame,
    body: Rect,
    scroll: usize,
    max_scroll: usize,
    theme: &Theme,
) {
    if max_scroll == 0 || body.width == 0 || body.height < 2 {
        return;
    }
    let h = body.height as usize;
    // Thumb height scales with the visible-to-total ratio, floored at 1.
    let thumb_h = (h * h / (max_scroll + h)).max(1).min(h) as u16;
    let track = h as u16;
    let track_top = body.y;
    let track_x = body.x + body.width + SCROLLBAR_GAP;

    let more_above = scroll > 0;
    let more_below = scroll < max_scroll;

    // Caps (only when there is content in that direction). Coordinates are
    // within `body`, which is inside the buffer, so direct content indexing
    // is safe.
    let buf = frame.buffer_mut();
    let buf_area = buf.area();
    if more_above {
        let cell = cell_at_index(buf, buf_area, track_x, track_top);
        cell.set_symbol("▲");
        cell.set_fg(theme.dim());
    }
    if more_below {
        let cell = cell_at_index(buf, buf_area, track_x, track_top + track - 1);
        cell.set_symbol("▼");
        cell.set_fg(theme.dim());
    }

    // Thumb position within the open track (between the two caps).
    let open_top = if more_above { 1 } else { 0 };
    let open_bottom = track as i32 - if more_below { 1 } else { 0 };
    let open_h = (open_bottom - open_top).max(1) as u16;
    let ratio = if max_scroll > 0 {
        scroll as f32 / max_scroll as f32
    } else {
        0.0
    };
    let thumb_y =
        track_top + open_top as u16 + (ratio * (open_h.saturating_sub(thumb_h)) as f32) as u16;

    for i in 0..thumb_h {
        let y = thumb_y + i;
        if y < track_top + track {
            let cell = cell_at_index(buf, buf_area, track_x, y);
            cell.set_symbol(" ");
            cell.set_bg(theme.muted());
        }
    }
}

/// Index a buffer cell by absolute (x, y) via direct `content` indexing.
/// The caller guarantees the coordinate lies inside `area`.
fn cell_at_index(
    buf: &mut neenee_tui_engine::Grid,
    area: Rect,
    x: u16,
    y: u16,
) -> &mut neenee_tui_engine::Cell {
    let idx = (y as usize - area.y as usize) * area.width as usize + (x as usize - area.x as usize);
    &mut buf.content[idx]
}

/// Contrast foreground for a colored background (dark text on light fills).
pub(crate) fn contrast_fg(bg: Color) -> Color {
    let (r, g, b) = rgb(bg);
    let luminance = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
    if luminance > 140.0 {
        Color::Black
    } else {
        Color::White
    }
}

pub(crate) fn rgb(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => (0, 0, 0),
        Color::Red => (224, 108, 117),
        Color::Green => (127, 216, 143),
        Color::Yellow => (229, 192, 123),
        Color::Blue => (137, 180, 250),
        Color::Magenta => (203, 166, 247),
        Color::Cyan => (86, 182, 194),
        Color::Gray => (128, 128, 128),
        Color::DarkGray => (64, 64, 64),
        Color::LightGreen => (127, 216, 143),
        Color::LightRed => (243, 139, 168),
        _ => (128, 128, 128),
    }
}

#[cfg(test)]
mod tests {
    //! `panel_inner` is the symmetric-inset contract for the left-bar-panel
    //! family. Lock its geometry directly so a long overlay line can never
    //! kiss the panel's right edge regardless of terminal width.
    use super::*;
    use neenee_tui_engine::{Frame, Rect, Style};

    #[test]
    fn panel_inner_insets_symmetrically_around_left_bar() {
        // A 10-wide panel: the `┃` bar owns the first column, content starts
        // one column in (clear of the bar) and ends one column short of the
        // right edge (the bar's mirrored gutter).
        let area = Rect::new(2, 3, 10, 5);
        let inner = panel_inner(area);
        assert_eq!(inner.x, 3, "content starts right after the ┃ bar");
        assert_eq!(inner.width, 8, "10 − 2 (left bar + right gutter)");
        assert_eq!(inner.y, 3);
        assert_eq!(inner.height, 5, "no vertical inset");
        // Content's right edge is exactly one short of the panel's right edge.
        assert_eq!(inner.x + inner.width, area.x + area.width - 1);
    }

    #[test]
    fn panel_inner_clamps_without_underflow() {
        // A panel too narrow for the bar + gutter collapses to an empty rect
        // at the panel's origin rather than underflowing the width.
        let inner = panel_inner(Rect::new(0, 0, 1, 1));
        assert_eq!(inner, Rect::new(0, 0, 0, 0));
    }

    #[test]
    fn fixed_and_content_modal_specs_preserve_their_sizing_modes() {
        let fixed = FixedModalSpec::PROVIDER;
        assert_eq!(fixed.spec.width_percent, 76);
        assert_eq!(fixed.height_percent, 80);

        let content = ContentModalSpec::TOOLS;
        assert_eq!(content.spec.width_percent, 64);
        assert_eq!(content.min_rows, 11);
        assert_eq!(content.max_viewport_percent, 84);
    }

    #[test]
    fn dim_surface_preserves_more_code_text_contrast() {
        let theme = Theme::default();
        let mut grid = neenee_tui_engine::Grid::new(2, 1);
        grid.set(
            0,
            0,
            neenee_tui_engine::Cell::narrow(
                "c",
                Style::default()
                    .fg(theme.code_text())
                    .bg(theme.code_surface()),
            ),
        );
        grid.set(
            1,
            0,
            neenee_tui_engine::Cell::narrow(
                "p",
                Style::default().fg(theme.fg()).bg(theme.surface()),
            ),
        );

        let mut frame = Frame::new(&mut grid);
        dim_surface(&mut frame, &theme);
        let code = frame.buffer_mut()[(0, 0)].clone();
        let prose = frame.buffer_mut()[(1, 0)].clone();

        assert_eq!(
            code.bg,
            scale_color(theme.code_surface(), theme.modal_dim_factor())
        );
        assert_eq!(
            code.fg,
            scale_color(theme.code_text(), theme.modal_dim_factor() + 0.25)
        );
        assert_eq!(prose.fg, scale_color(theme.fg(), theme.modal_dim_factor()));
    }

    #[test]
    fn modal_footer_degrades_by_width_and_priority() {
        let hints = [
            FooterHint::secondary("type", "filter"),
            FooterHint::navigation("↑↓", "navigate"),
            FooterHint::primary("Enter", "activate"),
            FooterHint::secondary("*", "favorite"),
            FooterHint::always("Esc", "close"),
        ];

        // Full width: every label kept, no `?` chip (show_more = false).
        // R2: peer affordances join with plain whitespace, not `·`.
        assert_eq!(
            modal_footer_text(&hints, 80),
            "type filter  ↑↓ navigate  Enter activate  * favorite  Esc close"
        );
        // Narrow widths drop lower-priority items. Assert invariants rather
        // than brittle full strings (the ladder depends on the budget).
        // Always keeps Esc; Primary keeps Enter; no `?` (default path).
        let mid = modal_footer_text(&hints, 30);
        assert!(
            mid.contains("Esc") || mid.starts_with('E') || mid.ends_with('…'),
            "narrow keeps Esc: {mid}"
        );
        assert!(!mid.contains('?'), "default path never appends ?: {mid}");
        assert_eq!(modal_footer_text(&hints, 3), "Esc");
        // 2 cols is too short for "Esc" — truncate with ellipsis.
        let tiny = modal_footer_text(&hints, 2);
        assert!(
            tiny.ends_with('…') || tiny == "E…",
            "tiny width truncates: {tiny}"
        );
    }

    #[test]
    fn even_width_floors_to_nearest_even_column() {
        assert_eq!(even_width(Rect::new(0, 0, 0, 5)), Rect::new(0, 0, 0, 5));
        assert_eq!(even_width(Rect::new(0, 0, 1, 5)), Rect::new(0, 0, 0, 5));
        assert_eq!(even_width(Rect::new(0, 0, 2, 5)), Rect::new(0, 0, 2, 5));
        assert_eq!(even_width(Rect::new(7, 3, 15, 9)), Rect::new(7, 3, 14, 9));
        assert_eq!(even_width(Rect::new(7, 3, 16, 9)), Rect::new(7, 3, 16, 9));
    }

    #[test]
    fn centered_rect_produces_even_width() {
        // `centered_rect` must shed the odd trailing column so the body it
        // encloses gets an even usable width. Check several host widths and
        // percentages — including odd host widths and odd splits. Centering
        // itself is the `Layout` engine's integer-division behaviour (which
        // already tolerates a column of asymmetry on percentage splits); the
        // guarantee we add is purely that the resulting width is even.
        for &host_w in &[79u16, 80, 81, 120, 121] {
            for &percent in &[50u16, 58, 64, 66, 72, 80] {
                let host = Rect::new(0, 0, host_w, 40);
                let area = centered_rect(percent, 50, host);
                assert_eq!(
                    area.width % 2,
                    0,
                    "width {w} for host {host_w}@{percent}% must be even",
                    w = area.width
                );
            }
        }
    }

    #[test]
    fn centered_rect_h_produces_even_width() {
        for &host_w in &[79u16, 80, 81, 120, 121] {
            for &percent in &[60u16, 64, 66] {
                let host = Rect::new(0, 0, host_w, 40);
                let area = centered_rect_h(percent, 12, host);
                assert_eq!(
                    area.width % 2,
                    0,
                    "width {w} for host {host_w}@{percent}% must be even",
                    w = area.width
                );
            }
        }
    }

    #[test]
    fn modal_area_body_is_even_width_for_cjk() {
        // The end-to-end invariant: a modal panel plus its symmetric inner
        // padding yields an even body width, so a run of full-width (2-col)
        // CJK glyphs tiles every row without stranding a trailing column.
        // Use a real grid + frame the way the renderers do, across odd and
        // even terminal widths.
        for &cols in &[79u16, 80, 81, 119, 120, 121, 200] {
            let mut grid = neenee_tui_engine::Grid::new(cols, 50);
            let frame = Frame::new(&mut grid);
            let area = modal_area(&frame, FixedModalSpec::HELP);
            assert_eq!(
                area.width % 2,
                0,
                "modal panel width must be even at {cols} cols"
            );
            // `modal_frame` insets by MODAL_INNER_H_PADDING on each side.
            let body_w = area.width.saturating_sub(2 * MODAL_INNER_H_PADDING);
            assert_eq!(
                body_w % 2,
                0,
                "modal body width must be even at {cols} cols (was {body_w})"
            );
        }
    }

    #[test]
    fn breadcrumb_parts_composes_parent_separator_child() {
        // The single breadcrumb convention for hierarchical modal headers.
        // A drill-in sub-page renders `Parent › Child`: muted parent, the
        // centralized `›` separator, bold child. Pins both the order/kind of
        // the parts and the exact separator glyph so all sub-pages stay uniform.
        let parts = breadcrumb_parts("Sessions", "Info");
        assert_eq!(parts.len(), 3);
        assert!(matches!(
            parts[0],
            HeaderPart::Text {
                text: "Sessions",
                accent: false
            }
        ));
        assert!(matches!(
            parts[1],
            HeaderPart::Text {
                text: " › ",
                accent: false
            }
        ));
        assert!(matches!(parts[2], HeaderPart::Title("Info")));
    }

    #[test]
    fn hierarchical_breadcrumb_handles_full_and_truncated_widths() {
        let levels = ["Connections", "Add", "Google Antigravity"];
        // Full width fitting
        let full = hierarchical_breadcrumb(&levels, 60);
        assert_eq!(full.len(), 5); // Connections, sep, Add, sep, Google Antigravity
        assert!(matches!(
            full[0],
            HeaderPart::Text {
                text: "Connections",
                ..
            }
        ));
        assert!(matches!(full[4], HeaderPart::Title("Google Antigravity")));

        // Tight width dropping "Connections"
        let truncated = hierarchical_breadcrumb(&levels, 32);
        assert_eq!(truncated.len(), 5); // ..., sep, Add, sep, Google Antigravity
        assert!(matches!(truncated[0], HeaderPart::Text { text: "...", .. }));
        assert!(matches!(
            truncated[4],
            HeaderPart::Title("Google Antigravity")
        ));

        // Very tight width dropping "Add" as well
        let tight = hierarchical_breadcrumb(&levels, 24);
        assert_eq!(tight.len(), 3); // ..., sep, Google Antigravity
        assert!(matches!(tight[0], HeaderPart::Text { text: "...", .. }));
        assert!(matches!(tight[2], HeaderPart::Title("Google Antigravity")));
    }
}
