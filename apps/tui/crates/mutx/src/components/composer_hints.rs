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

use mutx_engine::{Color, Modifier, Span, Style};

use crate::app::ComposerSendMode;

use super::super::Theme;
use super::super::keymap::{Key, keyvocab};
use super::keycap::keycap_style;

// Width ladder

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
        if row_width >= 50 {
            ActionDensity::Full
        } else if row_width >= 24 {
            ActionDensity::Compact
        } else {
            ActionDensity::Tiny
        }
    }

    fn compact(self) -> bool {
        matches!(self, ActionDensity::Compact | ActionDensity::Tiny)
    }
}

// Compose target (what the buffer is / will become)

/// What the live buffer currently holds and what it becomes on `Enter`.
///
/// Derived entirely from state the frame already tracks (buffer prefix,
/// `ComposerSendMode × busy`, completion, modal) — no new app-level state.
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
    /// Active completion popup (slash commands or path mentions).
    Completion {
        kind: crate::completion::CompletionKind,
    },
    /// Global history search panel is active (Ctrl+R).
    HistorySearch,
}

impl ComposeTarget {
    /// Consequence-class hue for the submission verb: amber when Enter's
    /// effect interrupts (steers) the live round, info blue when it queues or recalls,
    /// brand when it accepts/inserts, plain foreground (`Color::Reset`) when none applies.
    fn submit_color(self, theme: &Theme) -> Color {
        match self {
            ComposeTarget::Steer => theme.warn(),
            ComposeTarget::FollowUp => theme.info(),
            ComposeTarget::Completion { .. } => theme.brand(),
            ComposeTarget::HistorySearch => theme.brand(),
            ComposeTarget::Prompt | ComposeTarget::Command => Color::Reset,
        }
    }
}

/// Derive the compose target from already-available state.
///
/// `is_history_search` marks the Ctrl+R panel.
/// `completion_active` marks an open candidate list.
pub(crate) fn compose_target(
    busy: bool,
    send_mode: Option<ComposerSendMode>,
    is_slash: bool,
    completion_active: Option<crate::completion::CompletionKind>,
    is_history_search: bool,
) -> ComposeTarget {
    if is_history_search {
        return ComposeTarget::HistorySearch;
    }
    if let Some(kind) = completion_active {
        return ComposeTarget::Completion { kind };
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

// The hint row — one sentence about what Enter does

/// Build the composer's hint row separated into left (navigation/actions) and
/// right (execution/submit) spans.
pub(crate) fn hint_row_parts(
    can_retry: bool,
    density: ActionDensity,
    target: ComposeTarget,
    theme: &Theme,
    bg: Color,
) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
    let key_style = keycap_style(theme).bg(bg);
    let hint_style = theme.keycap_label_style().bg(bg);
    let verb_style = Style::default().bg(bg);
    let compact = density.compact();

    match target {
        ComposeTarget::HistorySearch => {
            let left = if compact {
                vec![
                    Span::styled(Key::ESC.display(), key_style),
                    Span::styled(" close", hint_style),
                ]
            } else {
                vec![
                    Span::styled(keyvocab::ARROWS_UD, key_style),
                    Span::styled(" select", hint_style),
                    Span::styled("   ", hint_style),
                    Span::styled(Key::ESC.display(), key_style),
                    Span::styled(" close", hint_style),
                ]
            };
            let right = vec![
                Span::styled(Key::TAB.display(), key_style),
                Span::styled(" / ", hint_style),
                Span::styled(Key::ENTER.display(), key_style),
                Span::styled(
                    " insert",
                    verb_style.fg(theme.brand()).add_modifier(Modifier::BOLD),
                ),
            ];
            (left, right)
        }
        ComposeTarget::Completion { .. } => {
            let left = if compact {
                vec![
                    Span::styled(Key::ESC.display(), key_style),
                    Span::styled(" dismiss", hint_style),
                ]
            } else {
                vec![
                    Span::styled(keyvocab::ARROWS_UD, key_style),
                    Span::styled(" navigate", hint_style),
                    Span::styled("   ", hint_style),
                    Span::styled(Key::ESC.display(), key_style),
                    Span::styled(" dismiss", hint_style),
                ]
            };
            let right = vec![
                Span::styled(Key::TAB.display(), key_style),
                Span::styled(" / ", hint_style),
                Span::styled(Key::ENTER.display(), key_style),
                Span::styled(
                    " select",
                    verb_style.fg(theme.brand()).add_modifier(Modifier::BOLD),
                ),
            ];
            (left, right)
        }
        ComposeTarget::Command => {
            let left = if matches!(density, ActionDensity::Tiny) {
                Vec::new()
            } else if compact {
                vec![
                    Span::styled("Ctrl+X", key_style),
                    Span::styled(" actions", hint_style),
                ]
            } else {
                vec![
                    Span::styled("Ctrl+X o", key_style),
                    Span::styled(" focus", hint_style),
                    Span::styled("   ", hint_style),
                    Span::styled("Ctrl+X", key_style),
                    Span::styled(" actions", hint_style),
                ]
            };
            let right = vec![
                Span::styled(Key::ENTER.display(), key_style),
                Span::styled(" send ", hint_style),
                Span::styled(
                    "command",
                    verb_style.fg(theme.brand()).add_modifier(Modifier::BOLD),
                ),
            ];
            (left, right)
        }
        ComposeTarget::Steer => {
            let left = if compact {
                vec![
                    Span::styled(Key::TAB.display(), key_style),
                    Span::styled(" follow-up", hint_style),
                ]
            } else {
                vec![
                    Span::styled(Key::TAB.display(), key_style),
                    Span::styled(" follow-up", hint_style),
                    Span::styled("   ", hint_style),
                    Span::styled("Esc Esc", key_style),
                    Span::styled(" interrupt", hint_style),
                ]
            };
            let right = vec![
                Span::styled(Key::ENTER.display(), key_style),
                Span::styled(" send ", hint_style),
                Span::styled("steer", verb_style.fg(target.submit_color(theme))),
            ];
            (left, right)
        }
        ComposeTarget::FollowUp => {
            let left = if compact {
                vec![
                    Span::styled(Key::TAB.display(), key_style),
                    Span::styled(" steer", hint_style),
                ]
            } else {
                vec![
                    Span::styled(Key::TAB.display(), key_style),
                    Span::styled(" steer", hint_style),
                    Span::styled("   ", hint_style),
                    Span::styled("Esc Esc", key_style),
                    Span::styled(" interrupt", hint_style),
                ]
            };
            let right = vec![
                Span::styled(Key::ENTER.display(), key_style),
                Span::styled(" send ", hint_style),
                Span::styled("follow-up", verb_style.fg(target.submit_color(theme))),
            ];
            (left, right)
        }
        ComposeTarget::Prompt => {
            let left = if matches!(density, ActionDensity::Tiny) {
                Vec::new()
            } else if compact {
                vec![
                    Span::styled("Ctrl+X", key_style),
                    Span::styled(" actions", hint_style),
                ]
            } else {
                vec![
                    Span::styled("Ctrl+X o", key_style),
                    Span::styled(" focus", hint_style),
                    Span::styled("   ", hint_style),
                    Span::styled("Ctrl+X", key_style),
                    Span::styled(" actions", hint_style),
                ]
            };
            let mut right = vec![
                Span::styled(Key::ENTER.display(), key_style),
                Span::styled(" send", hint_style),
            ];
            if can_retry {
                right.push(Span::styled("   ", hint_style));
                right.push(Span::styled("/retry", key_style));
                if !compact {
                    right.push(Span::styled(" retry", hint_style));
                }
            }
            (left, right)
        }
    }
}

/// Build the composer's combined hint row: what the next `Enter` does, the `Tab`
/// toggle while mid-round, and any escape hatch.
#[allow(dead_code)]
pub(crate) fn hint_row_spans(
    can_retry: bool,
    density: ActionDensity,
    target: ComposeTarget,
    theme: &Theme,
    bg: Color,
) -> Vec<Span<'static>> {
    let (left, right) = hint_row_parts(can_retry, density, target, theme, bg);
    if left.is_empty() {
        right
    } else {
        let mut spans = left;
        spans.push(Span::styled("   ", Style::default().bg(bg)));
        spans.extend(right);
        spans
    }
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
        assert_eq!(idle, "Ctrl+X o focus   Ctrl+X actions   Enter send");

        let idle_compact = text(&hint_row_spans(
            false,
            ActionDensity::Compact,
            ComposeTarget::Prompt,
            &theme,
            Color::default(),
        ));
        assert_eq!(idle_compact, "Ctrl+X actions   Enter send");

        let steer = text(&hint_row_spans(
            false,
            ActionDensity::Full,
            ComposeTarget::Steer,
            &theme,
            Color::default(),
        ));
        assert_eq!(steer, "Tab follow-up   Esc Esc interrupt   Enter send steer");

        let follow_up = text(&hint_row_spans(
            false,
            ActionDensity::Compact,
            ComposeTarget::FollowUp,
            &theme,
            Color::default(),
        ));
        assert_eq!(follow_up, "Tab steer   Enter send follow-up");

        let follow_up_full = text(&hint_row_spans(
            false,
            ActionDensity::Full,
            ComposeTarget::FollowUp,
            &theme,
            Color::default(),
        ));
        assert_eq!(
            follow_up_full,
            "Tab steer   Esc Esc interrupt   Enter send follow-up"
        );

        let completion = text(&hint_row_spans(
            false,
            ActionDensity::Full,
            ComposeTarget::Completion {
                kind: crate::completion::CompletionKind::Slash,
            },
            &theme,
            Color::default(),
        ));
        assert_eq!(completion, "↑↓ navigate   Esc dismiss   Tab / Enter select");

        let search = text(&hint_row_spans(
            false,
            ActionDensity::Full,
            ComposeTarget::HistorySearch,
            &theme,
            Color::default(),
        ));
        assert_eq!(search, "↑↓ select   Esc close   Tab / Enter insert");
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
        assert_eq!(row, "Ctrl+X o focus   Ctrl+X actions   Enter send command");
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
        assert_eq!(full, "Ctrl+X o focus   Ctrl+X actions   Enter send   /retry retry");
        let compact = text(&hint_row_spans(
            true,
            ActionDensity::Tiny,
            ComposeTarget::Prompt,
            &theme,
            Color::default(),
        ));
        assert_eq!(compact, "Enter send   /retry");
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
    }

    #[test]
    fn target_derivation_priority() {
        use crate::app::ComposerSendMode;
        // History search wins over all…
        assert_eq!(
            compose_target(
                true,
                Some(ComposerSendMode::Steer),
                true,
                Some(crate::completion::CompletionKind::Slash),
                true
            ),
            ComposeTarget::HistorySearch
        );
        // …completion wins over busy mode…
        assert_eq!(
            compose_target(
                true,
                Some(ComposerSendMode::Steer),
                false,
                Some(crate::completion::CompletionKind::Slash),
                false
            ),
            ComposeTarget::Completion {
                kind: crate::completion::CompletionKind::Slash
            }
        );
        // …busy mode wins over the slash prefix…
        assert_eq!(
            compose_target(
                true,
                Some(ComposerSendMode::FollowUp),
                true,
                None,
                false
            ),
            ComposeTarget::FollowUp
        );
        // …and slash only classifies an idle buffer.
        assert_eq!(
            compose_target(false, None, true, None, false),
            ComposeTarget::Command
        );
        assert_eq!(
            compose_target(false, None, false, None, false),
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
