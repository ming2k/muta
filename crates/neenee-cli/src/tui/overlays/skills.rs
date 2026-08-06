//! Skills modal — a centered, dismissable overlay listing every loaded skill.
//!
//! Opened via the `/skills` slash command (intercepted locally in `input.rs`,
//! never sent to the backend). Each row shows the skill name, a short hint, and
//! its enabled state. `Enter` toggles a detail expansion (full description,
//! version, source, tags); `r` reloads the skill registry via a slash command.
//! This replaces the Skills tab that previously lived inside the `/session`
//! modal, giving skills their own dedicated surface.

use neenee_tui_engine::{
    Frame, Style, {Line, Span},
};

use super::common::{placeholder, selectable_row};
use crate::tui::primitives::{
    FixedModalSpec, FooterHint, SCROLL_EDGE_MARGIN, keyvocab, modal_area, modal_frame,
    modal_header, render_body, render_modal_footer,
};
use crate::tui::view::Theme;

/// Draw the skills modal.
///
/// `session_context` provides `skills: Vec<SkillInfo>`. `modal_index` is the row
/// cursor; `expanded` is the index of the row whose detail block is shown (or
/// `None`). `scroll` is read AND written back so the caller's offset stays
/// consistent with the clamped body height.
pub fn draw_skills_modal(
    frame: &mut Frame,
    session_context: Option<&neenee_core::SessionContextSnapshot>,
    modal_index: usize,
    expanded: Option<usize>,
    scroll: &mut usize,
    theme: &Theme,
) -> neenee_tui_engine::Rect {
    let area = modal_area(frame, FixedModalSpec::SKILLS);
    let f = modal_frame(frame, area, theme.panel(), true, true);

    // ── Header ──
    modal_header(frame, f.header, "Skills", theme);

    // ── Body: the skill list with optional detail expansion ──
    let skills = session_context.map(|s| s.skills.as_slice()).unwrap_or(&[]);
    let body_w = f.body.width as usize;
    let mut body: Vec<Line> = Vec::new();

    if skills.is_empty() {
        body.push(placeholder(
            "No skills loaded.",
            session_context.is_some(),
            theme.muted(),
        ));
    } else {
        for (i, skill) in skills.iter().enumerate() {
            // The selectable row: name + description hint + enabled badge.
            body.push(selectable_row(
                i,
                modal_index,
                &skill.name,
                &skill.description,
                skill.enabled,
                "enabled",
                "disabled",
                body_w,
                theme,
            ));

            // Detail expansion for the selected row.
            if expanded == Some(i) {
                let detail_indent = "    ";
                let muted = Style::default().fg(theme.muted());
                let fg = Style::default().fg(theme.fg());

                // Full description (may be long — that's the point of the
                // detail view; the row hint is truncated, this is not).
                for line in skill.description.lines() {
                    body.push(Line::from(Span::styled(
                        format!("{}{}", detail_indent, line),
                        fg,
                    )));
                }

                // Metadata line: version + source + tags — same-rank peers
                // (R2), so they join with plain whitespace, no dot.
                let mut meta_parts: Vec<String> = Vec::new();
                if let Some(v) = &skill.version {
                    meta_parts.push(format!("v{}", v));
                }
                meta_parts.push(skill.source.clone());
                if !skill.tags.is_empty() {
                    meta_parts.push(format!("#{}", skill.tags.join(" #")));
                }
                body.push(Line::from(Span::styled(
                    format!(
                        "{}{}",
                        detail_indent,
                        meta_parts.join(&" ".repeat(crate::tui::design::JOIN_ENUMERATE_COLS))
                    ),
                    muted,
                )));

                body.push(Line::from(""));
            }
        }
    }

    let follow = if skills.is_empty() {
        None
    } else {
        Some(modal_index)
    };
    render_body(
        frame,
        f.body,
        body,
        scroll,
        follow,
        SCROLL_EDGE_MARGIN,
        false,
        theme,
    );

    // ── Footer ──
    if let Some(fo) = f.footer {
        let hints: &[FooterHint] = if skills.is_empty() {
            &[
                FooterHint::secondary("r", "reload"),
                FooterHint::always(keyvocab::ESC, "close"),
            ]
        } else {
            &[
                FooterHint::navigation(keyvocab::ARROWS_UD, "select"),
                FooterHint::primary(keyvocab::ENTER, "detail"),
                FooterHint::secondary("r", "reload"),
                FooterHint::always(keyvocab::ESC, "close"),
            ]
        };
        render_modal_footer(frame, fo, hints, theme);
    }

    // Return the panel rect so the event loop can register it as the
    // click-outside-to-dismiss target (the modal is in
    // `Modal::dismissable_by_outside_click`).
    area
}
