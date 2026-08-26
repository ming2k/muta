//! Contextual first-row header for every transcript page — plus the Runner
//! page's permanent key-legend footer.
//!
//! Every view — Main (session), `/btw`, Runner, and future focused pages —
//! shares one layout rule for the head row: identity and page-specific
//! context on the left, mode / index metadata on the right. Navigation
//! shortcuts do **not** live on the head row; the aside view carries them on
//! its second header row (ADR-0103 §3) and the Runner page on its permanent
//! three-row footer ([`draw_runner_footer`]) instead. Row 2 is demand-driven
//! (ADR-0104): it exists only while the view has something to say that no
//! other surface already says. Keeping this outside disclosure rendering
//! also leaves one clear extension point for future focused pages.

use mutx_engine::{Frame, Line, Modifier, Paragraph, Rect, Span, Style};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::{RunnerBarInfo, STEP_MIN_WIDTH, TRANSCRIPT_H_INSET, Theme};

pub(crate) enum PageHeader<'a> {
    /// The Main session view: `SESSION` identity, the session's persistent-id
    /// tail, and the workspace on the left; the session mode (e.g.
    /// `autopilot`) on the right.
    Session(&'a SessionHead<'a>),
    /// The `/btw` aside view (ADR-0103): identity + parent status on row 1;
    /// its shortcuts live on row 2 via [`draw_page_header_hints`].
    Btw(BtwHead),
    Runner(&'a RunnerBarInfo),
}

/// Row-1 content for the `/btw` aside view's head.
#[derive(Clone, Copy)]
pub(crate) struct BtwHead {
    /// Coarse primary-session status, rendered as the left context's meta
    /// segment ("main running", …).
    pub parent: muta_contracts::ParentStatus,
}

/// Row-2 (view affordance) context for every page kind. One struct because
/// the legend's *shape* is shared: a leading descriptive segment (the main
/// view's live aside count, the aside view's parent state) followed by
/// keycap pairs for the view's own shortcuts.
///
/// The band is **demand-driven** (ADR-0104): row 2 renders only when
/// [`PageHints::has_content`] is `true` — i.e. when this view genuinely has
/// page-specific affordances to announce. Nothing renders a row for pairs
/// that are either global (`F1 help` — every modal footer and the Help modal
/// own that discovery) or already carried by a *more specific* surface: the
/// main view's interrupt lives on the activity bar (which spells the real
/// double-Esc arming, `Esc Esc interrupt`), and the Runner page's legend
/// lives on its permanent footer ([`draw_runner_footer`]).
pub(crate) struct PageHints<'a> {
    /// Which page the legend belongs to — decides the keycap set.
    pub kind: PageKind,
    /// Live aside count + how many have a round in flight (main view only,
    /// ADR-0103 §3). `None` renders no aside segment.
    pub asides: Option<AsidesChip>,
    /// `true` when the viewed page has an in-flight round the user can
    /// interrupt (drives whether the interrupt pair is offered).
    pub interruptible: bool,
    /// Marker text for the aside view's legend (its parent's coarse state),
    /// already formatted; empty renders none.
    pub parent_note: &'a str,
}

impl PageHints<'_> {
    /// Whether row 2 has anything view-specific to say (ADR-0104). `false`
    /// means the caller must not reserve the row at all — the head collapses
    /// to a single row and the transcript reclaims the line.
    ///
    /// - **Main**: only while at least one aside is live (the aside chip +
    ///   `F5 asides` are exactly the affordances this row exists for).
    /// - **Btw**: always — `Ctrl-C back` is the view's single exit, and
    ///   no other surface repeats it.
    /// - **Runner**: never — its permanent footer already carries the same
    ///   legend (`draw_runner_footer`), so a row-2 copy would duplicate the
    ///   exact keycaps one screen apart.
    pub(crate) fn has_content(&self) -> bool {
        match self.kind {
            PageKind::Main => self.asides.is_some(),
            PageKind::Btw => true,
            PageKind::Runner => false,
        }
    }
}

/// The main view's live-asides chip: count + running count.
pub(crate) struct AsidesChip {
    pub total: usize,
    pub running: usize,
}

/// Which page the header band is describing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PageKind {
    Main,
    Btw,
    Runner,
}

impl From<&PageHeader<'_>> for PageKind {
    fn from(header: &PageHeader<'_>) -> Self {
        match header {
            PageHeader::Session(_) => PageKind::Main,
            PageHeader::Btw(_) => PageKind::Btw,
            PageHeader::Runner(_) => PageKind::Runner,
        }
    }
}

/// Left/right content for the Main session view's head row.
pub(crate) struct SessionHead<'a> {
    /// The session's persistent id (full string). Only its last four
    /// characters are shown, dimmed, as a disambiguating tag.
    pub session_id: &'a str,
    /// Tilde-shortened workspace path (e.g. `~/projects/xx`). Already
    /// abbreviated by the caller; rendered as-is.
    pub workspace: &'a str,
    /// `true` while the session runs in YOLO mode (`--yolo` /
    /// `/yolo on`). Shown as a warning-toned `YOLO` tag on the
    /// right — the session's persistent mode flag.
    pub yolo: bool,
}

struct HeaderContent {
    title: &'static str,
    /// The identity tail that sits right after the title (session-id tail,
    /// dimmed). Empty when the variant has none.
    tag: String,
    /// Optional `[ROLE]`-style tag rendered in the brand tone right after the
    /// identity tag (the Runner page's role). Empty when absent.
    badge: String,
    primary: String,
    meta: String,
    action: String,
}

/// Draw a single contextual header row. The primary action is always retained
/// on narrow terminals; descriptive text truncates first, while Runner sibling
/// shortcuts appear when there is enough room for them to remain legible.
///
/// The band's background spans the rect's full width — the head is top-level
/// chrome pinned to the terminal's top edge, so its `body` surface reaches
/// both edges like the Runner key-legend band at the bottom edge. The *text*
/// keeps the shared [`TRANSCRIPT_H_INSET`] horizontal inset (rendered as pad
/// spans) so it stays aligned with the transcript band below.
pub(crate) fn draw_page_header(
    frame: &mut Frame,
    rect: Rect,
    header: &PageHeader<'_>,
    theme: &Theme,
) {
    let full_width = rect.width as usize;
    if full_width < STEP_MIN_WIDTH {
        return;
    }

    let content = match header {
        PageHeader::Session(head) => HeaderContent {
            title: " SESSION ",
            tag: id_tail(head.session_id),
            badge: String::new(),
            primary: head.workspace.to_string(),
            meta: String::new(),
            action: if head.yolo {
                "YOLO ".to_string()
            } else {
                String::new()
            },
        },
        // Runner and /btw are contextual pages that replace the session head.
        PageHeader::Btw(head) => HeaderContent {
            title: " /btw ",
            tag: String::new(),
            badge: String::new(),
            primary: "Side conversation".to_string(),
            meta: parent_status_label(head.parent).to_string(),
            // Row 1 is identity + status only — the exit affordance moved to
            // the row-2 legend (ADR-0103 §3), so "Esc back" is gone here.
            action: String::new(),
        },
        // The Runner head shares the Session head's shape: uppercase identity
        // + `[ROLE]` tag + task title on the left, and pure index metadata on
        // the right — the sibling count `(i/n)`, shown only when there is
        // more than one sibling. Navigation shortcuts moved to the Runner
        // page's permanent footer (see `draw_runner_footer`).
        PageHeader::Runner(bar) => HeaderContent {
            title: " ENVOY ",
            tag: String::new(),
            badge: bar
                .role
                .as_ref()
                .map(|role| format!("[{}]", role.to_uppercase()))
                .unwrap_or_default(),
            primary: bar.label.clone(),
            meta: String::new(),
            action: if bar.total > 1 {
                format!("({}/{}) ", bar.index, bar.total)
            } else {
                String::new()
            },
        },
    };

    let bg = theme.body();
    let fill = Style::default().bg(bg);
    let title_style = fill.fg(theme.fg()).add_modifier(Modifier::BOLD);
    let tag_style = fill.fg(theme.dim());
    let badge_style = fill.fg(theme.brand()).add_modifier(Modifier::BOLD);
    let primary_style = fill.fg(theme.brand());
    let meta_style = match header {
        PageHeader::Btw(head)
            if matches!(
                head.parent,
                muta_contracts::ParentStatus::NeedsApproval
                    | muta_contracts::ParentStatus::NeedsInput
                    | muta_contracts::ParentStatus::Failed
                    | muta_contracts::ParentStatus::Interrupted
            ) =>
        {
            fill.fg(theme.warn()).add_modifier(Modifier::BOLD)
        }
        _ => fill.fg(theme.muted()),
    };
    // The session mode flag (`autopilot`) reads as a persistent safety state,
    // so it takes the warning tone; every other variant's right side is quiet
    // metadata (the `/btw` return hint, the Runner sibling count).
    let action_style = if matches!(header, PageHeader::Session(_)) {
        fill.fg(theme.warn()).add_modifier(Modifier::BOLD)
    } else {
        fill.fg(theme.muted())
    };

    // The text column is the full row minus the shared horizontal inset on
    // each side; the inset itself is painted as pad spans so the band's
    // background still owns every cell of the row.
    let pad = TRANSCRIPT_H_INSET as usize;
    let text_width = full_width.saturating_sub(2 * pad);

    let title_width = content.title.width();
    // The tag renders as `<tag> ` (tag + one trailing space) right after the
    // title — the title already ends with a space, so the tag needs no
    // leading separator. The badge (`[ROLE]`) follows the same rule.
    let tag_width = if content.tag.is_empty() {
        0
    } else {
        content.tag.width() + 1
    };
    let badge_width = if content.badge.is_empty() {
        0
    } else {
        content.badge.width() + 1
    };
    let action_width = content.action.width();
    let left_budget =
        text_width.saturating_sub(title_width + tag_width + badge_width + action_width + 1);
    let left = fit_context(&content.primary, &content.meta, left_budget);
    let left_width: usize = left.iter().map(|(text, _)| text.width()).sum();
    let gap = text_width
        .saturating_sub(title_width + tag_width + badge_width + left_width + action_width);

    let mut spans = vec![Span::styled(" ".repeat(pad), fill)];
    spans.push(Span::styled(content.title, title_style));
    if !content.tag.is_empty() {
        spans.push(Span::styled(format!("{} ", content.tag), tag_style));
    }
    if !content.badge.is_empty() {
        spans.push(Span::styled(format!("{} ", content.badge), badge_style));
    }
    for (text, tone) in left {
        let style = match tone {
            Tone::Primary => primary_style,
            Tone::Meta => meta_style,
        };
        spans.push(Span::styled(text, style));
    }
    spans.push(Span::styled(" ".repeat(gap), fill));
    if !content.action.is_empty() {
        spans.push(Span::styled(content.action, action_style));
    }
    // Trailing pad (plus any shortfall after the right-aligned action) so the
    // band's background owns the row out to the terminal's right edge.
    let used = pad + title_width + tag_width + badge_width + left_width + gap + action_width;
    spans.push(Span::styled(
        " ".repeat(full_width.saturating_sub(used)),
        fill,
    ));

    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
}

/// Draw the header band's second row: the view-level affordance legend
/// (ADR-0103 §3, demand-gated by ADR-0104). Row 1 carries identity +
/// status; this row carries *what the keys do in this view*.
///
/// Content by page kind, all horizontally centered on the band background:
/// - **Main**: the live aside chip (`btw: 2 total (1 active)`) and `F5 asides` —
///   rendered only while asides exist (the caller gates the row itself on
///   [`PageHints::has_content`]). No interrupt pair: the activity bar's
///   hint is the authoritative one and spells the real double-Esc arming
///   (`Esc Esc interrupt`), so a single-Esc copy here would both duplicate
///   and contradict it.
/// - **Btw**: `Ctrl-C back`, `F5 asides`, `Esc interrupt aside`, with the
///   parent note as the leading descriptive segment when set.
/// - **Runner**: never rendered — the Runner page's permanent footer already
///   carries the same legend, so the caller collapses the band to row 1.
///
/// No global affordances (`F1 help`) live here: every modal footer already
/// carries `? help`, so repeating it on a persistent top band is noise.
///
/// Pairs drop from the least page-specific end as the row narrows; the page's
/// single exit pair (Ctrl-C back) never drops.
pub(crate) fn draw_page_header_hints(
    frame: &mut Frame,
    rect: Rect,
    hints: &PageHints<'_>,
    theme: &Theme,
) {
    if rect.height == 0 || (rect.width as usize) < STEP_MIN_WIDTH {
        return;
    }

    let bg = theme.body();
    let fill = Style::default().bg(bg);
    let key_style = crate::components::keycap::keycap_style(theme).bg(bg);
    let hint_style = fill.fg(theme.muted());
    let note_style = fill.fg(theme.dim());

    // Leading descriptive segment (before the keycaps): the main view's live
    // aside chip, the aside view's parent note.
    let note: Option<String> = match hints.kind {
        PageKind::Main => hints.asides.as_ref().map(|chip| {
            if chip.running > 0 {
                format!("btw: {} total ({} active)", chip.total, chip.running)
            } else {
                format!("btw: {} total", chip.total)
            }
        }),
        PageKind::Btw => {
            let note = hints.parent_note.trim();
            (!note.is_empty()).then(|| note.to_string())
        }
        PageKind::Runner => None,
    };

    let pairs: Vec<(&'static str, &'static str)> = match hints.kind {
        // No interrupt pair on the main view (ADR-0104): the activity bar's
        // `Esc Esc interrupt` hint is the authoritative copy — it names the
        // real double-Esc arming — so a single-Esc legend here would both
        // duplicate it and misstate the gesture.
        PageKind::Main => {
            let mut pairs = Vec::new();
            if hints.asides.is_some() {
                pairs.push(("F5", "asides"));
            }
            pairs
        }
        PageKind::Btw => {
            let mut pairs = vec![("Ctrl-C", "back"), ("F5", "asides")];
            if hints.interruptible {
                pairs.push(("Esc", "interrupt aside"));
            }
            pairs
        }
        // The Runner legend lives on the page's permanent footer
        // (`draw_runner_footer`); row 2 never renders for this page kind.
        PageKind::Runner => Vec::new(),
    };

    let width = rect.width as usize;
    // Drop from the least page-specific pair first; the first pair (the
    // page's exit) never drops.
    let chosen: Vec<(&'static str, &'static str)> = {
        let mut chosen = pairs.clone();
        loop {
            let note_width = note.as_ref().map(|n| n.width() + 4).unwrap_or(0);
            let pairs_width: usize = chosen
                .iter()
                .map(|(key, label)| key.width() + 1 + label.width())
                .sum();
            let needed =
                note_width + pairs_width + ENVOY_FOOTER_PAIR_GAP * chosen.len().saturating_sub(1);
            if needed <= width.saturating_sub(2 * ENVOY_FOOTER_MARGIN_MIN) || chosen.len() <= 1 {
                break;
            }
            chosen.pop();
        }
        chosen
    };

    let note_width = note.as_ref().map(|n| n.width()).unwrap_or(0);
    let pairs_width: usize = chosen
        .iter()
        .map(|(key, label)| key.width() + 1 + label.width())
        .sum();
    let gaps =
        ENVOY_FOOTER_PAIR_GAP * chosen.len().saturating_sub(1) + if note.is_some() { 4 } else { 0 };
    let content_width = note_width + pairs_width + gaps;
    let margin = ((width.saturating_sub(content_width)) / 2).max(ENVOY_FOOTER_MARGIN_MIN);

    let mut spans = vec![Span::styled(" ".repeat(margin), fill)];
    if let Some(note) = note {
        spans.push(Span::styled(format!("{note}    "), note_style));
    }
    for (i, (key, label)) in chosen.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" ".repeat(ENVOY_FOOTER_PAIR_GAP), fill));
        }
        spans.push(Span::styled(*key, key_style));
        spans.push(Span::styled(format!(" {label}"), hint_style));
    }
    spans.push(Span::styled(
        " ".repeat(width.saturating_sub(margin + content_width)),
        fill,
    ));

    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
}

/// Draw the Runner page's permanent three-row footer. The band fills the
/// page-body background (`theme.body()`, the same tone the head row uses)
/// across its full width; the middle row is the actual content area carrying
/// the page's key shortcuts, and the top/bottom rows are blank padding, so
/// the legend reads as one continuous surface pinned to the terminal's
/// bottom edge.
///
/// The legend carries only the Runner-specific navigation — `Esc back`, and
/// `[ prev` / `] next` when the focused runner has siblings. No global
/// affordances (`F1 help`) live here either (ADR-0104): help is a global
/// capability whose discovery every modal footer (`? help`) and the Help
/// modal already own, not a property of *this* view — the same rule that
/// keeps the head band's row 2 free of it. Content is horizontally centered
/// with a minimum left margin.
pub(crate) fn draw_runner_footer(
    frame: &mut Frame,
    rect: Rect,
    info: &RunnerBarInfo,
    theme: &Theme,
) {
    if rect.height == 0 || (rect.width as usize) < STEP_MIN_WIDTH {
        return;
    }

    let bg = theme.body();
    let fill = Style::default().bg(bg);
    let key_style = crate::components::keycap::keycap_style(theme).bg(bg);
    let hint_style = fill.fg(theme.muted());

    // Build the legend as keycap + label pairs joined by a wide gap, all of
    // them Runner-specific: the page's own navigation (back, siblings). The
    // global `F1 help` pair is deliberately absent (ADR-0104) — help is not
    // a view-level affordance; the modal footers' `? help` chip and the Help
    // modal own its discovery. On narrow rows the sibling pair drops first
    // (it is already absent when `total < 2`); the back action never drops —
    // it is the page's single exit.
    let has_siblings = info.total > 1;
    let mut pairs: Vec<(&'static str, &'static str)> = vec![("Esc", "back")];
    if has_siblings {
        pairs.push(("[", "prev"));
        pairs.push(("]", "next"));
    }

    let width = rect.width as usize;
    let content: Vec<(&'static str, &'static str)> = {
        let mut chosen = pairs.clone();
        loop {
            let pairs_width: usize = chosen
                .iter()
                .map(|(key, label)| key.width() + 1 + label.width())
                .sum();
            let needed = pairs_width + ENVOY_FOOTER_PAIR_GAP * chosen.len().saturating_sub(1);
            if needed <= width.saturating_sub(2 * ENVOY_FOOTER_MARGIN_MIN) || chosen.len() == 1 {
                break;
            }
            // Drop the last pair (the least navigational one) and retry.
            chosen.pop();
        }
        chosen
    };

    let mut spans: Vec<Span<'static>> = Vec::new();
    for (idx, (key, label)) in content.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::styled(" ".repeat(ENVOY_FOOTER_PAIR_GAP), fill));
        }
        spans.push(Span::styled(key.to_string(), key_style));
        spans.push(Span::styled(format!(" {label}"), hint_style));
    }

    let used: usize = spans.iter().map(|span| span.content.width()).sum();
    let margin = width.saturating_sub(used) / 2;
    let mut row = vec![Span::styled(" ".repeat(margin), fill)];
    row.extend(spans);
    row.push(Span::styled(
        " ".repeat(width.saturating_sub(margin + used)),
        fill,
    ));

    // The middle row carries the legend; every other row of the band is blank
    // padding so the footer reads as one solid surface.
    let mid = rect.y + rect.height / 2;
    let blank = Line::from(Span::styled(" ".repeat(width), fill));
    for y in rect.y..rect.y + rect.height {
        let line = if y == mid {
            Line::from(row.clone())
        } else {
            blank.clone()
        };
        frame.render_widget(Paragraph::new(line), Rect::new(rect.x, y, rect.width, 1));
    }
}

/// Gap between adjacent keycap+label pairs in the Runner footer legend.
const ENVOY_FOOTER_PAIR_GAP: usize = 4;
/// Minimum left/right margin the legend keeps even on narrow rows.
const ENVOY_FOOTER_MARGIN_MIN: usize = 2;

/// The last four characters of a persistent session id — the short,
/// glanceable tag that disambiguates sessions without exposing the full id.
/// Returns the whole id when it is already four characters or fewer, and an
/// empty string for an empty id (so no tag is rendered at all).
fn id_tail(session_id: &str) -> String {
    if session_id.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = session_id.chars().collect();
    let take = chars.len().min(4);
    chars[chars.len() - take..].iter().collect()
}

#[derive(Clone, Copy)]
enum Tone {
    Primary,
    Meta,
}

/// Fit `primary  meta` into the left-hand budget while prioritizing the
/// compact state/index metadata. On very narrow rows the primary description
/// disappears before that orienting metadata does.
fn fit_context(primary: &str, meta: &str, budget: usize) -> Vec<(String, Tone)> {
    if budget == 0 {
        return Vec::new();
    }

    // A lone primary (no meta) fills the budget directly — the common case
    // for the Session head, whose workspace path has no trailing metadata.
    if meta.is_empty() {
        return vec![(truncate_to_width(primary, budget), Tone::Primary)];
    }

    let primary_width = primary.width();
    let meta_width = meta.width();
    const SEPARATOR: &str = "  ";
    let separator_width = SEPARATOR.width();

    if primary_width + separator_width + meta_width <= budget {
        return vec![
            (primary.to_string(), Tone::Primary),
            (SEPARATOR.to_string(), Tone::Meta),
            (meta.to_string(), Tone::Meta),
        ];
    }

    if budget > meta_width + separator_width {
        let primary_budget = budget - meta_width - separator_width;
        return vec![
            (truncate_to_width(primary, primary_budget), Tone::Primary),
            (SEPARATOR.to_string(), Tone::Meta),
            (meta.to_string(), Tone::Meta),
        ];
    }

    vec![(truncate_to_width(meta, budget), Tone::Meta)]
}

fn parent_status_label(status: muta_contracts::ParentStatus) -> &'static str {
    match status {
        muta_contracts::ParentStatus::Idle => "[main: idle]",
        muta_contracts::ParentStatus::Running => "[main: running]",
        muta_contracts::ParentStatus::NeedsApproval => "[⚠ main: approval needed]",
        muta_contracts::ParentStatus::NeedsInput => "[⚠ main: input needed]",
        muta_contracts::ParentStatus::Failed => "[⚠ main: failed]",
        muta_contracts::ParentStatus::Interrupted => "[⚠ main: interrupted]",
    }
}

fn truncate_to_width(text: &str, max_width: usize) -> String {
    if text.width() <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    if max_width == 1 {
        return "…".to_string();
    }

    let content_width = max_width - 1;
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + width > content_width {
            break;
        }
        out.push(ch);
        used += width;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered_row(width: u16, header: PageHeader<'_>) -> String {
        let theme = Theme::default();
        let mut terminal = mutx_engine::TestTerminal::new(width, 1);
        terminal.draw(|frame| {
            draw_page_header(frame, frame.area(), &header, &theme);
        });
        terminal
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn btw_header_identifies_page_parent_state_and_return_action() {
        let row = rendered_row(
            64,
            PageHeader::Btw(BtwHead {
                parent: muta_contracts::ParentStatus::NeedsApproval,
            }),
        );
        assert!(row.starts_with("   /btw Side conversation  [⚠ main: approval needed]"));
        // ADR-0103: the exit affordance moved to the row-2 legend; row 1 is
        // pure identity + status now.
        assert!(!row.trim_end().contains("Esc back"));
    }

    #[test]
    fn btw_hints_legend_leads_with_exit_and_asides() {
        let theme = Theme::default();
        let hints = PageHints {
            kind: PageKind::Btw,
            asides: None,
            interruptible: true,
            parent_note: "main running",
        };
        let mut terminal = mutx_engine::TestTerminal::new(80, 1);
        terminal.draw(|frame| {
            draw_page_header_hints(frame, frame.area(), &hints, &theme);
        });
        let row: String = terminal
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            row.contains("Ctrl-C"),
            "legend must lead with the exit pair: {row}"
        );
        assert!(
            row.contains("asides"),
            "legend must offer the asides modal: {row}"
        );
        assert!(
            row.contains("interrupt aside"),
            "legend must offer the aside interrupt: {row}"
        );
        assert!(
            !row.contains("F1"),
            "global help is not a view-level affordance: {row}"
        );
    }

    #[test]
    fn main_hints_legend_omits_interrupt_even_while_running() {
        // ADR-0104: the activity bar's `Esc Esc interrupt` hint is the
        // authoritative interrupt copy (it names the double-Esc arming), so
        // the main view's row-2 legend must never carry a single-Esc pair —
        // not even while a round is running.
        let theme = Theme::default();
        let hints = PageHints {
            kind: PageKind::Main,
            asides: None,
            interruptible: true,
            parent_note: "",
        };
        assert!(!hints.has_content(), "no asides → no row at all");
        let mut terminal = mutx_engine::TestTerminal::new(80, 1);
        terminal.draw(|frame| {
            draw_page_header_hints(frame, frame.area(), &hints, &theme);
        });
        let row: String = terminal
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(!row.contains("Esc"), "no interrupt pair: {row}");
        assert!(!row.contains("F1"), "no global help pair: {row}");
    }

    #[test]
    fn hints_presence_is_demand_driven_per_page_kind() {
        let mk = |kind: PageKind, asides: bool| PageHints {
            kind,
            asides: asides.then_some(AsidesChip {
                total: 1,
                running: 0,
            }),
            interruptible: false,
            parent_note: "",
        };
        // Main: only while asides are live.
        assert!(!mk(PageKind::Main, false).has_content());
        assert!(mk(PageKind::Main, true).has_content());
        // Btw: always (its exit pair exists on no other surface).
        assert!(mk(PageKind::Btw, false).has_content());
        // Runner: never (the permanent footer owns the legend).
        assert!(!mk(PageKind::Runner, false).has_content());
        assert!(!mk(PageKind::Runner, true).has_content());
    }

    #[test]
    fn main_hints_legend_shows_aside_chip_when_live() {
        let theme = Theme::default();
        let hints = PageHints {
            kind: PageKind::Main,
            asides: Some(AsidesChip {
                total: 2,
                running: 1,
            }),
            interruptible: false,
            parent_note: "",
        };
        let mut terminal = mutx_engine::TestTerminal::new(80, 1);
        terminal.draw(|frame| {
            draw_page_header_hints(frame, frame.area(), &hints, &theme);
        });
        let row: String = terminal
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(row.contains("btw: 2 total (1 active)"), "aside chip: {row}");
        assert!(row.contains("asides"), "F5 pair: {row}");
        assert!(!row.contains("Esc"), "no interrupt pair: {row}");
    }

    #[test]
    fn main_hints_legend_is_quiet_without_asides() {
        let theme = Theme::default();
        let hints = PageHints {
            kind: PageKind::Main,
            asides: None,
            interruptible: false,
            parent_note: "",
        };
        let mut terminal = mutx_engine::TestTerminal::new(80, 1);
        terminal.draw(|frame| {
            draw_page_header_hints(frame, frame.area(), &hints, &theme);
        });
        let row: String = terminal
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(!row.contains("btw"), "no chip without asides: {row}");
        assert!(!row.contains("asides"), "no F5 pair without asides: {row}");
    }

    #[test]
    fn runner_header_shows_identity_role_title_and_sibling_index() {
        let info = RunnerBarInfo {
            role: Some("explore".to_string()),
            label: "inspect the renderer".to_string(),
            index: 1,
            total: 2,
        };
        let row = rendered_row(80, PageHeader::Runner(&info));
        assert_eq!(
            row,
            "   ENVOY [EXPLORE] inspect the renderer                                 (1/2)   "
        );
    }

    #[test]
    fn runner_header_omits_role_tag_until_known_and_index_when_single() {
        let info = RunnerBarInfo {
            role: None,
            label: "a task without a role yet".to_string(),
            index: 1,
            total: 1,
        };
        let row = rendered_row(48, PageHeader::Runner(&info));
        assert!(row.starts_with("   ENVOY a task without a role yet"));
        assert!(!row.contains('['));
        assert!(!row.contains("(1/1)"));
        assert_eq!(row.width(), 48);
    }

    #[test]
    fn narrow_runner_header_preserves_identity_and_sibling_index() {
        let info = RunnerBarInfo {
            role: Some("plan".to_string()),
            label: "a very long runner task description that cannot fit".to_string(),
            index: 12,
            total: 24,
        };
        let row = rendered_row(36, PageHeader::Runner(&info));
        assert!(row.starts_with("   ENVOY [PLAN] "));
        assert!(row.contains("(12/24)"));
        assert_eq!(row.width(), 36);
    }

    #[test]
    fn runner_footer_centers_legend_on_a_solid_three_row_band() {
        let theme = Theme::default();
        let info = RunnerBarInfo {
            role: Some("explore".to_string()),
            label: "inspect the renderer".to_string(),
            index: 1,
            total: 2,
        };
        let mut terminal = mutx_engine::TestTerminal::new(40, 3);
        terminal.draw(|frame| {
            draw_runner_footer(frame, frame.area(), &info, &theme);
        });
        let buffer = terminal.buffer();
        let width = buffer.area().width as usize;
        let row_text = |row: usize| -> String {
            buffer.content[row * width..(row + 1) * width]
                .iter()
                .map(|cell| cell.symbol())
                .collect()
        };
        // The legend lives on the middle row only; the padding rows are blank.
        assert_eq!(row_text(0), " ".repeat(40));
        let mid = row_text(1);
        // Width 40 fits back + siblings; the global help pair no longer
        // exists on this surface at all (ADR-0104).
        assert!(mid.trim() == "Esc back    [ prev    ] next", "{mid:?}");
        assert!(!mid.contains("F1"), "no global help pair: {mid:?}");
        let lead = mid.len() - mid.trim_start().len();
        let trail = mid.len() - mid.trim_end().len();
        assert!(
            lead >= 2 && (lead as isize - trail as isize).abs() <= 1,
            "centered with a minimum margin: {mid:?}"
        );
        assert_eq!(row_text(2), " ".repeat(40));
        // The whole band — padding rows included — paints the page-body
        // background so the footer reads as one solid surface.
        for cell in &buffer.content {
            assert_eq!(cell.bg, theme.body());
        }
    }

    #[test]
    fn runner_footer_drops_affordances_as_the_row_narrows() {
        let theme = Theme::default();
        let footer_text = |width: u16, total: usize| -> String {
            let info = RunnerBarInfo {
                role: None,
                label: String::new(),
                index: 1,
                total,
            };
            let mut terminal = mutx_engine::TestTerminal::new(width, 3);
            terminal.draw(|frame| {
                draw_runner_footer(frame, frame.area(), &info, &theme);
            });
            let width = terminal.buffer().area().width as usize;
            terminal.buffer().content[width..2 * width]
                .iter()
                .map(|cell| cell.symbol())
                .collect()
        };
        // No siblings: the prev/next pair never renders, and the legend is
        // the exit pair alone — help is global, not an Runner affordance
        // (ADR-0104).
        let single = footer_text(40, 1);
        assert!(
            single.contains("Esc back") && !single.contains("prev"),
            "{single:?}"
        );
        assert!(
            !single.contains("next") && !single.contains("F1"),
            "{single:?}"
        );
        // Narrow: the sibling pair drops; back never drops.
        let narrow = footer_text(28, 2);
        assert!(
            narrow.contains("Esc back") && narrow.contains("[ prev"),
            "{narrow:?}"
        );
        assert!(!narrow.contains("F1"), "{narrow:?}");
        let tiny = footer_text(16, 2);
        assert!(tiny.contains("Esc back"), "{tiny:?}");
        assert!(!tiny.contains("prev"), "{tiny:?}");
    }

    #[test]
    fn session_header_shows_id_tail_workspace_and_yolo() {
        let head = SessionHead {
            session_id: "sess-01a2b3c4",
            workspace: "~/projects/xx",
            yolo: true,
        };
        let row = rendered_row(80, PageHeader::Session(&head));
        assert!(row.starts_with("   SESSION b3c4 ~/projects/xx"));
        assert!(row.trim_end().ends_with("YOLO"));
    }

    #[test]
    fn session_header_hides_mode_and_short_id_tail() {
        let head = SessionHead {
            session_id: "ab",
            workspace: "~/work",
            yolo: false,
        };
        let row = rendered_row(40, PageHeader::Session(&head));
        assert!(row.starts_with("   SESSION ab ~/work"));
        assert!(!row.contains("YOLO"));
    }

    /// The head band is top-level chrome: its `body` background owns every
    /// cell of the terminal row — no `app_bg` gap at either edge — while the
    /// text keeps the shared 2-col inset.
    #[test]
    fn header_band_paints_the_full_row_width() {
        let theme = Theme::default();
        let head = SessionHead {
            session_id: "sess-01a2b3c4",
            workspace: "~/projects/xx",
            yolo: true,
        };
        let mut terminal = mutx_engine::TestTerminal::new(40, 1);
        terminal.draw(|frame| {
            draw_page_header(frame, frame.area(), &PageHeader::Session(&head), &theme);
        });
        for cell in &terminal.buffer().content {
            assert_eq!(cell.bg, theme.body());
        }
        let row: String = terminal
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(row.starts_with("  "), "left pad: {row:?}");
        assert!(row.ends_with("  "), "right pad: {row:?}");
    }
}
