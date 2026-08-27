//! Composer-native meta rows: the two hint lines painted inside the
//! composer's top/bottom padding bands.
//!
//! Row 1 ("the `as:` row") states what the buffer *is* right now and what it
//! will become on commit: a plain prompt, a resolved slash command, a steer /
//! follow-up mid-round, or an in-place edit of a queued message.
//!
//! Row 3 ("the keys row") states what the next `Enter` does, plus any escape
//! hatch (`Esc cancel`, `/retry`). Its verbs intentionally mirror the `as:`
//! values so the two rows read as one sentence about the same target.
//!
//! Both builders take the composer's panel background so the keycaps and the
//! tinted verbs blend into the box instead of carrying the outer surface color
//! the old standalone bar below the input used.

use unicode_width::UnicodeWidthStr;

use mutx_engine::{Color, Modifier, Span, Style};

use crate::app::ComposerSendMode;

use super::keycap::keycap_style;
use super::super::Theme;
use super::super::keymap::Key;

// ---------------------------------------------------------------------------
// Width ladder
// ---------------------------------------------------------------------------

/// Width-degradation ladder for the keys row. `Full` renders every label in
/// long form; `Compact` trims optional suffixes; `Tiny` keeps only the
/// mandatory keys/verbs. Mirrors the ladder the standalone bar used before the
/// rows moved inside the composer.
#[derive(Clone, Copy)]
pub(crate) enum ActionDensity {
    Full,
    Compact,
    Tiny,
}

impl ActionDensity {
    /// Pick the density that fits `row_width` columns. Thresholds keep the
    /// long-form busy sentence plus a short char counter comfortable inside an
    /// 80-col composer and degrade before the counter would ever be dropped.
    pub(crate) fn for_width(row_width: usize) -> Self {
        if row_width >= 44 {
            ActionDensity::Full
        } else if row_width >= 28 {
            ActionDensity::Compact
        } else {
            ActionDensity::Tiny
        }
    }

    fn compact(self) -> bool {
        matches!(self, ActionDensity::Compact | ActionDensity::Tiny)
    }
}

// ---------------------------------------------------------------------------
// Compose target (what the buffer is / will become)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QueueEditKind {
    Steer,
    FollowUp,
}

impl QueueEditKind {
    /// The plural noun naming the group of queued items being pointed at.
    pub(crate) fn plural_noun(self) -> &'static str {
        match self {
            QueueEditKind::Steer => "steers",
            QueueEditKind::FollowUp => "follow-ups",
        }
    }

    /// Single-word future tense used after `as:` while editing an item that
    /// would be re-delivered into this group.
    fn future_word(self) -> &'static str {
        match self {
            QueueEditKind::Steer => "steer",
            QueueEditKind::FollowUp => "follow-up",
        }
    }

    fn consequence_color(self, theme: &Theme) -> Color {
        match self {
            // Amber: re-delivery interrupts the running round.
            QueueEditKind::Steer => theme.warn(),
            // Info blue: re-delivery appends to the queue.
            QueueEditKind::FollowUp => theme.info(),
        }
    }
}

/// What the live buffer currently holds and what it becomes on `Enter`.
///
/// Derived entirely from state the frame already tracks (buffer prefix,
/// `ComposerSendMode × busy`, queue pointer) — no new app-level state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ComposeTarget {
    /// A plain prompt opening a new round (idle).
    #[default]
    Prompt,
    /// The buffer starts with a resolved slash command.
    Command,
    /// Mid-round steering input (delivered at the next safe boundary).
    Steer,
    /// Mid-round follow-up appended to the delivery queue.
    FollowUp,
    /// An in-place edit of a queued message, armed via the queue pointer.
    QueueEdit {
        kind: QueueEditKind,
        number: usize,
        dirty: bool,
    },
}

impl ComposeTarget {
    /// Consequence-class hue for the submission verb: amber when Enter's
    /// effect interrupts (steers) the live round, info blue when it queues,
    /// plain foreground (`Color::Reset`) when neither class applies.
    fn submit_color(self, theme: &Theme) -> Color {
        match self {
            ComposeTarget::Steer => theme.warn(),
            ComposeTarget::FollowUp => theme.info(),
            ComposeTarget::QueueEdit { kind, .. } => kind.consequence_color(theme),
            ComposeTarget::Prompt | ComposeTarget::Command => Color::Reset,
        }
    }
}

/// Derive the compose target from already-available state.
///
/// `queue_editing` carries `(kind, number)` for an armed queue pointer (the
/// structured form of the old `[edit: steer #1]` badge). `is_slash` marks a
/// buffer whose leading token resolves as a command; it never wins over the
/// queue pointer because a pointer edit renders the stored item verbatim.
pub(crate) fn compose_target(
    busy: bool,
    send_mode: Option<ComposerSendMode>,
    queue_editing: Option<(QueueEditKind, usize)>,
    is_slash: bool,
) -> ComposeTarget {
    if let Some((kind, number)) = queue_editing {
        return ComposeTarget::QueueEdit {
            kind,
            number,
            dirty: false,
        };
    }
    if busy {
        return match send_mode.unwrap_or_default() {
            ComposerSendMode::Steer => ComposeTarget::Steer,
            ComposerSendMode::FollowUp => ComposeTarget::FollowUp,
        };
    }
    if is_slash {
        ComposeTarget::Command
    } else {
        ComposeTarget::Prompt
    }
}

/// Owned meta-row inputs for both composer rows. Built by the event loop
/// before the mutable composer borrow begins (`draw_composer` mutates
/// `input_scroll`, so no `&App` borrows may be threaded through), then copied
/// into `ComposerDrawOptions`.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ComposerHints {
    /// What the buffer is and what commit will do with it.
    pub compose_target: ComposeTarget,
    /// Stopped round parked for `/retry` — mirrored from `SessionChrome`,
    /// same affordance the gauge bar used to repeat.
    pub can_retry: bool,
}

// ---------------------------------------------------------------------------
// Row 1 — the `as:` row (what the buffer is / will become)
// ---------------------------------------------------------------------------

/// Build the `as:` row: one clause normally, two while a queue pointer is
/// armed (`compose:` states what is being held, `as:` what commit will do).
///
/// ```text
/// as: prompt
/// as: command                       ← resolved `/command` buffer
/// as: steer prompt                  ← mid-round, interrupting
/// as: follow-up prompt              ← mid-round, queueing
/// compose: follow-ups[#2] · edited · as: follow-up
/// ```
///
/// Value hues encode the *consequence class* of pressing Enter: default fg
/// for an ordinary fresh round, amber for interrupting steers, info blue for
/// queue-appending follow-ups. Labels stay muted so the verb reads first.
pub(crate) fn compose_target_spans(
    target: ComposeTarget,
    theme: &Theme,
    bg: Color,
) -> Vec<Span<'static>> {
    let base = Style::default().bg(bg);
    let muted = base.fg(theme.muted());

    let push_clause = |spans: &mut Vec<Span<'static>>, label: &'static str, value: String, value_style: Style| {
        spans.push(Span::styled(format!("{label}: "), muted));
        spans.push(Span::styled(value, value_style));
    };

    let mut spans = Vec::new();
    match target {
        ComposeTarget::Prompt | ComposeTarget::Command => {
            if target == ComposeTarget::Command {
                // Echo the in-box resolved-command treatment: brand + bold.
                push_clause(
                    &mut spans,
                    "as",
                    "command".to_string(),
                    base.fg(theme.brand()).add_modifier(Modifier::BOLD),
                );
            } else {
                push_clause(&mut spans, "as", "prompt".to_string(), base);
            }
        }
        ComposeTarget::Steer => {
            push_clause(
                &mut spans,
                "as",
                "steer prompt".to_string(),
                base.fg(theme.warn()),
            );
        }
        ComposeTarget::FollowUp => {
            push_clause(
                &mut spans,
                "as",
                "follow-up prompt".to_string(),
                base.fg(theme.info()),
            );
        }
        ComposeTarget::QueueEdit {
            kind,
            number,
            dirty,
        } => {
            // First clause: what is being held, tinted by its delivery group.
            spans.push(Span::styled("compose: ", muted));
            let mut group = format!("{}[#{number}]", kind.plural_noun());
            if dirty {
                group.push_str(" · edited");
            }
            spans.push(Span::styled(group, base.fg(kind.consequence_color(theme))));
            // Second clause: what saving will re-deliver it as.
            spans.push(Span::styled(" · ", muted));
            push_clause(
                &mut spans,
                "as",
                kind.future_word().to_string(),
                base.fg(kind.consequence_color(theme)),
            );
        }
    }
    spans
}

// ---------------------------------------------------------------------------
// Char counter (right side of the keys row)
// ---------------------------------------------------------------------------

/// Human-sized char count for the keys row: exact below 1k, one-decimal `k`
/// above (`990 chars`, `14.2k chars`). Chars, never tokens — committed-token
/// accounting lives exclusively on the model bar to avoid double reading.
pub(crate) fn format_char_count(chars: usize) -> String {
    if chars < 1_000 {
        format!("{chars} chars")
    } else {
        format!("{:.1}k chars", chars as f64 / 1_000.0)
    }
}

/// Width of a span run in display columns.
/// Abbreviated display width of a span run (unused today; kept beside the
/// span builders so width math never gets re-implemented ad hoc).
#[allow(dead_code)]
pub(crate) fn spans_width(spans: &[Span<'static>]) -> usize {
    spans.iter().map(|span| span.content.width()).sum()
}

// ---------------------------------------------------------------------------
// Row 3 — the keys row (what Enter does, escape hatches)
// ---------------------------------------------------------------------------

/// Build the left side of the keys row. The switch runs over the compose
/// target so the verbs can never drift out of sync with the `as:` row above:
///
/// ```text
/// Enter send                 ← idle plain / command buffer
/// Enter steer  Tab follow-up   ← mid-round, steering armed
/// Enter follow-up  Tab steer   ← mid-round, queueing armed
/// Enter save  Esc cancel       ← queue-pointer edit
/// Enter send  /retry           ← stopped round parked for retry
/// ```
///
/// Only the primary verb carries the consequence hue; secondary keys (Tab,
/// Esc, `/retry`) keep the calm keycap treatment.
pub(crate) fn keys_row_spans(
    can_retry: bool,
    density: ActionDensity,
    target: ComposeTarget,
    theme: &Theme,
    bg: Color,
) -> Vec<Span<'static>> {
    let key_style = keycap_style(theme).bg(bg);
    let hint_style = Style::default().fg(theme.muted()).bg(bg);
    let compact = density.compact();
    let mut spans = vec![Span::styled(Key::ENTER.display(), key_style)];

    if let ComposeTarget::QueueEdit { kind, .. } = target {
        // In-place save of the pointed-at item. The badge itself moved to the
        // `as:` row; here only the actions remain. Both survive any width.
        spans.push(Span::styled(" save", hint_style.fg(kind.consequence_color(theme))));
        spans.push(Span::styled("  ", hint_style));
        spans.push(Span::styled(Key::ESC.display(), key_style));
        spans.push(Span::styled(" cancel", hint_style));
    } else if matches!(target, ComposeTarget::Steer | ComposeTarget::FollowUp) {
        let primary = Style::default()
            .fg(target.submit_color(theme))
            .bg(bg);
        match target {
            ComposeTarget::Steer => {
                spans.push(Span::styled(" steer", primary));
                spans.push(Span::styled("  ", hint_style));
                spans.push(Span::styled(Key::TAB.display(), key_style));
                spans.push(Span::styled(
                    if compact { " follow-up" } else { " follow-up mode" },
                    hint_style,
                ));
            }
            _ => {
                spans.push(Span::styled(" follow-up", primary));
                spans.push(Span::styled("  ", hint_style));
                spans.push(Span::styled(Key::TAB.display(), key_style));
                spans.push(Span::styled(if compact { " steer" } else { " steer mode" }, hint_style));
            }
        }
    } else if can_retry {
        spans.push(Span::styled(" send", hint_style));
        spans.push(Span::styled("  ", hint_style));
        spans.push(Span::styled("/retry", key_style));
        if !compact {
            spans.push(Span::styled(" to retry", hint_style));
        }
    } else {
        spans.push(Span::styled(" send", hint_style));
    }

    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(spans: &[Span<'static>]) -> String {
        spans.iter().map(|span| span.content.to_string()).collect()
    }

    #[test]
    fn keys_row_follows_the_compose_target() {
        let theme = Theme::default();
        let idle = text(&keys_row_spans(
            false,
            ActionDensity::Full,
            ComposeTarget::Prompt,
            &theme,
            Color::default(),
        ));
        assert!(idle.contains("Enter send"));

        let steer = text(&keys_row_spans(
            false,
            ActionDensity::Full,
            ComposeTarget::Steer,
            &theme,
            Color::default(),
        ));
        assert!(steer.contains("Enter steer"));
        assert!(steer.contains("Tab follow-up mode"));

        let follow_up = text(&keys_row_spans(
            false,
            ActionDensity::Compact,
            ComposeTarget::FollowUp,
            &theme,
            Color::default(),
        ));
        assert!(follow_up.contains("Enter follow-up"));
        assert!(follow_up.contains("Tab steer"));
        assert!(!follow_up.contains("mode"));
    }

    #[test]
    fn queue_edit_keys_save_and_cancel() {
        let theme = Theme::default();
        let row = text(&keys_row_spans(
            false,
            ActionDensity::Full,
            ComposeTarget::QueueEdit {
                kind: QueueEditKind::FollowUp,
                number: 2,
                dirty: true,
            },
            &theme,
            Color::default(),
        ));
        assert!(row.contains("Enter save"));
        assert!(row.contains("Esc cancel"));
        // The pointer badge lives on the `as:` row now.
        assert!(!row.contains("#2"));
    }

    #[test]
    fn retry_hatch_survives_compact() {
        let theme = Theme::default();
        let full = text(&keys_row_spans(
            true,
            ActionDensity::Full,
            ComposeTarget::Prompt,
            &theme,
            Color::default(),
        ));
        assert!(full.contains("/retry to retry"));
        let compact = text(&keys_row_spans(
            true,
            ActionDensity::Tiny,
            ComposeTarget::Prompt,
            &theme,
            Color::default(),
        ));
        assert!(compact.contains("/retry"));
        assert!(!compact.contains("to retry"));
    }

    #[test]
    fn as_row_states_target_with_consequence_colors() {
        let theme = Theme::default();
        let bg = Color::default();

        let plain = compose_target_spans(ComposeTarget::Prompt, &theme, bg);
        assert_eq!(text(&plain), "as: prompt");

        let command = compose_target_spans(ComposeTarget::Command, &theme, bg);
        assert_eq!(text(&command), "as: command");

        let steer = compose_target_spans(ComposeTarget::Steer, &theme, bg);
        assert_eq!(text(&steer), "as: steer prompt");
        assert_eq!(steer.last().unwrap().style.fg, theme.warn());

        let follow_up = compose_target_spans(ComposeTarget::FollowUp, &theme, bg);
        assert_eq!(text(&follow_up), "as: follow-up prompt");
        assert_eq!(follow_up.last().unwrap().style.fg, theme.info());
    }

    #[test]
    fn as_row_names_the_pointed_at_queue_group() {
        let theme = Theme::default();
        let dirty = compose_target_spans(
            ComposeTarget::QueueEdit {
                kind: QueueEditKind::FollowUp,
                number: 2,
                dirty: true,
            },
            &theme,
            Color::default(),
        );
        assert_eq!(
            text(&dirty),
            "compose: follow-ups[#2] · edited · as: follow-up"
        );

        let clean = compose_target_spans(
            ComposeTarget::QueueEdit {
                kind: QueueEditKind::Steer,
                number: 1,
                dirty: false,
            },
            &theme,
            Color::default(),
        );
        assert_eq!(text(&clean), "compose: steers[#1] · as: steer");
    }

    #[test]
    fn target_derivation_priority() {
        use crate::app::ComposerSendMode;
        // Queue pointer wins over busy-mode classification…
        assert_eq!(
            compose_target(true, Some(ComposerSendMode::Steer), Some((QueueEditKind::FollowUp, 3)), false),
            ComposeTarget::QueueEdit {
                kind: QueueEditKind::FollowUp,
                number: 3,
                dirty: false
            }
        );
        // …busy mode wins over the slash prefix…
        assert_eq!(
            compose_target(true, Some(ComposerSendMode::FollowUp), None, true),
            ComposeTarget::FollowUp
        );
        // …and slash only classifies an idle buffer.
        assert_eq!(compose_target(false, None, None, true), ComposeTarget::Command);
        assert_eq!(compose_target(false, None, None, false), ComposeTarget::Prompt);
    }

    #[test]
    fn char_count_formats_like_the_model_bar_cluster() {
        assert_eq!(format_char_count(0), "0 chars");
        assert_eq!(format_char_count(990), "990 chars");
        assert_eq!(format_char_count(14_236), "14.2k chars");
    }

    #[test]
    fn density_ladder_degrades_then_floors() {
        assert!(matches!(
            ActionDensity::for_width(80),
            ActionDensity::Full
        ));
        assert!(matches!(
            ActionDensity::for_width(30),
            ActionDensity::Compact
        ));
        assert!(matches!(ActionDensity::for_width(10), ActionDensity::Tiny));
    }
}
