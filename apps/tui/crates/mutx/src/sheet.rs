//! AI-initiated interaction sheets (ADR-0173 §3): the inline surfaces that
//! replace the composer slot when the agent needs a decision from the user.
//!
//! A sheet is *not* a modal and *not* a surface: it never floats above the
//! session view, so it has no `Recess` policy, no outside-click dismissal,
//! and no centered geometry. It is a **sibling component of the composer**:
//! the view's bottom slot mounts either the draft editor or one sheet, the
//! sheet replacing the composer wholesale (same bottom edge, height computed
//! from its content), while the transcript behind it stays live and
//! scrollable. Slot state lives on `App::active_sheet` — the surface router
//! never sees it. The initiator taxonomy is the boundary: AI → user
//! interaction requests are sheets; user-invoked tools are modals;
//! user-invoked spaces are full-screen views.

use crate::modal::Claims;

/// The AI-initiated interaction sheets, in queue arrival order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SheetKind {
    /// Tool-permission approval sheet.
    Permission,
    /// `ask_user` question sheet.
    Question,
    /// Interactive-input injection sheet (L3.5 β).
    InputInjection,
}

pub(crate) use SheetKind::{InputInjection, Permission, Question};

impl SheetKind {
    /// Every sheet kind, for exhaustive policy assertions.
    pub const ALL: [SheetKind; 3] = [Permission, Question, InputInjection];

    /// The keyboard-ownership declaration for this sheet (ADR-0173 §2).
    ///
    /// The permission sheet is the one deliberate pass-through surface: it
    /// owns only its decision keys, and transcript navigation/scrolling fall
    /// through to the chat surface beneath so the history stays readable
    /// while a prompt is pending. The other sheets consume the keyboard
    /// (unhandled keys are inert, never passed down).
    pub fn keyboard_claims(self) -> Claims {
        match self {
            Permission => Claims {
                text_entry: false,
                list_nav: false,
                body_scroll: false,
                decide: true,
                opaque: false,
            },
            Question => Claims {
                text_entry: false,
                list_nav: true,
                body_scroll: true,
                decide: true,
                opaque: true,
            },
            // The injection sheet borrows the composer line itself: full
            // text entry, no scrollable body.
            InputInjection => Claims {
                text_entry: true,
                list_nav: false,
                body_scroll: false,
                decide: true,
                opaque: true,
            },
        }
    }

    /// Whether the sheet unconditionally renders its own text caret.
    /// The question sheet's "Other" field is state-dependent and resolved in
    /// `App::caret_owner` instead.
    pub fn owns_caret(self) -> bool {
        matches!(self, InputInjection)
    }
}

/// The sheets' self-owned key schemes (ADR-0173 §3): a key the sheet owns
/// resolves here; everything else falls through to the shared affordance
/// library and the sheet arms of the input router.
///
/// The question sheet's printable family: `space` toggles the highlighted
/// option, digits pick an option outright, every other character edits the
/// "Other" free-text field. The permission and input-injection sheets are
/// keyboard-driven through Enter/Esc/←/→ (decision cursor) and own no
/// printable verbs.
pub(crate) fn resolve_sheet_key(
    kind: SheetKind,
    key: crate::keymap::Key,
    ctx: &crate::input::InputContext,
) -> Option<crate::input::InputAction> {
    use crate::input::InputAction;

    if kind != Question {
        return None;
    }
    let c = match key.code {
        crossterm::event::KeyCode::Char(c)
            if !key.modifiers.intersects(
                crossterm::event::KeyModifiers::CONTROL
                    | crossterm::event::KeyModifiers::ALT
                    | crossterm::event::KeyModifiers::SUPER,
            ) =>
        {
            c
        }
        _ => return None,
    };
    if c == ' ' && !ctx.question_other_highlighted {
        Some(InputAction::QuestionToggle)
    } else if let Some(d) = c.to_digit(10)
        && (1..=9).contains(&d)
    {
        Some(InputAction::QuestionSelect(d as usize))
    } else {
        Some(InputAction::QuestionInsertChar(c))
    }
}
