//! Authoritative Action & Command Registry — Single Source of Truth (SSOT).
//!
//! Every action, shortcut, command palette entry, slash command, F1 help item,
//! and footer hint across the application is declared here as a [`CommandSpec`].
//!
//! ## Core Architectural Principles
//!
//! 1. **Composer-first, no input modality**: typing always flows to Composer.
//! 2. **Single input owner**: only one region/dialog owns focus at any moment.
//! 3. **Visible, predictable, recoverable focus**: overlays trap focus, closing restores source.
//! 4. **Single semantic origin**: one action has one semantic source.
//! 5. **Unified derivation**: shortcuts, Help, Footer, and Command Palette are derived from this registry.
//! 6. **Discovery over memorization**: rare actions are found via `Ctrl+L` Command Palette.
//! 7. **No modal penetration**: overlays strictly isolate input from background views.
//! 8. **Zero loss of printable characters**: typing in transcript bounces back to composer.
//! 9. **Terminal independence**: core workflows work without Kitty enhanced keyboard protocol.
//! 10. **Zero legacy baggage**: breaking clean from leader chords and modal keymaps.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::SessionFocusRegion;
use crate::modal::Modal;
use crate::surfaces::View;

// ─────────────────────────────────────────────────────────────────────────────
// Canonical key vocabulary and display formatting
// ─────────────────────────────────────────────────────────────────────────────

/// Repeated legend tokens — glyph strings that stand for an affordance.
pub mod keyvocab {
    pub const ARROWS_UD: &str = "↑↓";
    pub const ARROWS_LR: &str = "←→";
    pub const UP: &str = "↑";
    pub const DOWN: &str = "↓";
    pub const SPACE: &str = "Space";
    pub const SHIFT_TAB: &str = "⇧Tab";
    pub const SHIFT_ENTER: &str = "⇧Enter";
}

/// The compact token for a core [`KeyCode`] in lowercase chord notation.
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

/// The display token for a core [`KeyCode`] in capitalized human notation.
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
        KeyCode::BackTab => keyvocab::SHIFT_TAB,
        KeyCode::Backspace => "Backspace",
        KeyCode::Esc => "Esc",
        KeyCode::Up => keyvocab::UP,
        KeyCode::Down => keyvocab::DOWN,
        KeyCode::Left => "←",
        KeyCode::Right => "→",
        KeyCode::Home => "Home",
        KeyCode::End => "End",
        KeyCode::PageUp => "PageUp",
        KeyCode::PageDown => "PageDown",
        KeyCode::F(1) => "F1",
        KeyCode::F(2) => "F2",
        KeyCode::F(3) => "F3",
        KeyCode::F(4) => "F4",
        KeyCode::F(5) => "F5",
        _ => "·",
    }
}

/// A physical key with optional modifier flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub modifiers: KeyModifiers,
    pub code: KeyCode,
}

impl Key {
    pub const ESC: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Esc,
    };
    pub const ENTER: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Enter,
    };
    pub const TAB: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Tab,
    };
    pub const BACKTAB: Key = Key {
        modifiers: KeyModifiers::SHIFT,
        code: KeyCode::BackTab,
    };
    pub const UP: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Up,
    };
    pub const DOWN: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Down,
    };
    pub const PAGE_UP: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::PageUp,
    };
    pub const PAGE_DOWN: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::PageDown,
    };
    pub const HOME: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::Home,
    };
    pub const END: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::End,
    };
    pub const F1: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::F(1),
    };
    pub const F5: Key = Key {
        modifiers: KeyModifiers::NONE,
        code: KeyCode::F(5),
    };

    pub const CTRL_L: Key = Key::ctrl('l');
    pub const CTRL_C: Key = Key::ctrl('c');
    pub const CTRL_P: Key = Key::ctrl('p');
    pub const CTRL_Q: Key = Key::ctrl('q');
    pub const CTRL_R: Key = Key::ctrl('r');
    pub const CTRL_O: Key = Key::ctrl('o');
    pub const CTRL_N: Key = Key::ctrl('n');
    pub const CTRL_T: Key = Key::ctrl('t');
    pub const CTRL_M: Key = Key::ctrl('m');
    pub const CTRL_S: Key = Key::ctrl('s');
    pub const CTRL_G: Key = Key::ctrl('g');
    pub const CTRL_X: Key = Key::ctrl('x');
    pub const CTRL_J: Key = Key::ctrl('j');
    pub const CTRL_U: Key = Key::ctrl('u');
    pub const CTRL_A: Key = Key::ctrl('a');
    pub const CTRL_E: Key = Key::ctrl('e');
    pub const CTRL_K: Key = Key::ctrl('k');
    pub const CTRL_W: Key = Key::ctrl('w');
    pub const CTRL_V: Key = Key::ctrl('v');

    pub const ALT_S: Key = Key::alt('s');
    pub const ALT_ENTER: Key = Key {
        modifiers: KeyModifiers::ALT,
        code: KeyCode::Enter,
    };

    pub const CTRL_SHIFT_C: Key = Key {
        modifiers: KeyModifiers::CONTROL.union(KeyModifiers::SHIFT),
        code: KeyCode::Char('c'),
    };
    pub const CMD_C: Key = Key {
        modifiers: KeyModifiers::SUPER,
        code: KeyCode::Char('c'),
    };

    pub const fn ctrl(c: char) -> Self {
        Self {
            modifiers: KeyModifiers::CONTROL,
            code: KeyCode::Char(c),
        }
    }

    pub const fn alt(c: char) -> Self {
        Self {
            modifiers: KeyModifiers::ALT,
            code: KeyCode::Char(c),
        }
    }

    pub fn from_event(event: KeyEvent) -> Self {
        let mut code = event.code;
        let mut modifiers = event.modifiers;

        if let KeyCode::Char(c) = code {
            if c.is_ascii_uppercase() && !modifiers.contains(KeyModifiers::CONTROL) && !modifiers.contains(KeyModifiers::ALT) {
                modifiers.remove(KeyModifiers::SHIFT);
            } else if modifiers.contains(KeyModifiers::CONTROL) {
                code = KeyCode::Char(c.to_ascii_lowercase());
            }
        }

        Self { modifiers, code }
    }

    pub const fn chord(&self) -> &'static str {
        let ctrl = self.modifiers.contains(KeyModifiers::CONTROL);
        let alt = self.modifiers.contains(KeyModifiers::ALT);
        let shift = self.modifiers.contains(KeyModifiers::SHIFT);
        let cmd = self.modifiers.contains(KeyModifiers::SUPER);

        if ctrl && shift {
            match self.code {
                KeyCode::Char('c') | KeyCode::Char('C') => "ctrl+shift+c",
                _ => "·",
            }
        } else if ctrl {
            match self.code {
                KeyCode::Char(c) => match c.to_ascii_lowercase() {
                    'a' => "ctrl+a",
                    'b' => "ctrl+b",
                    'c' => "ctrl+c",
                    'd' => "ctrl+d",
                    'e' => "ctrl+e",
                    'f' => "ctrl+f",
                    'g' => "ctrl+g",
                    'h' => "ctrl+h",
                    'i' => "ctrl+i",
                    'j' => "ctrl+j",
                    'k' => "ctrl+k",
                    'l' => "ctrl+l",
                    'm' => "ctrl+m",
                    'n' => "ctrl+n",
                    'o' => "ctrl+o",
                    'p' => "ctrl+p",
                    'q' => "ctrl+q",
                    'r' => "ctrl+r",
                    's' => "ctrl+s",
                    't' => "ctrl+t",
                    'u' => "ctrl+u",
                    'v' => "ctrl+v",
                    'w' => "ctrl+w",
                    'x' => "ctrl+x",
                    'y' => "ctrl+y",
                    'z' => "ctrl+z",
                    _ => "·",
                },
                KeyCode::Up => "ctrl+↑",
                KeyCode::Down => "ctrl+↓",
                KeyCode::Left => "ctrl+←",
                KeyCode::Right => "ctrl+→",
                _ => "·",
            }
        } else if alt {
            match self.code {
                KeyCode::Char(c) => match c.to_ascii_lowercase() {
                    'a' => "alt+a",
                    'b' => "alt+b",
                    'c' => "alt+c",
                    'd' => "alt+d",
                    'e' => "alt+e",
                    'f' => "alt+f",
                    'g' => "alt+g",
                    'h' => "alt+h",
                    'i' => "alt+i",
                    'j' => "alt+j",
                    'k' => "alt+k",
                    'l' => "alt+l",
                    'm' => "alt+m",
                    'n' => "alt+n",
                    'o' => "alt+o",
                    'p' => "alt+p",
                    'q' => "alt+q",
                    'r' => "alt+r",
                    's' => "alt+s",
                    't' => "alt+t",
                    'u' => "alt+u",
                    'v' => "alt+v",
                    'w' => "alt+w",
                    'x' => "alt+x",
                    'y' => "alt+y",
                    'z' => "alt+z",
                    _ => "·",
                },
                KeyCode::Enter => "alt+enter",
                KeyCode::Up => "alt+↑",
                KeyCode::Down => "alt+↓",
                _ => "·",
            }
        } else if shift {
            match self.code {
                KeyCode::Tab | KeyCode::BackTab => "shift+tab",
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

    pub const fn display(&self) -> &'static str {
        let ctrl = self.modifiers.contains(KeyModifiers::CONTROL);
        let alt = self.modifiers.contains(KeyModifiers::ALT);
        let shift = self.modifiers.contains(KeyModifiers::SHIFT);
        let cmd = self.modifiers.contains(KeyModifiers::SUPER);

        if ctrl && shift {
            match self.code {
                KeyCode::Char('c') | KeyCode::Char('C') => "Ctrl+Shift+C",
                _ => "·",
            }
        } else if ctrl {
            match self.code {
                KeyCode::Char(c) => match c.to_ascii_lowercase() {
                    'a' => "Ctrl+A",
                    'b' => "Ctrl+B",
                    'c' => "Ctrl+C",
                    'd' => "Ctrl+D",
                    'e' => "Ctrl+E",
                    'f' => "Ctrl+F",
                    'g' => "Ctrl+G",
                    'h' => "Ctrl+H",
                    'i' => "Ctrl+I",
                    'j' => "Ctrl+J",
                    'k' => "Ctrl+K",
                    'l' => "Ctrl+L",
                    'm' => "Ctrl+M",
                    'n' => "Ctrl+N",
                    'o' => "Ctrl+O",
                    'p' => "Ctrl+P",
                    'q' => "Ctrl+Q",
                    'r' => "Ctrl+R",
                    's' => "Ctrl+S",
                    't' => "Ctrl+T",
                    'u' => "Ctrl+U",
                    'v' => "Ctrl+V",
                    'w' => "Ctrl+W",
                    'x' => "Ctrl+X",
                    'y' => "Ctrl+Y",
                    'z' => "Ctrl+Z",
                    _ => "·",
                },
                KeyCode::Up => "Ctrl+↑",
                KeyCode::Down => "Ctrl+↓",
                KeyCode::Left => "Ctrl+←",
                KeyCode::Right => "Ctrl+→",
                _ => "·",
            }
        } else if alt {
            match self.code {
                KeyCode::Char(c) => match c.to_ascii_lowercase() {
                    'a' => "Alt+A",
                    'b' => "Alt+B",
                    'c' => "Alt+C",
                    'd' => "Alt+D",
                    'e' => "Alt+E",
                    'f' => "Alt+F",
                    'g' => "Alt+G",
                    'h' => "Alt+H",
                    'i' => "Alt+I",
                    'j' => "Alt+J",
                    'k' => "Alt+K",
                    'l' => "Alt+L",
                    'm' => "Alt+M",
                    'n' => "Alt+N",
                    'o' => "Alt+O",
                    'p' => "Alt+P",
                    'q' => "Alt+Q",
                    'r' => "Alt+R",
                    's' => "Alt+S",
                    't' => "Alt+T",
                    'u' => "Alt+U",
                    'v' => "Alt+V",
                    'w' => "Alt+W",
                    'x' => "Alt+X",
                    'y' => "Alt+Y",
                    'z' => "Alt+Z",
                    _ => "·",
                },
                KeyCode::Enter => "Alt+Enter",
                KeyCode::Up => "Alt+↑",
                KeyCode::Down => "Alt+↓",
                _ => "·",
            }
        } else if shift {
            match self.code {
                KeyCode::Tab | KeyCode::BackTab => keyvocab::SHIFT_TAB,
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

// ─────────────────────────────────────────────────────────────────────────────
// Command Registry SSOT Specification Types
// ─────────────────────────────────────────────────────────────────────────────

/// Exhaustive identifier for every executable command in the application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommandId {
    // ── Global (6 Hard-Bound Shortcuts) ──
    Help,
    CommandPalette,
    CancelOrBack,
    InterruptTask,
    Quit,
    CopySelection,

    // ── Session & Composer ──
    SendPrompt,
    QueueFollowUp,
    SteerImmediate,
    InsertNewline,
    HistorySearch,
    FocusTranscript,
    FocusComposer,
    ScrollTranscriptUp,
    ScrollTranscriptDown,

    // ── Transcript Focus ──
    TranscriptMoveUp,
    TranscriptMoveDown,
    TranscriptOpenOrToggle,
    TranscriptTop,
    TranscriptBottom,

    // ── Surface Navigation ──
    NavigateSession,
    NavigateDashboard,
    NavigateSettings,
    OpenTodos,
    OpenQueue,
    OpenTelemetry,
    OpenModels,
    OpenConnections,
    OpenTools,
    OpenMcp,
    OpenSkills,
    OpenPermissions,
    OpenUsage,
    OpenTree,
    OpenBtw,
    OpenSessions,

    // ── Management & Actions ──
    ToggleQueueBlock,
    ClearQueue,
    McpReconnectSelected,
    McpToggleSelected,
    ToolsToggleSelected,
    PermissionsRevokeSelected,
    PermissionsClearAll,
    SkillsToggleDetail,
    ProviderAddConnection,
    ProviderEditSelected,
    ProviderDeleteSelected,
    ProviderToggleFavorite,
    RedrawScreen,
}

/// Scope context where a command is applicable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    Global,
    Session,
    Composer,
    Transcript,
    BrowsePanel,
    BlockingDialog,
}

/// Category of the command for palette grouping and help presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommandCategory {
    Global,
    Navigate,
    Session,
    Actions,
    Settings,
}

impl CommandCategory {
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Global => "Global",
            Self::Navigate => "Navigate",
            Self::Session => "Session",
            Self::Actions => "Actions",
            Self::Settings => "Settings",
        }
    }
}

/// Danger classification for critical/destructive actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DangerLevel {
    Safe,
    Cautious,
    Dangerous,
}

/// Progressive disclosure priority level (L0 to L3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DisclosurePriority {
    /// L0: Maximum of 3 primary actions rendered in the active footer.
    L0Footer,
    /// L1: Local action displayed in a focused region bar.
    L1FocusRegion,
    /// L2: Searchable through the `Ctrl+L` Command Palette.
    L2Palette,
    /// L3: Full contextual reference visible only in F1 Help.
    L3HelpOnly,
}

/// Dynamic availability status for a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    Available,
    Unavailable(&'static str),
}

/// Snapshot of application state passed to availability predicates.
#[derive(Debug, Clone, Copy, Default)]
pub struct AppContext {
    pub active_view: View,
    pub active_modal: Modal,
    pub session_focus: SessionFocusRegion,
    pub is_responding: bool,
    pub has_input: bool,
    pub has_selection: bool,
    pub has_running_task: bool,
    pub in_runner_view: bool,
    pub in_side_view: bool,
    pub queue_count: usize,
    pub has_focused_target: bool,
}

/// Authoritative declaration of a single application command.
#[derive(Debug, Clone, Copy)]
pub struct CommandSpec {
    pub id: CommandId,
    pub label: &'static str,
    pub hint: &'static str,
    pub category: CommandCategory,
    pub scope: Scope,
    pub bindings: &'static [Key],
    pub slash: Option<&'static str>,
    pub availability: fn(&AppContext) -> Availability,
    pub disclosure: DisclosurePriority,
    pub danger: DangerLevel,
    pub description: &'static str,
}

// ─────────────────────────────────────────────────────────────────────────────
// Availability Predicates
// ─────────────────────────────────────────────────────────────────────────────

fn avail_always(_: &AppContext) -> Availability {
    Availability::Available
}

fn avail_session(ctx: &AppContext) -> Availability {
    if ctx.active_view == View::Session && ctx.active_modal == Modal::None {
        Availability::Available
    } else {
        Availability::Unavailable("only in session")
    }
}

fn avail_running(ctx: &AppContext) -> Availability {
    if ctx.is_responding || ctx.has_running_task {
        Availability::Available
    } else {
        Availability::Unavailable("only while running")
    }
}

fn avail_idle_composer(ctx: &AppContext) -> Availability {
    if ctx.active_modal != Modal::None {
        Availability::Unavailable("modal active")
    } else if ctx.is_responding {
        Availability::Unavailable("currently running")
    } else {
        Availability::Available
    }
}

fn avail_selection(ctx: &AppContext) -> Availability {
    if ctx.has_selection {
        Availability::Available
    } else {
        Availability::Unavailable("no active selection")
    }
}

fn avail_queue_nonempty(ctx: &AppContext) -> Availability {
    if ctx.queue_count > 0 {
        Availability::Available
    } else {
        Availability::Unavailable("queue is empty")
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Static Command Registry Master Table
// ─────────────────────────────────────────────────────────────────────────────

pub static COMMAND_REGISTRY: &[CommandSpec] = &[
    // ── 6 Canonical Global Bindings ──
    CommandSpec {
        id: CommandId::Help,
        label: "Help",
        hint: "F1",
        category: CommandCategory::Global,
        scope: Scope::Global,
        bindings: &[Key::F1],
        slash: Some("/help"),
        availability: avail_always,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Safe,
        description: "Show context-sensitive help and key references",
    },
    CommandSpec {
        id: CommandId::CommandPalette,
        label: "Command Palette",
        hint: "Ctrl+L",
        category: CommandCategory::Global,
        scope: Scope::Global,
        bindings: &[Key::CTRL_L],
        slash: Some("/commands"),
        availability: avail_always,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Safe,
        description: "Open unified command palette and surface switcher",
    },
    CommandSpec {
        id: CommandId::CancelOrBack,
        label: "Back / Cancel",
        hint: "Esc",
        category: CommandCategory::Global,
        scope: Scope::Global,
        bindings: &[Key::ESC],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Safe,
        description: "Dismiss active overlay, step back, or return to composer",
    },
    CommandSpec {
        id: CommandId::InterruptTask,
        label: "Interrupt Task",
        hint: "Ctrl+C",
        category: CommandCategory::Global,
        scope: Scope::Global,
        bindings: &[Key::CTRL_C],
        slash: Some("/interrupt"),
        availability: avail_running,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Cautious,
        description: "Interrupt currently executing turn / task",
    },
    CommandSpec {
        id: CommandId::Quit,
        label: "Quit Muta",
        hint: "Ctrl+Q",
        category: CommandCategory::Global,
        scope: Scope::Global,
        bindings: &[Key::CTRL_Q],
        slash: Some("/exit"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Dangerous,
        description: "Exit application gracefully",
    },
    CommandSpec {
        id: CommandId::CopySelection,
        label: "Copy Selection",
        hint: "Ctrl+Shift+C",
        category: CommandCategory::Global,
        scope: Scope::Global,
        bindings: &[Key::CTRL_SHIFT_C, Key::CMD_C],
        slash: None,
        availability: avail_selection,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Copy selected text to clipboard",
    },

    // ── Session & Composer Controls ──
    CommandSpec {
        id: CommandId::SendPrompt,
        label: "Send Prompt",
        hint: "Enter",
        category: CommandCategory::Session,
        scope: Scope::Composer,
        bindings: &[Key::ENTER],
        slash: Some("/send"),
        availability: avail_idle_composer,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Safe,
        description: "Send prompt text to agent",
    },
    CommandSpec {
        id: CommandId::QueueFollowUp,
        label: "Queue Follow-up",
        hint: "Enter (running)",
        category: CommandCategory::Session,
        scope: Scope::Composer,
        bindings: &[Key::ENTER],
        slash: None,
        availability: avail_running,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Safe,
        description: "Enqueue prompt as next-round follow-up message",
    },
    CommandSpec {
        id: CommandId::SteerImmediate,
        label: "Steer Now",
        hint: "Alt+S",
        category: CommandCategory::Session,
        scope: Scope::Composer,
        bindings: &[Key::ALT_S],
        slash: Some("/steer"),
        availability: avail_running,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Cautious,
        description: "Inject prompt immediately at next safe boundary",
    },
    CommandSpec {
        id: CommandId::InsertNewline,
        label: "Insert Newline",
        hint: "Alt+Enter / Ctrl+J",
        category: CommandCategory::Session,
        scope: Scope::Composer,
        bindings: &[Key::ALT_ENTER, Key::CTRL_J],
        slash: None,
        availability: avail_session,
        disclosure: DisclosurePriority::L3HelpOnly,
        danger: DangerLevel::Safe,
        description: "Insert literal newline into composer buffer",
    },
    CommandSpec {
        id: CommandId::HistorySearch,
        label: "Search History",
        hint: "Ctrl+R",
        category: CommandCategory::Session,
        scope: Scope::Composer,
        bindings: &[Key::CTRL_R],
        slash: Some("/history"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Search and recall past prompt history",
    },
    CommandSpec {
        id: CommandId::FocusTranscript,
        label: "Focus Transcript",
        hint: "Tab",
        category: CommandCategory::Session,
        scope: Scope::Composer,
        bindings: &[Key::TAB],
        slash: None,
        availability: avail_session,
        disclosure: DisclosurePriority::L0Footer,
        danger: DangerLevel::Safe,
        description: "Switch focus from Composer to Transcript region",
    },
    CommandSpec {
        id: CommandId::FocusComposer,
        label: "Focus Composer",
        hint: "Tab / Esc",
        category: CommandCategory::Session,
        scope: Scope::Transcript,
        bindings: &[Key::TAB, Key::BACKTAB, Key::ESC],
        slash: None,
        availability: avail_session,
        disclosure: DisclosurePriority::L1FocusRegion,
        danger: DangerLevel::Safe,
        description: "Switch focus from Transcript to Composer region",
    },
    CommandSpec {
        id: CommandId::ScrollTranscriptUp,
        label: "Scroll Transcript Up",
        hint: "PageUp",
        category: CommandCategory::Session,
        scope: Scope::Session,
        bindings: &[Key::PAGE_UP],
        slash: None,
        availability: avail_session,
        disclosure: DisclosurePriority::L3HelpOnly,
        danger: DangerLevel::Safe,
        description: "Scroll transcript viewport upward",
    },
    CommandSpec {
        id: CommandId::ScrollTranscriptDown,
        label: "Scroll Transcript Down",
        hint: "PageDown",
        category: CommandCategory::Session,
        scope: Scope::Session,
        bindings: &[Key::PAGE_DOWN],
        slash: None,
        availability: avail_session,
        disclosure: DisclosurePriority::L3HelpOnly,
        danger: DangerLevel::Safe,
        description: "Scroll transcript viewport downward",
    },

    // ── Transcript Focus Region Actions ──
    CommandSpec {
        id: CommandId::TranscriptMoveUp,
        label: "Previous Step",
        hint: "↑",
        category: CommandCategory::Session,
        scope: Scope::Transcript,
        bindings: &[Key::UP],
        slash: None,
        availability: avail_session,
        disclosure: DisclosurePriority::L1FocusRegion,
        danger: DangerLevel::Safe,
        description: "Move focus to previous interactive step or card",
    },
    CommandSpec {
        id: CommandId::TranscriptMoveDown,
        label: "Next Step",
        hint: "↓",
        category: CommandCategory::Session,
        scope: Scope::Transcript,
        bindings: &[Key::DOWN],
        slash: None,
        availability: avail_session,
        disclosure: DisclosurePriority::L1FocusRegion,
        danger: DangerLevel::Safe,
        description: "Move focus to next interactive step or card",
    },
    CommandSpec {
        id: CommandId::TranscriptOpenOrToggle,
        label: "Open / Expand Step",
        hint: "Enter",
        category: CommandCategory::Session,
        scope: Scope::Transcript,
        bindings: &[Key::ENTER],
        slash: None,
        availability: avail_session,
        disclosure: DisclosurePriority::L1FocusRegion,
        danger: DangerLevel::Safe,
        description: "Expand, collapse, or drill down into focused step",
    },
    CommandSpec {
        id: CommandId::TranscriptTop,
        label: "Transcript Top",
        hint: "Home",
        category: CommandCategory::Session,
        scope: Scope::Transcript,
        bindings: &[Key::HOME],
        slash: None,
        availability: avail_session,
        disclosure: DisclosurePriority::L3HelpOnly,
        danger: DangerLevel::Safe,
        description: "Jump to beginning of transcript",
    },
    CommandSpec {
        id: CommandId::TranscriptBottom,
        label: "Transcript Bottom",
        hint: "End",
        category: CommandCategory::Session,
        scope: Scope::Transcript,
        bindings: &[Key::END],
        slash: None,
        availability: avail_session,
        disclosure: DisclosurePriority::L3HelpOnly,
        danger: DangerLevel::Safe,
        description: "Jump to end of transcript",
    },

    // ── Surface Navigation ──
    CommandSpec {
        id: CommandId::NavigateSession,
        label: "Session",
        hint: "/session",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/session"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Switch to live conversation session view",
    },
    CommandSpec {
        id: CommandId::NavigateDashboard,
        label: "Session Dashboard",
        hint: "/dashboard",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/dashboard"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Open daemon session orchestrator dashboard",
    },
    CommandSpec {
        id: CommandId::NavigateSettings,
        label: "Settings",
        hint: "/settings",
        category: CommandCategory::Settings,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/settings"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Open application and appearance settings",
    },
    CommandSpec {
        id: CommandId::OpenTodos,
        label: "Todos",
        hint: "/todos",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/todos"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "View agent task list and progression status",
    },
    CommandSpec {
        id: CommandId::OpenQueue,
        label: "Queue (Outbox)",
        hint: "/queue",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/queue"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Inspect and manage pending message outbox",
    },
    CommandSpec {
        id: CommandId::OpenTelemetry,
        label: "Session Telemetry",
        hint: "/telemetry",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/telemetry"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "View context token accounting and model telemetry",
    },
    CommandSpec {
        id: CommandId::OpenModels,
        label: "Switch Model",
        hint: "/models",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/models"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Browse and select available LLM models",
    },
    CommandSpec {
        id: CommandId::OpenConnections,
        label: "Connections",
        hint: "/connections",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/connections"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Manage LLM provider endpoints and API credentials",
    },
    CommandSpec {
        id: CommandId::OpenTools,
        label: "Tools",
        hint: "/tools",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/tools"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Inspect and configure session tool capability pool",
    },
    CommandSpec {
        id: CommandId::OpenMcp,
        label: "MCP Servers",
        hint: "/mcp",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/mcp"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Manage Model Context Protocol server connections",
    },
    CommandSpec {
        id: CommandId::OpenSkills,
        label: "Skills",
        hint: "/skills",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/skills"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Inspect discovered workspace skills and guidelines",
    },
    CommandSpec {
        id: CommandId::OpenPermissions,
        label: "Permissions",
        hint: "/permissions",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/permissions"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Review and revoke cached tool execution rules",
    },
    CommandSpec {
        id: CommandId::OpenUsage,
        label: "Usage Statistics",
        hint: "/usage",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/usage"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "View cross-session token usage and activity ledger",
    },
    CommandSpec {
        id: CommandId::OpenTree,
        label: "Session Tree",
        hint: "/tree",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/tree"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "View DAG tree of session rounds and turns",
    },
    CommandSpec {
        id: CommandId::OpenBtw,
        label: "Asides (/btw)",
        hint: "/btw",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/btw"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "List background aside conversations",
    },
    CommandSpec {
        id: CommandId::OpenSessions,
        label: "Sessions",
        hint: "/sessions",
        category: CommandCategory::Navigate,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/sessions"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Switch between saved project sessions",
    },

    // ── Management Actions ──
    CommandSpec {
        id: CommandId::ToggleQueueBlock,
        label: "Block / Resume Queue",
        hint: "Action",
        category: CommandCategory::Actions,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/queue block"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Cautious,
        description: "Toggle dispatch latch on outgoing follow-up messages",
    },
    CommandSpec {
        id: CommandId::ClearQueue,
        label: "Clear Queue",
        hint: "Action",
        category: CommandCategory::Actions,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/queue clear"),
        availability: avail_queue_nonempty,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Dangerous,
        description: "Discard all staged outgoing messages in outbox",
    },
    CommandSpec {
        id: CommandId::McpReconnectSelected,
        label: "Reconnect MCP Server",
        hint: "Action",
        category: CommandCategory::Actions,
        scope: Scope::BrowsePanel,
        bindings: &[],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Restart and reconnect highlighted MCP server",
    },
    CommandSpec {
        id: CommandId::McpToggleSelected,
        label: "Toggle MCP Server",
        hint: "Space",
        category: CommandCategory::Actions,
        scope: Scope::BrowsePanel,
        bindings: &[],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L1FocusRegion,
        danger: DangerLevel::Safe,
        description: "Enable or disable highlighted MCP server for session",
    },
    CommandSpec {
        id: CommandId::ToolsToggleSelected,
        label: "Toggle Tool",
        hint: "Space",
        category: CommandCategory::Actions,
        scope: Scope::BrowsePanel,
        bindings: &[],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L1FocusRegion,
        danger: DangerLevel::Safe,
        description: "Enable or disable selected tool",
    },
    CommandSpec {
        id: CommandId::PermissionsRevokeSelected,
        label: "Revoke Permission Rule",
        hint: "Space",
        category: CommandCategory::Actions,
        scope: Scope::BrowsePanel,
        bindings: &[],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L1FocusRegion,
        danger: DangerLevel::Cautious,
        description: "Revoke selected cached execution rule",
    },
    CommandSpec {
        id: CommandId::PermissionsClearAll,
        label: "Revoke All Permissions",
        hint: "Action",
        category: CommandCategory::Actions,
        scope: Scope::BrowsePanel,
        bindings: &[],
        slash: Some("/permissions clear"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Dangerous,
        description: "Clear all cached execution rules for workspace",
    },
    CommandSpec {
        id: CommandId::SkillsToggleDetail,
        label: "Toggle Skill Details",
        hint: "Enter",
        category: CommandCategory::Actions,
        scope: Scope::BrowsePanel,
        bindings: &[],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L1FocusRegion,
        danger: DangerLevel::Safe,
        description: "Expand or collapse details and guidance for selected skill",
    },
    CommandSpec {
        id: CommandId::ProviderAddConnection,
        label: "Add Provider Connection",
        hint: "Action",
        category: CommandCategory::Actions,
        scope: Scope::BrowsePanel,
        bindings: &[],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Configure a new provider or custom endpoint",
    },
    CommandSpec {
        id: CommandId::ProviderEditSelected,
        label: "Edit Provider / Model",
        hint: "Action",
        category: CommandCategory::Actions,
        scope: Scope::BrowsePanel,
        bindings: &[],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Edit credentials or capability overrides for selection",
    },
    CommandSpec {
        id: CommandId::ProviderDeleteSelected,
        label: "Delete Provider Connection",
        hint: "Action",
        category: CommandCategory::Actions,
        scope: Scope::BrowsePanel,
        bindings: &[],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Dangerous,
        description: "Delete custom provider endpoint",
    },
    CommandSpec {
        id: CommandId::ProviderToggleFavorite,
        label: "Toggle Favorite Model",
        hint: "Action",
        category: CommandCategory::Actions,
        scope: Scope::BrowsePanel,
        bindings: &[],
        slash: None,
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Star or unstar model for quick switching",
    },
    CommandSpec {
        id: CommandId::RedrawScreen,
        label: "Redraw Screen",
        hint: "Action",
        category: CommandCategory::Actions,
        scope: Scope::Global,
        bindings: &[],
        slash: Some("/redraw"),
        availability: avail_always,
        disclosure: DisclosurePriority::L2Palette,
        danger: DangerLevel::Safe,
        description: "Force full TUI terminal redraw and layout sync",
    },
];

// ─────────────────────────────────────────────────────────────────────────────
// Registry Lookup & Derivation Utilities
// ─────────────────────────────────────────────────────────────────────────────

/// Complete set of registered command specs.
pub fn all_commands() -> &'static [CommandSpec] {
    COMMAND_REGISTRY
}

/// Look up a command by its unique identifier.
pub fn find_command(id: CommandId) -> Option<&'static CommandSpec> {
    COMMAND_REGISTRY.iter().find(|cmd| cmd.id == id)
}

/// Look up a command by its slash trigger.
pub fn find_by_slash(slash: &str) -> Option<&'static CommandSpec> {
    let clean = slash.trim().to_ascii_lowercase();
    COMMAND_REGISTRY.iter().find(|cmd| {
        cmd.slash.map(|s| s.eq_ignore_ascii_case(&clean)).unwrap_or(false)
    })
}

/// Resolve one of the 6 canonical global bindings.
///
/// Returns `Some(CommandId)` only when the key matches a designated global binding.
pub fn resolve_global_key(key: Key) -> Option<CommandId> {
    if key == Key::F1 {
        Some(CommandId::Help)
    } else if key == Key::CTRL_L {
        Some(CommandId::CommandPalette)
    } else if key == Key::ESC {
        Some(CommandId::CancelOrBack)
    } else if key == Key::CTRL_C {
        Some(CommandId::InterruptTask)
    } else if key == Key::CTRL_Q {
        Some(CommandId::Quit)
    } else if key == Key::CTRL_SHIFT_C || key == Key::CMD_C {
        Some(CommandId::CopySelection)
    } else {
        None
    }
}

/// Derive command palette entries with current availability flags.
pub fn commands_for_palette(ctx: &AppContext) -> Vec<(&'static CommandSpec, Availability)> {
    COMMAND_REGISTRY
        .iter()
        .filter(|cmd| cmd.disclosure >= DisclosurePriority::L2Palette || cmd.scope == Scope::Global)
        .map(|cmd| (cmd, (cmd.availability)(ctx)))
        .collect()
}

/// Derive contextual commands for the dynamic F1 Help modal.
pub fn commands_for_help(ctx: &AppContext) -> Vec<&'static CommandSpec> {
    COMMAND_REGISTRY
        .iter()
        .filter(|cmd| {
            // Include globals, current scope commands, and relevant navigate/settings commands
            match cmd.scope {
                Scope::Global => true,
                Scope::Session => ctx.active_view == View::Session && ctx.active_modal == Modal::None,
                Scope::Composer => {
                    ctx.active_view == View::Session
                        && ctx.active_modal == Modal::None
                        && ctx.session_focus == SessionFocusRegion::Composer
                }
                Scope::Transcript => {
                    ctx.active_view == View::Session
                        && ctx.active_modal == Modal::None
                        && ctx.session_focus == SessionFocusRegion::Transcript
                }
                Scope::BrowsePanel => ctx.active_modal != Modal::None,
                Scope::BlockingDialog => ctx.active_modal != Modal::None,
            }
        })
        .collect()
}

/// Derive L0 footer hints for the current context (maximum of 3 items).
pub fn footer_hints_for_context(ctx: &AppContext) -> Vec<(&'static str, &'static str)> {
    if ctx.active_modal != Modal::None {
        // Browse Panel or Dialog footer
        vec![
            (keyvocab::ARROWS_UD, "select"),
            (Key::ENTER.display(), "confirm"),
            (Key::ESC.display(), "close"),
        ]
    } else if ctx.active_view == View::Session {
        match ctx.session_focus {
            SessionFocusRegion::Composer => {
                if ctx.is_responding {
                    vec![
                        (Key::TAB.display(), "transcript"),
                        (Key::ALT_S.display(), "steer now"),
                        (Key::ENTER.display(), "queue follow-up"),
                    ]
                } else {
                    vec![
                        (Key::TAB.display(), "transcript"),
                        (Key::ENTER.display(), "send"),
                    ]
                }
            }
            SessionFocusRegion::Transcript => {
                vec![
                    (keyvocab::ARROWS_UD, "move"),
                    (Key::ENTER.display(), "open"),
                    (Key::ESC.display(), "compose"),
                ]
            }
        }
    } else {
        vec![
            (Key::F1.display(), "help"),
            (Key::CTRL_L.display(), "commands"),
            (Key::ESC.display(), "back"),
        ]
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_keys_resolve_correctly() {
        assert_eq!(resolve_global_key(Key::F1), Some(CommandId::Help));
        assert_eq!(resolve_global_key(Key::CTRL_L), Some(CommandId::CommandPalette));
        assert_eq!(resolve_global_key(Key::ESC), Some(CommandId::CancelOrBack));
        assert_eq!(resolve_global_key(Key::CTRL_C), Some(CommandId::InterruptTask));
        assert_eq!(resolve_global_key(Key::CTRL_Q), Some(CommandId::Quit));
        assert_eq!(resolve_global_key(Key::CTRL_SHIFT_C), Some(CommandId::CopySelection));
        assert_eq!(resolve_global_key(Key::CMD_C), Some(CommandId::CopySelection));
    }

    #[test]
    fn non_global_keys_do_not_resolve_as_global() {
        assert_eq!(resolve_global_key(Key::ENTER), None);
        assert_eq!(resolve_global_key(Key::TAB), None);
        assert_eq!(resolve_global_key(Key::CTRL_R), None);
        assert_eq!(resolve_global_key(Key::ctrl('x')), None);
        assert_eq!(resolve_global_key(Key::alt('x')), None);
    }

    #[test]
    fn all_commands_have_valid_labels_and_descriptions() {
        for cmd in COMMAND_REGISTRY {
            assert!(!cmd.label.is_empty(), "command {:?} has empty label", cmd.id);
            assert!(!cmd.description.is_empty(), "command {:?} has empty description", cmd.id);
        }
    }

    #[test]
    fn find_by_slash_resolves_all_slash_triggers() {
        assert_eq!(find_by_slash("/models").map(|c| c.id), Some(CommandId::OpenModels));
        assert_eq!(find_by_slash("/settings").map(|c| c.id), Some(CommandId::NavigateSettings));
        assert_eq!(find_by_slash("/help").map(|c| c.id), Some(CommandId::Help));
        assert_eq!(find_by_slash("/commands").map(|c| c.id), Some(CommandId::CommandPalette));
    }

    #[test]
    fn footer_hints_respect_max_three_items() {
        let mut ctx = AppContext::default();
        ctx.active_view = View::Session;
        ctx.session_focus = SessionFocusRegion::Composer;
        ctx.is_responding = false;

        let hints = footer_hints_for_context(&ctx);
        assert!(hints.len() <= 3);

        ctx.is_responding = true;
        let hints_running = footer_hints_for_context(&ctx);
        assert!(hints_running.len() <= 3);

        ctx.session_focus = SessionFocusRegion::Transcript;
        let hints_transcript = footer_hints_for_context(&ctx);
        assert!(hints_transcript.len() <= 3);
    }
}
