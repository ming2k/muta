//! Transient notice bubbles: copy result and armed-action toasts.

use neenee_tui_engine::{Color, Frame};

use crate::tui::components::toast::{ToastBubble, ToastKind};
use crate::tui::view::Theme;

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

#[allow(dead_code)]
pub fn toast(frame: &mut Frame, theme: &Theme, message: &str, color: Color, width: u16) {
    ToastBubble {
        message,
        kind: ToastKind::Custom(color),
    }
    .render_at_width(frame, theme, width);
}
