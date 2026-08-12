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
            ///
            /// Canonical names are matched against the declarative list;
            /// backward-compatible aliases (renamed commands) are then matched
            /// by [`BuiltinCmd::from_alias`] so old invocations keep working
            /// without appearing in completion / `/help`.
            pub fn from_slash(input: &str) -> Option<Self> {
                $( if input == $name { return Some(BuiltinCmd::$variant); } )+
                Self::from_alias(input)
            }
        }
    };
}

define_builtin_commands! {
    Models      = "/models"       : "Switch the active model",
    Connections = "/connections"  : "Manage LLM provider connections",
    Tools       = "/tools"        : "Manage session tools (enable/disable)",
    Mcp         = "/mcp"          : "Manage MCP servers (enable/disable, reconnect)",
    Compact     = "/compact"      : "Compact older complete rounds now",
    New         = "/new"          : "Start a new session, keeping the current one in history",
    Permissions = "/permissions"  : "Show or clear always-allowed tool rules",
    Config      = "/config"       : "Open user configuration",
    Autopilot  = "/autopilot"   : "Toggle autopilot mode — agent runs without human intervention (on|off; no argument toggles)",
    Principal   = "/principal"    : "Switch the principal role (code|architect|reviewer|security) — changes persona and capability scope",
    Review      = "/review"       : "Run an on-demand session-review diagnostic of the current round",
    Search      = "/search"       : "Semantic search over the project's session history",
    Session     = "/session"      : "Manage durable sessions (status|list|resume|fork|open|new)",
    Sessions    = "/sessions"     : "Browse past sessions",
    Dashboard   = "/dashboard"    : "Session dashboard — live status and control over every daemon session",
    Btw         = "/btw"          : "Open a side conversation that runs alongside the main session",
    Resume      = "/resume"       : "Resume the most recent or selected session",
    Repeat      = "/repeat"       : "Schedule a prompt on a cron: /repeat <cron> <prompt>",
    Schedule    = "/schedule"     : "Schedule a prompt: cron (recurring) or countdown/absolute-time (one-shot). /schedule <when> <prompt>",
    Skills      = "/skills"       : "List or reload available skills (list|reload)",
    Skill       = "/skill"        : "Load a skill by name",
    Init        = "/init"         : "Initialize a .neenee/ config tree",
    Reload      = "/reload"       : "Re-read config.toml and apply changes live (MCP servers, permissions, bash policy, hooks)",
    Trust       = "/trust"        : "Trust this project's .neenee/config.toml (MCP servers + hooks) and load them",
    Untrust     = "/untrust"      : "Revoke trust for this project (disconnects MCP, unloads hooks)",
    Export      = "/export"       : "Export this conversation to the clipboard as Markdown",
    Debug       = "/debug"        : "Debug tools: /debug trace on|off, /debug preview (dry run)",
    Help        = "/help"         : "Show available commands and keybindings",
    Exit        = "/exit"         : "Exit the program",
}

impl BuiltinCmd {
    /// Backward-compatible aliases for renamed commands. These resolve exactly
    /// like their canonical target but are deliberately absent from
    /// [`BuiltinCmd::ALL`], so they never appear in completion or `/help`.
    fn from_alias(input: &str) -> Option<Self> {
        match input {
            // `/host` was the pre-dashboard name (it leaked the daemon "host"
            // concept, ADR-0096); the surface is now the session dashboard.
            "/host" => Some(BuiltinCmd::Dashboard),
            _ => None,
        }
    }
}

/// Trigger-word → command suggestions ("did you mean …"), the
/// **presentation-only** counterpart of [`BuiltinCmd::from_alias`].
///
/// Each row is `(trigger, target_slash, reason)`:
///
/// - `trigger` — a bare word the user might type after `/` (no slash, no
///   arguments). It is **never parsed as a command**: unlike a `from_alias`
///   alias it does not dispatch, does not accept arguments, and is invisible
///   to every consumer of [`BuiltinCmd::ALL`] — it only produces a completion
///   popup row pointing at the real command.
/// - `target_slash` — the canonical built-in (leading slash) the suggestion
///   accepts to; it must resolve through [`BuiltinCmd::from_slash`].
/// - `reason` — the one-line hint shown next to the target in the popup.
///
/// This is the place to catch retired commands, common synonyms, and foreign
/// idioms and steer them onto the supported vocabulary without growing the
/// executable surface. Adding a row here is all it takes: completion reads
/// the table through [`suggest_for_trigger`].
pub const TRIGGER_WORD_SUGGESTIONS: &[(&str, &str, &str)] = &[
    // `/clear` used to wipe the live transcript in place — a destructive,
    // data-losing gesture. It is retired in favour of `/new`, which opens a
    // fresh session and keeps the old one on disk. Steer the muscle memory.
    (
        "clear",
        "/new",
        "Clearing in place is gone: /new starts a fresh session and keeps this one",
    ),
    (
        "reset",
        "/new",
        "/new starts a fresh session, keeping the current one in history",
    ),
    (
        "continue",
        "/resume",
        "/resume picks the session up where it left off",
    ),
];

/// Resolve a bare word typed after `/` into a "did you mean" suggestion from
/// [`TRIGGER_WORD_SUGGESTIONS`]. Returns `(target_slash, reason)` for an exact
/// match, `None` otherwise. Exact-match only: a partial trigger (`/cle`) is
/// still prose-in-progress, and a non-trigger word is an unknown command.
pub fn suggest_for_trigger(word: &str) -> Option<(&'static str, &'static str)> {
    TRIGGER_WORD_SUGGESTIONS
        .iter()
        .find(|(trigger, _, _)| *trigger == word)
        .map(|(_, target, reason)| (*target, *reason))
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
    /// `neenee resume` with no id: pop the sessions picker overlay so the
    /// user can choose which session to resume. Distinct from
    /// `Resume(None)` (which would auto-resume the most-recent session) so
    /// the two stay explicit.
    Picker,
    Doctor,
    /// Attach the TUI to an already-running session server for this project
    /// (`neenee attach [id]`). The id is the session to attach to; `None`
    /// attaches to whatever the server hosts. Purely client-side: the caller
    /// must intercept this variant BEFORE invoking `bootstrap::assemble` — no
    /// local harness is assembled in attach mode.
    Attach(Option<String>),
    /// `neenee serve [--port <n>] [--public]` (ADR-0094, renamed from the
    /// never-released `neenee daemon` of ADR-0089): run the headless
    /// multi-session host in the FOREGROUND (equivalent to the
    /// `neenee-server` binary). The caller intercepts this before `assemble`
    /// and runs the host main instead.
    Serve {
        port: u16,
        public: bool,
        detach: bool,
    },
    /// `neenee status [--watch] [--json] [--all]` (ADR-0093): observe the
    /// project's session host — one snapshot and exit, a live table with `watch`,
    /// JSON frames with `json`. Purely client-side: never spawns a host and
    /// never assembles a local harness.
    Status {
        watch: bool,
        json: bool,
        include_idle: bool,
    },
    /// `neenee dashboard`: open the full-screen session dashboard directly
    /// (the interactive sibling of `status`). The client attaches to the
    /// daemon's most-recently-active hosted session purely as the underlying
    /// TUI carrier, then raises the dashboard over it; Esc from that opening
    /// dashboard quits (there is no conversation the user asked for behind
    /// it), while Enter on a row attaches to that session as usual. Like
    /// `status` it never spawns a daemon — observing requires a running host.
    Dashboard,
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
    let mut autopilot = false;
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
        } else if arg == "--autopilot" {
            autopilot = true;
        } else if arg == "--single-instance" {
            single_instance = true;
        } else {
            rest.push(arg);
        }
    }

    if let Some(id) = attach {
        if rest.is_empty() {
            return (StartupMode::Attach(id), project, autopilot, single_instance);
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

    // `status` is the only subcommand whose trailing flags are parsed here
    // (`--watch`/`--json`/`--all` landed in `rest` because they follow the
    // positional command).
    if rest.first().map(String::as_str) == Some("status") {
        let mut watch = false;
        let mut json = false;
        let mut include_idle = false;
        for flag in &rest[1..] {
            match flag.as_str() {
                "--watch" => watch = true,
                "--json" => json = true,
                "--all" => include_idle = true,
                other => {
                    eprintln!(
                        "Unknown status option '{other}'. Usage:\n  neenee status [--watch] [--json] [--all]\n\n  --watch   keep streaming live updates\n  --json    emit one JSON frame per update\n  --all     include idle sessions (default: only sessions needing attention)"
                    );
                    std::process::exit(2);
                }
            }
        }
        return (
            StartupMode::Status {
                watch,
                json,
                include_idle,
            },
            project,
            autopilot,
            single_instance,
        );
    }

    // `serve` (ADR-0094): the foreground multi-session host. `--port` /
    // `--public` mirror the `neenee-server` binary's flags.
    if rest.first().map(String::as_str) == Some("serve") {
        let mut port: u16 = 0;
        let mut public = false;
        let mut detach = false;
        let mut flags = rest[1..].iter().peekable();
        while let Some(flag) = flags.next() {
            match flag.as_str() {
                "--port" => {
                    let value = flags.next().unwrap_or_else(|| {
                        eprintln!("--port requires a value");
                        std::process::exit(2);
                    });
                    port = value.parse().unwrap_or_else(|_| {
                        eprintln!("invalid --port value '{value}'");
                        std::process::exit(2);
                    });
                }
                "--public" => public = true,
                "--detach" => detach = true,
                other => {
                    eprintln!(
                        "Unknown serve option '{other}'. Usage:\n  neenee serve [--port <n>] [--public] [--detach]\n\n  --port <n>  listen on port <n> (default: OS-assigned)\n  --public    bind all interfaces and require a bearer token\n  --detach    fork into the background and return"
                    );
                    std::process::exit(2);
                }
            }
        }
        return (
            StartupMode::Serve {
                port,
                public,
                detach,
            },
            project,
            autopilot,
            single_instance,
        );
    }

    let mode = match rest.as_slice() {
        [] => StartupMode::Fresh,
        [cmd] if cmd == "resume" => StartupMode::Picker,
        [cmd, id] if cmd == "resume" => StartupMode::Resume(Some(id.clone())),
        [cmd] if cmd == "dashboard" => StartupMode::Dashboard,
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
                "Unknown command '{}'. Usage:\n  neenee                  start a fresh session\n  neenee resume [id]      resume a session (picker when no id)\n  neenee serve [--port <n>] [--public] [--detach]\n                          run the session daemon (foreground, or background with --detach)\n  neenee attach [id]      attach the TUI to a session the host serves (spawning one if none is running)\n  neenee status [--watch] [--json] [--all]\n                          show the host's sessions needing attention\n  neenee dashboard        open the full-screen session dashboard\n  neenee doctor           verify stored session integrity\n{showcase_line}\nOptions:\n  --project <path>        operate on the project at <path>\n  --autopilot            run without human intervention (no confirmations, no questions) this session\n  --single-instance       require exclusive per-project lock (pre-ADR-0018 default)",
                cmd
            );
            std::process::exit(2);
        }
    };
    (mode, project, autopilot, single_instance)
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
    fn dashboard_is_the_canonical_command() {
        assert!(matches!(
            BuiltinCmd::from_slash("/dashboard"),
            Some(BuiltinCmd::Dashboard)
        ));
    }

    #[test]
    fn host_resolves_as_dashboard_alias_but_is_not_listed() {
        // The renamed command still parses, but stays out of completion/help.
        assert!(matches!(
            BuiltinCmd::from_slash("/host"),
            Some(BuiltinCmd::Dashboard)
        ));
        assert!(!BuiltinCmd::ALL.iter().any(|(name, _)| *name == "/host"));
        assert!(
            BuiltinCmd::ALL
                .iter()
                .any(|(name, _)| *name == "/dashboard")
        );
    }

    #[test]
    fn trigger_words_never_execute() {
        // The retired `/clear` and friends are steering words, not commands:
        // none of them parse through dispatch, land in completion's canonical
        // list, or collide with a real built-in.
        for (trigger, _, _) in crate::startup::TRIGGER_WORD_SUGGESTIONS {
            let slashed = format!("/{trigger}");
            assert!(
                BuiltinCmd::from_slash(&slashed).is_none(),
                "trigger word {slashed} must not resolve to a command"
            );
            assert!(
                !BuiltinCmd::ALL.iter().any(|(name, _)| *name == slashed),
                "trigger word {slashed} must not shadow a built-in"
            );
        }
        // …and `/clear` in particular is gone from the executable surface.
        assert!(BuiltinCmd::from_slash("/clear").is_none());
    }

    #[test]
    fn trigger_word_suggestions_point_at_real_commands() {
        // Every suggestion target must resolve to a listed built-in and carry
        // an explanation, so the popup never steers to a dead or cryptic row.
        for (trigger, target, reason) in crate::startup::TRIGGER_WORD_SUGGESTIONS {
            assert!(
                BuiltinCmd::from_slash(target).is_some(),
                "suggestion target {target} for trigger /{trigger} is not a command"
            );
            assert!(
                BuiltinCmd::ALL.iter().any(|(name, _)| name == target),
                "suggestion target {target} for trigger /{trigger} is not a listed built-in"
            );
            assert!(
                !reason.is_empty(),
                "trigger /{trigger} must explain the steer"
            );
        }
        // The lookup itself: exact, bare-word only.
        assert_eq!(
            crate::startup::suggest_for_trigger("clear").map(|(t, _)| t),
            Some("/new")
        );
        assert_eq!(
            crate::startup::suggest_for_trigger("continue").map(|(t, _)| t),
            Some("/resume")
        );
        assert!(crate::startup::suggest_for_trigger("cle").is_none());
        assert!(crate::startup::suggest_for_trigger("new").is_none());
        assert!(crate::startup::suggest_for_trigger("").is_none());
    }

    #[test]
    fn no_args_is_fresh() {
        let (mode, project, autopilot, single) = parse_args(Vec::new());
        assert!(matches!(mode, StartupMode::Fresh));
        assert!(project.is_none());
        assert!(!autopilot);
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

    #[test]
    fn serve_subcommand_forms() {
        let (mode, ..) = parse_args(args(&["serve"]));
        assert!(matches!(
            mode,
            StartupMode::Serve {
                port: 0,
                public: false,
                detach: false
            }
        ));
        let (mode, ..) = parse_args(args(&["serve", "--port", "8765", "--public", "--detach"]));
        assert!(matches!(
            mode,
            StartupMode::Serve {
                port: 8765,
                public: true,
                detach: true
            }
        ));
        let (mode, project, ..) = parse_args(args(&["--project", "/p", "serve"]));
        assert!(matches!(mode, StartupMode::Serve { .. }));
        assert_eq!(project, Some(PathBuf::from("/p")));
    }

    #[test]
    fn dashboard_subcommand_form() {
        let (mode, ..) = parse_args(args(&["dashboard"]));
        assert!(matches!(mode, StartupMode::Dashboard));
        let (mode, project, ..) = parse_args(args(&["--project", "/p", "dashboard"]));
        assert!(matches!(mode, StartupMode::Dashboard));
        assert_eq!(project, Some(PathBuf::from("/p")));
    }

    #[test]
    fn status_subcommand_forms() {
        let (mode, ..) = parse_args(args(&["status"]));
        assert!(matches!(
            mode,
            StartupMode::Status {
                watch: false,
                json: false,
                include_idle: false
            }
        ));
        let (mode, ..) = parse_args(args(&["status", "--watch", "--json", "--all"]));
        assert!(matches!(
            mode,
            StartupMode::Status {
                watch: true,
                json: true,
                include_idle: true
            }
        ));
        let (mode, project, ..) = parse_args(args(&["--project", "/p", "status", "--watch"]));
        assert!(matches!(mode, StartupMode::Status { watch: true, .. }));
        assert_eq!(project, Some(PathBuf::from("/p")));
    }
}
