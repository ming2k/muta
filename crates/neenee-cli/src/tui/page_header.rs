//! Contextual first-row header for every transcript page.
//!
//! Every view — Main (session), `/btw`, Envoy, and future focused pages —
//! shares one layout rule: identity and page-specific context on the left,
//! mode / navigation actions on the right. Keeping this outside disclosure
//! rendering also leaves one clear extension point for future focused pages.

use neenee_tui_engine::{Frame, Line, Modifier, Paragraph, Rect, Span, Style};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::{EnvoyBarInfo, STEP_MIN_WIDTH, Theme};

pub(crate) enum PageHeader<'a> {
    /// The Main session view: `SESSION` identity, the session's persistent-id
    /// tail, and the workspace on the left; the session mode (e.g.
    /// `autopilot`) on the right.
    Session(&'a SessionHead<'a>),
    Btw(neenee_core::ParentStatus),
    Envoy(&'a EnvoyBarInfo),
}

/// Left/right content for the Main session view's head row.
pub(crate) struct SessionHead<'a> {
    /// The session's persistent id (full string). Only its last four
    /// characters are shown, dimmed, as a disambiguating tag.
    pub session_id: &'a str,
    /// Tilde-shortened workspace path (e.g. `~/projects/xx`). Already
    /// abbreviated by the caller; rendered as-is.
    pub workspace: &'a str,
    /// `true` while the session runs in autopilot mode (`--autopilot` /
    /// `/autopilot on`). Shown as a warning-toned `autopilot` tag on the
    /// right — the session's persistent mode flag.
    pub autopilot: bool,
}

struct HeaderContent {
    title: &'static str,
    /// The identity tail that sits right after the title (session-id tail,
    /// dimmed). Empty when the variant has none.
    tag: String,
    primary: String,
    meta: String,
    action: String,
}

/// Draw a single contextual header row. The primary action is always retained
/// on narrow terminals; descriptive text truncates first, while Envoy sibling
/// shortcuts appear when there is enough room for them to remain legible.
pub(crate) fn draw_page_header(
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
        PageHeader::Session(head) => HeaderContent {
            title: " SESSION ",
            tag: id_tail(head.session_id),
            primary: head.workspace.to_string(),
            meta: String::new(),
            action: if head.autopilot {
                "autopilot ".to_string()
            } else {
                String::new()
            },
        },
        // Envoy and /btw are contextual pages that replace the session head.
        PageHeader::Btw(status) => HeaderContent {
            title: " /btw ",
            tag: String::new(),
            primary: "Side conversation".to_string(),
            meta: parent_status_label(*status).to_string(),
            action: "Esc back ".to_string(),
        },
        PageHeader::Envoy(bar) => HeaderContent {
            title: " Envoy ",
            tag: String::new(),
            primary: bar.label.clone(),
            meta: format!("{} of {}", bar.index, bar.total),
            action: if bar.total > 1 && full_width >= 64 {
                "Esc back   [ prev   ] next ".to_string()
            } else {
                "Esc back ".to_string()
            },
        },
    };

    let bg = theme.body();
    let fill = Style::default().bg(bg);
    let title_style = fill.fg(theme.fg()).add_modifier(Modifier::BOLD);
    let tag_style = fill.fg(theme.dim());
    let primary_style = fill.fg(theme.brand());
    let meta_style = fill.fg(theme.muted());
    // The session mode flag (`autopilot`) reads as a persistent safety state,
    // so it takes the warning tone; every other variant's action is a quiet
    // navigation affordance.
    let action_style = if matches!(header, PageHeader::Session(_)) {
        fill.fg(theme.warn()).add_modifier(Modifier::BOLD)
    } else {
        fill.fg(theme.muted())
    };

    let title_width = content.title.width();
    // The tag renders as `<tag> ` (tag + one trailing space) right after the
    // title — the title already ends with a space, so the tag needs no
    // leading separator.
    let tag_width = if content.tag.is_empty() {
        0
    } else {
        content.tag.width() + 1
    };
    let action_width = content.action.width();
    let left_budget = full_width.saturating_sub(title_width + tag_width + action_width + 1);
    let left = fit_context(&content.primary, &content.meta, left_budget);
    let left_width: usize = left.iter().map(|(text, _)| text.width()).sum();
    let gap = full_width.saturating_sub(title_width + tag_width + left_width + action_width);

    let mut spans = vec![Span::styled(content.title, title_style)];
    if !content.tag.is_empty() {
        spans.push(Span::styled(format!("{} ", content.tag), tag_style));
    }
    for (text, tone) in left {
        let style = match tone {
            Tone::Primary => primary_style,
            Tone::Meta => meta_style,
        };
        spans.push(Span::styled(text, style));
    }
    spans.push(Span::styled(" ".repeat(gap), fill));
    if !content.action.is_empty() {
        spans.push(Span::styled(content.action, action_style));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
}

/// The last four characters of a persistent session id — the short,
/// glanceable tag that disambiguates sessions without exposing the full id.
/// Returns the whole id when it is already four characters or fewer, and an
/// empty string for an empty id (so no tag is rendered at all).
fn id_tail(session_id: &str) -> String {
    if session_id.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = session_id.chars().collect();
    let take = chars.len().min(4);
    chars[chars.len() - take..].iter().collect()
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

    // A lone primary (no meta) fills the budget directly — the common case
    // for the Session head, whose workspace path has no trailing metadata.
    if meta.is_empty() {
        return vec![(truncate_to_width(primary, budget), Tone::Primary)];
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
        let mut terminal = neenee_tui_engine::TestTerminal::new(width, 1);
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

    #[test]
    fn session_header_shows_id_tail_workspace_and_autopilot() {
        let head = SessionHead {
            session_id: "sess-01a2b3c4",
            workspace: "~/projects/xx",
            autopilot: true,
        };
        let row = rendered_row(80, PageHeader::Session(&head));
        assert!(row.starts_with(" SESSION b3c4 ~/projects/xx"));
        assert!(row.ends_with("autopilot "));
    }

    #[test]
    fn session_header_hides_mode_and_short_id_tail() {
        let head = SessionHead {
            session_id: "ab",
            workspace: "~/work",
            autopilot: false,
        };
        let row = rendered_row(40, PageHeader::Session(&head));
        assert!(row.starts_with(" SESSION ab ~/work"));
        assert!(!row.contains("autopilot"));
    }
}
