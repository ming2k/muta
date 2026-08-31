//! Reusable Secure / Sensitive Input component with privacy masking and status indicators.
//!
//! Provides consistent rendering for API keys, bearer tokens, URLs, and password-like
//! credentials across all settings and provider configuration views.

#![allow(dead_code)]

use mutx_engine::{Line, Modifier, Span, Style};

use crate::theme::Theme;

/// Mask a secret string into bullets `•` or asterisks `*` while preserving character count.
pub fn mask_secret(value: &str) -> String {
    let count = value.chars().count();
    if count == 0 {
        String::new()
    } else {
        "•".repeat(count.min(32))
    }
}

/// Properties and state for rendering a secure input row.
#[derive(Debug, Clone)]
pub struct SecureInput<'a> {
    pub label: &'a str,
    pub value: &'a str,
    pub cursor_position: usize,
    pub is_editing: bool,
    pub is_secret: bool,
    pub is_configured: bool,
    pub is_required: bool,
    pub hint: &'a str,
    pub is_selected: bool,
    pub is_focused: bool,
}

impl<'a> SecureInput<'a> {
    pub fn new(label: &'a str, value: &'a str) -> Self {
        Self {
            label,
            value,
            cursor_position: value.len(),
            is_editing: false,
            is_secret: true,
            is_configured: !value.is_empty(),
            is_required: false,
            hint: "",
            is_selected: false,
            is_focused: false,
        }
    }

    pub fn editing(mut self, editing: bool) -> Self {
        self.is_editing = editing;
        self
    }

    pub fn secret(mut self, secret: bool) -> Self {
        self.is_secret = secret;
        self
    }

    pub fn configured(mut self, configured: bool) -> Self {
        self.is_configured = configured;
        self
    }

    pub fn required(mut self, required: bool) -> Self {
        self.is_required = required;
        self
    }

    pub fn cursor(mut self, pos: usize) -> Self {
        self.cursor_position = pos;
        self
    }

    pub fn hint(mut self, hint: &'a str) -> Self {
        self.hint = hint;
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.is_selected = selected;
        self
    }

    pub fn focused(mut self, focused: bool) -> Self {
        self.is_focused = focused;
        self
    }

    /// Render the component into a pair of Lines (primary input row + descriptive hint row).
    pub fn render_lines(&self, theme: &Theme) -> Vec<Line<'static>> {
        let cursor = if self.is_selected { "›" } else { " " };
        let cursor_style = Style::default().fg(if self.is_selected {
            theme.brand()
        } else {
            theme.dim()
        });

        let label_style = if self.is_selected && self.is_focused {
            Style::default()
                .fg(theme.brand())
                .add_modifier(Modifier::BOLD)
        } else if self.is_selected {
            Style::default().fg(theme.fg()).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.fg())
        };

        let mut row_spans = vec![
            Span::styled(format!(" {cursor} "), cursor_style),
            Span::styled(format!("{:<18}", self.label), label_style),
        ];

        if self.is_editing {
            // Render active input box with cursor
            let shown = if self.is_secret {
                mask_secret(self.value)
            } else {
                self.value.to_string()
            };
            let char_count = shown.chars().count();
            let char_pos = self.cursor_position.min(char_count);
            let byte_pos = shown
                .char_indices()
                .nth(char_pos)
                .map(|(i, _)| i)
                .unwrap_or(shown.len());

            let (left, right) = shown.split_at(byte_pos);
            let (mid, right) = if let Some(ch) = right.chars().next() {
                right.split_at(ch.len_utf8())
            } else {
                (" ", "")
            };

            row_spans.push(Span::styled(
                format!("[ {left}"),
                Style::default().fg(theme.brand()),
            ));
            row_spans.push(Span::styled(
                mid.to_string(),
                Style::default().bg(theme.brand()).fg(theme.body()),
            ));
            row_spans.push(Span::styled(
                format!("{right} ]"),
                Style::default().fg(theme.brand()),
            ));

            if self.is_secret && !self.value.is_empty() {
                row_spans.push(Span::styled(
                    format!("  ({} chars)", self.value.chars().count()),
                    Style::default().fg(theme.dim()),
                ));
            }
        } else if self.is_secret {
            if self.is_configured {
                row_spans.push(Span::styled(
                    "● Configured (••••••••)",
                    Style::default().fg(theme.ok()),
                ));
            } else if self.is_required {
                row_spans.push(Span::styled(
                    "⚠ Key Required",
                    Style::default().fg(theme.warn()),
                ));
            } else {
                row_spans.push(Span::styled(
                    "○ Not set (optional)",
                    Style::default().fg(theme.dim()),
                ));
            }
        } else if self.value.is_empty() {
            row_spans.push(Span::styled(
                "[ (not configured) ]",
                Style::default().fg(theme.dim()),
            ));
        } else {
            row_spans.push(Span::styled(
                format!("[ {} ]", self.value),
                Style::default().fg(theme.fg()),
            ));
        }

        let action_hint = if self.is_editing {
            "  [Enter save · Empty Enter clear · Esc cancel]"
        } else if self.is_configured {
            "  [Enter to replace / clear]"
        } else {
            "  [Enter to configure]"
        };

        let mut out = vec![Line::from(row_spans)];
        if !self.hint.is_empty() || self.is_editing {
            out.push(Line::from(vec![
                Span::raw("     "),
                Span::styled(self.hint.to_string(), Style::default().fg(theme.muted())),
                Span::styled(action_hint, Style::default().fg(theme.dim())),
            ]));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mask_secret_hides_content() {
        assert_eq!(mask_secret(""), "");
        assert_eq!(mask_secret("sk-12345"), "••••••••");
    }

    #[test]
    fn secure_input_renders_masked_when_secret() {
        let theme = Theme::default();
        let input = SecureInput::new("API Key", "secret-token")
            .secret(true)
            .configured(true);
        let lines = input.render_lines(&theme);
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("API Key"));
        assert!(text.contains("● Configured (••••••••)"));
        assert!(!text.contains("secret-token"));
    }

    #[test]
    fn secure_input_editing_renders_input_box() {
        let theme = Theme::default();
        let input = SecureInput::new("API Key", "jina_secret_key")
            .secret(true)
            .editing(true)
            .cursor(4);
        let lines = input.render_lines(&theme);
        let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("[ ••••"));
        assert!(text.contains("15 chars"));
        assert!(!text.contains("jina_secret_key"));
    }
}
