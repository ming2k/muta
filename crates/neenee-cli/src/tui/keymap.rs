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
//! 2. Push a [`Binding`] into [`Registry::build`] with its [`Key`], gate, the
//!    [`Action`] it maps to, and a short human description (shown in Help).
//!
//! The Help modal and the resolver pick it up with no further wiring.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::Modal;
use super::input::InputAction;

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

    /// The canonical lowercase label for this key, in the `ctrl+t` /
    /// `alt+enter` / `f1` / `esc` / `↑` notation used by the Help modal.
    /// Joined keys use `+`, matching the established Help copy.
    ///
    /// Returns `&'static str` because every declared binding is a fixed,
    /// compile-time-known combination, so the label can be stored alongside
    /// the `&'static str` description in the Help rows without allocation.
    pub fn label(self) -> &'static str {
        // Modifier prefix (the common combos used by global bindings).
        let prefix: &'static str =
            if self.modifiers == (KeyModifiers::CONTROL | KeyModifiers::SHIFT) {
                "ctrl+shift+"
            } else if self.modifiers == KeyModifiers::CONTROL {
                "ctrl+"
            } else if self.modifiers == KeyModifiers::ALT {
                "alt+"
            } else if self.modifiers == KeyModifiers::SHIFT {
                "shift+"
            } else if self.modifiers == KeyModifiers::SUPER {
                "cmd+"
            } else if self.modifiers.is_empty() {
                ""
            } else {
                // Unreachable for declared bindings; keep a stable fallback.
                ""
            };
        // Core token. `shift+tab` is carried by `BackTab`, so map it directly
        // to its single-token label and drop the `shift+` prefix below.
        let (core, is_backtab): (&'static str, bool) = match self.code {
            KeyCode::Char(c) => match c.to_ascii_lowercase() {
                'a' => ("a", false),
                'b' => ("b", false),
                'c' => ("c", false),
                'd' => ("d", false),
                'e' => ("e", false),
                'f' => ("f", false),
                'h' => ("h", false),
                'j' => ("j", false),
                'k' => ("k", false),
                'm' => ("m", false),
                'r' => ("r", false),
                't' => ("t", false),
                'u' => ("u", false),
                'v' => ("v", false),
                'w' => ("w", false),
                '?' => ("?", false),
                '/' => ("/", false),
                _ => ("·", false),
            },
            KeyCode::Enter => ("enter", false),
            KeyCode::Tab => ("tab", false),
            KeyCode::BackTab => ("shift+tab", true),
            KeyCode::Backspace => ("backspace", false),
            KeyCode::Esc => ("esc", false),
            KeyCode::Up => ("↑", false),
            KeyCode::Down => ("↓", false),
            KeyCode::Left => ("←", false),
            KeyCode::Right => ("→", false),
            KeyCode::Home => ("home", false),
            KeyCode::End => ("end", false),
            KeyCode::PageUp => ("pgup", false),
            KeyCode::PageDown => ("pgdn", false),
            KeyCode::F(1) => ("f1", false),
            KeyCode::F(2) => ("f2", false),
            KeyCode::F(3) => ("f3", false),
            KeyCode::F(4) => ("f4", false),
            _ => ("·", false),
        };
        if is_backtab {
            // `BackTab` already carries its full label; ignore the prefix.
            return core;
        }
        // Concatenate prefix + core against the small fixed set of combos the
        // registry actually uses, so the result stays `&'static str` with no
        // allocation. Order matters: check the longer prefixes first.
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
            ("ctrl+", "m") => "ctrl+m",
            ("ctrl+", "r") => "ctrl+r",
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
            // Fallback for any combo not enumerated above. Declared bindings
            // never reach here; returning the core keeps the function total.
            (_, c) => c,
        }
    }
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
            description: "copy · clear input · quit (×2)",
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

        let action = registry.resolve(
            key(KeyCode::Char('c'), KeyModifiers::SUPER),
            Modal::Models,
        );
        assert_eq!(action, Some(InputAction::CopySelection));
    }

    #[test]
    fn labels_use_canonical_help_notation() {
        let ctrl_t = Key {
            modifiers: KeyModifiers::CONTROL,
            code: KeyCode::Char('t'),
        };
        assert_eq!(ctrl_t.label(), "ctrl+t");

        let alt_enter = Key {
            modifiers: KeyModifiers::ALT,
            code: KeyCode::Enter,
        };
        assert_eq!(alt_enter.label(), "alt+enter");

        let f1 = Key {
            modifiers: KeyModifiers::NONE,
            code: KeyCode::F(1),
        };
        assert_eq!(f1.label(), "f1");

        let esc = Key {
            modifiers: KeyModifiers::NONE,
            code: KeyCode::Esc,
        };
        assert_eq!(esc.label(), "esc");

        let backtab = Key {
            modifiers: KeyModifiers::SHIFT,
            code: KeyCode::BackTab,
        };
        assert_eq!(backtab.label(), "shift+tab");
    }

    #[test]
    fn every_binding_has_a_nonempty_unique_label() {
        let registry = Registry::new();
        let mut labels = Vec::new();
        for binding in registry.bindings() {
            let label = binding.key.label();
            assert!(!label.is_empty(), "empty label for {:?}", binding.action);
            assert!(
                !labels.contains(&label),
                "duplicate binding label {label:?}"
            );
            labels.push(label);
            assert!(!binding.description.is_empty());
        }
    }
}
