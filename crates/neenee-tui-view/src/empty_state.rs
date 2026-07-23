//! Empty-state hero shown in place of the transcript when a session holds no
//! messages yet.
//!
//! This is a **replacement** for the transcript stream, not content rendered
//! *inside* it: `draw_transcript` short-circuits to this component when
//! `messages` is empty (and no envoy/side view is open), keeping the empty
//! state out of the message-rendering pipeline entirely. Responsibilities stay
//! clean — the empty state never participates in scroll, selection, or
//! attribution logic.
//!
//! The footer (input box, status bar, hint bar) renders exactly as in a live
//! session, so the user lands in a familiar composer immediately.
//!
//! The logo source is pluggable: a caller may pass user-supplied lines (loaded
//! from `$XDG_CONFIG_HOME/neenee/logo.txt`); when absent the built-in figlet
//! wordmark is used. Either way the art is clamped to a safe bounding box so a
//! giant paste can never blow out the welcome screen.

use neenee_tui_engine::{
    Alignment, Frame, Paragraph, Rect, {Line, Span}, {Modifier, Style},
};

use super::theme::Theme;

/// Hard width cap (in terminal columns) for any logo line. A wider line is
/// truncated at a character boundary. 60 keeps comfortable side margins inside
/// an 80-column terminal while leaving room for the tagline beneath.
pub(super) const MAX_LOGO_COLS: usize = 60;

/// Hard height cap (in rows) for the logo block. More lines than this are
/// dropped from the bottom. 20 leaves the welcome screen readable on a
/// 24-row terminal even before vertical centering.
pub(super) const MAX_LOGO_ROWS: usize = 20;

/// The built-in wordmark, rendered when no user logo is supplied (figlet
/// "small" font). Compact enough to fit an 80-column terminal with room for the
/// tagline beneath, while still reading as a logo at a glance rather than
/// competing with the transcript that will replace it.
const BUILTIN_LOGO: &[&str] = &[
    " _ _  ___ ___ _ _  ___ ___ ",
    "| ' \\/ -_) -_) ' \\/ -_) -_|",
    "|_||_\\___\\___|_||_\\___\\___|",
];

/// Parse a raw logo file into a display-safe line vector, enforcing the
/// `MAX_LOGO_COLS` × `MAX_LOGO_ROWS` bounding box:
///
/// - Lines are split on `\n`; trailing `\r` (CRLF files) is stripped.
/// - Each line is truncated to `MAX_LOGO_COLS` chars.
/// - Leading/trailing blank lines are dropped (so a trailing newline doesn't
///   waste a centered row).
/// - At most `MAX_LOGO_ROWS` lines are kept (excess dropped from the bottom).
///
/// Returns `None` when the input yields no visible lines, so the caller falls
/// back to the built-in logo rather than rendering nothing.
pub fn parse_logo(raw: &str) -> Option<Vec<String>> {
    let mut lines: Vec<String> = raw
        .split('\n')
        .map(|l| l.trim_end_matches('\r'))
        .map(|l| truncate_chars(l, MAX_LOGO_COLS))
        .skip_while(|l| l.trim().is_empty())
        .collect();
    // Trim trailing blank lines.
    while lines.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        lines.pop();
    }
    lines.truncate(MAX_LOGO_ROWS);
    if lines.is_empty() { None } else { Some(lines) }
}

/// Truncate a string to at most `max` display chars (by Unicode scalar value,
/// not graphemes — terminals are cell-oriented and ASCII art is the target
/// use case). Does not split multi-byte chars.
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

/// Compute the height the empty state occupies for a given logo + guidance,
/// without drawing. Lets the transcript renderer keep its `content_lines`
/// accounting honest so the app loop does not treat an empty session as a
/// zero-height stream. `logo` is the effective lines (user-supplied or
/// built-in); `guidance` adds the lines beneath the gap (tagline + any
/// onboarding copy).
fn empty_state_height(logo: &[&str], guidance: EmptyStateGuidance) -> usize {
    logo.len() + 2 // logo rows + blank gap before the tagline
        + guidance_line_count(guidance)
}

/// Resolve the effective logo lines: user-supplied lines when present,
/// otherwise the built-in wordmark.
fn effective_logo(user_logo: Option<&[String]>) -> Vec<&str> {
    if let Some(lines) = user_logo
        && !lines.is_empty()
    {
        return lines.iter().map(String::as_str).collect();
    }
    BUILTIN_LOGO.to_vec()
}

/// Which guidance variant the empty-state hero shows beneath the logo.
///
/// The empty state is a **contextual** surface (ADR-0057): it adapts to
/// whether the user has a working LLM configured and whether they have seen
/// the one-time onboarding, rather than showing every user the same static
/// tagline. The variants are mutually exclusive and ordered by priority — a
/// missing provider beats a not-yet-onboarded provider — so the hero never
/// shows two competing calls to action. [`Self::None`] leaves the classic
/// minimal tagline, preserving the calm "landing strip" for returning users.
///
/// The app shell selects the variant; the view layer owns the copy and
/// styling. This keeps the policy (when to nudge) in the shell and the
/// presentation (what the nudge looks like) in the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EmptyStateGuidance {
    /// No extra guidance — the classic single-line tagline only. The default
    /// for a returning, fully-configured user so the hero stays a calm landing
    /// strip rather than a recurring billboard.
    #[default]
    None,
    /// No usable LLM provider is configured. Steers the user to `/provider`
    /// before they type, since a message sent against the mock provider goes
    /// nowhere. This is a real setup blocker, not onboarding — it clears the
    /// moment a keyed provider exists, independently of the onboarding flag.
    NeedsProvider,
    /// A provider is configured but the user has not yet sent their first
    /// message. Surfaces a few distinctive slash commands (`/skills`,
    /// `/pursue`, `/help`) so the capability set is discoverable without a
    /// `/help` round-trip. Dismissed permanently on the first send.
    Onboarding,
}

/// Number of text lines the guidance section renders beneath the logo + gap.
/// Used by [`empty_state_content_lines`] to keep the app loop's scroll
/// accounting honest without needing a `Theme` — the count is variant-only,
/// never wrap-dependent (every guidance line fits the minimum terminal width).
fn guidance_line_count(guidance: EmptyStateGuidance) -> usize {
    match guidance {
        EmptyStateGuidance::None => 1,          // tagline only
        EmptyStateGuidance::NeedsProvider => 2, // blocker + action
        EmptyStateGuidance::Onboarding => 2,    // tagline + capability hint
    }
}

/// Build the styled text lines for the guidance section. The logo + blank gap
/// are rendered by [`draw_empty_state`]; this owns only the copy beneath. The
/// `info` tone highlights actionable tokens (`/provider`, `/skills`, …) so the
/// next step reads as a affordance, not ambient text — mirroring how the hint
/// bar uses `info` for the `◆ effort` tag.
fn guidance_section(guidance: EmptyStateGuidance, theme: &Theme) -> Vec<Line<'static>> {
    let muted = Style::default().fg(theme.muted());
    let info = Style::default().fg(theme.info());
    let warn = Style::default().fg(theme.warn());
    match guidance {
        EmptyStateGuidance::None => vec![Line::from(vec![Span::styled(
            "Type a message below to begin.",
            muted,
        )])],
        EmptyStateGuidance::NeedsProvider => vec![
            Line::from(vec![Span::styled(
                "No LLM provider is configured yet.",
                warn,
            )]),
            Line::from(vec![
                Span::styled("Run ", muted),
                Span::styled("/provider", info),
                Span::styled(" to set one up.", muted),
            ]),
        ],
        EmptyStateGuidance::Onboarding => vec![
            Line::from(vec![Span::styled("Type a message below to begin.", muted)]),
            Line::from(vec![
                Span::styled("Try ", muted),
                Span::styled("/skills", info),
                Span::styled(" · ", muted),
                Span::styled("/pursue <goal>", info),
                Span::styled(" · ", muted),
                Span::styled("/help", info),
            ]),
        ],
    }
}

/// Draw the empty-state hero centered in `area`. Paints nothing outside the
/// given rect.
///
/// `user_logo` — when `Some` and non-empty, replaces the built-in wordmark.
/// The caller is responsible for having loaded + parsed it (clamped here as a
/// safety net regardless).
///
/// `guidance` selects the copy shown beneath the logo+gap (ADR-0057). It is
/// the app shell's policy decision — the view layer only renders what it is
/// given. `None` reproduces the classic single-line tagline.
pub(super) fn draw_empty_state(
    frame: &mut Frame,
    area: Rect,
    user_logo: Option<&[String]>,
    guidance: EmptyStateGuidance,
    theme: &Theme,
) {
    // If the user logo somehow slipped through un-clamped, clamp it here too so
    // rendering stays within bounds even if a caller bypassed `parse_logo`.
    let user_clamped: Option<Vec<String>> =
        user_logo.map(|lines| parse_logo(&lines.join("\n")).unwrap_or_default());
    let logo_refs: Vec<&str> = if let Some(ref clamped) = user_clamped {
        if !clamped.is_empty() {
            clamped.iter().map(String::as_str).collect()
        } else {
            BUILTIN_LOGO.to_vec()
        }
    } else {
        BUILTIN_LOGO.to_vec()
    };

    let logo_fg = theme.brand();
    let logo_style = Style::default().fg(logo_fg).add_modifier(Modifier::BOLD);

    let section = guidance_section(guidance, theme);

    let mut lines: Vec<Line> = Vec::with_capacity(logo_refs.len() + 2 + section.len());
    for row in &logo_refs {
        lines.push(Line::from(vec![Span::styled(*row, logo_style)]));
    }
    // Blank gap, then the guidance section (tagline + optional onboarding).
    lines.push(Line::raw(""));
    lines.extend(section);

    // Center vertically: push the whole block down by half the slack so it sits
    // roughly in the middle of the viewport rather than pinned to the top.
    let slack = area.height.saturating_sub(lines.len() as u16) / 2;
    let top = area.y + slack;

    let para = Paragraph::new(lines).alignment(Alignment::Center);
    frame.render_widget(
        para,
        Rect::new(area.x, top, area.width, area.height - slack),
    );
}

/// Height the empty state reports for `content_lines` accounting. Uses the
/// user logo's line count when supplied (clamped), else the built-in height,
/// plus the guidance section's line count.
pub(super) fn empty_state_content_lines(
    user_logo: Option<&[String]>,
    guidance: EmptyStateGuidance,
) -> usize {
    let refs = effective_logo(user_logo);
    empty_state_height(&refs, guidance)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_state_renders_builtin_without_panicking() {
        let mut terminal = neenee_tui_engine::TestTerminal::new(80, 24);
        let theme = Theme::default();
        terminal.draw(|f| {
            draw_empty_state(f, f.area(), None, EmptyStateGuidance::None, &theme);
        });
    }

    #[test]
    fn empty_state_renders_user_logo_without_panicking() {
        let mut terminal = neenee_tui_engine::TestTerminal::new(80, 24);
        let theme = Theme::default();
        let logo = vec!["  X X  ".to_string(), " X X X ".to_string()];
        terminal.draw(|f| {
            draw_empty_state(
                f,
                f.area(),
                Some(&logo),
                EmptyStateGuidance::Onboarding,
                &theme,
            );
        });
    }

    #[test]
    fn empty_state_renders_needs_provider_guidance_without_panicking() {
        let mut terminal = neenee_tui_engine::TestTerminal::new(80, 24);
        let theme = Theme::default();
        terminal.draw(|f| {
            draw_empty_state(f, f.area(), None, EmptyStateGuidance::NeedsProvider, &theme);
        });
    }

    #[test]
    fn parse_logo_truncates_wide_lines() {
        let wide = "a".repeat(200);
        let out = parse_logo(&wide).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].chars().count(), MAX_LOGO_COLS);
    }

    #[test]
    fn parse_logo_truncates_tall_blocks() {
        let tall = (0..MAX_LOGO_ROWS + 50)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let out = parse_logo(&tall).unwrap();
        assert_eq!(out.len(), MAX_LOGO_ROWS);
    }

    #[test]
    fn parse_logo_strips_crlf_and_trailing_blanks() {
        let raw = "\r\nhello\r\nworld\r\n\r\n";
        let out = parse_logo(raw).unwrap();
        assert_eq!(out, vec!["hello", "world"]);
    }

    #[test]
    fn parse_logo_returns_none_for_empty_input() {
        assert!(parse_logo("").is_none());
        assert!(parse_logo("\n\n\n").is_none());
        assert!(parse_logo("   \n  \n").is_none());
    }

    #[test]
    fn builtin_height_matches_logo_plus_gap() {
        let refs = effective_logo(None);
        assert_eq!(
            empty_state_height(&refs, EmptyStateGuidance::None),
            BUILTIN_LOGO.len() + 2 + 1
        );
    }

    #[test]
    fn content_lines_reflects_user_logo_size() {
        let user = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        // None guidance: 3 logo + 2 (blank gap) + 1 (tagline) = 6.
        assert_eq!(
            empty_state_content_lines(Some(&user), EmptyStateGuidance::None),
            6
        );
        assert_eq!(
            empty_state_content_lines(None, EmptyStateGuidance::None),
            BUILTIN_LOGO.len() + 2 + 1
        );
    }

    #[test]
    fn content_lines_reflects_guidance_variant() {
        // Same logo, three guidance tiers — height rises with extra copy.
        let base_logo = None::<&[String]>;
        let none = empty_state_content_lines(base_logo, EmptyStateGuidance::None);
        let onboarding = empty_state_content_lines(base_logo, EmptyStateGuidance::Onboarding);
        let needs = empty_state_content_lines(base_logo, EmptyStateGuidance::NeedsProvider);
        // None = tagline (1); Onboarding = tagline + hint (2); Needs = blocker
        // + action (2). Onboarding/Needs each add one line over None.
        assert_eq!(onboarding, none + 1, "Onboarding adds one line over None");
        assert_eq!(needs, none + 1, "NeedsProvider adds one line over None");
        assert_eq!(guidance_line_count(EmptyStateGuidance::None), 1);
        assert_eq!(guidance_line_count(EmptyStateGuidance::Onboarding), 2);
        assert_eq!(guidance_line_count(EmptyStateGuidance::NeedsProvider), 2);
    }
}
