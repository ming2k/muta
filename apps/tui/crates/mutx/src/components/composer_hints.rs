//! Composer-native meta row: the single hint line painted inside the composer panel.
//!
//! Under the Composer-first architecture:
//! - Idle: `Tab transcript` (left) / `Enter send` (right)
//! - Running: `Tab transcript   Alt+S steer now` (left) / `Enter queue follow-up` (right)
//! - Completion: `Esc dismiss` (left) / `Tab / Enter select` (right)
//! - History: `Esc close` (left) / `Tab / Enter insert` (right)

use mutx_engine::{Color, Modifier, Span, Style};

use super::super::Theme;
use super::super::keymap::Key;
use super::keycap::keycap_style;

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

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ComposerHints {
    pub compose_target: ComposeTarget,
    pub can_retry: bool,
}

/// Build the composer's hint row separated into left and right spans.
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
            let left = vec![
                Span::styled(Key::ESC.display(), key_style),
                Span::styled(" close", hint_style),
            ];
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
            let left = vec![
                Span::styled(Key::ESC.display(), key_style),
                Span::styled(" dismiss", hint_style),
            ];
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
        ComposeTarget::Running => {
            let left = if compact {
                vec![
                    Span::styled(Key::TAB.display(), key_style),
                    Span::styled(" transcript", hint_style),
                ]
            } else {
                vec![
                    Span::styled(Key::TAB.display(), key_style),
                    Span::styled(" transcript", hint_style),
                    Span::styled("   ", hint_style),
                    Span::styled(Key::ALT_S.display(), key_style),
                    Span::styled(" steer now", hint_style),
                ]
            };
            let right = vec![
                Span::styled(Key::ENTER.display(), key_style),
                Span::styled(" queue follow-up", verb_style.fg(theme.info())),
            ];
            (left, right)
        }
        ComposeTarget::Command => {
            let left = if matches!(density, ActionDensity::Tiny) {
                Vec::new()
            } else {
                vec![
                    Span::styled(Key::TAB.display(), key_style),
                    Span::styled(" transcript", hint_style),
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
        ComposeTarget::Prompt => {
            let left = if matches!(density, ActionDensity::Tiny) {
                Vec::new()
            } else {
                vec![
                    Span::styled(Key::TAB.display(), key_style),
                    Span::styled(" transcript", hint_style),
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

/// Build the composer's combined hint row.
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
    fn prompt_hint_shows_tab_transcript_and_enter_send() {
        let theme = Theme::default();
        let (left, right) = hint_row_parts(false, ActionDensity::Full, ComposeTarget::Prompt, &theme, Color::Reset);
        assert_eq!(text(&left), "Tab transcript");
        assert_eq!(text(&right), "Enter send");
    }

    #[test]
    fn running_hint_shows_steer_and_queue_follow_up() {
        let theme = Theme::default();
        let (left, right) = hint_row_parts(false, ActionDensity::Full, ComposeTarget::Running, &theme, Color::Reset);
        assert_eq!(text(&left), "Tab transcript   Alt+S steer now");
        assert_eq!(text(&right), "Enter queue follow-up");
    }
}
