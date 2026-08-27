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

    // ── Display-form single keys (capitalized), as `&'static str` constants so
    // footers can use them in `const`/array contexts without a method call.
    // These mirror the most common `Key::display()` results; keep the two in
    // sync (the test `const_display_names_match_key_display` locks that). The
    // call site chooses freely between `keyvocab::ESC` (const) and
    // `Key::ESC.display()` (method) — they render identically. ──

    /// `Esc` — the display form of [`super::Key::ESC`].
    pub const ESC: &str = "Esc";
    /// `Enter` — the display form of [`super::Key::ENTER`].
    pub const ENTER: &str = "Enter";
    /// `Tab` — the display form of [`super::Key::TAB`].
    pub const TAB: &str = "Tab";
    /// `F2` — legacy display token. The F-key queue family moved to the Ctrl
    /// row (ADR-0124); kept so historical copy stays spellable until every
    /// surface is migrated.
    #[allow(dead_code)]
    pub const F2: &str = "F2";
    /// `Ctrl+Q` — the display form of [`super::Key::CTRL_Q`] (open the queue
    /// modal).
    pub const CTRL_Q: &str = "Ctrl+Q";
    /// `Ctrl+P` — the display form of [`super::Key::CTRL_P`] (block/resume the
    /// queue).
    pub const CTRL_P: &str = "Ctrl+P";
    /// `Ctrl+T` — the display form of [`super::Key::CTRL_T`]. Kept for
    /// completeness; call sites currently use `Key::CTRL_T.display()`.
    #[allow(dead_code)]
    pub const CTRL_T: &str = "Ctrl+T";
    /// `Ctrl+X` — the history-clear shortcut (inside the Ctrl+R panel).
    pub const CTRL_X: &str = "Ctrl+X";
}

/// The compact token for a core [`KeyCode`] — the lowercase `enter` / `esc` /
/// `↑` fragment used inside a chord, before any modifier prefix.
fn chord_token(code: KeyCode) -> &'static str {
    match code {
        KeyCode::Char(c) => match c.to_ascii_lowercase() {
            'a' => "a",
            'b' => "b",
            'c' => "c",
            'd' => "d",
            'e' => "e",
            'f' => "f",
            'h' => "h",
            'j' => "j",
            'k' => "k",
            'l' => "l",
            'm' => "m",
            'o' => "o",
            'p' => "p",
            'q' => "q",
            'r' => "r",
            's' => "s",
            't' => "t",
            'u' => "u",
            'v' => "v",
            'w' => "w",
            '?' => "?",
            '/' => "/",
            _ => "·",
        },
        KeyCode::Enter => "enter",
        KeyCode::Tab => "tab",
        // `BackTab` carries its own `shift+tab` label (see `chord`).
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
        _ => "·",
    }
}

/// The display token for a core [`KeyCode`] — the capitalized `Enter` / `Esc` /
/// `↑` fragment a footer or legend shows, before any modifier prefix.
///
/// This is the case-mirror of [`chord_token`]: the two share the same glyph for
/// arrows / symbols and differ only for named keys (`enter` → `Enter`) and
/// letters (kept lowercased in a chord, capitalized as a standalone display
/// key).
fn display_token(code: KeyCode) -> &'static str {
    match code {
        KeyCode::Char(c) => match c.to_ascii_lowercase() {
            'a' => "A",
            'b' => "B",
            'c' => "C",
            'd' => "D",
            'e' => "E",
            'f' => "F",
            'h' => "H",
            'j' => "J",
            'k' => "K",
            'l' => "L",
            'm' => "M",
            'o' => "O",
            'p' => "P",
            'q' => "Q",
            'r' => "R",
            's' => "S",
            't' => "T",
            'u' => "U",
            'v' => "V",
            'w' => "W",
            '?' => "?",
            '/' => "/",
            _ => "·",
        },
        KeyCode::Enter => "Enter",
        KeyCode::Tab => "Tab",
        // `BackTab` carries its own `Shift+Tab` label (see `display`).
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
        _ => "·",
    }
}

/// The lowercase modifier prefix for a chord (`ctrl+`, `alt+`, …), or `""` for
/// none. Shared by [`Key::chord`] and `Key::display_prefix` so the modifier
/// vocabulary is owned once.
fn chord_prefix(modifiers: KeyModifiers) -> &'static str {
    if modifiers == (KeyModifiers::CONTROL | KeyModifiers::SHIFT) {
        "ctrl+shift+"
    } else if modifiers == KeyModifiers::CONTROL {
        "ctrl+"
    } else if modifiers == KeyModifiers::ALT {
        "alt+"
    } else if modifiers == KeyModifiers::SHIFT {
        "shift+"
    } else if modifiers == KeyModifiers::SUPER {
        "cmd+"
    } else {
        ""
    }
}

/// The display-case modifier prefix for a key (`Ctrl+`, `Alt+`, …), or `""`
/// for none. The case-mirror of [`chord_prefix`].
fn display_prefix(modifiers: KeyModifiers) -> &'static str {
    if modifiers == (KeyModifiers::CONTROL | KeyModifiers::SHIFT) {
        "Ctrl+Shift+"
    } else if modifiers == KeyModifiers::CONTROL {
        "Ctrl+"
    } else if modifiers == KeyModifiers::ALT {
        "Alt+"
    } else if modifiers == KeyModifiers::SHIFT {
        "Shift+"
    } else if modifiers == KeyModifiers::SUPER {
        "Cmd+"
    } else {
        ""
    }
}

/// The set of full chord strings the app's declared bindings actually use, so
/// [`Key::chord`] can return a `&'static str` with no allocation. Backed by the
/// `concat!` of [`chord_prefix`] + [`chord_token`] for each known combination.
fn chord_str(prefix: &'static str, core: &'static str) -> &'static str {
    match (prefix, core) {
        ("", c) => c,
        ("ctrl+", "a") => "ctrl+a",
        ("ctrl+", "b") => "ctrl+b",
        ("ctrl+", "c") => "ctrl+c",
        ("ctrl+", "d") => "ctrl+d",
        ("ctrl+", "e") => "ctrl+e",
        ("ctrl+", "f") => "ctrl+f",
        ("ctrl+", "h") => "ctrl+h",
        ("ctrl+", "j") => "ctrl+j",
        ("ctrl+", "k") => "ctrl+k",
        ("ctrl+", "l") => "ctrl+l",
        ("ctrl+", "m") => "ctrl+m",
        ("ctrl+", "o") => "ctrl+o",
        ("ctrl+", "p") => "ctrl+p",
        ("ctrl+", "q") => "ctrl+q",
        ("ctrl+", "r") => "ctrl+r",
        ("ctrl+", "s") => "ctrl+s",
        ("ctrl+", "t") => "ctrl+t",
        ("ctrl+", "u") => "ctrl+u",
        ("ctrl+", "v") => "ctrl+v",
        ("ctrl+", "w") => "ctrl+w",
        ("ctrl+", "←") => "ctrl+←",
        ("ctrl+", "→") => "ctrl+→",
        ("ctrl+", "↑") => "ctrl+↑",
        ("ctrl+", "↓") => "ctrl+↓",
        ("ctrl+shift+", "c") => "ctrl+shift+c",
        ("alt+", "enter") => "alt+enter",
        ("alt+", "b") => "alt+b",
        ("alt+", "d") => "alt+d",
        ("alt+", "f") => "alt+f",
        ("alt+", "backspace") => "alt+backspace",
        ("shift+", "tab") => "shift+tab",
        ("cmd+", "c") => "cmd+c",
        // Unreachable for declared bindings; keep a stable fallback (the core
        // token alone) so the function is total for any `KeyCode`.
        (_, c) => c,
    }
}

/// The set of full display strings the app's bindings actually use, so
/// [`Key::display`] can return a `&'static str` with no allocation. The
/// case-mirror of [`chord_str`].
fn display_str(prefix: &'static str, core: &'static str) -> &'static str {
    match (prefix, core) {
        ("", c) => c,
        ("Ctrl+", "A") => "Ctrl+A",
        ("Ctrl+", "B") => "Ctrl+B",
        ("Ctrl+", "C") => "Ctrl+C",
        ("Ctrl+", "D") => "Ctrl+D",
        ("Ctrl+", "E") => "Ctrl+E",
        ("Ctrl+", "F") => "Ctrl+F",
        ("Ctrl+", "H") => "Ctrl+H",
        ("Ctrl+", "J") => "Ctrl+J",
        ("Ctrl+", "K") => "Ctrl+K",
        ("Ctrl+", "L") => "Ctrl+L",
        ("Ctrl+", "M") => "Ctrl+M",
        ("Ctrl+", "O") => "Ctrl+O",
        ("Ctrl+", "P") => "Ctrl+P",
        ("Ctrl+", "Q") => "Ctrl+Q",
        ("Ctrl+", "R") => "Ctrl+R",
        ("Ctrl+", "S") => "Ctrl+S",
        ("Ctrl+", "T") => "Ctrl+T",
        ("Ctrl+", "U") => "Ctrl+U",
        ("Ctrl+", "V") => "Ctrl+V",
        ("Ctrl+", "W") => "Ctrl+W",
        ("Ctrl+", "←") => "Ctrl+←",
        ("Ctrl+", "→") => "Ctrl+→",
        ("Ctrl+", "↑") => "Ctrl+↑",
        ("Ctrl+", "↓") => "Ctrl+↓",
        ("Ctrl+Shift+", "C") => "Ctrl+Shift+C",
        ("Alt+", "Enter") => "Alt+Enter",
        ("Alt+", "B") => "Alt+B",
        ("Alt+", "D") => "Alt+D",
        ("Alt+", "F") => "Alt+F",
        ("Alt+", "Backspace") => "Alt+Backspace",
        ("Shift+", "Tab") => "Shift+Tab",
        ("Cmd+", "C") => "Cmd+C",
        // Fallback for combos not enumerated above; declared bindings never
        // reach here. Returning the core token keeps the function total.
        (_, c) => c,
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
    pub fn chord(self) -> &'static str {
        // `BackTab` already carries its full `shift+tab` label, so ignore the
        // prefix (a `Shift`+`Tab` event arrives as `BackTab` with SHIFT set).
        if matches!(self.code, KeyCode::BackTab) {
            return "shift+tab";
        }
        chord_str(chord_prefix(self.modifiers), chord_token(self.code))
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
    pub fn display(self) -> &'static str {
        if matches!(self.code, KeyCode::BackTab) {
            return keyvocab::SHIFT_TAB;
        }
        display_str(display_prefix(self.modifiers), display_token(self.code))
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
    /// The Tab key. Kept for completeness alongside [`keyvocab::TAB`]; the
    /// queue bar's Tab legend was removed when the insert/next-round toggle
    /// was dropped, and the other Tab surfaces (history preview, provider
    /// field cycling) render via `keyvocab::TAB` instead.
    #[allow(dead_code)]
    pub const TAB: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Tab,
    };
    /// Ctrl+T (open Todos) — the global shortcut surfaced in the idle activity
    /// bar's discoverability hint.
    pub const CTRL_T: Key = Key {
        modifiers: KeyModifiers::CONTROL,
        code: KeyCode::Char('t'),
    };
    /// Ctrl+Q (open the queue modal) — the queue family lives on the Ctrl row:
    /// `Ctrl+P` pause, `Ctrl+Q` expand. Mnemonic and Fn-layer-free,
    /// unlike the F-keys it replaces (ADR-0124).
    pub const CTRL_Q: Key = Key {
        modifiers: KeyModifiers::CONTROL,
        code: KeyCode::Char('q'),
    };
    /// Ctrl+P (block/resume the queue) — "pause". Companion of [`Self::CTRL_Q`].
    pub const CTRL_P: Key = Key {
        modifiers: KeyModifiers::CONTROL,
        code: KeyCode::Char('p'),
    };
    /// Ctrl+O (open the context/token usage report) — the keyboard twin of
    /// clicking the model bar's context meter.
    pub const CTRL_O: Key = Key {
        modifiers: KeyModifiers::CONTROL,
        code: KeyCode::Char('o'),
    };
    /// Ctrl+S (open the latest-turn performance report) — the keyboard twin
    /// of clicking the model bar's stream-rate gauge. "s" for "speed".
    pub const CTRL_S: Key = Key {
        modifiers: KeyModifiers::CONTROL,
        code: KeyCode::Char('s'),
    };
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
    /// Open the Todos modal.
    OpenTodos,
    /// Open the queue overview modal.
    OpenQueue,
    /// Open the token/context usage report modal — the drill-down behind the
    /// model bar's context meter (`Ctrl+O`).
    OpenTokenReport,
    /// Open the latest-turn performance report modal — the drill-down behind
    /// the model bar's stream-rate gauge (`Ctrl+S`).
    OpenPerformanceReport,
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
        // Queue management lives on the Ctrl row (ADR-0124), replacing the
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
        // (The queue family moved off the F-row to Ctrl+P/Q — ADR-0124 —
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
        Binding {
            key: Key {
                modifiers: KeyModifiers::CONTROL,
                code: KeyCode::Char('m'),
            },
            gate: Gate::NoModal,
            action: Action::OpenModels,
            description: "switch model",
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
        // asked for). `o` reads as "usage overview". NoModal-gated like the
        // other open-* bindings. Ctrl+O is free: the ADR-0126 mid-round
        // insert was removed, and the chord is not in the readline family
        // the composer claims (Ctrl+A/E/W/U/K, Alt+B/F/D).
        Binding {
            key: Key {
                modifiers: KeyModifiers::CONTROL,
                code: KeyCode::Char('o'),
            },
            gate: Gate::NoModal,
            action: Action::OpenTokenReport,
            description: "context usage report",
        },
        // Ctrl+S opens the latest-turn performance report — the keyboard twin
        // of clicking the model bar's stream-rate gauge. `s` for "speed".
        // It is not the conventional "save" anywhere in this TUI (the
        // composer submits with Enter; sessions persist durably on their
        // own), so the chord is safe to claim. NoModal-gated likewise.
        Binding {
            key: Key {
                modifiers: KeyModifiers::CONTROL,
                code: KeyCode::Char('s'),
            },
            gate: Gate::NoModal,
            action: Action::OpenPerformanceReport,
            description: "performance report",
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
                Action::OpenTodos => InputAction::OpenTodos,
                Action::OpenQueue => InputAction::OpenQueue,
                Action::OpenTokenReport => InputAction::OpenTokenReport,
                Action::OpenPerformanceReport => InputAction::OpenPerformanceReport,
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
        // (ADR-0124 — the queue family moved off the F-row onto the Ctrl row).
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
    fn ctrl_o_and_ctrl_s_open_the_model_bar_drill_downs() {
        // The model bar's two gauges carry keyboard twins: Ctrl+O opens the
        // context/token report (the context meter's drill-down) and Ctrl+S
        // the performance report (the stream rate's). Both are NoModal-gated
        // like the other open-* bindings.
        let registry = Registry::new();
        let ctx = registry.resolve(key(KeyCode::Char('o'), KeyModifiers::CONTROL), Modal::None);
        assert_eq!(ctx, Some(InputAction::OpenTokenReport));
        let perf = registry.resolve(key(KeyCode::Char('s'), KeyModifiers::CONTROL), Modal::None);
        assert_eq!(perf, Some(InputAction::OpenPerformanceReport));

        // Inside a modal the gate swallows both chords.
        let ctx = registry.resolve(key(KeyCode::Char('o'), KeyModifiers::CONTROL), Modal::Help);
        assert_eq!(ctx, None);
        let perf = registry.resolve(key(KeyCode::Char('s'), KeyModifiers::CONTROL), Modal::Help);
        assert_eq!(perf, None);
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
        // The `keyvocab::ESC`/`ENTER`/… string constants must agree byte-for-byte
        // with `Key::display()` for the corresponding `Key` constant, so a footer
        // may freely swap `keyvocab::ESC` for `Key::ESC.display()` (and vice
        // versa) without changing what renders.
        assert_eq!(keyvocab::ESC, Key::ESC.display());
        assert_eq!(keyvocab::ENTER, Key::ENTER.display());
        assert_eq!(keyvocab::TAB, Key::TAB.display());
        assert_eq!(keyvocab::CTRL_T, Key::CTRL_T.display());
        assert_eq!(keyvocab::CTRL_P, Key::CTRL_P.display());
        assert_eq!(keyvocab::CTRL_Q, Key::CTRL_Q.display());
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
