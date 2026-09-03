//! Composer-native meta row: the single hint line painted inside the composer panel.
//!
//! Under the plane-less chat surface (ADR-0173):
//! - Idle: `Enter send` (right)
//! - Running: `Alt+S steer now` (left) / `Enter queue follow-up` (right)
//! - Completion: `Esc dismiss` (left) / `Tab / Enter select` (right)
//! - History: `Esc close` (left) / `Tab / Enter insert` (right)

use mutx_engine::{Color, Modifier, Span, Style};

use super::super::Theme;
use super::super::keymap::{HintSide, LiveHint};
use super::keycap::keycap_style;
use crate::modal_keys::live_history_hints;
use crate::session::{HintState, live_chat_hints};

// Width ladder

/// Width-degradation ladder for the keys row.
#[derive(Clone, Copy)]
pub(crate) enum ActionDensity {
    Full,
    Compact,
    Tiny,
}

impl ActionDensity {
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

// Compose target

/// What the live buffer represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ComposeTarget {
    /// Plain prompt opening a new turn (idle).
    #[default]
    Prompt,
    /// Slash command buffer.
    Command,
    /// Agent running: Enter queues follow-up, Alt+S steers now.
    Running,
    /// Active completion popup.
    Completion {
        kind: crate::completion::CompletionKind,
    },
    /// History search panel active (Ctrl+R).
    HistorySearch,
}

/// Derive the compose target from current state.
pub(crate) fn compose_target(
    busy: bool,
    _send_mode: Option<crate::app::ComposerSendMode>,
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
        return ComposeTarget::Running;
    }
    if is_slash {
        ComposeTarget::Command
    } else {
        ComposeTarget::Prompt
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ComposerHints {
    pub compose_target: ComposeTarget,
    pub can_retry: bool,
    /// The effective chord for the Session view's `steer` verb (ADR-0172):
    /// the hint row advertises exactly the binding that fires. Defaults to the
    /// canonical `Alt+S` when unremapped.
    pub steer_key: crate::keymap::Key,
}

impl Default for ComposerHints {
    fn default() -> Self {
        Self {
            compose_target: ComposeTarget::Prompt,
            can_retry: false,
            steer_key: crate::keymap::Key::ALT_S,
        }
    }
}

/// Build the composer's hint row separated into left and right spans.
///
/// The chord set (and its labels) come from the Session view's own scheme
/// (`session::live_chat_hints`, ADR-0172): what the row advertises is exactly
/// what `resolve_chat_surface_key` handles, so a hint can never drift from a
/// dead shortcut. Only the *presentation* — which side a chord lands on, the
/// 3-col gap between nav chords, the `Tab / Enter` action pairing, per-state
/// label styling, and the `command`/`retry` branding — lives here.
pub(crate) fn hint_row_parts(
    can_retry: bool,
    density: ActionDensity,
    target: ComposeTarget,
    theme: &Theme,
    bg: Color,
    steer_key: crate::keymap::Key,
) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
    let key_style = keycap_style(theme).bg(bg);
    let hint_style = theme.keycap_label_style().bg(bg);
    let verb_style = Style::default().bg(bg);
    let compact = density.compact();
    let tiny = matches!(density, ActionDensity::Tiny);

    // HistorySearch is a modal whose keys are owned by its own scheme
    // (`modal_keys::live_history_hints`, ADR-0172); this row renders exactly
    // those chords.
    if target == ComposeTarget::HistorySearch {
        let hints = live_history_hints();
        let mut left: Vec<Span<'static>> = Vec::new();
        for h in hints {
            if h.side != HintSide::Nav {
                continue;
            }
            left.push(Span::styled(h.key.display(), key_style));
            left.push(Span::styled(format!(" {}", h.label), hint_style));
        }
        let actions: Vec<&LiveHint> = hints
            .iter()
            .filter(|h| h.side == HintSide::Action)
            .collect();
        let mut right: Vec<Span<'static>> = Vec::new();
        for (i, h) in actions.iter().enumerate() {
            if i > 0 {
                right.push(Span::styled(" / ", hint_style));
            }
            right.push(Span::styled(h.key.display(), key_style));
        }
        if let Some(last) = actions.last() {
            right.push(Span::styled(
                format!(" {}", last.label),
                verb_style.fg(theme.brand()).add_modifier(Modifier::BOLD),
            ));
        }
        return (left, right);
    }

    let (state, action_label_style) = match target {
        ComposeTarget::Prompt => (HintState::Idle, hint_style),
        ComposeTarget::Command => (HintState::Command, hint_style),
        ComposeTarget::Running => (HintState::Running, verb_style.fg(theme.info())),
        ComposeTarget::Completion { .. } => (
            HintState::Completion,
            verb_style.fg(theme.brand()).add_modifier(Modifier::BOLD),
        ),
        ComposeTarget::HistorySearch => unreachable!(),
    };
    let hints = live_chat_hints(state, steer_key);

    // Left: navigation affordances, joined by a 3-col gap. Hidden on Tiny
    // terminals for the plain prompt / command rows; the steer verb's hint
    // (canonical Alt+S, remapped per ADR-0172) drops when compact so a running
    // row stays tight.
    let mut left: Vec<Span<'static>> = Vec::new();
    let hide_nav = tiny && matches!(state, HintState::Idle | HintState::Command);
    if !hide_nav {
        for h in &hints {
            if h.side != HintSide::Nav {
                continue;
            }
            if h.key == steer_key && compact {
                continue;
            }
            if !left.is_empty() {
                left.push(Span::styled("   ", hint_style));
            }
            left.push(Span::styled(h.key.display(), key_style));
            left.push(Span::styled(format!(" {}", h.label), hint_style));
        }
    }

    // Right: action affordances. Multiple keys pair as `Tab / Enter`, with the
    // verb label styled per state; the `command` and `retry` suffixes are
    // presentation branding on top of the scheme's `send` chord.
    let mut right: Vec<Span<'static>> = Vec::new();
    let actions: Vec<&crate::keymap::LiveHint> = hints
        .iter()
        .filter(|h| h.side == HintSide::Action)
        .collect();
    for (i, h) in actions.iter().enumerate() {
        if i > 0 {
            right.push(Span::styled(" / ", hint_style));
        }
        right.push(Span::styled(h.key.display(), key_style));
    }
    if let Some(last) = actions.last() {
        right.push(Span::styled(format!(" {}", last.label), action_label_style));
    }
    if target == ComposeTarget::Command {
        right.push(Span::styled(
            " command",
            verb_style.fg(theme.brand()).add_modifier(Modifier::BOLD),
        ));
    }
    if can_retry && target == ComposeTarget::Prompt {
        right.push(Span::styled("   ", hint_style));
        right.push(Span::styled("/retry", key_style));
        if !compact {
            right.push(Span::styled(" retry", hint_style));
        }
    }

    (left, right)
}

/// Build the composer's combined hint row.
#[allow(dead_code)]
pub(crate) fn hint_row_spans(
    can_retry: bool,
    density: ActionDensity,
    target: ComposeTarget,
    theme: &Theme,
    bg: Color,
    steer_key: crate::keymap::Key,
) -> Vec<Span<'static>> {
    let (left, right) = hint_row_parts(can_retry, density, target, theme, bg, steer_key);
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
    fn prompt_hint_shows_only_enter_send() {
        let theme = Theme::default();
        let (left, right) = hint_row_parts(
            false,
            ActionDensity::Full,
            ComposeTarget::Prompt,
            &theme,
            Color::Reset,
            crate::keymap::Key::ALT_S,
        );
        assert_eq!(text(&left), "", "idle row carries no nav chords (ADR-0173)");
        assert_eq!(text(&right), "Enter send");
    }

    #[test]
    fn running_hint_shows_steer_and_queue_follow_up() {
        let theme = Theme::default();
        let (left, right) = hint_row_parts(
            false,
            ActionDensity::Full,
            ComposeTarget::Running,
            &theme,
            Color::Reset,
            crate::keymap::Key::ALT_S,
        );
        assert_eq!(text(&left), "Alt+S steer now");
        assert_eq!(text(&right), "Enter queue follow-up");
    }

    #[test]
    fn running_hint_advertises_remapped_steer_chord() {
        let theme = Theme::default();
        let (left, _) = hint_row_parts(
            false,
            ActionDensity::Full,
            ComposeTarget::Running,
            &theme,
            Color::Reset,
            crate::keymap::Key::ALT_ENTER,
        );
        assert_eq!(
            text(&left),
            "Alt+Enter steer now",
            "the hint must advertise the effective steer binding, not the canonical"
        );
    }
}
