//! Contextual first-row header for transcript pages other than Main.
//!
//! `/btw` and Envoy are different views of the same transcript surface, so
//! they share one layout rule: identity and page-specific context on the left,
//! navigation actions on the right. Keeping this outside disclosure rendering
//! also leaves one clear extension point for future focused pages.

use neenee_tui::{Frame, Line, Modifier, Paragraph, Rect, Span, Style};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::{EnvoyBarInfo, STEP_MIN_WIDTH, Theme};

pub(super) enum PageHeader<'a> {
    Btw(neenee_core::ParentStatus),
    Envoy(&'a EnvoyBarInfo),
}

struct HeaderContent {
    title: &'static str,
    primary: String,
    meta: String,
    action: &'static str,
}

/// Draw a single contextual header row. The primary action is always retained
/// on narrow terminals; descriptive text truncates first, while Envoy sibling
/// shortcuts appear when there is enough room for them to remain legible.
pub(super) fn draw_page_header(
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
        PageHeader::Btw(status) => HeaderContent {
            title: " /btw ",
            primary: "Side conversation".to_string(),
            meta: parent_status_label(*status).to_string(),
            action: "Esc back ",
        },
        PageHeader::Envoy(bar) => HeaderContent {
            title: " Envoy ",
            primary: bar.label.clone(),
            meta: format!("{} of {}", bar.index, bar.total),
            action: if bar.total > 1 && full_width >= 64 {
                "Esc back   [ prev   ] next "
            } else {
                "Esc back "
            },
        },
    };

    let bg = theme.body();
    let fill = Style::default().bg(bg);
    let title_style = fill.fg(theme.fg()).add_modifier(Modifier::BOLD);
    let primary_style = fill.fg(theme.brand());
    let meta_style = fill.fg(theme.muted());
    let action_style = fill.fg(theme.muted());

    let title_width = content.title.width();
    let action_width = content.action.width();
    let left_budget = full_width.saturating_sub(title_width + action_width + 1);
    let left = fit_context(&content.primary, &content.meta, left_budget);
    let left_width: usize = left.iter().map(|(text, _)| text.width()).sum();
    let gap = full_width.saturating_sub(title_width + left_width + action_width);

    let mut spans = vec![Span::styled(content.title, title_style)];
    for (text, tone) in left {
        let style = match tone {
            Tone::Primary => primary_style,
            Tone::Meta => meta_style,
        };
        spans.push(Span::styled(text, style));
    }
    spans.push(Span::styled(" ".repeat(gap), fill));
    spans.push(Span::styled(content.action, action_style));

    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
}

#[derive(Clone, Copy)]
enum Tone {
    Primary,
    Meta,
}

/// Fit `primary · meta` into the left-hand budget while prioritizing the
/// compact state/index metadata. On very narrow rows the primary description
/// disappears before that orienting metadata does.
fn fit_context(primary: &str, meta: &str, budget: usize) -> Vec<(String, Tone)> {
    if budget == 0 {
        return Vec::new();
    }

    let primary_width = primary.width();
    let meta_width = meta.width();
    const SEPARATOR: &str = " · ";
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

fn parent_status_label(status: neenee_core::ParentStatus) -> &'static str {
    match status {
        neenee_core::ParentStatus::Idle => "main idle",
        neenee_core::ParentStatus::Running => "main running",
        neenee_core::ParentStatus::NeedsApproval => "main needs approval",
        neenee_core::ParentStatus::NeedsInput => "main needs input",
        neenee_core::ParentStatus::Failed => "main failed",
        neenee_core::ParentStatus::Interrupted => "main interrupted",
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
        let mut terminal = neenee_tui::TestTerminal::new(width, 1);
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
            PageHeader::Btw(neenee_core::ParentStatus::NeedsApproval),
        );
        assert!(row.starts_with(" /btw Side conversation · main needs approval"));
        assert!(row.ends_with("Esc back "));
    }

    #[test]
    fn envoy_header_keeps_context_and_sibling_navigation_on_wide_rows() {
        let info = EnvoyBarInfo {
            label: "inspect the renderer".to_string(),
            index: 2,
            total: 3,
        };
        let row = rendered_row(80, PageHeader::Envoy(&info));
        assert!(row.starts_with(" Envoy inspect the renderer · 2 of 3"));
        assert!(row.ends_with("Esc back   [ prev   ] next "));
    }

    #[test]
    fn narrow_header_preserves_identity_metadata_and_back_action() {
        let info = EnvoyBarInfo {
            label: "a very long envoy task description that cannot fit".to_string(),
            index: 12,
            total: 24,
        };
        let row = rendered_row(36, PageHeader::Envoy(&info));
        assert!(row.starts_with(" Envoy "));
        assert!(row.contains("12 of 24"));
        assert!(row.ends_with("Esc back "));
        assert_eq!(row.width(), 36);
    }
}
