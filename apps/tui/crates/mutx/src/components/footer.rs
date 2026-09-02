//! Width-aware modal footer hints.
//!
//! Each hint carries a numeric priority. When the footer is too narrow to show
//! everything, lower-priority items are dropped first. If anything is hidden or
//! labels are stripped, a trailing `? help` chip is appended (mandatory — never
//! omitted when collapsed) so the user can open the in-modal keymap page.

use mutx_engine::{Frame, Line, Paragraph, Rect, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::super::Theme;
use super::keycap::keycap_style;

/// Coarse priority band for a footer hint. Higher-ranked variants survive
/// longer under width pressure.
///
/// This enum covers the common cases. For a finer-grained priority (e.g. a
/// destructive action that must outlive a plain secondary), use
/// [`FooterHint::with_band`] with an explicit numeric band.
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
    pub(crate) const fn key_always(key: crate::keymap::Key, label: &'static str) -> Self {
        Self::always(key.display(), label)
    }

    pub(crate) const fn key_primary(key: crate::keymap::Key, label: &'static str) -> Self {
        Self::primary(key.display(), label)
    }

    #[allow(dead_code)]
    pub(crate) const fn key_navigation(key: crate::keymap::Key, label: &'static str) -> Self {
        Self::navigation(key.display(), label)
    }

    pub(crate) const fn key_secondary(key: crate::keymap::Key, label: &'static str) -> Self {
        Self::secondary(key.display(), label)
    }

    #[allow(dead_code)]
    pub(crate) const fn key_with_band(
        key: crate::keymap::Key,
        label: &'static str,
        band: u8,
    ) -> FooterHintWithBand {
        Self::with_band(key.display(), label, band)
    }

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

/// Trailing chip shown when the footer has collapsed anything. The chip
/// advertises the in-modal keymap expand key (`?`), so it always carries its
/// full **label** `? help` (the `?` key opens the help/keybindings page). The
/// label is non-negotiable: it is the only way the user learns what the chip
/// offers (the hidden keymap), so it is never degraded to `? …` or a bare `?`.
/// When space is tight the caller drops another hint to make room for `? help`;
/// only an absurdly narrow terminal (fewer columns than `? help` is wide)
/// collapses to `…`.
const MORE_FULL: &str = "? help";

/// Render the one-line modal command strip with width-aware degradation.
///
/// **Does not** append the `? help` chip — this is the default path for modals
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
/// hints (e.g. `D delete` at band 70) and enables the mandatory `? help` chip
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

/// Build the footer text for `width` (no `? help` chip). Used by tests and
/// modals that only need the string.
pub(crate) fn modal_footer_text(hints: &[FooterHint], width: usize) -> String {
    layout_footer(hints, &[], width, false).text
}

/// Build the footer text for `width`, enabling the mandatory `? help` chip and
/// accepting custom-band hints. (Used by tests.)
#[cfg(test)]
pub(crate) fn modal_footer_text_with_more(
    hints: &[FooterHint],
    extra: &[FooterHintWithBand],
    width: usize,
) -> String {
    layout_footer(hints, extra, width, true).text
}

/// A single rendered segment of the footer line. Keys are tagged so the
/// renderer can apply the unified keycap style while everything else stays
/// muted — without the layout having to know about styling.
#[derive(Clone)]
enum FooterSeg {
    /// A keyboard-key label (rendered with the keycap style).
    Key(String),
    /// Any other text: a hint label, a ` · ` separator, or the `? help` chip.
    Text(String),
}

impl FooterSeg {
    fn text(&self) -> &str {
        match self {
            FooterSeg::Key(s) | FooterSeg::Text(s) => s,
        }
    }
}

/// Flatten a segment list to its plain-text form (used by the width-only /
/// test-facing string APIs, which must not change just because keys are now
/// styled differently).
fn segs_to_string(segs: &[FooterSeg]) -> String {
    segs.iter().map(|s| s.text()).collect()
}

/// Materialize the segment list as styled spans: keys take the unified keycap
/// style, hint labels take the keycap label style. This is the single place that decides how
/// footer keys look, so it can never drift from the activity bar / Help modal.
fn segs_to_spans(segs: &[FooterSeg], theme: &Theme) -> Vec<Span<'static>> {
    let key_style = keycap_style(theme);
    let label_style = theme.keycap_label_style();
    segs.iter()
        .map(|seg| match seg {
            FooterSeg::Key(s) => Span::styled(s.clone(), key_style),
            FooterSeg::Text(s) => Span::styled(s.clone(), label_style),
        })
        .collect()
}

/// Result of laying out a footer for a given width.
struct FooterLayout {
    text: String,
    segs: Vec<FooterSeg>,
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
        Paragraph::new(Line::from(segs_to_spans(&layout.segs, theme))),
        rect,
    );
}

/// Lay out the footer for `width`, dropping lowest-priority hints first.
///
/// Algorithm:
/// 1. Try full labels for the entire set (complete → no `? help`).
/// 2. Drop the lowest-priority hint(s) one at a time (still full labels).
/// 3. Compact remaining (keys only), same drop ladder.
/// 4. Last resort: the bare `? help` chip alone (no hints); `…` only when the
///    terminal is too narrow even for `? help`.
///
/// **Invariant when `show_more` is true and the strip is incomplete**
/// (any hint dropped, or labels stripped to keys-only): the rendered line
/// **always ends with `? help`** — the chip's label is non-negotiable. It is
/// the only way the user learns what the chip offers (the hidden keymap), so
/// another hint is always dropped to make room rather than truncating the label
/// to `? …` or `?`. Only a terminal narrower than `? help` itself collapses to
/// `…`.
fn layout_footer(
    hints: &[FooterHint],
    extra: &[FooterHintWithBand],
    width: usize,
    show_more: bool,
) -> FooterLayout {
    if width == 0 || hints.is_empty() && extra.is_empty() {
        return FooterLayout {
            text: String::new(),
            segs: Vec::new(),
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

    // Wrap a segment list into a complete FooterLayout (text mirrors the
    // segments so the width-only / test APIs stay byte-identical to before).
    let finish = |segs: Vec<FooterSeg>| FooterLayout {
        text: segs_to_string(&segs),
        segs,
    };

    // Pass 1: full labels, progressively dropping lowest-priority items.
    for drop_count in 0..=ranked.len().saturating_sub(1) {
        let any_dropped = drop_count > 0;
        if let Some(segs) = try_subset(
            &ranked,
            &drop_order,
            drop_count,
            FooterLabelMode::Full,
            width,
            show_more,
            any_dropped,
        ) {
            return finish(segs);
        }
    }

    // Pass 2: compact (keys only), same drop ladder. Compact is collapsed.
    for drop_count in 0..=ranked.len().saturating_sub(1) {
        if let Some(segs) = try_subset(
            &ranked,
            &drop_order,
            drop_count,
            FooterLabelMode::Compact,
            width,
            show_more,
            true,
        ) {
            return finish(segs);
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
        // Prefer the bare chip (no hints) at its full label `? help`. If even
        // that does not fit an absurdly narrow terminal, show `…` rather than
        // truncating the label — never a `?` without its meaning.
        for candidate in [append_more(&base, MORE_FULL), only_text(MORE_FULL)] {
            let text = segs_to_string(&candidate);
            if !text.is_empty() && text.width() <= width {
                return finish(candidate);
            }
        }
        return finish(only_text("…"));
    }

    let text = segs_to_string(&base);
    let segs = if text.width() <= width {
        base
    } else {
        only_text_truncated(&text, width)
    };
    finish(segs)
}

/// A segment list that is just one plain-text run (used for the bare `? help`
/// chip fallback, which carries no keys).
fn only_text(s: &str) -> Vec<FooterSeg> {
    vec![FooterSeg::Text(s.to_string())]
}

/// A segment list that is one plain-text run, truncated to `max` cells.
fn only_text_truncated(s: &str, max: usize) -> Vec<FooterSeg> {
    only_text(&truncate_to_width(s, max))
}

/// Try a subset (dropping the first `drop_count` lowest-priority rows) for the
/// given label mode. Returns the segment list if it fits, else None.
fn try_subset(
    ranked: &[RankedHint],
    drop_order: &[usize],
    drop_count: usize,
    mode: FooterLabelMode,
    width: usize,
    show_more: bool,
    collapsed: bool,
) -> Option<Vec<FooterSeg>> {
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
    let text = segs_to_string(&base);
    if !show_more || !collapsed {
        return (text.width() <= width).then_some(base);
    }
    // Incomplete strip: the `? help` chip is mandatory and must be the last
    // token. Its label is **non-negotiable** — it is the only way the user
    // learns what the chip offers (the hidden keymap), so we never degrade it
    // to `? …` / `?`. If `? help` does not fit, return None so the caller
    // drops another hint and retries; the drop ladder bottoms out in the
    // last-resort below (which shows only the chip, or `…` if even that won't
    // fit in an absurdly narrow terminal).
    let candidate = append_more(&base, MORE_FULL);
    let t = segs_to_string(&candidate);
    (t.width() <= width).then_some(candidate)
}

/// Join hints in stable display order (`order`) for a given label mode into a
/// segment list. Keys are tagged `FooterSeg::Key` so the renderer can apply the
/// keycap style; separators and labels are `Text`.
///
/// Each hint is a same-rank peer affordance (R2), so hints are separated by
/// plain whitespace — no `·` (which the join ladder reserves for the
/// keycap → label modification inside each hint).
fn join_hints(hints: &[&RankedHint], mode: FooterLabelMode) -> Vec<FooterSeg> {
    let mut ordered: Vec<&&RankedHint> = hints.iter().collect();
    ordered.sort_by_key(|r| r.order);
    let mut segs: Vec<FooterSeg> = Vec::new();
    for (idx, hint) in ordered.iter().enumerate() {
        if idx > 0 {
            segs.push(FooterSeg::Text(
                " ".repeat(super::super::design::JOIN_ENUMERATE_COLS),
            ));
        }
        segs.push(FooterSeg::Key(hint.key.to_string()));
        if let FooterLabelMode::Full = mode
            && !hint.label.is_empty()
        {
            segs.push(FooterSeg::Text(format!(" {}", hint.label)));
        }
    }
    segs
}

fn append_more(base: &[FooterSeg], chip: &str) -> Vec<FooterSeg> {
    let mut out = base.to_vec();
    if out.is_empty() {
        out.push(FooterSeg::Text(chip.to_string()));
    } else {
        // R2: `? help` is another peer affordance.
        out.push(FooterSeg::Text(format!(
            "{}{chip}",
            " ".repeat(super::super::design::JOIN_ENUMERATE_COLS)
        )));
    }
    out
}

fn truncate_to_width(s: &str, max: usize) -> String {
    if s.width() <= max && !s.contains(['\n', '\r']) {
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
        if c == '\n' || c == '\r' {
            break;
        }
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
        // R2: same-rank peer affordances are separated by plain whitespace
        // (JOIN_ENUMERATE_COLS), not the `·` reserved for key→label joins.
        assert_eq!(
            text,
            "type filter  ↑↓ navigate  Enter activate  * favorite  Esc close"
        );
        assert!(!text.contains('·'));
        assert!(!text.contains('?'));
    }

    #[test]
    fn incomplete_strip_always_ends_with_more_chip() {
        // When show_more is on and anything is incomplete, the last token is
        // always the `? help` chip at its FULL label — never degraded to
        // `? …` / `?` (the label is the only way the user learns what the chip
        // offers, so it is non-negotiable). Only an absurdly narrow terminal
        // (fewer columns than `? help` is wide) collapses to `…`. Sweep many
        // widths so the invariant is width-stable.
        let hints = sample_hints();
        let full = modal_footer_text_with_more(&hints, &[], 80);
        for width in 1..=full.width() {
            let text = modal_footer_text_with_more(&hints, &[], width);
            if text != full {
                let t = text.trim_end();
                if width >= MORE_FULL.width() {
                    // Room for `? help`: the full label must be present.
                    assert!(
                        t.ends_with("? help"),
                        "width {width}: chip must keep its full `? help` label, got {t:?}"
                    );
                } else {
                    // Too narrow even for the chip: a bare `…`, never a bare `?`.
                    assert_eq!(
                        t, "…",
                        "width {width}: too-narrow footer must be `…`, got {t:?}"
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
