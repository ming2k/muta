//! Contextual first-row header for every view — plus the Runner
//! page's permanent key-legend footer.
//!
//! Every view — Main (session), `/btw`, Runner, Settings, and future focused pages —
//! shares one layout rule for the head row: identity and view-specific
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

pub(crate) enum ViewHeader<'a> {
    /// The Main session view: `SESSION` identity, the session's persistent-id
    /// tail, and the workspace on the left; the session mode (e.g.
    /// `DELEGATED`) on the right.
    Session(&'a SessionHead<'a>),
    /// The `/btw` aside view (ADR-0103): identity + parent status on row 1;
    /// its shortcuts live on row 2 via [`draw_view_header_hints`].
    Btw(BtwHead),
    Runner(&'a RunnerBarInfo),
    /// Full-screen Settings View (ADR-0141): `SETTINGS` identity + workspace on
    /// the left; active category on the right.
    Settings(&'a SettingsHead<'a>),
}

/// Row-1 content for the Settings view's head.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SettingsHead<'a> {
    pub workspace: &'a str,
    pub category: &'a str,
    pub subtitle: &'a str,
}

/// Row-1 content for the `/btw` aside view's head.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BtwHead {
    /// Coarse primary-session status, rendered as the left context's meta
    /// segment ("main running", …).
    pub parent: muta_contracts::ParentStatus,
}

/// Row-2 (view affordance) context for every view kind. One struct because
/// the legend's *shape* is shared: a leading descriptive segment (the main
/// view's live aside count, the aside view's parent state) followed by
/// keycap pairs for the view's own shortcuts.
///
/// The band is **demand-driven** (ADR-0104): row 2 renders only when
/// [`ViewHints::has_content`] is `true` — i.e. when this view genuinely has
/// view-specific affordances to announce. Nothing renders a row for pairs
/// that are either global (`F1 help` — every modal footer and the Help modal
/// own that discovery) or already carried by a *more specific* surface: the
/// main view's interrupt lives on the activity bar (which spells the real
/// double-Esc arming, `Esc Esc interrupt`), and the Runner page's legend
/// lives on its permanent footer ([`draw_runner_footer`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ViewHints<'a> {
    /// Which view the legend belongs to — decides the keycap set.
    pub kind: ViewKind,
    /// Live aside count + how many have a round in flight (main view only,
    /// ADR-0103 §3). `None` renders no aside segment.
    pub asides: Option<AsidesChip>,
    /// `true` when the viewed view has an in-flight round the user can
    /// interrupt (drives whether the interrupt pair is offered).
    pub interruptible: bool,
    /// Marker text for the aside view's legend (its parent's coarse state),
    /// already formatted; empty renders none.
    pub parent_note: &'a str,
    /// Optional view stack breadcrumbs.
    pub breadcrumbs: Option<&'a str>,
}

impl ViewHints<'_> {
    /// Whether row 2 has anything view-specific to say (ADR-0104). `false`
    /// means the caller must not reserve the row at all — the head collapses
    /// to a single row and the transcript reclaims the line.
    ///
    /// - **Breadcrumbs active**: always expands row 2.
    /// - **Main**: only while at least one aside is live (the aside chip +
    ///   `F5 asides` are exactly the affordances this row exists for).
    /// - **Btw**: always — `Ctrl-C back` is the view's single exit, and
    ///   no other surface repeats it.
    /// - **Runner**: never — its permanent footer already carries the same
    ///   legend (`draw_runner_footer`), so a row-2 copy would duplicate the
    ///   exact keycaps one screen apart.
    pub(crate) fn has_content(&self) -> bool {
        if self.breadcrumbs.is_some() {
            return true;
        }
        match self.kind {
            ViewKind::Main => self.asides.is_some(),
            ViewKind::Btw | ViewKind::Settings => true,
            ViewKind::Runner => false,
        }
    }
}

/// The main view's live-asides chip: count + running count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AsidesChip {
    pub total: usize,
    pub running: usize,
}

/// Which view the header band is describing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ViewKind {
    Main,
    Btw,
    Runner,
    Settings,
}

impl From<&ViewHeader<'_>> for ViewKind {
    fn from(header: &ViewHeader<'_>) -> Self {
        match header {
            ViewHeader::Session(_) => ViewKind::Main,
            ViewHeader::Btw(_) => ViewKind::Btw,
            ViewHeader::Runner(_) => ViewKind::Runner,
            ViewHeader::Settings(_) => ViewKind::Settings,
        }
    }
}

/// Left/right content for the Main session view's head row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SessionHead<'a> {
    /// The session's persistent id (full string). Only its last four
    /// characters are shown, dimmed, as a disambiguating tag.
    pub session_id: &'a str,
    /// Tilde-shortened workspace path (e.g. `~/projects/xx`). Already
    /// abbreviated by the caller; rendered as-is.
    pub workspace: &'a str,
    /// `true` while the session runs in delegated autonomous execution
    /// mode (`--delegate` / `/delegate on`). Shown as a warning-toned
    /// `DELEGATED` tag on the right — the session's persistent mode flag.
    pub delegated: bool,
    /// `true` while the session runs in unconfined filesystem access mode
    /// (`/jail off`). Shown as a warning-toned `UNCONFINED` tag on the right.
    pub unconfined: bool,
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
pub(crate) fn draw_view_header(
    frame: &mut Frame,
    rect: Rect,
    header: &ViewHeader<'_>,
    theme: &Theme,
) {
    let full_width = rect.width as usize;
    if full_width < STEP_MIN_WIDTH {
        return;
    }

    let content = match header {
        ViewHeader::Session(head) => {
            let mut action = String::new();
            if head.delegated {
                action.push_str("DELEGATED ");
            }
            if head.unconfined {
                action.push_str("UNCONFINED ");
            }
            HeaderContent {
                title: " SESSION ",
                tag: id_tail(head.session_id),
                badge: String::new(),
                primary: head.workspace.to_string(),
                meta: String::new(),
                action,
            }
        }
        // Runner and /btw are contextual views that replace the session head.
        ViewHeader::Btw(head) => HeaderContent {
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
        ViewHeader::Runner(bar) => HeaderContent {
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
        ViewHeader::Settings(head) => HeaderContent {
            title: " SETTINGS ",
            tag: head.workspace.to_string(),
            badge: String::new(),
            primary: head.category.to_string(),
            meta: head.subtitle.to_string(),
            action: String::new(),
        },
    };

    let bg = theme.body();
    let fill = Style::default().bg(bg);
    let title_style = fill.fg(theme.fg()).add_modifier(Modifier::BOLD);
    let tag_style = fill.fg(theme.dim());
    let badge_style = fill.fg(theme.brand()).add_modifier(Modifier::BOLD);
    let primary_style = fill.fg(theme.brand());
    let meta_style = match header {
        ViewHeader::Btw(head)
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
    // The session mode flag (`DELEGATED`) reads as a persistent safety state,
    // so it takes the warning tone; every other variant's right side is quiet
    // metadata (the `/btw` return hint, the Runner sibling count).
    let action_style = if matches!(header, ViewHeader::Session(_)) {
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
pub(crate) fn draw_view_header_hints(
    frame: &mut Frame,
    rect: Rect,
    hints: &ViewHints<'_>,
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

    if let Some(crumbs) = hints.breadcrumbs {
        let left = Span::styled(format!("   {crumbs}"), Style::default().fg(theme.fg()));
        let right_key = crate::components::keycap::keycap_span(theme, "C-x b");
        let right_desc = Span::styled(" view  ", Style::default().fg(theme.muted()));
        let back_key = crate::components::keycap::keycap_span(theme, "Esc");
        let back_desc = Span::styled(" close", Style::default().fg(theme.muted()));

        let left_len = crumbs.width() + 3;
        let right_len = 22;
        let pad_len = (rect.width as usize).saturating_sub(left_len + right_len);
        let pad = " ".repeat(pad_len);

        let line = Line::from(vec![
            left,
            Span::raw(pad),
            right_key,
            right_desc,
            back_key,
            back_desc,
        ]);
        frame.render_widget(Paragraph::new(line).style(fill), rect);
        return;
    }

    // Leading descriptive segment (before the keycaps): the main view's live
    // aside chip, the aside view's parent note.
    let note: Option<String> = match hints.kind {
        ViewKind::Main => hints.asides.as_ref().map(|chip| {
            if chip.running > 0 {
                format!("btw: {} total ({} active)", chip.total, chip.running)
            } else {
                format!("btw: {} total", chip.total)
            }
        }),
        ViewKind::Btw => {
            let note = hints.parent_note.trim();
            (!note.is_empty()).then(|| note.to_string())
        }
        ViewKind::Runner | ViewKind::Settings => None,
    };

    let pairs: Vec<(&'static str, &'static str)> = match hints.kind {
        ViewKind::Main => {
            let mut pairs = Vec::new();
            if hints.asides.is_some() {
                pairs.push(("F5", "asides"));
            }
            pairs
        }
        ViewKind::Btw => {
            let mut pairs = vec![("Ctrl-C", "back"), ("F5", "asides")];
            if hints.interruptible {
                pairs.push(("Esc", "interrupt aside"));
            }
            pairs
        }
        ViewKind::Runner => Vec::new(),
        ViewKind::Settings => vec![("Esc", "close")],
    };

    let width = rect.width as usize;
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

/// Draw the Runner page's permanent three-row footer.
pub(crate) fn draw_runner_footer(
    frame: &mut Frame,
    rect: Rect,
    info: &RunnerBarInfo,
    theme: &Theme,
) {
    if rect.height == 0 {
        return;
    }

    let bg = theme.body();
    let fill = Style::default().bg(bg);
    let key_style = crate::components::keycap::keycap_style(theme).bg(bg);
    let hint_style = fill.fg(theme.muted());

    let mut pairs: Vec<(&'static str, &'static str)> = vec![("Esc", "back")];
    if info.total > 1 {
        pairs.push(("[", "prev"));
        pairs.push(("]", "next"));
    }

    let content_len: usize = pairs
        .iter()
        .map(|(key, label)| key.width() + 1 + label.width())
        .sum::<usize>()
        + ENVOY_FOOTER_PAIR_GAP * pairs.len().saturating_sub(1);
    let width = rect.width as usize;
    let margin = (width.saturating_sub(content_len)) / 2;

    let mut spans: Vec<Span<'static>> = vec![Span::styled(" ".repeat(margin), fill)];
    for (i, (key, label)) in pairs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" ".repeat(ENVOY_FOOTER_PAIR_GAP), fill));
        }
        spans.push(Span::styled(*key, key_style));
        spans.push(Span::styled(format!(" {label}"), hint_style));
    }
    spans.push(Span::styled(
        " ".repeat(width.saturating_sub(margin + content_len)),
        fill,
    ));

    let footer_lines = vec![
        Line::from(Span::styled(" ".repeat(width), fill)),
        Line::from(spans),
        Line::from(Span::styled(" ".repeat(width), fill)),
    ];

    frame.render_widget(Paragraph::new(footer_lines), rect);
}

const ENVOY_FOOTER_PAIR_GAP: usize = 3;
const ENVOY_FOOTER_MARGIN_MIN: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tone {
    Primary,
    Meta,
}

fn fit_context(primary: &str, meta: &str, budget: usize) -> Vec<(String, Tone)> {
    if budget == 0 {
        return Vec::new();
    }

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

fn truncate_to_width(text: &str, max_width: usize) -> String {
    if text.width() <= max_width && !text.contains(['\n', '\r']) {
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
        if ch == '\n' || ch == '\r' {
            break;
        }
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

fn id_tail(id: &str) -> String {
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = trimmed.chars().collect();
    let take = chars.len().min(4);
    chars[chars.len() - take..].iter().collect()
}

fn parent_status_label(parent: muta_contracts::ParentStatus) -> &'static str {
    match parent {
        muta_contracts::ParentStatus::Idle => "[main: idle]",
        muta_contracts::ParentStatus::Running => "[main: running]",
        muta_contracts::ParentStatus::NeedsApproval => "[⚠ main: approval needed]",
        muta_contracts::ParentStatus::NeedsInput => "[⚠ main: input needed]",
        muta_contracts::ParentStatus::Failed => "[⚠ main: failed]",
        muta_contracts::ParentStatus::Interrupted => "[⚠ main: interrupted]",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rendered_row(width: u16, header: ViewHeader<'_>) -> String {
        let theme = Theme::default();
        let mut terminal = mutx_engine::TestTerminal::new(width, 1);
        terminal.draw(|frame| {
            draw_view_header(frame, frame.area(), &header, &theme);
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
            ViewHeader::Btw(BtwHead {
                parent: muta_contracts::ParentStatus::NeedsApproval,
            }),
        );
        assert!(row.starts_with("   /btw Side conversation  [⚠ main: approval needed]"));
        assert!(!row.trim_end().contains("Esc back"));
    }

    #[test]
    fn btw_hints_legend_leads_with_exit_and_asides() {
        let theme = Theme::default();
        let hints = ViewHints {
            kind: ViewKind::Btw,
            asides: None,
            interruptible: true,
            parent_note: "main running",
            breadcrumbs: None,
        };
        let mut terminal = mutx_engine::TestTerminal::new(80, 1);
        terminal.draw(|frame| {
            draw_view_header_hints(frame, frame.area(), &hints, &theme);
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
        let theme = Theme::default();
        let hints = ViewHints {
            kind: ViewKind::Main,
            asides: None,
            interruptible: true,
            parent_note: "",
            breadcrumbs: None,
        };
        assert!(!hints.has_content(), "no asides → no row at all");
        let mut terminal = mutx_engine::TestTerminal::new(80, 1);
        terminal.draw(|frame| {
            draw_view_header_hints(frame, frame.area(), &hints, &theme);
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
        let mk = |kind: ViewKind, asides: bool| ViewHints {
            kind,
            asides: asides.then_some(AsidesChip {
                total: 1,
                running: 0,
            }),
            interruptible: false,
            parent_note: "",
            breadcrumbs: None,
        };
        assert!(!mk(ViewKind::Main, false).has_content());
        assert!(mk(ViewKind::Main, true).has_content());
        assert!(mk(ViewKind::Btw, false).has_content());
        assert!(!mk(ViewKind::Runner, false).has_content());
        assert!(!mk(ViewKind::Runner, true).has_content());

        let with_crumbs = ViewHints {
            kind: ViewKind::Main,
            asides: None,
            interruptible: false,
            parent_note: "",
            breadcrumbs: Some("Main › Runner"),
        };
        assert!(with_crumbs.has_content());
    }

    #[test]
    fn main_hints_legend_shows_aside_chip_when_live() {
        let theme = Theme::default();
        let hints = ViewHints {
            kind: ViewKind::Main,
            asides: Some(AsidesChip {
                total: 2,
                running: 1,
            }),
            interruptible: false,
            parent_note: "",
            breadcrumbs: None,
        };
        let mut terminal = mutx_engine::TestTerminal::new(80, 1);
        terminal.draw(|frame| {
            draw_view_header_hints(frame, frame.area(), &hints, &theme);
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
    fn breadcrumbs_render_in_row_two() {
        let theme = Theme::default();
        let hints = ViewHints {
            kind: ViewKind::Main,
            asides: None,
            interruptible: false,
            parent_note: "",
            breadcrumbs: Some("Main › Runner[explore]"),
        };
        let mut terminal = mutx_engine::TestTerminal::new(80, 1);
        terminal.draw(|frame| {
            draw_view_header_hints(frame, frame.area(), &hints, &theme);
        });
        let row: String = terminal
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(row.contains("Main › Runner[explore]"));
        assert!(row.contains("C-x b"));
        assert!(row.contains("Esc"));
    }

    #[test]
    fn runner_header_shows_identity_role_title_and_sibling_index() {
        let info = RunnerBarInfo {
            role: Some("explore".to_string()),
            label: "inspect the renderer".to_string(),
            index: 1,
            total: 2,
        };
        let row = rendered_row(80, ViewHeader::Runner(&info));
        assert_eq!(
            row,
            "   ENVOY [EXPLORE] inspect the renderer                                 (1/2)   "
        );
    }

    #[test]
    fn session_header_shows_id_tail_workspace_and_delegated() {
        let head = SessionHead {
            session_id: "sess-01a2b3c4",
            workspace: "~/projects/xx",
            delegated: true,
            unconfined: false,
        };
        let row = rendered_row(80, ViewHeader::Session(&head));
        assert!(row.starts_with("   SESSION b3c4 ~/projects/xx"));
        assert!(row.trim_end().ends_with("DELEGATED"));
    }

    #[test]
    fn header_band_paints_the_full_row_width() {
        let theme = Theme::default();
        let head = SessionHead {
            session_id: "sess-01a2b3c4",
            workspace: "~/projects/xx",
            delegated: true,
            unconfined: false,
        };
        let mut terminal = mutx_engine::TestTerminal::new(40, 1);
        terminal.draw(|frame| {
            draw_view_header(frame, frame.area(), &ViewHeader::Session(&head), &theme);
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
