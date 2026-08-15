//! Unified renderer for harness-level notices (errors, turn-pause signals,
//! status summaries).
//!
//! Replaces the ad-hoc `TranscriptMessage::new(Role::System, format!("Error: …"))`
//! pattern that left every notice indistinguishable from any other system
//! message and forced consumers to string-sniff `"Error:"` prefixes to recover
//! severity. The [`NoticeSeverity`] → color/icon mapping lives here as the
//! single source of truth, so adding a new severity (or retuning its color)
//! touches one match arm instead of scattered call sites.
//!
//! [`NoticeSeverity`]: crate::model::document::NoticeSeverity

use neenee_tui_engine::{Frame, Rect};

use crate::model::document::TranscriptMessage;

use super::Theme;
use super::components::notice::{NoticeView, draw_notice_view};

/// Render a notice message: a severity-colored glyph followed by the notice
/// text, wrapped to the transcript body width. Mirrors the row-accounting
/// contract of `draw_message_body` (`skip_rows` / `current_y` /
/// `content_lines`) so it drops into the same per-message render loop without
/// special-casing.
#[allow(clippy::too_many_arguments)]
pub fn draw_notice(
    frame: &mut Frame,
    area: Rect,
    msg: &TranscriptMessage,
    skip_rows: &mut usize,
    current_y: &mut u16,
    content_lines: &mut usize,
    theme: &Theme,
) {
    draw_notice_view(
        frame,
        area,
        NoticeView { message: msg },
        skip_rows,
        current_y,
        content_lines,
        theme,
    );
}
