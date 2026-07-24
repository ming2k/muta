//! Width-aware modal footer hints.
//!
//! Each hint carries a numeric priority. When the footer is too narrow to show
//! everything, lower-priority items are dropped first. If anything is hidden or
//! labels are stripped, a trailing `? more` chip is appended (mandatory — never
//! omitted when collapsed) so the user can open the in-modal keymap page.

use neenee_tui_engine::{Frame, Line, Modifier, Paragraph, Rect, Span, Style};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::super::Theme;

/// Coarse priority band for a footer hint. Higher-ranked variants survive
/// longer under width pressure.
///
/// This enum covers the common cases. For a finer-grained priority (e.g. a
/// destructive action that must outlive a plain secondary), use
/// [`FooterHint::new`] with an explicit numeric band.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum FooterPriority {
    /// Dismiss / cancel (`Esc`). Highest survival.
    Always,
    /// Primary confirm / activate action (`Enter`, destructive actions).
    Primary,
    /// Cursor / list navigation (`↑↓`).
    Navigation,
    /// Secondary or uncommon actions. First to collapse.
    Secondary,
}

impl FooterPriority {
    /// Numeric rank for sorting: higher = kept longer under width pressure.
    fn rank(self) -> u8 {
        match self {
            FooterPriority::Always => 100,
            FooterPriority::Primary => 80,
            FooterPriority::Navigation => 60,
            FooterPriority::Secondary => 40,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct FooterHint {
    pub key: &'static str,
    pub label: &'static str,
    pub priority: FooterPriority,
}

impl FooterHint {
    pub(crate) const fn always(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            priority: FooterPriority::Always,
        }
    }

    pub(crate) const fn primary(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            priority: FooterPriority::Primary,
        }
    }

    pub(crate) const fn navigation(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            priority: FooterPriority::Navigation,
        }
    }

    pub(crate) const fn secondary(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            priority: FooterPriority::Secondary,
        }
    }

    /// Constructor for a hint whose priority falls between the coarse bands.
    /// The numeric `band` is compared against [`FooterPriority::rank`] bands:
    /// pass `70` to sit between Primary (80) and Navigation (60) — the right
    /// spot for a destructive action (`D delete`) that should outlive plain
    /// secondaries but not the always-keep `Esc`.
    pub(crate) const fn with_band(
        key: &'static str,
        label: &'static str,
        band: u8,
    ) -> FooterHintWithBand {
        FooterHintWithBand {
            key,
            label,
            rank: band,
        }
    }
}

/// A footer hint with an explicit numeric priority, produced by
/// [`FooterHint::with_band`]. Renders identically to [`FooterHint`]; the only
/// difference is the custom rank used for collapse ordering. Convertible into
/// the ranked slice the layout consumes.
#[derive(Clone, Copy)]
pub(crate) struct FooterHintWithBand {
    pub key: &'static str,
    pub label: &'static str,
    pub rank: u8,
}

/// Internal: every hint flattened to a (key, label, rank) row, so the layout
/// can sort/drop uniformly regardless of enum-vs-custom origin.
struct RankedHint {
    key: &'static str,
    label: &'static str,
    rank: u8,
    /// Original index in the caller's order, for stable display ordering.
    order: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FooterLabelMode {
    Full,
    Compact,
}

/// Trailing chip shown when the footer has collapsed anything. `? more` is the
/// full form; `?` is the compact fallback when space is extremely tight.
const MORE_FULL: &str = "? more";
const MORE_COMPACT: &str = "?";

/// Render the one-line modal command strip with width-aware degradation.
///
/// **Does not** append the `? more` chip — this is the default path for modals
/// that do not wire an in-modal keymap page (e.g. question, model editor).
pub(crate) fn render_modal_footer(
    frame: &mut Frame,
    rect: Rect,
    hints: &[FooterHint],
    theme: &Theme,
) {
    render_footer_impl(frame, rect, hints, &[], theme, false);
}

/// Like [`render_modal_footer`], but accepts an extra slice of custom-band
/// hints (e.g. `D delete` at band 70) and enables the mandatory `? more` chip
/// when the strip has collapsed. List modals that support in-modal `?` expand
/// use this.
pub(crate) fn render_modal_footer_with_more(
    frame: &mut Frame,
    rect: Rect,
    hints: &[FooterHint],
    extra: &[FooterHintWithBand],
    theme: &Theme,
) {
    render_footer_impl(frame, rect, hints, extra, theme, true);
}

/// Build the footer text for `width` (no `? more` chip). Used by tests and
/// modals that only need the string.
pub(crate) fn modal_footer_text(hints: &[FooterHint], width: usize) -> String {
    layout_footer(hints, &[], width, false).text
}

/// Build the footer text for `width`, enabling the mandatory `? more` chip and
/// accepting custom-band hints. (Used by tests; also available to callers.)
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn modal_footer_text_with_more(
    hints: &[FooterHint],
    extra: &[FooterHintWithBand],
    width: usize,
) -> String {
    layout_footer(hints, extra, width, true).text
}

/// Body lines for the in-modal keymap page: one row per hint, key brand+bold,
/// description muted. Used when the user presses `?` on a collapsible modal.
pub(crate) fn keymap_body_lines<'a>(
    hints: &'a [FooterHint],
    extra: &'a [FooterHintWithBand],
    theme: &Theme,
) -> Vec<Line<'static>> {
    // Stable display order: standard hints first (caller order), then extras.
    let mut rows: Vec<(&'static str, &'static str)> =
        hints.iter().map(|h| (h.key, h.label)).collect();
    rows.extend(extra.iter().map(|h| (h.key, h.label)));
    let key_width = rows
        .iter()
        .map(|(k, _)| k.width())
        .max()
        .unwrap_or(0)
        .max(2);
    let mut lines = Vec::with_capacity(rows.len() + 2);
    lines.push(Line::from(Span::styled(
        " Keybindings",
        Style::default().fg(theme.muted()),
    )));
    lines.push(Line::from(""));
    for (key, label) in rows {
        let pad = key_width.saturating_sub(key.width());
        let key_cell = format!("  {}{}", key, " ".repeat(pad));
        lines.push(Line::from(vec![
            Span::styled(
                key_cell,
                Style::default()
                    .fg(theme.brand())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  {}", label), Style::default().fg(theme.fg())),
        ]));
    }
    lines
}

/// Footer hints shown while the in-modal keymap page is open.
pub(crate) fn keymap_page_footer_hints() -> [FooterHint; 2] {
    [
        FooterHint::navigation("↑↓", "scroll"),
        FooterHint::always("Esc", "back"),
    ]
}

/// Result of laying out a footer for a given width.
struct FooterLayout {
    text: String,
    /// True when at least one hint was dropped or labels were stripped.
    /// (Kept for readability of the layout control flow; not all paths read it.)
    #[allow(dead_code)]
    collapsed: bool,
}

fn render_footer_impl(
    frame: &mut Frame,
    rect: Rect,
    hints: &[FooterHint],
    extra: &[FooterHintWithBand],
    theme: &Theme,
    show_more: bool,
) {
    let layout = layout_footer(hints, extra, rect.width as usize, show_more);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            layout.text,
            Style::default().fg(theme.muted()),
        ))),
        rect,
    );
}

/// Lay out the footer for `width`, dropping lowest-priority hints first.
///
/// Algorithm:
/// 1. Try full labels for the entire set (complete → no `? more`).
/// 2. Drop the lowest-priority hint(s) one at a time (still full labels).
/// 3. Compact remaining (keys only), same drop ladder.
/// 4. Last resort: Always keys + mandatory `?` chip.
///
/// **Invariant when `show_more` is true and the strip is incomplete**
/// (any hint dropped, or labels stripped to keys-only): the rendered line
/// **always ends with** `? more` or `?`. The chip is not optional — prefer
/// dropping another key over omitting it.
fn layout_footer(
    hints: &[FooterHint],
    extra: &[FooterHintWithBand],
    width: usize,
    show_more: bool,
) -> FooterLayout {
    if width == 0 || hints.is_empty() && extra.is_empty() {
        return FooterLayout {
            text: String::new(),
            collapsed: false,
        };
    }

    // Flatten to ranked rows in stable display order (hints, then extras).
    let ranked: Vec<RankedHint> = hints
        .iter()
        .enumerate()
        .map(|(i, h)| RankedHint {
            key: h.key,
            label: h.label,
            rank: h.priority.rank(),
            order: i,
        })
        .chain(extra.iter().enumerate().map(|(i, h)| RankedHint {
            key: h.key,
            label: h.label,
            rank: h.rank,
            order: hints.len() + i,
        }))
        .collect();

    // Drop order: lowest rank first; tiebreak by later display order so a
    // trailing secondary goes before an earlier one of equal rank.
    let mut drop_order: Vec<usize> = (0..ranked.len()).collect();
    drop_order.sort_by_key(|&i| (ranked[i].rank, usize::MAX - ranked[i].order));

    // Pass 1: full labels, progressively dropping lowest-priority items.
    for drop_count in 0..=ranked.len().saturating_sub(1) {
        let any_dropped = drop_count > 0;
        if let Some(text) = try_subset(
            &ranked,
            &drop_order,
            drop_count,
            FooterLabelMode::Full,
            width,
            show_more,
            any_dropped,
        ) {
            return FooterLayout {
                text,
                collapsed: any_dropped,
            };
        }
    }

    // Pass 2: compact (keys only), same drop ladder. Compact is collapsed.
    for drop_count in 0..=ranked.len().saturating_sub(1) {
        if let Some(text) = try_subset(
            &ranked,
            &drop_order,
            drop_count,
            FooterLabelMode::Compact,
            width,
            show_more,
            true,
        ) {
            return FooterLayout {
                text,
                collapsed: true,
            };
        }
    }

    // Last resort: Always keys only, still requiring the chip when show_more.
    let always: Vec<&RankedHint> = ranked.iter().filter(|r| r.rank >= 100).collect();
    let base_set = if always.is_empty() {
        ranked.iter().collect()
    } else {
        always
    };
    let base = join_hints(&base_set, FooterLabelMode::Compact);

    if show_more {
        for candidate in [
            append_more(&base, MORE_FULL),
            append_more(&base, MORE_COMPACT),
            MORE_FULL.to_string(),
            MORE_COMPACT.to_string(),
        ] {
            if !candidate.is_empty() && candidate.width() <= width {
                return FooterLayout {
                    text: candidate,
                    collapsed: true,
                };
            }
        }
        return FooterLayout {
            text: truncate_to_width(MORE_COMPACT, width),
            collapsed: true,
        };
    }

    let text = if base.width() <= width {
        base
    } else {
        truncate_to_width(&base, width)
    };
    FooterLayout {
        text,
        collapsed: true,
    }
}

/// Try a subset (dropping the first `drop_count` lowest-priority rows) for the
/// given label mode. Returns the joined string if it fits, else None.
fn try_subset(
    ranked: &[RankedHint],
    drop_order: &[usize],
    drop_count: usize,
    mode: FooterLabelMode,
    width: usize,
    show_more: bool,
    collapsed: bool,
) -> Option<String> {
    let dropped: std::collections::HashSet<usize> =
        drop_order.iter().take(drop_count).copied().collect();
    let subset: Vec<&RankedHint> = ranked
        .iter()
        .enumerate()
        .filter(|(i, _)| !dropped.contains(i))
        .map(|(_, r)| r)
        .collect();
    if subset.is_empty() {
        return None;
    }
    let base = join_hints(&subset, mode);
    if base.is_empty() {
        return None;
    }
    if !show_more || !collapsed {
        return (base.width() <= width).then_some(base);
    }
    // Incomplete strip: chip is mandatory and must be the last token.
    for chip in [MORE_FULL, MORE_COMPACT] {
        let candidate = append_more(&base, chip);
        if candidate.width() <= width {
            return Some(candidate);
        }
    }
    None
}

/// Join hints in stable display order (`order`) for a given label mode.
fn join_hints(hints: &[&RankedHint], mode: FooterLabelMode) -> String {
    let mut ordered: Vec<&&RankedHint> = hints.iter().collect();
    ordered.sort_by_key(|r| r.order);
    ordered
        .iter()
        .map(|hint| match mode {
            FooterLabelMode::Full if !hint.label.is_empty() => {
                format!("{} {}", hint.key, hint.label)
            }
            _ => hint.key.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

fn append_more(base: &str, chip: &str) -> String {
    if base.is_empty() {
        chip.to_string()
    } else {
        format!("{base} · {chip}")
    }
}

fn truncate_to_width(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    if max == 1 {
        return "…".to_string();
    }
    let mut out = String::new();
    let mut width = 0usize;
    for c in s.chars() {
        let cw = UnicodeWidthChar::width(c).unwrap_or(0).max(1);
        if width + cw > max - 1 {
            break;
        }
        out.push(c);
        width += cw;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_hints() -> [FooterHint; 5] {
        [
            FooterHint::secondary("type", "filter"),
            FooterHint::navigation("↑↓", "navigate"),
            FooterHint::primary("Enter", "activate"),
            FooterHint::secondary("*", "favorite"),
            FooterHint::always("Esc", "close"),
        ]
    }

    #[test]
    fn full_width_keeps_every_label() {
        let hints = sample_hints();
        let text = modal_footer_text_with_more(&hints, &[], 80);
        assert_eq!(
            text,
            "type filter · ↑↓ navigate · Enter activate · * favorite · Esc close"
        );
        assert!(!text.contains('?'));
    }

    #[test]
    fn incomplete_strip_always_ends_with_more_chip() {
        // When show_more is on and anything is incomplete, the last token is
        // always `? more` or `?` — never a half-visible key legend without an
        // escape hatch. Sweep many widths so the invariant is width-stable.
        let hints = sample_hints();
        let full = modal_footer_text_with_more(&hints, &[], 80);
        for width in 1..=full.width() {
            let text = modal_footer_text_with_more(&hints, &[], width);
            if text != full {
                // Collapsed: must surface `?` (chip) whenever there's room for
                // at least the compact chip, and must end with the chip / ellipsis.
                let t = text.trim_end();
                assert!(
                    t.ends_with("? more") || t.ends_with('?') || t.ends_with('…'),
                    "width {width}: incomplete footer must end with ? more / ? / …, got {t:?}"
                );
                if width >= MORE_COMPACT.width() {
                    assert!(
                        t.contains('?'),
                        "width {width}: must surface ? when there is room, got {t:?}"
                    );
                }
            } else {
                assert!(
                    !text.contains('?'),
                    "complete strip must not show ?: {:?}",
                    text
                );
            }
        }
    }

    #[test]
    fn custom_band_protects_destructive_action() {
        // `D delete` at band 70 must outlive a secondary `*` at band 40.
        let hints = [
            FooterHint::navigation("↑↓", "navigate"),
            FooterHint::primary("Enter", "select"),
            FooterHint::secondary("*", "favorite"),
            FooterHint::always("Esc", "close"),
        ];
        let extra = [FooterHint::with_band("D", "delete", 70)];
        let text = modal_footer_text_with_more(&hints, &extra, 44);
        assert!(text.contains('D'), "band-70 D must survive: {text:?}");
        assert!(!text.contains('*'), "band-40 * should drop first: {text:?}");
    }

    #[test]
    fn show_more_false_omits_chip() {
        let hints = sample_hints();
        let text = modal_footer_text(&hints, 30);
        assert!(
            !text.contains('?'),
            "no chip when show_more is false: {text:?}"
        );
    }

    #[test]
    fn default_path_never_appends_more() {
        // modal_footer_text is the default path for modals that do not wire
        // in-modal keymap expand. It must never append `?`.
        let hints = sample_hints();
        let mid = modal_footer_text(&hints, 40);
        assert!(!mid.contains('?'));
    }
}
