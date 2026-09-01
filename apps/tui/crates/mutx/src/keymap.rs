//! Unified keybinding registry — the single source of truth for the app's
//! global keyboard shortcuts.
//!
//! Every global shortcut (a key that works from the top level, i.e. while no
//! modal owns the surface) is declared once here as a [`Binding`]. The input
//! handler resolves global keys by consulting this table, and the Help modal
//! renders its keybindings list from the same table, so the two can never
//! drift apart. Adding a global shortcut means adding one entry: it shows up
//! in Help automatically.
//!
//! ## Two canonical labels per key
//!
//! Every key has **two** display strings, both derived from one token table
//! so they can never drift:
//!
//! - [`Key::chord`] — the compact lowercase form (`ctrl+t`, `enter`, `esc`,
//!   `↑`). Used by the Help modal's prose rows.
//! - [`Key::display`] — the capitalized human form (`Ctrl+T`, `Enter`, `Esc`,
//!   `↑`). Used by footer hint strips, the activity-bar interrupt hint, and
//!   in-modal legends — i.e. every place a keycap is rendered standalone.
//!
//! Footers and legends never type a key glyph inline: a single physical key is
//! spelled `Key::display()`, and a repeated affordance (arrow pair, Space,
//! Shift+Tab, …) is a named constant in [`keyvocab`]. That makes the whole UI
//! share one key vocabulary.
//!
//! ## Scope and limits
//!
//! The registry covers **global** bindings only — keys whose meaning is fixed
//! from the top level (open a modal, copy, quit, …). It deliberately does
//! *not* cover:
//!
//! - **Text-editing** keys (`Ctrl+A/E/U/K/W`, `Alt+B/F/D`, arrows, Backspace,
//!   …). These depend on whether a text field is focused and on the readline
//!   family, so they stay as explicit arms in `input::process_event`.
//! - **Contextual / modal-internal** keys (modal ↑/↓ selection, `Space` toggles
//!   inside Tools/MCP/Config, `/` entering a search sub-layer, `e`/`*`/`D`
//!   inside the provider picker, Esc's modal-specific hierarchy, …). These are
//!   tied to the active modal and its sub-layer state, so they stay inline.
//! - **Enter / Tab**: their behavior is polymorphic across every surface, so
//!   they stay hand-routed.
//!
//! ## Gating
//!
//! Every binding carries an optional [`Gate`] — the precondition under which
//! the key is active. Most global shortcuts require [`Gate::NoModal`] (so they
//! don't fire while a modal is open); a few (copy) work everywhere. The
//! resolver applies the gate before returning an action, so the binding
//! declarations read declaratively.
//!
//! ## Adding a binding
//!
//! 1. Add a variant to [`Action`] (or reuse an existing one) and dispatch it
//!    in the event loop.
//! 2. Push a [`Binding`] into [`GLOBAL_BINDINGS`] with its [`Key`], gate, the
//!    [`Action`] it maps to, and a short human description (shown in Help).
//!
//! The Help modal and the resolver pick it up with no further wiring.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::Modal;
use super::input::InputAction;

// ─────────────────────────────────────────────────────────────────────────────
// Canonical key display vocabulary.
//
// A physical key has **two** canonical strings in this app, and both are
// derived from the one place below so they can never drift:
//
// - [`Key::chord`] — the lowercase machine/compact form (`ctrl+t`, `enter`,
//   `esc`, `↑`). Used by the Help prose rows and anywhere space is tight.
// - [`Key::display`] — the capitalized human form (`Ctrl+T`, `Enter`, `Esc`,
//   `↑`). Used by the footer hint strips, the activity-bar interrupt hint, and
//   any in-modal legend.
//
// Both come out of the same token table ([`chord_token`] / [`display_token`]),
// so the only way to add a key to the UI is to spell it once here; every footer
// literal and every help row then agrees on the same glyph and the same case.
//
// A few legend tokens are not single physical keys but affordances shown as a
// keycap (`↑↓` for "arrow keys", `←→`, `Space`, …). Those have no
// `KeyCode`, so they live as named constants in [`keyvocab`] and the footer /
// legend call sites reference them by name instead of hand-typing the glyph.
// ─────────────────────────────────────────────────────────────────────────────

/// Repeated legend tokens — glyph strings that stand for an affordance rather
/// than a single physical key (`↑↓` = "the arrow keys", `←→`, `Space`,
/// `⇧Tab`, `Enter/Space`, …).
///
/// Footers and legends reference these `const`s instead of typing the glyph
/// inline, so the spelling and width of a repeated affordance is owned in one
/// place. New affordances go here; single physical keys are spelled via
/// [`Key::display`] (e.g. `Key::ESC.display()`).
pub mod keyvocab {
    /// Vertical arrow pair: "the up/down keys". The single most-repeated
    /// footer token in the app.
    pub const ARROWS_UD: &str = "↑↓";
    /// Horizontal arrow pair: "the left/right keys".
    pub const ARROWS_LR: &str = "←→";
    /// Up arrow alone (Help prose / single-direction affordances).
    #[allow(dead_code)]
    pub const UP: &str = "↑";
    /// Down arrow alone.
    #[allow(dead_code)]
    pub const DOWN: &str = "↓";
    /// Space bar, shown as the word `Space` (matches the capitalization of
    /// `Enter`/`Esc`/`Tab`).
    pub const SPACE: &str = "Space";
    /// Shift+Tab, shown with the shift sign so it reads at a glance.
    pub const SHIFT_TAB: &str = "⇧Tab";
    /// The `Enter` key with a leading shift sign — used where the affordance is
    /// "Shift+Enter" but the glyph must stay compact. Kept so the family is
    /// exhaustive; remove if it stays dead.
    #[allow(dead_code)]
    pub const SHIFT_ENTER: &str = "⇧Enter";
}

/// The compact token for a core [`KeyCode`] — the lowercase `enter` / `esc` /
/// `↑` fragment used inside a chord, before any modifier prefix.
pub const fn chord_token(code: KeyCode) -> &'static str {
    match code {
        KeyCode::Char(c) => match c.to_ascii_lowercase() {
            'a' => "a",
            'b' => "b",
            'c' => "c",
            'd' => "d",
            'e' => "e",
            'f' => "f",
            'g' => "g",
            'h' => "h",
            'i' => "i",
            'j' => "j",
            'k' => "k",
            'l' => "l",
            'm' => "m",
            'n' => "n",
            'o' => "o",
            'p' => "p",
            'q' => "q",
            'r' => "r",
            's' => "s",
            't' => "t",
            'u' => "u",
            'v' => "v",
            'w' => "w",
            'x' => "x",
            'y' => "y",
            'z' => "z",
            '0' => "0",
            '1' => "1",
            '2' => "2",
            '3' => "3",
            '4' => "4",
            '5' => "5",
            '6' => "6",
            '7' => "7",
            '8' => "8",
            '9' => "9",
            '?' => "?",
            '/' => "/",
            _ => "·",
        },
        KeyCode::Enter => "enter",
        KeyCode::Tab => "tab",
        KeyCode::BackTab => "shift+tab",
        KeyCode::Backspace => "backspace",
        KeyCode::Esc => "esc",
        KeyCode::Up => "↑",
        KeyCode::Down => "↓",
        KeyCode::Left => "←",
        KeyCode::Right => "→",
        KeyCode::Home => "home",
        KeyCode::End => "end",
        KeyCode::PageUp => "pgup",
        KeyCode::PageDown => "pgdn",
        KeyCode::F(1) => "f1",
        KeyCode::F(2) => "f2",
        KeyCode::F(3) => "f3",
        KeyCode::F(4) => "f4",
        KeyCode::F(5) => "f5",
        _ => "·",
    }
}

/// The display token for a core [`KeyCode`] — the capitalized `Enter` / `Esc` /
/// `↑` fragment a footer or legend shows, before any modifier prefix.
pub const fn display_token(code: KeyCode) -> &'static str {
    match code {
        KeyCode::Char(c) => match c.to_ascii_lowercase() {
            'a' => "A",
            'b' => "B",
            'c' => "C",
            'd' => "D",
            'e' => "E",
            'f' => "F",
            'g' => "G",
            'h' => "H",
            'i' => "I",
            'j' => "J",
            'k' => "K",
            'l' => "L",
            'm' => "M",
            'n' => "N",
            'o' => "O",
            'p' => "P",
            'q' => "Q",
            'r' => "R",
            's' => "S",
            't' => "T",
            'u' => "U",
            'v' => "V",
            'w' => "W",
            'x' => "X",
            'y' => "Y",
            'z' => "Z",
            '0' => "0",
            '1' => "1",
            '2' => "2",
            '3' => "3",
            '4' => "4",
            '5' => "5",
            '6' => "6",
            '7' => "7",
            '8' => "8",
            '9' => "9",
            '?' => "?",
            '/' => "/",
            _ => "·",
        },
        KeyCode::Enter => "Enter",
        KeyCode::Tab => "Tab",
        KeyCode::BackTab => "Shift+Tab",
        KeyCode::Backspace => "Backspace",
        KeyCode::Esc => "Esc",
        KeyCode::Up => "↑",
        KeyCode::Down => "↓",
        KeyCode::Left => "←",
        KeyCode::Right => "→",
        KeyCode::Home => "Home",
        KeyCode::End => "End",
        KeyCode::PageUp => "PgUp",
        KeyCode::PageDown => "PgDn",
        KeyCode::F(1) => "F1",
        KeyCode::F(2) => "F2",
        KeyCode::F(3) => "F3",
        KeyCode::F(4) => "F4",
        KeyCode::F(5) => "F5",
        _ => "·",
    }
}

/// The lowercase modifier prefix for a chord (`ctrl+`, `alt+`, …), or `""` for none.
pub const fn chord_prefix(modifiers: KeyModifiers) -> &'static str {
    if modifiers.bits() == (KeyModifiers::CONTROL.bits() | KeyModifiers::SHIFT.bits()) {
        "ctrl+shift+"
    } else if modifiers.bits() == KeyModifiers::CONTROL.bits() {
        "ctrl+"
    } else if modifiers.bits() == KeyModifiers::ALT.bits() {
        "alt+"
    } else if modifiers.bits() == KeyModifiers::SHIFT.bits() {
        "shift+"
    } else if modifiers.bits() == KeyModifiers::SUPER.bits() {
        "cmd+"
    } else {
        ""
    }
}

/// The display-case modifier prefix for a key (`Ctrl+`, `Alt+`, …), or `""` for none.
pub const fn display_prefix(modifiers: KeyModifiers) -> &'static str {
    if modifiers.bits() == (KeyModifiers::CONTROL.bits() | KeyModifiers::SHIFT.bits()) {
        "Ctrl+Shift+"
    } else if modifiers.bits() == KeyModifiers::CONTROL.bits() {
        "Ctrl+"
    } else if modifiers.bits() == KeyModifiers::ALT.bits() {
        "Alt+"
    } else if modifiers.bits() == KeyModifiers::SHIFT.bits() {
        "Shift+"
    } else if modifiers.bits() == KeyModifiers::SUPER.bits() {
        "Cmd+"
    } else {
        ""
    }
}

/// A physical key combination, independent of crossterm's `KeyEvent` envelope.
///
/// `Ctrl`/`Alt`/`Shift` modifiers are tracked explicitly so the registry (and
/// the Help text) can describe bindings in the same `ctrl+t` / `alt+enter`
/// notation users see in the UI. Shift alone does not count as a modifier for
/// letter keys (a shifted letter is just a different `char`), so a `Key`
/// built from a `KeyEvent` normalizes bare `Shift` away to keep `C-t` and
/// `Ctrl+Shift+T` (the same letter intent) on one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub modifiers: KeyModifiers,
    pub code: KeyCode,
}

impl Key {
    /// Build a [`Key`] from a raw crossterm event, normalizing a bare
    /// `Shift` on a letter key away (see the type docs for why).
    pub fn from_event(event: KeyEvent) -> Self {
        let mut modifiers = event.modifiers;
        if let KeyCode::Char(ch) = event.code
            && ch.is_ascii_alphabetic()
            && modifiers == KeyModifiers::SHIFT
        {
            modifiers = KeyModifiers::NONE;
        }
        Self {
            modifiers,
            code: event.code,
        }
    }

    /// Construct a bare character key with no modifiers.
    pub const fn from_char(ch: char) -> Self {
        Self {
            modifiers: KeyModifiers::NONE,
            code: KeyCode::Char(ch),
        }
    }

    /// Construct a Ctrl+key combination.
    pub const fn ctrl(ch: char) -> Self {
        Self {
            modifiers: KeyModifiers::CONTROL,
            code: KeyCode::Char(ch),
        }
    }

    /// Construct an Alt+key combination.
    pub const fn alt(ch: char) -> Self {
        Self {
            modifiers: KeyModifiers::ALT,
            code: KeyCode::Char(ch),
        }
    }

    /// The canonical lowercase chord for this key, in the `ctrl+t` /
    /// `alt+enter` / `f1` / `esc` / `↑` notation used by the Help modal's prose
    /// rows. Joined keys use `+`, matching the established Help copy.
    ///
    /// This is the compact/machine form; the capitalized human form a footer
    /// renders is [`Key::display`]. Both come from the same token table, so a
    /// key is spelled exactly once for the whole app.
    ///
    /// Returns `&'static str` because every declared binding is a fixed,
    /// compile-time-known combination, so the label can be stored alongside
    /// the `&'static str` description in the Help rows without allocation.
    pub const fn chord(self) -> &'static str {
        if matches!(self.code, KeyCode::BackTab) {
            return "shift+tab";
        }
        let ctrl = self.modifiers.bits() & KeyModifiers::CONTROL.bits() != 0;
        let shift = self.modifiers.bits() & KeyModifiers::SHIFT.bits() != 0;
        let alt = self.modifiers.bits() & KeyModifiers::ALT.bits() != 0;
        let cmd = self.modifiers.bits() & KeyModifiers::SUPER.bits() != 0;

        if ctrl && shift {
            match self.code {
                KeyCode::Char('c') | KeyCode::Char('C') => "ctrl+shift+c",
                _ => "·",
            }
        } else if ctrl {
            match self.code {
                KeyCode::Char('a') | KeyCode::Char('A') => "ctrl+a",
                KeyCode::Char('b') | KeyCode::Char('B') => "ctrl+b",
                KeyCode::Char('c') | KeyCode::Char('C') => "ctrl+c",
                KeyCode::Char('d') | KeyCode::Char('D') => "ctrl+d",
                KeyCode::Char('e') | KeyCode::Char('E') => "ctrl+e",
                KeyCode::Char('f') | KeyCode::Char('F') => "ctrl+f",
                KeyCode::Char('g') | KeyCode::Char('G') => "ctrl+g",
                KeyCode::Char('h') | KeyCode::Char('H') => "ctrl+h",
                KeyCode::Char('i') | KeyCode::Char('I') => "ctrl+i",
                KeyCode::Char('j') | KeyCode::Char('J') => "ctrl+j",
                KeyCode::Char('k') | KeyCode::Char('K') => "ctrl+k",
                KeyCode::Char('l') | KeyCode::Char('L') => "ctrl+l",
                KeyCode::Char('m') | KeyCode::Char('M') => "ctrl+m",
                KeyCode::Char('n') | KeyCode::Char('N') => "ctrl+n",
                KeyCode::Char('o') | KeyCode::Char('O') => "ctrl+o",
                KeyCode::Char('p') | KeyCode::Char('P') => "ctrl+p",
                KeyCode::Char('q') | KeyCode::Char('Q') => "ctrl+q",
                KeyCode::Char('r') | KeyCode::Char('R') => "ctrl+r",
                KeyCode::Char('s') | KeyCode::Char('S') => "ctrl+s",
                KeyCode::Char('t') | KeyCode::Char('T') => "ctrl+t",
                KeyCode::Char('u') | KeyCode::Char('U') => "ctrl+u",
                KeyCode::Char('v') | KeyCode::Char('V') => "ctrl+v",
                KeyCode::Char('w') | KeyCode::Char('W') => "ctrl+w",
                KeyCode::Char('x') | KeyCode::Char('X') => "ctrl+x",
                KeyCode::Char('y') | KeyCode::Char('Y') => "ctrl+y",
                KeyCode::Char('z') | KeyCode::Char('Z') => "ctrl+z",
                KeyCode::Left => "ctrl+←",
                KeyCode::Right => "ctrl+→",
                KeyCode::Up => "ctrl+↑",
                KeyCode::Down => "ctrl+↓",
                _ => "·",
            }
        } else if alt {
            match self.code {
                KeyCode::Enter => "alt+enter",
                KeyCode::Char('b') | KeyCode::Char('B') => "alt+b",
                KeyCode::Char('d') | KeyCode::Char('D') => "alt+d",
                KeyCode::Char('f') | KeyCode::Char('F') => "alt+f",
                KeyCode::Char('o') | KeyCode::Char('O') => "alt+o",
                KeyCode::Char('p') | KeyCode::Char('P') => "alt+p",
                KeyCode::Char('n') | KeyCode::Char('N') => "alt+n",
                KeyCode::Up => "alt+↑",
                KeyCode::Down => "alt+↓",
                KeyCode::Backspace => "alt+backspace",
                _ => "·",
            }
        } else if shift {
            match self.code {
                KeyCode::Tab => "shift+tab",
                _ => chord_token(self.code),
            }
        } else if cmd {
            match self.code {
                KeyCode::Char('c') | KeyCode::Char('C') => "cmd+c",
                _ => "·",
            }
        } else {
            chord_token(self.code)
        }
    }

    /// The canonical capitalized display name for this key — `Enter`, `Esc`,
    /// `Tab`, `Ctrl+T`, `↑` — i.e. the form a footer hint strip or an
    /// activity-bar interrupt hint renders as a standalone keycap.
    ///
    /// This is the human form; the compact lowercase form is [`Key::chord`].
    /// Footers and legends call this (or a [`keyvocab`] affordance constant)
    /// instead of typing the glyph, so every surface agrees on case and glyph.
    ///
    /// Returns `&'static str` for the same allocation-free reason as
    /// [`Key::chord`].
    pub const fn display(self) -> &'static str {
        if matches!(self.code, KeyCode::BackTab) {
            return keyvocab::SHIFT_TAB;
        }
        let ctrl = self.modifiers.bits() & KeyModifiers::CONTROL.bits() != 0;
        let shift = self.modifiers.bits() & KeyModifiers::SHIFT.bits() != 0;
        let alt = self.modifiers.bits() & KeyModifiers::ALT.bits() != 0;
        let cmd = self.modifiers.bits() & KeyModifiers::SUPER.bits() != 0;

        if ctrl && shift {
            match self.code {
                KeyCode::Char('c') | KeyCode::Char('C') => "Ctrl+Shift+C",
                _ => "·",
            }
        } else if ctrl {
            match self.code {
                KeyCode::Char('a') | KeyCode::Char('A') => "Ctrl+A",
                KeyCode::Char('b') | KeyCode::Char('B') => "Ctrl+B",
                KeyCode::Char('c') | KeyCode::Char('C') => "Ctrl+C",
                KeyCode::Char('d') | KeyCode::Char('D') => "Ctrl+D",
                KeyCode::Char('e') | KeyCode::Char('E') => "Ctrl+E",
                KeyCode::Char('f') | KeyCode::Char('F') => "Ctrl+F",
                KeyCode::Char('g') | KeyCode::Char('G') => "Ctrl+G",
                KeyCode::Char('h') | KeyCode::Char('H') => "Ctrl+H",
                KeyCode::Char('i') | KeyCode::Char('I') => "Ctrl+I",
                KeyCode::Char('j') | KeyCode::Char('J') => "Ctrl+J",
                KeyCode::Char('k') | KeyCode::Char('K') => "Ctrl+K",
                KeyCode::Char('l') | KeyCode::Char('L') => "Ctrl+L",
                KeyCode::Char('m') | KeyCode::Char('M') => "Ctrl+M",
                KeyCode::Char('n') | KeyCode::Char('N') => "Ctrl+N",
                KeyCode::Char('o') | KeyCode::Char('O') => "Ctrl+O",
                KeyCode::Char('p') | KeyCode::Char('P') => "Ctrl+P",
                KeyCode::Char('q') | KeyCode::Char('Q') => "Ctrl+Q",
                KeyCode::Char('r') | KeyCode::Char('R') => "Ctrl+R",
                KeyCode::Char('s') | KeyCode::Char('S') => "Ctrl+S",
                KeyCode::Char('t') | KeyCode::Char('T') => "Ctrl+T",
                KeyCode::Char('u') | KeyCode::Char('U') => "Ctrl+U",
                KeyCode::Char('v') | KeyCode::Char('V') => "Ctrl+V",
                KeyCode::Char('w') | KeyCode::Char('W') => "Ctrl+W",
                KeyCode::Char('x') | KeyCode::Char('X') => "Ctrl+X",
                KeyCode::Char('y') | KeyCode::Char('Y') => "Ctrl+Y",
                KeyCode::Char('z') | KeyCode::Char('Z') => "Ctrl+Z",
                KeyCode::Left => "Ctrl+←",
                KeyCode::Right => "Ctrl+→",
                KeyCode::Up => "Ctrl+↑",
                KeyCode::Down => "Ctrl+↓",
                _ => "·",
            }
        } else if alt {
            match self.code {
                KeyCode::Enter => "Alt+Enter",
                KeyCode::Char('b') | KeyCode::Char('B') => "Alt+B",
                KeyCode::Char('d') | KeyCode::Char('D') => "Alt+D",
                KeyCode::Char('f') | KeyCode::Char('F') => "Alt+F",
                KeyCode::Char('o') | KeyCode::Char('O') => "Alt+O",
                KeyCode::Char('p') | KeyCode::Char('P') => "Alt+P",
                KeyCode::Char('n') | KeyCode::Char('N') => "Alt+N",
                KeyCode::Up => "Alt+↑",
                KeyCode::Down => "Alt+↓",
                KeyCode::Backspace => "Alt+Backspace",
                _ => "·",
            }
        } else if shift {
            match self.code {
                KeyCode::Tab => keyvocab::SHIFT_TAB,
                _ => display_token(self.code),
            }
        } else if cmd {
            match self.code {
                KeyCode::Char('c') | KeyCode::Char('C') => "Cmd+C",
                _ => "·",
            }
        } else {
            display_token(self.code)
        }
    }
}

impl Key {
    // Frequently-displayed single keys, as `const` values so call sites read as
    // `Key::ESC.display()` instead of rebuilding a `Key` literal each time.
    // These are the keys whose display name recurs across footers and legends.

    /// The Escape key.
    pub const ESC: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Esc,
    };
    /// The Enter / Return key.
    pub const ENTER: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Enter,
    };
    /// The Tab key.
    pub const TAB: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Tab,
    };
    /// The Backspace key.
    pub const BACKSPACE: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Backspace,
    };
    /// The Up arrow key.
    pub const UP: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Up,
    };
    /// The Down arrow key.
    pub const DOWN: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Down,
    };
    /// The Left arrow key.
    pub const LEFT: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Left,
    };
    /// The Right arrow key.
    pub const RIGHT: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Right,
    };
    /// F1.
    pub const F1: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::F(1),
    };
    /// F2.
    pub const F2: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::F(2),
    };
    /// F3.
    pub const F3: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::F(3),
    };
    /// F4.
    pub const F4: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::F(4),
    };
    /// F5.
    pub const F5: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::F(5),
    };
    /// Page Up.
    pub const PAGE_UP: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::PageUp,
    };
    /// Page Down.
    pub const PAGE_DOWN: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::PageDown,
    };
    /// Alt+Up.
    pub const ALT_UP: Key = Key {
        modifiers: KeyModifiers::ALT,
        code: KeyCode::Up,
    };
    /// Alt+Down.
    pub const ALT_DOWN: Key = Key {
        modifiers: KeyModifiers::ALT,
        code: KeyCode::Down,
    };
    /// Alt+O.
    pub const ALT_O: Key = Key {
        modifiers: KeyModifiers::ALT,
        code: KeyCode::Char('o'),
    };
    /// Ctrl+A.
    pub const CTRL_A: Key = Key::ctrl('a');
    /// Ctrl+B.
    pub const CTRL_B: Key = Key::ctrl('b');
    /// Ctrl+C.
    pub const CTRL_C: Key = Key::ctrl('c');
    /// Ctrl+D.
    pub const CTRL_D: Key = Key::ctrl('d');
    /// Ctrl+E.
    pub const CTRL_E: Key = Key::ctrl('e');
    /// Ctrl+F.
    pub const CTRL_F: Key = Key::ctrl('f');
    /// Ctrl+G.
    pub const CTRL_G: Key = Key::ctrl('g');
    /// Ctrl+H.
    pub const CTRL_H: Key = Key::ctrl('h');
    /// Ctrl+K.
    pub const CTRL_K: Key = Key::ctrl('k');
    /// Ctrl+M.
    pub const CTRL_M: Key = Key::ctrl('m');
    /// Ctrl+N.
    pub const CTRL_N: Key = Key::ctrl('n');
    /// Ctrl+O.
    pub const CTRL_O: Key = Key::ctrl('o');
    /// Ctrl+P.
    pub const CTRL_P: Key = Key::ctrl('p');
    /// Ctrl+Q.
    pub const CTRL_Q: Key = Key::ctrl('q');
    /// Ctrl+R.
    pub const CTRL_R: Key = Key::ctrl('r');
    /// Ctrl+S.
    pub const CTRL_S: Key = Key::ctrl('s');
    /// Ctrl+T.
    pub const CTRL_T: Key = Key::ctrl('t');
    /// Ctrl+U.
    pub const CTRL_U: Key = Key::ctrl('u');
    /// Ctrl+W.
    pub const CTRL_W: Key = Key::ctrl('w');
    /// Ctrl+X.
    pub const CTRL_X: Key = Key::ctrl('x');
    /// Ctrl+Y.
    pub const CTRL_Y: Key = Key::ctrl('y');
    /// Ctrl+Z.
    pub const CTRL_Z: Key = Key::ctrl('z');
}

/// The precondition under which a binding is active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// Active in every context, including while a modal is open.
    Always,
    /// Active only from the top level — no modal owns the surface.
    NoModal,
}

/// One declared global shortcut: a [`Key`] that maps to an [`Action`] while
/// its [`Gate`] holds, with a short description for the Help modal.
#[derive(Debug, Clone, Copy)]
pub struct Binding {
    pub key: Key,
    pub gate: Gate,
    pub action: Action,
    pub description: &'static str,
}

/// The semantic global action a binding resolves to. Kept separate from
/// [`InputAction`] so the registry can be enumerated for Help without owning
/// the full (large, contextual) action enum; the resolver is the only place
/// that bridges [`Action`] → [`InputAction`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Open the help / keybindings modal.
    OpenHelp,
    /// Open the input-history recall modal.
    OpenHistory,
    /// Open the flat model picker.
    OpenModels,
    /// Open the active connection detail modal (`Ctrl+N`).
    OpenConnectionDetail,
    /// Open the Todos modal.
    OpenTodos,
    /// Open the queue overview modal.
    OpenQueue,
    /// Open the session telemetry modal (context tokens & performance) (`Ctrl+O`).
    OpenTelemetry,
    /// Open the `/btw` asides list modal (ADR-0103 §5): live background
    /// asides, jump back in, or close one outright.
    OpenBtwList,
    /// Open the global view quick switcher (ADR-0133, `Ctrl+L`).
    OpenViewSwitcher,
    /// Toggle the user block on the viewed session's outbox. While blocked, no
    /// queued message auto-drains (not even after the round completes).
    ToggleQueueBlock,
    /// Copy the current selection (or clear input / arm quit — resolved by the
    /// app loop).
    CopyOrClear,
    /// Copy the selection unconditionally (Ctrl+Shift+C / Cmd+C).
    CopySelection,
}

/// The registry of global bindings, in Help display order.
///
/// Wraps a shared lazy-initialized binding list so resolving a key never
/// allocates (the list is built once, on first use). That list is the
/// **single source of truth** for the app's global shortcuts; both the input
/// resolver and the Help modal read from it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Registry;

/// The canonical global bindings, built once on first access. Declared so the
/// Help modal (via a data bridge) and the input resolver share one list — see
/// [`Registry`]. `LazyLock` because `KeyModifiers` is a `bitflags` type whose
/// `bitor` is not `const`, so the list cannot live in a plain `static`.
pub static GLOBAL_BINDINGS: std::sync::LazyLock<Vec<Binding>> = std::sync::LazyLock::new(|| {
    // Order here is the order rows appear in the Help modal. Keep the most
    // discoverable / general shortcuts first.
    vec![
        Binding {
            key: Key {
                modifiers: KeyModifiers::CONTROL,
                code: KeyCode::Char('t'),
            },
            gate: Gate::NoModal,
            action: Action::OpenTodos,
            description: "open todos",
        },
        // Queue management lives on the Ctrl row (ADR-0126), replacing the
        // old F2/F3 bindings. Fn-dispatch is OS/terminal policy, not app
        // policy: terminals, window managers, and browser embedders may
        // reserve or remap those keys without the application seeing them.
        // Ctrl chords are distinct bytes on every terminal (raw mode keeps
        // ISIG/IXON off), survive tmux/screen, and sit one row from the
        // Enter the same gesture ends with. Mnemonics: Q = queue list and
        // P = pause the queue.
        Binding {
            key: Key {
                modifiers: KeyModifiers::CONTROL,
                code: KeyCode::Char('q'),
            },
            gate: Gate::NoModal,
            action: Action::OpenQueue,
            description: "open queue (outbox)",
        },
        // F5 opens the `/btw` asides list (ADR-0103). A function key rather
        // than a Ctrl combo: Ctrl+G is byte-collided with readline's
        // abort-to-start-of-line in terminals without the Kitty protocol.
        // (The queue family moved off the F-row to Ctrl+P/Q — ADR-0126 —
        // but this list surface keeps F5, a rarer, less time-sensitive
        // affordance with no clean free Ctrl slot.)
        Binding {
            key: Key {
                modifiers: KeyModifiers::NONE,
                code: KeyCode::F(5),
            },
            gate: Gate::NoModal,
            action: Action::OpenBtwList,
            description: "open /btw asides",
        },
        Binding {
            key: Key {
                modifiers: KeyModifiers::CONTROL,
                code: KeyCode::Char('p'),
            },
            gate: Gate::NoModal,
            action: Action::ToggleQueueBlock,
            description: "block/resume queue",
        },
        // Ctrl+M opens the model picker. Portability caveat, mirrored in the
        // description below: in a raw terminal Ctrl+M is byte-identical to
        // Enter (0x0D), so this binding only fires under the Kitty enhanced
        // keyboard protocol (requested in `run_tui`). `/models` is the
        // portable path — a slash command always arrives as text — so the
        // chord is a convenience on modern terminals, not the only door.
        Binding {
            key: Key {
                modifiers: KeyModifiers::CONTROL,
                code: KeyCode::Char('m'),
            },
            gate: Gate::NoModal,
            action: Action::OpenModels,
            description: "switch model (kitty-protocol chord; /models always works)",
        },
        Binding {
            key: Key {
                modifiers: KeyModifiers::CONTROL,
                code: KeyCode::Char('n'),
            },
            gate: Gate::NoModal,
            action: Action::OpenConnectionDetail,
            description: "active connection detail",
        },
        Binding {
            key: Key {
                modifiers: KeyModifiers::CONTROL,
                code: KeyCode::Char('r'),
            },
            gate: Gate::NoModal,
            action: Action::OpenHistory,
            description: "search history",
        },
        // Ctrl+L opens the global view quick switcher (ADR-0133). `Gate::Always`
        // — the whole point of a switcher is that it works *over* whatever
        // surface is up (matching tmux's window switcher, the direct prior
        // art), so it must not be NoModal-gated like the open-* bindings. A
        // Ctrl chord rather than the F-row for the same portability reasons
        // the queue family recorded (ADR-0126): Ctrl bytes are distinct in
        // raw mode, survive tmux/screen, and sit one row from Enter. Ctrl+L
        // is free — not in the readline family the composer uses
        // (Ctrl+A/E/W/U/K, Alt+B/F/D) and unclaimed by any global binding
        // (Ctrl+G was rejected for colliding with readline's abort;
        // Ctrl+Tab/Ctrl+Space are terminal/IME territory).
        Binding {
            key: Key {
                modifiers: KeyModifiers::CONTROL,
                code: KeyCode::Char('l'),
            },
            gate: Gate::Always,
            action: Action::OpenViewSwitcher,
            description: "switch view",
        },
        // Ctrl+O opens the context/token report — the keyboard twin of
        // clicking the model bar's context meter (progressive disclosure:
        // the glanceable `89.2k (8%)` gauge hides the full breakdown until
        // Ctrl+O opens the unified session telemetry report — the keyboard twin of clicking
        // the context or stream rate gauges in the model bar.
        Binding {
            key: Key {
                modifiers: KeyModifiers::CONTROL,
                code: KeyCode::Char('o'),
            },
            gate: Gate::NoModal,
            action: Action::OpenTelemetry,
            description: "session telemetry report",
        },
        // `?` / `f1` / `ctrl+h` all open help, but they are context-sensitive (`?`
        // only fires on an empty prompt) and `ctrl+h` needs the Kitty protocol —
        // so they stay hand-routed in the input handler and are *documented* in
        // Help via the legacy fallback rows. Only the portable `f1` is declared
        // here, so the registry owns at least one canonical help binding.
        Binding {
            key: Key {
                modifiers: KeyModifiers::NONE,
                code: KeyCode::F(1),
            },
            gate: Gate::NoModal,
            action: Action::OpenHelp,
            description: "this help (? / ctrl+h)",
        },
        // The copy family. Plain Ctrl+C is the semantic copy/clear/quit (resolved
        // by the app loop); Ctrl+Shift+C and Cmd+C copy the selection outright.
        // All three are Always-on so they work over any modal.
        Binding {
            key: Key {
                modifiers: KeyModifiers::CONTROL,
                code: KeyCode::Char('c'),
            },
            gate: Gate::Always,
            action: Action::CopyOrClear,
            description: "copy  clear input  quit (×2)",
        },
        Binding {
            key: Key {
                modifiers: KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                code: KeyCode::Char('c'),
            },
            gate: Gate::Always,
            action: Action::CopySelection,
            description: "copy selection",
        },
        Binding {
            key: Key {
                modifiers: KeyModifiers::SUPER,
                code: KeyCode::Char('c'),
            },
            gate: Gate::Always,
            action: Action::CopySelection,
            description: "copy selection (cmd+c)",
        },
    ]
});

impl Registry {
    /// Construct the registry. Cheap — the binding list is shared and built
    /// once on first access (see [`GLOBAL_BINDINGS`]).
    pub const fn new() -> Self {
        Self
    }

    /// Iterate the declared bindings in Help display order.
    pub fn bindings(&self) -> &'static [Binding] {
        &GLOBAL_BINDINGS
    }

    /// Resolve a key event to a global [`InputAction`], applying the binding's
    /// gate against the current `active_modal`. Returns `None` when the key is
    /// not a declared global shortcut (so the caller falls through to its
    /// contextual/text-editing handling).
    ///
    /// This is the bridge the input handler calls *before* its contextual
    /// match arms. Global bindings win over contextual ones by construction:
    /// every declared key here is one we want to mean the same thing from the
    /// top level regardless of input-box state.
    pub fn resolve(&self, event: KeyEvent, active_modal: Modal) -> Option<InputAction> {
        let key = Key::from_event(event);
        for binding in GLOBAL_BINDINGS.iter() {
            if binding.key != key {
                continue;
            }
            match binding.gate {
                Gate::Always => {}
                Gate::NoModal => {
                    if active_modal != Modal::None {
                        continue;
                    }
                }
            }
            return Some(match binding.action {
                Action::OpenHelp => InputAction::OpenHelp,
                Action::OpenHistory => InputAction::OpenHistory,
                Action::OpenModels => InputAction::OpenModels,
                Action::OpenConnectionDetail => InputAction::OpenActiveConnectionDetail,
                Action::OpenTodos => InputAction::OpenTodos,
                Action::OpenQueue => InputAction::OpenQueue,
                Action::OpenTelemetry => InputAction::OpenTelemetry,
                Action::OpenBtwList => InputAction::OpenBtwList,
                Action::OpenViewSwitcher => InputAction::ViewSwitcherToggle,
                Action::ToggleQueueBlock => InputAction::QueueToggleBlock,
                Action::CopyOrClear => InputAction::CtrlC,
                Action::CopySelection => InputAction::CopySelection,
            });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn ctrl_t_resolves_to_open_todos_from_top_level() {
        let registry = Registry::new();
        let action = registry.resolve(key(KeyCode::Char('t'), KeyModifiers::CONTROL), Modal::None);
        assert_eq!(action, Some(InputAction::OpenTodos));
    }

    #[test]
    fn ctrl_t_does_not_fire_while_a_modal_is_open() {
        let registry = Registry::new();
        let action = registry.resolve(key(KeyCode::Char('t'), KeyModifiers::CONTROL), Modal::Help);
        assert_eq!(action, None);
    }

    #[test]
    fn ctrl_c_is_always_active_even_in_a_modal() {
        let registry = Registry::new();
        let action = registry.resolve(
            key(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Modal::Models,
        );
        assert_eq!(action, Some(InputAction::CtrlC));
    }

    #[test]
    fn f1_opens_help_from_top_level() {
        let registry = Registry::new();
        let action = registry.resolve(key(KeyCode::F(1), KeyModifiers::NONE), Modal::None);
        assert_eq!(action, Some(InputAction::OpenHelp));
    }

    #[test]
    fn non_global_keys_are_not_resolved_here() {
        // Text-editing keys like Ctrl+A are not global bindings; they fall
        // through to the input handler's contextual arms.
        let registry = Registry::new();
        let action = registry.resolve(key(KeyCode::Char('a'), KeyModifiers::CONTROL), Modal::None);
        assert_eq!(action, None);
    }

    #[test]
    fn ctrl_q_opens_queue_from_top_level() {
        // Ctrl+Q is the global binding for the queue (outbox) overview modal
        // (ADR-0126 — the queue family moved off the F-row onto the Ctrl row).
        let registry = Registry::new();
        let action = registry.resolve(key(KeyCode::Char('q'), KeyModifiers::CONTROL), Modal::None);
        assert_eq!(action, Some(InputAction::OpenQueue));
    }

    #[test]
    fn ctrl_q_does_not_fire_while_a_modal_is_open() {
        let registry = Registry::new();
        let action = registry.resolve(key(KeyCode::Char('q'), KeyModifiers::CONTROL), Modal::Help);
        assert_eq!(action, None);
    }

    #[test]
    fn ctrl_p_toggles_queue_block_from_top_level() {
        // Ctrl+P is the global binding for the queue block/resume override.
        // It is gated NoModal: inside a modal the contextual input handler
        // routes it instead (only the Queue modal honors it there).
        let registry = Registry::new();
        let action = registry.resolve(key(KeyCode::Char('p'), KeyModifiers::CONTROL), Modal::None);
        assert_eq!(action, Some(InputAction::QueueToggleBlock));
    }

    #[test]
    fn ctrl_p_does_not_fire_via_registry_while_a_modal_is_open() {
        // The global registry is NoModal-gated, so Ctrl+P resolves to None
        // inside any modal. (The Queue modal routes Ctrl+P through its
        // contextual arm; other modals treat it as a no-op.)
        let registry = Registry::new();
        let action = registry.resolve(key(KeyCode::Char('p'), KeyModifiers::CONTROL), Modal::Help);
        assert_eq!(action, None);
    }

    #[test]
    fn ctrl_o_opens_the_session_telemetry_drill_down() {
        // Ctrl+O opens the unified session telemetry report.
        let registry = Registry::new();
        let ctx = registry.resolve(key(KeyCode::Char('o'), KeyModifiers::CONTROL), Modal::None);
        assert_eq!(ctx, Some(InputAction::OpenTelemetry));

        // Inside a modal the gate swallows the chord.
        let ctx = registry.resolve(key(KeyCode::Char('o'), KeyModifiers::CONTROL), Modal::Help);
        assert_eq!(ctx, None);
    }

    #[test]
    fn ctrl_n_opens_connection_detail_from_top_level() {
        // Ctrl+N opens the active connection detail modal.
        let registry = Registry::new();
        let action = registry.resolve(key(KeyCode::Char('n'), KeyModifiers::CONTROL), Modal::None);
        assert_eq!(action, Some(InputAction::OpenActiveConnectionDetail));

        // Inside a modal the gate swallows the chord.
        let action = registry.resolve(key(KeyCode::Char('n'), KeyModifiers::CONTROL), Modal::Help);
        assert_eq!(action, None);
    }

    #[test]
    fn f5_opens_the_btw_asides_from_top_level() {
        // F5 is the global binding for the `/btw` asides list (ADR-0103 §5).
        // Gated NoModal like the rest of the F-row list family: inside a
        // modal it is swallowed by the gate — except inside the asides modal
        // itself, where the contextual arm turns it into a refresh.
        let registry = Registry::new();
        let action = registry.resolve(key(KeyCode::F(5), KeyModifiers::NONE), Modal::None);
        assert_eq!(action, Some(InputAction::OpenBtwList));

        let action = registry.resolve(key(KeyCode::F(5), KeyModifiers::NONE), Modal::Models);
        assert_eq!(action, None);
    }

    #[test]
    fn shift_on_a_letter_is_normalized() {
        // A bare `Shift+T` normalizes to plain `T` (a shifted letter is just a
        // different char), so `Shift+letter` is not a separate binding from
        // the unshifted letter. `Ctrl+Shift+T`, however, keeps its modifiers —
        // a chord's extra modifiers are meaningful (e.g. `Ctrl+Shift+C` is a
        // distinct copy-selection binding from plain `Ctrl+C`).
        let bare_shift_t = Key::from_event(key(KeyCode::Char('T'), KeyModifiers::SHIFT));
        assert_eq!(
            bare_shift_t,
            Key {
                modifiers: KeyModifiers::NONE,
                code: KeyCode::Char('T'),
            }
        );
        let ctrl_shift_t = Key::from_event(key(
            KeyCode::Char('T'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ));
        assert_eq!(
            ctrl_shift_t.modifiers,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        );
    }

    #[test]
    fn ctrl_shift_c_and_cmd_c_copy_selection() {
        // The copy-selection chord and the macOS Cmd+C chord both map to
        // CopySelection and work over any modal (Always gate).
        let registry = Registry::new();
        let action = registry.resolve(
            key(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
            Modal::Models,
        );
        assert_eq!(action, Some(InputAction::CopySelection));

        let action = registry.resolve(key(KeyCode::Char('c'), KeyModifiers::SUPER), Modal::Models);
        assert_eq!(action, Some(InputAction::CopySelection));
    }

    #[test]
    fn chord_uses_canonical_lowercase_help_notation() {
        let ctrl_t = Key {
            modifiers: KeyModifiers::CONTROL,
            code: KeyCode::Char('t'),
        };
        assert_eq!(ctrl_t.chord(), "ctrl+t");

        let alt_enter = Key {
            modifiers: KeyModifiers::ALT,
            code: KeyCode::Enter,
        };
        assert_eq!(alt_enter.chord(), "alt+enter");

        let f1 = Key {
            modifiers: KeyModifiers::NONE,
            code: KeyCode::F(1),
        };
        assert_eq!(f1.chord(), "f1");

        let esc = Key {
            modifiers: KeyModifiers::NONE,
            code: KeyCode::Esc,
        };
        assert_eq!(esc.chord(), "esc");

        let backtab = Key {
            modifiers: KeyModifiers::SHIFT,
            code: KeyCode::BackTab,
        };
        assert_eq!(backtab.chord(), "shift+tab");
    }

    #[test]
    fn display_uses_capitalized_footer_notation() {
        // Single named keys: capitalized words matching the footer style.
        assert_eq!(Key::ENTER.display(), "Enter");
        assert_eq!(Key::ESC.display(), "Esc");
        assert_eq!(Key::TAB.display(), "Tab");
        assert_eq!(
            Key {
                modifiers: KeyModifiers::NONE,
                code: KeyCode::Char(' '),
            }
            .display(),
            "·",
            "Space has no KeyCode; footers use the keyvocab::SPACE affordance"
        );

        // Arrows share their glyph between the two forms (only named keys
        // differ in case), and BackTab renders as Shift+Tab.
        let up = Key {
            modifiers: KeyModifiers::NONE,
            code: KeyCode::Up,
        };
        assert_eq!(up.display(), "↑");
        let backtab = Key {
            modifiers: KeyModifiers::SHIFT,
            code: KeyCode::BackTab,
        };
        assert_eq!(backtab.display(), keyvocab::SHIFT_TAB);
        assert_eq!(backtab.display(), "⇧Tab");

        // Modifier chords: capitalized prefix + capitalized core.
        assert_eq!(Key::CTRL_T.display(), "Ctrl+T");
        assert_eq!(Key::CTRL_P.display(), "Ctrl+P");
        assert_eq!(Key::CTRL_Q.display(), "Ctrl+Q");
        assert_eq!(Key::CTRL_O.display(), "Ctrl+O");
        assert_eq!(Key::CTRL_S.display(), "Ctrl+S");
        let ctrl_shift_c = Key {
            modifiers: KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            code: KeyCode::Char('c'),
        };
        assert_eq!(ctrl_shift_c.display(), "Ctrl+Shift+C");
        let cmd_c = Key {
            modifiers: KeyModifiers::SUPER,
            code: KeyCode::Char('c'),
        };
        assert_eq!(cmd_c.display(), "Cmd+C");
    }

    #[test]
    fn const_aliases_match_their_constructed_keys() {
        // The frequently-used `Key::ESC`/`ENTER`/`TAB`/`CTRL_T`/queue-family
        // consts must agree with the equivalent ad-hoc construction, so
        // swapping a hand-built literal for the alias is a pure refactor.
        assert_eq!(
            Key::ESC,
            Key {
                modifiers: KeyModifiers::NONE,
                code: KeyCode::Esc,
            }
        );
        assert_eq!(
            Key::CTRL_T,
            Key {
                modifiers: KeyModifiers::CONTROL,
                code: KeyCode::Char('t'),
            }
        );
        assert_eq!(
            Key::CTRL_P,
            Key {
                modifiers: KeyModifiers::CONTROL,
                code: KeyCode::Char('p'),
            }
        );
        assert_eq!(
            Key::CTRL_Q,
            Key {
                modifiers: KeyModifiers::CONTROL,
                code: KeyCode::Char('q'),
            }
        );
        // And their display names match what the footer expects.
        assert_eq!(Key::ESC.display(), "Esc");
        assert_eq!(Key::ENTER.display(), "Enter");
        assert_eq!(Key::TAB.display(), "Tab");
        assert_eq!(Key::CTRL_T.display(), "Ctrl+T");
        assert_eq!(Key::CTRL_P.display(), "Ctrl+P");
        assert_eq!(Key::CTRL_Q.display(), "Ctrl+Q");
    }

    #[test]
    fn const_display_names_match_key_display() {
        // Key::display() is a compile-time const fn that accurately returns
        // the canonical capitalized human form for every key.
        assert_eq!(Key::ESC.display(), "Esc");
        assert_eq!(Key::ENTER.display(), "Enter");
        assert_eq!(Key::TAB.display(), "Tab");
        assert_eq!(Key::CTRL_T.display(), "Ctrl+T");
        assert_eq!(Key::CTRL_P.display(), "Ctrl+P");
        assert_eq!(Key::CTRL_Q.display(), "Ctrl+Q");
        assert_eq!(Key::CTRL_X.display(), "Ctrl+X");
        assert_eq!(Key::CTRL_C.display(), "Ctrl+C");
        assert_eq!(Key::CTRL_G.display(), "Ctrl+G");
        assert_eq!(Key::CTRL_R.display(), "Ctrl+R");
        assert_eq!(Key::CTRL_M.display(), "Ctrl+M");
        assert_eq!(Key::CTRL_O.display(), "Ctrl+O");
        assert_eq!(Key::CTRL_N.display(), "Ctrl+N");
        assert_eq!(Key::CTRL_S.display(), "Ctrl+S");
        assert_eq!(Key::F1.display(), "F1");
        assert_eq!(Key::F5.display(), "F5");
        // SHIFT_TAB matches the BackTab display form.
        assert_eq!(
            keyvocab::SHIFT_TAB,
            Key {
                modifiers: KeyModifiers::SHIFT,
                code: KeyCode::BackTab,
            }
            .display()
        );
    }

    #[test]
    fn chord_and_display_share_glyphs_for_symbols_and_arrows() {
        // The two forms differ only for named keys and letters; arrows and
        // symbol keys (`?`, `/`) must be byte-identical so a footer and a Help
        // row describing the same arrow never disagree on the glyph.
        for code in [
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Char('?'),
            KeyCode::Char('/'),
        ] {
            let k = Key {
                modifiers: KeyModifiers::NONE,
                code,
            };
            assert_eq!(k.chord(), k.display(), "glyph mismatch for {code:?}");
        }
    }

    #[test]
    fn declared_binding_labels_always_carry_their_modifier_prefix() {
        // [`chord_str`] / [`display_str`] are hand-maintained match tables
        // with a bare-core-token fallback (`(_, c) => c`). A binding added
        // without its table entry would silently render as `n` instead of
        // `ctrl+n` in Help — and the non-empty-label test above cannot catch
        // it (`n` is non-empty). Lock the *full* label (prefix + token) for
        // every declared binding, so the fallback is unreachable for real
        // bindings and a forgotten table entry fails here instead.
        for binding in Registry::new().bindings() {
            if matches!(binding.key.code, KeyCode::BackTab) {
                // BackTab carries its own full label; both forms are locked
                // by the dedicated tests above.
                assert_eq!(binding.key.chord(), "shift+tab");
                assert_eq!(binding.key.display(), keyvocab::SHIFT_TAB);
                continue;
            }
            assert_ne!(
                chord_token(binding.key.code),
                "·",
                "undeclared key {:?}: add it to chord_token/display_token",
                binding.key.code
            );
            let expected_chord = format!(
                "{}{}",
                chord_prefix(binding.key.modifiers),
                chord_token(binding.key.code)
            );
            assert_eq!(
                binding.key.chord(),
                expected_chord,
                "chord_str is missing an entry for {:?}",
                binding.key
            );
            let expected_display = format!(
                "{}{}",
                display_prefix(binding.key.modifiers),
                display_token(binding.key.code)
            );
            assert_eq!(
                binding.key.display(),
                expected_display,
                "display_str is missing an entry for {:?}",
                binding.key
            );
        }
    }

    #[test]
    fn every_binding_has_a_nonempty_unique_label() {
        let registry = Registry::new();
        let mut labels = Vec::new();
        for binding in registry.bindings() {
            let label = binding.key.chord();
            assert!(!label.is_empty(), "empty label for {:?}", binding.action);
            assert!(
                !labels.contains(&label),
                "duplicate binding label {label:?}"
            );
            labels.push(label);
            // The display form must also be non-empty and unique among the
            // bindings (two distinct chords could in principle collapse to one
            // display string; guard against that).
            let disp = binding.key.display();
            assert!(!disp.is_empty(), "empty display for {:?}", binding.action);
            labels.push(disp);
            assert!(!binding.description.is_empty());
        }
    }
}
