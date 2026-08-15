//! Transient notice bubbles: copy result and armed-action toasts.

use neenee_tui_engine::{Color, Frame};

use crate::components::toast::{ToastBubble, ToastKind};
use crate::model::document::NoticeSeverity;
use crate::view::Theme;

pub fn draw_armed_toast(frame: &mut Frame, message: &str, theme: &Theme) {
    ToastBubble {
        message,
        kind: ToastKind::Armed,
    }
    .render(frame, theme);
}

pub fn draw_copy_toast(frame: &mut Frame, message: &str, failed: bool, theme: &Theme) {
    ToastBubble {
        message,
        kind: if failed {
            ToastKind::CopyFailed
        } else {
            ToastKind::CopyOk
        },
    }
    .render(frame, theme);
}

/// Draw a toast-surfaced notice (a command acknowledgment such as
/// `/autopilot on`). The bubble's accent color follows the notice severity,
/// reusing the same severity→color map as the inline notice renderer so the
/// two stay visually consistent. Unlike the copy/armed toasts this is driven
/// by a `RoundEvent::Notice` forwarded across the listener→loop boundary.
pub fn draw_notice_toast(
    frame: &mut Frame,
    message: &str,
    severity: NoticeSeverity,
    theme: &Theme,
) {
    let color = match severity {
        NoticeSeverity::Error => theme.err(),
        NoticeSeverity::Warning => theme.warn(),
        NoticeSeverity::Info => theme.info(),
    };
    ToastBubble {
        message,
        kind: ToastKind::Custom(color),
    }
    .render(frame, theme);
}

#[allow(dead_code)]
pub fn toast(frame: &mut Frame, theme: &Theme, message: &str, color: Color, width: u16) {
    ToastBubble {
        message,
        kind: ToastKind::Custom(color),
    }
    .render_at_width(frame, theme, width);
}
