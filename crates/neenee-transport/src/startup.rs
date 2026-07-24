//! CLI bootstrap helpers: arg parsing, startup-mode selection, tracing init,
//! and the slash-command vocabulary used to distinguish built-in commands
//! from user-defined ones.
//!
//! These are pure (or near-pure: `init_tracing` does touch the env / filesystem)
//! helpers that lived inline at the top of `main.rs` before being grouped here.
//! They have no dependence on `main.rs` state.

use std::path::PathBuf;
use tracing_appender::non_blocking::WorkerGuard;

use neenee_persistence::paths;

/// Single source of truth for the built-in slash-command vocabulary.
///
/// Each entry `Variant = "/name" : "description"` generates a [`BuiltinCmd`]
/// enum variant, a row in [`BuiltinCmd::ALL`] (consumed by input completion,
/// `/help`, and the custom-command filter), and an arm of
/// [`BuiltinCmd::from_slash`].
///
/// The dispatch `match` in `main.rs` is over `Option<BuiltinCmd>` and is kept
/// non-exhaustive (no `Some(_)` catch-all). Adding a variant here without a
/// matching handler arm is therefore a **compile error**, so completion,
/// `/help`, and dispatch can never drift — a command appears in all three or
/// the build breaks.
macro_rules! define_builtin_commands {
    ( $( $variant:ident = $name:literal : $desc:literal ),+ $(,)? ) => {
        /// The set of built-in slash commands. Generated from a single
        /// declarative list — see `define_builtin_commands`.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum BuiltinCmd {
            $( $variant ),+
        }

        impl BuiltinCmd {
            /// Every built-in command as `(slash_name, description)`, in
            /// declaration order. Completion, `/help`, and the custom-command
            /// filter all read from this — it is the only place command
            /// metadata is written.
            pub const ALL: &[(&'static str, &'static str)] = &[ $( ($name, $desc) ),+ ];

            /// Parse a `/<name>` token into a variant, or `None` when it is
            /// not a built-in (i.e. a custom command). The dispatch `match`
            /// consumes the `None` arm to run the custom-command path.
            pub fn from_slash(input: &str) -> Option<Self> {
                $( if input == $name { return Some(BuiltinCmd::$variant); } )+
                None
            }
        }
    };
}

define_builtin_commands! {
    Models      = "/models"       : "Switch the active model",
    Connections = "/connections"  : "Manage LLM provider connections",
    Tools       = "/tools"        : "Manage session tools (enable/disable)",
    Mcp         = "/mcp"          : "Manage MCP servers (enable/disable, reconnect)",
    Compact     = "/compact"      : "Compact older complete turns now",
    Clear       = "/clear"        : "Clear the conversation history",
    Permissions = "/permissions"  : "Show or clear always-allowed tool rules",
    Config      = "/config"       : "Open user configuration",
    Unattended  = "/unattended"   : "Toggle unattended mode — agent runs without human intervention (on/off)",
    Review      = "/review"       : "Run an on-demand session-review diagnostic of the current turn",
    Search      = "/search"       : "Semantic search over the project's session history",
    Session     = "/session"      : "Manage durable sessions (status|list|resume|fork|open|new)",
    Sessions    = "/sessions"     : "Browse past sessions",
    Btw         = "/btw"          : "Open a side conversation that runs alongside the main session",
    Resume      = "/resume"       : "Resume the most recent or selected session",
    Pursue      = "/pursue"       : "Pursue a condition: drive the agent until it is met, or manage the pursuit",
    Repeat      = "/repeat"       : "Schedule a prompt on a cron: /repeat <cron> <prompt>",
    Skills      = "/skills"       : "List or reload available skills (list|reload)",
    Skill       = "/skill"        : "Load a skill by name",
    Init        = "/init"         : "Initialize a .neenee/ config tree",
    Export      = "/export"       : "Export this conversation to the clipboard as Markdown",
    Debug       = "/debug"        : "Debug tools: /debug trace on|off, /debug preview (dry run)",
    Help        = "/help"         : "Show available commands and keybindings",
    Exit        = "/exit"         : "Exit the program",
}

/// Split `/<name> <arguments>` into `(name_without_slash, arguments_trimmed)`.
/// A bare `/name` with no arguments yields an empty arguments string.
pub fn split_custom_command(input: &str) -> (&str, &str) {
    let input = input.trim();
    let split_at = input.find(char::is_whitespace).unwrap_or(input.len());
    let (name, arguments) = input.split_at(split_at);
    (name.trim_start_matches('/'), arguments.trim())
}

#[derive(Debug)]
pub enum StartupMode {
    Fresh,
    Resume(Option<String>),
    Picker,
    Doctor,
    /// Attach the TUI to an already-running session server for this project
    /// (`neenee --attach [id]`). The id is the session to attach to; `None`
    /// attaches to whatever the server hosts. Purely client-side: the caller
    /// must intercept this variant BEFORE invoking `bootstrap::assemble` — no
    /// local harness is assembled in attach mode.
    Attach(Option<String>),
    /// Render a single UI component in isolation for interactive development
    /// (`neenee showcase <component>`). No agent, no session, no network —
    /// just the component's model + renderer wired to a real terminal so you
    /// can see and interact with it standalone.
    #[cfg(debug_assertions)]
    Showcase(String),
}

pub fn parse_args(args: Vec<String>) -> (StartupMode, Option<PathBuf>, bool, bool) {
    let mut iter = args.into_iter().peekable();
    let mut project: Option<PathBuf> = None;
    let mut unattended = false;
    let mut single_instance = false;
    // `Some(inner)` once `--attach` is seen; `inner` is the optional session id.
    let mut attach: Option<Option<String>> = None;
    let mut rest = Vec::new();
    while let Some(arg) = iter.next() {
        if arg == "--project" {
            project = iter.next().map(PathBuf::from);
        } else if let Some(value) = arg.strip_prefix("--project=") {
            project = Some(PathBuf::from(value));
        } else if arg == "--attach" {
            // `--attach <id>`: the next token is the session id only when it
            // is not another flag — a following `--flag` (or end of args)
            // means "attach to whatever the server hosts".
            let id = match iter.peek() {
                Some(next) if !next.starts_with("--") => iter.next(),
                _ => None,
            };
            attach = Some(id);
        } else if let Some(value) = arg.strip_prefix("--attach=") {
            attach = Some(Some(value.to_string()));
        } else if arg == "--unattended" {
            unattended = true;
        } else if arg == "--single-instance" {
            single_instance = true;
        } else {
            rest.push(arg);
        }
    }

    if let Some(id) = attach {
        if rest.is_empty() {
            return (StartupMode::Attach(id), project, unattended, single_instance);
        }
        // `--attach` with a positional subcommand is ambiguous (the client
        // drives a remote session; local modes like resume/doctor do not
        // compose with it).
        eprintln!(
            "--attach cannot be combined with '{}'. Usage:\n  neenee --attach [id]    attach to a session server (spawning one if none is running)\n\nOptions:\n  --project <path>        operate on the project at <path>",
            rest[0]
        );
        std::process::exit(2);
    }

    let mode = match rest.as_slice() {
        [] => StartupMode::Fresh,
        [cmd] if cmd == "resume" => StartupMode::Picker,
        [cmd, id] if cmd == "resume" => StartupMode::Resume(Some(id.clone())),
        [cmd, ..] if cmd == "doctor" => StartupMode::Doctor,
        #[cfg(debug_assertions)]
        [cmd, component] if cmd == "showcase" => StartupMode::Showcase(component.clone()),
        [cmd, ..] => {
            // `showcase` is a debug-only subcommand; omit it from the release
            // usage string so we don't advertise a command that doesn't exist.
            #[cfg(debug_assertions)]
            let showcase_line =
                "  neenee showcase <name>  render a single UI component standalone\n";
            #[cfg(not(debug_assertions))]
            let showcase_line = "";
            eprintln!(
                "Unknown command '{}'. Usage:\n  neenee                  start a fresh session\n  neenee resume [id]      resume a session (picker when no id)\n  neenee --attach [id]    attach to a session server (spawning one if none is running)\n  neenee doctor           verify stored session integrity\n{showcase_line}\nOptions:\n  --project <path>        operate on the project at <path>\n  --unattended            run without human intervention (no confirmations, no questions) this session\n  --single-instance       require exclusive per-project lock (pre-ADR-0018 default)",
                cmd
            );
            std::process::exit(2);
        }
    };
    (mode, project, unattended, single_instance)
}

/// Initialise file-based tracing for the process.
///
/// A TUI cannot log to stdout (it would corrupt the display), so tracing
/// always writes to a **file** under the XDG state directory:
/// `$XDG_STATE_HOME/neenee/log/neenee.log` (daily-rotated, so each calendar
/// day rolls into its own file).
///
/// # Verbosity
///
/// `NEENEE_LOG` controls the level. Recognised values:
/// - `off` — disable tracing entirely (no file, no guard).
/// - `error` / `warn` / `info` / `debug` / `trace` — global level.
/// - _unrecognised / unset_ — defaults to `info`.
///
/// `RUST_LOG` still takes precedence per-target when set (e.g.
/// `RUST_LOG=neenee=debug,neenee_transport=trace`), because
/// `EnvFilter::try_from_default_env` is consulted first. This keeps the
/// familiar `RUST_LOG` ergonomics for fine-grained filtering while giving a
/// sane always-on default out of the box.
///
/// The returned guard flushes the non-blocking writer on drop and must live
/// for the whole process (main binds it to a local).
pub fn init_tracing() -> Option<WorkerGuard> {
    // Level via NEENEE_LOG; "off" disables tracing entirely.
    let level = std::env::var("NEENEE_LOG").unwrap_or_else(|_| String::from("info"));
    if level.eq_ignore_ascii_case("off") {
        return None;
    }

    // Resolve the XDG state log directory and create it lazily.
    let dir = paths::get().log_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        // Last-resort: never block startup over logging. Drop to stderr-free
        // no-op by returning None; diagnostics are impossible from a TUI anyway.
        eprintln!("neenee: could not create log dir {}: {e}", dir.display());
        return None;
    }

    let (writer, guard) =
        tracing_appender::non_blocking(tracing_appender::rolling::daily(&dir, "neenee.log"));

    // Per-target RUST_LOG wins; otherwise apply the NEENEE_LOG level to the
    // neenee crates and keep everything else quiet.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        let l = level.to_ascii_lowercase();
        let lvl = matches!(l.as_str(), "error" | "warn" | "info" | "debug" | "trace")
            .then_some(l.as_str())
            .unwrap_or("info");
        tracing_subscriber::EnvFilter::new(format!(
            "neenee={lvl},neenee_core={lvl},neenee_transport={lvl}"
        ))
    });
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false)
        .init();
    tracing::info!(log_dir = %dir.display(), level = %level, "neenee tracing initialised");
    Some(guard)
}


#[cfg(test)]
mod tests {
    use super::*;

    fn args(tokens: &[&str]) -> Vec<String> {
        tokens.iter().map(|t| t.to_string()).collect()
    }

    #[test]
    fn no_args_is_fresh() {
        let (mode, project, unattended, single) = parse_args(Vec::new());
        assert!(matches!(mode, StartupMode::Fresh));
        assert!(project.is_none());
        assert!(!unattended);
        assert!(!single);
    }

    #[test]
    fn resume_forms_are_unchanged() {
        let (mode, ..) = parse_args(args(&["resume"]));
        assert!(matches!(mode, StartupMode::Picker));
        let (mode, ..) = parse_args(args(&["resume", "abc"]));
        assert!(matches!(mode, StartupMode::Resume(Some(id)) if id == "abc"));
    }

    #[test]
    fn attach_bare_means_any_session() {
        let (mode, ..) = parse_args(args(&["--attach"]));
        assert!(matches!(mode, StartupMode::Attach(None)));
    }

    #[test]
    fn attach_with_id_both_styles() {
        let (mode, ..) = parse_args(args(&["--attach", "sess-1"]));
        assert!(matches!(mode, StartupMode::Attach(Some(id)) if id == "sess-1"));
        let (mode, ..) = parse_args(args(&["--attach=sess-2"]));
        assert!(matches!(mode, StartupMode::Attach(Some(id)) if id == "sess-2"));
    }

    #[test]
    fn attach_does_not_swallow_a_following_flag() {
        // `--attach --project /p` must parse as Attach(None) + project, not
        // as an attach id of "--project".
        let (mode, project, ..) = parse_args(args(&["--attach", "--project", "/p"]));
        assert!(matches!(mode, StartupMode::Attach(None)));
        assert_eq!(project, Some(PathBuf::from("/p")));
    }

    #[test]
    fn attach_combines_with_project_flag() {
        let (mode, project, ..) = parse_args(args(&["--project", "/p", "--attach", "s"]));
        assert!(matches!(mode, StartupMode::Attach(Some(id)) if id == "s"));
        assert_eq!(project, Some(PathBuf::from("/p")));
    }

    #[test]
    fn project_flag_still_works_without_attach() {
        let (mode, project, ..) = parse_args(args(&["--project=/q"]));
        assert!(matches!(mode, StartupMode::Fresh));
        assert_eq!(project, Some(PathBuf::from("/q")));
    }
}
