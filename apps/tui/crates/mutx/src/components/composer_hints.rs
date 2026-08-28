//! Composer-native meta row: the single hint line painted inside the
//! composer panel's bottom band.
//!
//! The row reads as one sentence about the same target: what the next
//! `Enter` does (`Enter send prompt`, `Enter send steer`, `Enter send
//! follow-up`, `Enter update follow-ups[2]`), the `Tab` toggle that swaps
//! steer / follow-up while a round is live, and the char count. The verbs
//! name the delivery group the buffer will land in, so the row and the
//! transcript's queued-message badges can never drift apart.
//!
//! The builder takes the composer's panel background so the keycaps and the
//! tinted verbs blend into the box instead of carrying the outer surface
//! color the old standalone bar below the input used.

use unicode_width::UnicodeWidthStr;

use mutx_engine::{Color, Modifier, Span, Style};

use crate::app::ComposerSendMode;

use super::super::Theme;
use super::super::keymap::Key;
use super::keycap::keycap_style;

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
    /// The plural noun naming the group of queued items being pointed at —
    /// the same word the hint row's `update follow-ups[2]` verb uses.
    pub(crate) fn plural_noun(self) -> &'static str {
        match self {
            QueueEditKind::Steer => "steers",
            QueueEditKind::FollowUp => "follow-ups",
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

/// Owned inputs for the composer's hint row. Built by the event loop before
/// the mutable composer borrow begins (`draw_composer` mutates
/// `input_scroll`, so no `&App` borrows may be threaded through), then copied
/// into `ComposerDrawOptions`.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ComposerHints {
    /// What the buffer is and what commit will do with it — drives the
    /// sentence's verb (`send prompt` / `send steer` / `update follow-ups[2]`).
    pub compose_target: ComposeTarget,
    /// Stopped round parked for `/retry` — mirrored from `SessionChrome`,
    /// same affordance the gauge bar used to repeat.
    pub can_retry: bool,
}

// ---------------------------------------------------------------------------
// The hint row — one sentence about what Enter does
// ---------------------------------------------------------------------------

/// Build the composer's hint row: what the next `Enter` does, the `Tab`
/// toggle while mid-round, and any escape hatch. The switch runs over the
/// compose target so the verb can never drift out of sync with where the
/// buffer will actually land:
///
/// ```text
/// Enter send prompt                 ← idle plain buffer
/// Enter send command                ← resolved `/command` buffer
/// Enter send steer  Tab follow-up   ← mid-round, steering armed
/// Enter send follow-up  Tab steer   ← mid-round, queueing armed
/// Enter update follow-ups[2]        ← queue-pointer edit
/// Enter send  /retry to retry       ← stopped round parked for retry
/// ```
///
/// The verb names the delivery group; `update follow-ups[2]` names the exact
/// item an armed pointer will re-deliver. Only the primary verb carries the
/// consequence hue (amber = interrupts the live round, info blue = queues);
/// secondary keys (`Tab`, `/retry`) keep the calm keycap treatment.
pub(crate) fn hint_row_spans(
    can_retry: bool,
    density: ActionDensity,
    target: ComposeTarget,
    theme: &Theme,
    bg: Color,
) -> Vec<Span<'static>> {
    let key_style = keycap_style(theme).bg(bg);
    let hint_style = Style::default().fg(theme.muted()).bg(bg);
    let verb_style = Style::default().bg(bg);
    let compact = density.compact();
    let mut spans = vec![Span::styled(Key::ENTER.display(), key_style)];

    if let ComposeTarget::QueueEdit { kind, number, .. } = target {
        // In-place update of the pointed-at item: the verb names both the
        // delivery group and the exact position, so the sentence stays true
        // even when the edit is dirty. Survives any width — this clause is
        // the reason the row exists.
        spans.push(Span::styled(" update ", hint_style));
        spans.push(Span::styled(
            format!("{}[{number}]", kind.plural_noun()),
            verb_style.fg(kind.consequence_color(theme)),
        ));
        return spans;
    }

    spans.push(Span::styled(" send", hint_style));

    match target {
        ComposeTarget::Command => {
            // Echo the in-box resolved-command treatment: brand + bold.
            spans.push(Span::styled(
                " command",
                verb_style.fg(theme.brand()).add_modifier(Modifier::BOLD),
            ));
        }
        ComposeTarget::Steer | ComposeTarget::FollowUp => {
            let primary = verb_style.fg(target.submit_color(theme));
            let (verb, other, other_label) = match target {
                ComposeTarget::Steer => (
                    " steer",
                    "follow-up",
                    if compact {
                        " follow-up"
                    } else {
                        " follow-up mode"
                    },
                ),
                _ => (
                    " follow-up",
                    "steer",
                    if compact { " steer" } else { " steer mode" },
                ),
            };
            spans.push(Span::styled(verb, primary));
            spans.push(Span::styled("  ", hint_style));
            spans.push(Span::styled(Key::TAB.display(), key_style));
            spans.push(Span::styled(other_label, hint_style));
            // `other` kept for symmetry with the compact ladder above.
            let _ = other;
        }
        ComposeTarget::Prompt | ComposeTarget::QueueEdit { .. } => {
            if can_retry {
                spans.push(Span::styled("  ", hint_style));
                spans.push(Span::styled("/retry", key_style));
                if !compact {
                    spans.push(Span::styled(" to retry", hint_style));
                }
            } else {
                spans.push(Span::styled(" prompt", hint_style));
            }
        }
    }
    spans
}

/// Width of a span run in display columns.
/// Abbreviated display width of a span run (unused today; kept beside the
/// span builders so width math never gets re-implemented ad hoc).
#[allow(dead_code)]
pub(crate) fn spans_width(spans: &[Span<'static>]) -> usize {
    spans.iter().map(|span| span.content.width()).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(spans: &[Span<'static>]) -> String {
        spans.iter().map(|span| span.content.to_string()).collect()
    }

    #[test]
    fn hint_row_follows_the_compose_target() {
        let theme = Theme::default();
        let idle = text(&hint_row_spans(
            false,
            ActionDensity::Full,
            ComposeTarget::Prompt,
            &theme,
            Color::default(),
        ));
        assert_eq!(idle, "Enter send prompt");

        let steer = text(&hint_row_spans(
            false,
            ActionDensity::Full,
            ComposeTarget::Steer,
            &theme,
            Color::default(),
        ));
        assert_eq!(steer, "Enter send steer  Tab follow-up mode");

        let follow_up = text(&hint_row_spans(
            false,
            ActionDensity::Compact,
            ComposeTarget::FollowUp,
            &theme,
            Color::default(),
        ));
        assert_eq!(follow_up, "Enter send follow-up  Tab steer");
    }

    #[test]
    fn hint_row_command_buffer_names_the_command_verb() {
        let theme = Theme::default();
        let row = text(&hint_row_spans(
            false,
            ActionDensity::Full,
            ComposeTarget::Command,
            &theme,
            Color::default(),
        ));
        assert_eq!(row, "Enter send command");
    }

    #[test]
    fn queue_edit_verb_names_the_group_and_position() {
        let theme = Theme::default();
        let row = text(&hint_row_spans(
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
        assert_eq!(row, "Enter update follow-ups[2]");

        let steer_edit = text(&hint_row_spans(
            false,
            ActionDensity::Full,
            ComposeTarget::QueueEdit {
                kind: QueueEditKind::Steer,
                number: 1,
                dirty: false,
            },
            &theme,
            Color::default(),
        ));
        assert_eq!(steer_edit, "Enter update steers[1]");
    }

    #[test]
    fn retry_hatch_survives_compact() {
        let theme = Theme::default();
        let full = text(&hint_row_spans(
            true,
            ActionDensity::Full,
            ComposeTarget::Prompt,
            &theme,
            Color::default(),
        ));
        assert_eq!(full, "Enter send  /retry to retry");
        let compact = text(&hint_row_spans(
            true,
            ActionDensity::Tiny,
            ComposeTarget::Prompt,
            &theme,
            Color::default(),
        ));
        assert_eq!(compact, "Enter send  /retry");
    }

    #[test]
    fn hint_row_consequence_colors() {
        let theme = Theme::default();
        let bg = Color::default();

        // Steer interrupts the live round: amber verb.
        let steer = hint_row_spans(false, ActionDensity::Full, ComposeTarget::Steer, &theme, bg);
        assert!(
            steer.iter().any(|span| span.style.fg == theme.warn()),
            "steer verb must carry the warn hue"
        );

        // Follow-up queues: info blue verb.
        let follow_up = hint_row_spans(
            false,
            ActionDensity::Full,
            ComposeTarget::FollowUp,
            &theme,
            bg,
        );
        assert!(
            follow_up.iter().any(|span| span.style.fg == theme.info()),
            "follow-up verb must carry the info hue"
        );

        // Queue-edit verb inherits the delivery group's consequence color.
        let edit = hint_row_spans(
            false,
            ActionDensity::Full,
            ComposeTarget::QueueEdit {
                kind: QueueEditKind::FollowUp,
                number: 2,
                dirty: false,
            },
            &theme,
            bg,
        );
        assert!(
            edit.iter().any(|span| span.style.fg == theme.info()),
            "queue-edit verb must carry the group's consequence hue"
        );
    }

    #[test]
    fn target_derivation_priority() {
        use crate::app::ComposerSendMode;
        // Queue pointer wins over busy-mode classification…
        assert_eq!(
            compose_target(
                true,
                Some(ComposerSendMode::Steer),
                Some((QueueEditKind::FollowUp, 3)),
                false
            ),
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
        assert_eq!(
            compose_target(false, None, None, true),
            ComposeTarget::Command
        );
        assert_eq!(
            compose_target(false, None, None, false),
            ComposeTarget::Prompt
        );
    }

    #[test]
    fn density_ladder_degrades_then_floors() {
        assert!(matches!(ActionDensity::for_width(80), ActionDensity::Full));
        assert!(matches!(
            ActionDensity::for_width(30),
            ActionDensity::Compact
        ));
        assert!(matches!(ActionDensity::for_width(10), ActionDensity::Tiny));
    }
}
