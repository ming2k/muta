//! Reusable Floating Dropdown / Popover Component for TUI.
//!
//! Provides a floating overlay dialog for selecting options (providers,
//! connections, models, presets) with keyboard navigation, readiness indicators,
//! badges, descriptions, and clean borders.

use mutx_engine::{
    Alignment, Clear, Frame, Line, Modifier, Paragraph, Rect, Span, Style,
    widgets::Block,
};

use crate::view::Theme;

/// A single selectable entry in a dropdown.
#[derive(Debug, Clone)]
pub struct DropdownItem {
    pub id: String,
    pub title: String,
    pub description: String,
    pub badge: Option<String>,
    pub is_ready: bool,
}

/// State for an active floating dropdown.
#[derive(Debug, Clone)]
pub struct DropdownState {
    pub title: String,
    pub selected_index: usize,
    pub items: Vec<DropdownItem>,
    pub context: String,
}

impl DropdownState {
    pub fn new(
        title: impl Into<String>,
        items: Vec<DropdownItem>,
        selected_id: &str,
        context: impl Into<String>,
    ) -> Self {
        let selected_index = items
            .iter()
            .position(|it| it.id == selected_id)
            .unwrap_or(0);
        Self {
            title: title.into(),
            selected_index,
            items,
            context: context.into(),
        }
    }

    pub fn select_prev(&mut self) {
        if self.items.is_empty() {
            return;
        }
        if self.selected_index == 0 {
            self.selected_index = self.items.len().saturating_sub(1);
        } else {
            self.selected_index -= 1;
        }
    }

    pub fn select_next(&mut self) {
        if self.items.is_empty() {
            return;
        }
        if self.selected_index + 1 >= self.items.len() {
            self.selected_index = 0;
        } else {
            self.selected_index += 1;
        }
    }

    pub fn selected_item(&self) -> Option<&DropdownItem> {
        self.items.get(self.selected_index)
    }
}

/// Render the floating dropdown popup overlay on top of the frame.
pub fn draw_dropdown_overlay(
    f: &mut Frame<'_>,
    state: &DropdownState,
    theme: &Theme,
    screen_area: Rect,
) {
    if state.items.is_empty() {
        return;
    }

    // Geometry calculation: center the floating popover with responsive bounds
    let item_height = 2; // 2 lines per item (title + badge on line 1, desc on line 2)
    let content_height = (state.items.len() * item_height) as u16 + 4; // + padding + title + hints
    let height = content_height.min(screen_area.height.saturating_sub(4)).max(8);
    let width = 72.min(screen_area.width.saturating_sub(6)).max(40);

    let x = screen_area.x + (screen_area.width.saturating_sub(width)) / 2;
    let y = screen_area.y + (screen_area.height.saturating_sub(height)) / 2;
    let popup_area = Rect::new(x, y, width, height);

    // 1. Clear background under popup
    f.render_widget(Clear, popup_area);

    // 2. Draw border and container
    let block = Block::bordered()
        .title(Line::from(vec![
            Span::styled("  ▼ ", Style::default().fg(theme.active_tab)),
            Span::styled(
                &state.title,
                Style::default()
                    .fg(theme.text)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
        ]))
        .border_style(Style::default().fg(theme.active_tab))
        .style(Style::default().bg(theme.panel_bg));

    f.render_widget(block, popup_area);

    let inner_x = popup_area.x + 2;
    let inner_y = popup_area.y + 1;
    let inner_width = popup_area.width.saturating_sub(4);
    let inner_height = popup_area.height.saturating_sub(3);

    // 3. Render items
    let mut lines: Vec<Line<'_>> = Vec::new();

    for (idx, item) in state.items.iter().enumerate() {
        let is_selected = idx == state.selected_index;

        let cursor_span = if is_selected {
            Span::styled("› ", Style::default().fg(theme.active_tab).add_modifier(Modifier::BOLD))
        } else {
            Span::raw("  ")
        };

        let indicator = if item.is_ready {
            Span::styled("● ", Style::default().fg(theme.diff_add))
        } else {
            Span::styled("○ ", Style::default().fg(theme.diff_delete))
        };

        let title_style = if is_selected {
            Style::default()
                .fg(theme.text)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text)
        };

        let mut spans = vec![
            cursor_span,
            indicator,
            Span::styled(&item.title, title_style),
        ];

        if let Some(badge) = &item.badge {
            let badge_color = if item.is_ready {
                theme.diff_add
            } else {
                theme.hint
            };
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                format!("[{badge}]"),
                Style::default().fg(badge_color),
            ));
        }

        // Line 1: Header with title & badges
        lines.push(Line::from(spans));

        // Line 2: Description
        let desc_style = if is_selected {
            Style::default().fg(theme.active_tab)
        } else {
            Style::default().fg(theme.hint)
        };
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(&item.description, desc_style),
        ]));
    }

    let items_rect = Rect::new(inner_x, inner_y, inner_width, inner_height);
    f.render_widget(Paragraph::new(lines), items_rect);

    // 4. Render footer key hints at bottom of popup
    let footer_rect = Rect::new(
        popup_area.x + 2,
        popup_area.y + popup_area.height - 2,
        popup_area.width.saturating_sub(4),
        1,
    );

    let footer_line = Line::from(vec![
        Span::styled("↑/↓", Style::default().fg(theme.active_tab).add_modifier(Modifier::BOLD)),
        Span::styled(" Select  ", Style::default().fg(theme.hint)),
        Span::styled("↵", Style::default().fg(theme.active_tab).add_modifier(Modifier::BOLD)),
        Span::styled(" Confirm  ", Style::default().fg(theme.hint)),
        Span::styled("Esc", Style::default().fg(theme.active_tab).add_modifier(Modifier::BOLD)),
        Span::styled(" Cancel", Style::default().fg(theme.hint)),
    ]);

    f.render_widget(Paragraph::new(footer_line).alignment(Alignment::Right), footer_rect);
}
