//! The `neenee` command line — parsed where it belongs (ADR-0116).
//!
//! For most of the project's life this lived in `neenee-runtime::startup`
//! (`parse_args`), which put a frontend concern inside the session-runtime
//! library and let two flag tables (`serve` vs `daemon start`) drift
//! independently. The vocabulary also accreted: `serve`/`daemon start`,
//! `status`/`daemon status`, `attach`/`resume`, `stop`/`daemon stop` —
//! four noun-verb spellings for one daemon. ADR-0116 fixes both:
//!
//! - **One noun per resource, one verb per action.** The daemon is managed
//!   by `neenee daemon start|stop|status`; sessions by `neenee session ls|
//!   rm`; `attach` always ends in a real TUI session picker, never a
//!   printed list. The retired top-level spellings (`serve`, `stop`,
//!   `status`, `resume`, `exec`) are refused with a pointer at the
//!   canonical form, not silently accepted forever.
//! - **The parser is a table, not a hand-rolled ladder.** One spec drives
//!   parsing, help, error messages, and shell completions, so a flag
//!   cannot exist in one place and not the other (the `--expose`/
//!   `--public` usage drift this replaces).
//!
//! This module decides *what was asked*; the dispatch in `main.rs` decides
//! *what happens*.

use std::collections::BTreeMap;
use std::path::PathBuf;

// ─────────────────────────────────────────────────────────────────────────
// The parsed command line
// ─────────────────────────────────────────────────────────────────────────

/// What the user asked the binary to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    /// Bare `neenee` / `neenee "prompt"`: an interactive session — with
    /// the prompt queued for the first turn. Headless is selected by
    /// `-p`, `--json`, or a piped stdin (see `main.rs`).
    Fresh,
    /// `neenee run <prompt>`: the explicit headless one-shot.
    Run {
        prompt: String,
    },
    /// `neenee attach [id]`: join a hosted session — through the TUI
    /// picker when no id is given.
    Attach {
        id: Option<String>,
    },
    Session(SessionAction),
    Daemon(DaemonAction),
    Config(ConfigAction),
    Auth(AuthAction),
    /// `neenee mcp ls` — list configured MCP servers.
    Mcp(McpAction),
    /// `neenee skill ls` — list discovered skills.
    Skill(SkillAction),
    Dashboard,
    Doctor,
    /// `neenee panel`: print the web panel URL for the running daemon.
    /// `neenee panel [url|open]`: the web panel's address for the running
    /// daemon — printed, or additionally opened in the platform browser.
    Panel(PanelAction),
    /// `neenee completions <shell>`.
    Completions(Shell),
    /// `--version` / `-V`.
    Version,
    /// `--help` / `-h` / `help [topic]`.
    Help(Option<String>),
    /// Render one UI component standalone (debug builds only).
    #[cfg(debug_assertions)]
    Showcase(String),
}

/// `neenee mcp …`
#[derive(Debug, Clone, PartialEq)]
pub enum McpAction {
    /// `neenee mcp ls` — list configured MCP servers.
    List,
}

/// `neenee skill …`
#[derive(Debug, Clone, PartialEq)]
pub enum SkillAction {
    /// `neenee skill ls` — list discovered skills.
    List,
}

/// `neenee session …`
#[derive(Debug, Clone, PartialEq)]
pub enum SessionAction {
    /// `neenee session rm <id>` — terminate a hosted session. Listing is
    /// `daemon status`: the session table is the daemon's view.
    Delete(String),
}

/// `neenee daemon …`
#[derive(Debug, Clone, PartialEq)]
pub enum DaemonAction {
    /// `neenee daemon start` — start the session daemon.
    Start {
        /// `--fg`: stay in the foreground (the systemd/tmux shape).
        /// Detaching is the default because "start" asks for a daemon,
        /// not a foreground process; `--fg` is for supervisors that
        /// provide their own daemonization.
        foreground: bool,
        port: Option<u16>,
        public: bool,
        no_local_auth: bool,
        idle_exit_minutes: Option<u64>,
        shutdown_grace_secs: Option<u64>,
    },
    /// `neenee daemon stop` — graceful, budget-aware drain.
    Stop,
    /// `neenee daemon status` — the daemon's session table and endpoints.
    Status {
        watch: bool,
        json: bool,
        include_idle: bool,
        diagnostic: bool,
    },
}

/// `neenee config …`
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigAction {
    List,
    Path,
    Get(String),
    Set {
        key: String,
        value: String,
    },
    /// `neenee config check` — validate `config.toml` against the schema:
    /// hard errors, typo'd keys, and dead legacy spellings.
    Check,
}

/// `neenee auth …`
#[derive(Debug, Clone, PartialEq)]
pub enum AuthAction {
    List,
    Show(String),
    Set { provider: String, key: String },
}

/// `neenee panel …`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelAction {
    /// `neenee panel [url]` — print the panel URL (with token).
    Url,
    /// `neenee panel open` — print it and launch the platform browser.
    Open,
}

/// A shell whose completion script `neenee completions` can print.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
}

impl Shell {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "bash" => Some(Self::Bash),
            "zsh" => Some(Self::Zsh),
            "fish" => Some(Self::Fish),
            _ => None,
        }
    }
}

/// The parsed command line: the [`Mode`] plus the global options.
#[derive(Debug, Clone)]
pub struct CliArgs {
    pub mode: Mode,
    /// `--project <path>`: operate on the project at `<path>`.
    pub project: Option<PathBuf>,
    /// `--autopilot` / `--yolo` / `-y`: no confirmations or questions.
    pub autopilot: bool,
    /// `--interactive` / `-i`: force the TUI even when headless would
    /// otherwise apply.
    pub interactive: bool,
    /// `-p`/`--prompt`/`--print` or a positional prompt phrase.
    pub prompt: Option<String>,
    /// The prompt came from `-p`/`--prompt` (not positional): headless is
    /// the intent, `-i` excepted.
    pub prompt_from_flag: bool,
    /// `-j`/`--json`: structured output where a command supports it.
    pub json: bool,
    /// `--remote <addr>` / `--token <token>`: daemon endpoint override.
    pub remote: Option<String>,
    pub token: Option<String>,
    /// `--home <dir>`: the **instance root** (ADR-0121) — one flag moves
    /// every directory neenee touches (config, credentials, sessions,
    /// skills, logs, and the daemon's socket/lock/discovery record) under
    /// `<dir>/neenee/`. The CLI form of `NEENEE_HOME`; parsed here,
    /// installed once in `main` before any path is resolved.
    pub home: Option<PathBuf>,
    /// `--config-dir` / `--data-dir` / `--state-dir` / `--cache-dir`
    /// (ADR-0014 §3 tier 1): per-category overrides for setups where only
    /// one location must move (e.g. a shared config on NFS, data on a big
    /// disk). Parsed here, installed with the same one-time pre-parse as
    /// `--home`; each is the CLI form of its `NEENEE_*_DIR` env var and a
    /// category-specific flag **wins** over `--home` for that category.
    pub config_dir: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub cache_dir: Option<PathBuf>,
}

// ─────────────────────────────────────────────────────────────────────────
// The command spec — one source of truth for parse, help, and completion
// ─────────────────────────────────────────────────────────────────────────

/// A command or subcommand entry.
struct Spec {
    /// The canonical name (what help and completion advertise).
    name: &'static str,
    /// Accepted spellings — canonical first, aliases after. Aliases parse
    /// but are not advertised: they exist to retire gently.
    names: &'static [&'static str],
    about: &'static str,
}

const SESSION_SUBS: &[Spec] = &[
    // The listing is `daemon status`, not a session subcommand: the
    // session table is the daemon's view of what it hosts (ADR-0116's
    // one-noun-per-resource — `session ls` duplicated `daemon status`
    // verbatim).
    Spec {
        name: "rm",
        names: &["rm", "delete"],
        about: "terminate a hosted session by id",
    },
];

const DAEMON_SUBS: &[Spec] = &[
    Spec {
        name: "start",
        names: &["start"],
        about: "start the daemon (detached by default; --fg stays in the foreground)",
    },
    Spec {
        name: "stop",
        names: &["stop"],
        about: "stop the daemon gracefully",
    },
    Spec {
        name: "status",
        names: &["status"],
        about: "show the daemon's sessions and endpoints",
    },
];

const CONFIG_SUBS: &[Spec] = &[
    Spec {
        name: "list",
        names: &["list", "show"],
        about: "show current configuration",
    },
    Spec {
        name: "get",
        names: &["get"],
        about: "get a configuration value",
    },
    Spec {
        name: "set",
        names: &["set"],
        about: "set a configuration value",
    },
    Spec {
        name: "path",
        names: &["path"],
        about: "print the configuration file path",
    },
    Spec {
        name: "check",
        names: &["check"],
        about: "validate config.toml: syntax errors, typo'd keys, dead legacy keys",
    },
];

const AUTH_SUBS: &[Spec] = &[
    Spec {
        name: "list",
        names: &["list", "status"],
        about: "list configured providers and auth status",
    },
    Spec {
        name: "show",
        names: &["show"],
        about: "show one provider's credential status",
    },
    Spec {
        name: "set",
        names: &["set"],
        about: "set a provider API key",
    },
];

const MCP_SUBS: &[Spec] = &[Spec {
    name: "ls",
    names: &["ls", "list"],
    about: "list configured MCP servers",
}];

const SKILL_SUBS: &[Spec] = &[Spec {
    name: "ls",
    names: &["ls", "list"],
    about: "list discovered skills",
}];

const COMMANDS: &[Spec] = &[
    Spec {
        name: "run",
        names: &["run"],
        about: "execute a prompt non-interactively (headless one-shot)",
    },
    Spec {
        name: "session",
        names: &["session"],
        about: "manage sessions (rm; listing is `daemon status`)",
    },
    Spec {
        name: "daemon",
        names: &["daemon"],
        about: "manage the session daemon (start, stop, status)",
    },
    Spec {
        name: "attach",
        names: &["attach"],
        about: "attach the TUI to a hosted session (picker when no id)",
    },
    Spec {
        name: "panel",
        names: &["panel"],
        about: "the web panel URL (url) or open it in a browser (open)",
    },
    Spec {
        name: "dashboard",
        names: &["dashboard"],
        about: "open the full-screen session dashboard",
    },
    Spec {
        name: "config",
        names: &["config"],
        about: "inspect or modify configuration",
    },
    Spec {
        name: "auth",
        names: &["auth"],
        about: "manage provider credentials and API keys",
    },
    Spec {
        name: "mcp",
        names: &["mcp"],
        about: "manage MCP servers (ls)",
    },
    Spec {
        name: "skill",
        names: &["skill", "skills"],
        about: "manage skills (ls)",
    },
    Spec {
        name: "doctor",
        names: &["doctor"],
        about: "verify stored session integrity",
    },
    Spec {
        name: "completions",
        names: &["completions"],
        about: "print a shell completion script",
    },
    #[cfg(debug_assertions)]
    Spec {
        name: "showcase",
        names: &["showcase"],
        about: "render a single UI component standalone (debug only)",
    },
    Spec {
        name: "help",
        names: &["help"],
        about: "print help for a command",
    },
];

/// All accepted top-level spellings → canonical command, for help topics
/// and "did you mean" suggestions.
fn command_index() -> BTreeMap<&'static str, &'static str> {
    let mut index = BTreeMap::new();
    for spec in COMMANDS {
        for name in spec.names {
            index.insert(*name, spec.name);
        }
    }
    index
}

/// Resolve a word against a spec list by canonical name or alias.
fn resolve<'a>(word: &str, specs: &'a [Spec]) -> Option<&'a Spec> {
    specs.iter().find(|s| s.names.contains(&word))
}

// ─────────────────────────────────────────────────────────────────────────
// Flag parsing
// ─────────────────────────────────────────────────────────────────────────

/// A flag misuse, rendered as `--flag: message`.
struct FlagError(String);

impl From<FlagError> for String {
    fn from(error: FlagError) -> Self {
        error.0
    }
}

impl FlagError {
    fn new(flag: &str, message: impl std::fmt::Display) -> Self {
        Self(format!("{flag}: {message}"))
    }
}

/// Split `--flag=value` into `("--flag", Some("value"))`; a bare flag is
/// `("--flag", None)`; a positional word passes through unchanged.
fn split_flag(arg: &str) -> (&str, Option<&str>) {
    match arg.split_once('=') {
        Some((name, value)) if name.starts_with("--") => (name, Some(value)),
        _ => (arg, None),
    }
}

/// `--flag value` / `--flag=value`: take the inline value or pull the
/// next token.
fn flag_value<'a, I: Iterator<Item = &'a String>>(
    flag: &str,
    inline: Option<&str>,
    iter: &mut I,
) -> Result<String, FlagError> {
    if let Some(v) = inline {
        return Ok(v.to_string());
    }
    iter.next()
        .cloned()
        .ok_or_else(|| FlagError::new(flag, "requires a value"))
}

fn parse_u16(flag: &str, value: &str) -> Result<u16, FlagError> {
    value
        .parse()
        .map_err(|_| FlagError::new(flag, format!("'{value}' is not a port number (0-65535)")))
}

fn parse_u64(flag: &str, value: &str) -> Result<u64, FlagError> {
    value
        .parse()
        .map_err(|_| FlagError::new(flag, format!("'{value}' is not a number")))
}

/// The `daemon start` flags (ADR-0116: one table — the `serve` vs
/// `daemon start` duplication is gone).
#[derive(Default)]
struct DaemonStartFlags {
    foreground: bool,
    port: Option<u16>,
    public: bool,
    no_local_auth: bool,
    idle_exit_minutes: Option<u64>,
    shutdown_grace_secs: Option<u64>,
}

fn parse_daemon_start_flags(args: &[String]) -> Result<DaemonStartFlags, FlagError> {
    let mut flags = DaemonStartFlags::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let (name, inline) = split_flag(arg);
        match name {
            "--fg" | "--foreground" => flags.foreground = true,
            "--port" => {
                flags.port = Some(parse_u16(
                    "--port",
                    &flag_value("--port", inline, &mut iter)?,
                )?);
            }
            "--public" => flags.public = true,
            "--no-local-auth" => flags.no_local_auth = true,
            "--idle-exit" => {
                flags.idle_exit_minutes = Some(parse_u64(
                    "--idle-exit",
                    &flag_value("--idle-exit", inline, &mut iter)?,
                )?);
            }
            "--grace" => {
                flags.shutdown_grace_secs = Some(parse_u64(
                    "--grace",
                    &flag_value("--grace", inline, &mut iter)?,
                )?);
            }
            other => return Err(FlagError::new(other, "not a recognized flag here")),
        }
    }
    Ok(flags)
}

/// The `--watch/--json/--all` flags shared by the table-shaped commands.
#[derive(Default)]
struct TableFlags {
    watch: bool,
    json: bool,
    include_idle: bool,
    diagnostic: bool,
}

fn parse_table_flags(args: &[String], diagnostic: bool) -> Result<TableFlags, FlagError> {
    let mut flags = TableFlags {
        diagnostic,
        ..TableFlags::default()
    };
    for arg in args {
        let (name, inline) = split_flag(arg);
        if inline.is_some() {
            return Err(FlagError::new(name, "does not take a value"));
        }
        match name {
            "--watch" => flags.watch = true,
            "--json" => flags.json = true,
            "--all" => flags.include_idle = true,
            "--diagnostic" | "--diag" => flags.diagnostic = true,
            other => return Err(FlagError::new(other, "not a recognized flag here")),
        }
    }
    Ok(flags)
}

// ─────────────────────────────────────────────────────────────────────────
// parse
// ─────────────────────────────────────────────────────────────────────────

/// Parse the command line into [`CliArgs`]. Errors are short, actionable
/// strings; the caller owns the exit policy (GNU: stderr + exit 2).
pub fn parse(args: &[String]) -> Result<CliArgs, String> {
    let mut project: Option<PathBuf> = None;
    let mut autopilot = false;
    let mut interactive = false;
    let mut prompt: Option<String> = None;
    let mut prompt_from_flag = false;
    let mut json = false;
    let mut version = false;
    let mut remote: Option<String> = None;
    let mut token: Option<String> = None;
    let mut home: Option<PathBuf> = None;
    let mut config_dir: Option<PathBuf> = None;
    let mut data_dir: Option<PathBuf> = None;
    let mut state_dir: Option<PathBuf> = None;
    let mut cache_dir: Option<PathBuf> = None;
    let mut rest: Vec<String> = Vec::new();

    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        let (name, inline) = split_flag(arg);
        match name {
            "--project" => {
                project = Some(PathBuf::from(flag_value("--project", inline, &mut iter)?));
            }
            "--home" => {
                home = Some(PathBuf::from(flag_value("--home", inline, &mut iter)?));
            }
            "--config-dir" => {
                config_dir = Some(PathBuf::from(flag_value(
                    "--config-dir",
                    inline,
                    &mut iter,
                )?));
            }
            "--data-dir" => {
                data_dir = Some(PathBuf::from(flag_value("--data-dir", inline, &mut iter)?));
            }
            "--state-dir" => {
                state_dir = Some(PathBuf::from(flag_value("--state-dir", inline, &mut iter)?));
            }
            "--cache-dir" => {
                cache_dir = Some(PathBuf::from(flag_value("--cache-dir", inline, &mut iter)?));
            }
            "--autopilot" | "--yolo" | "-y" => autopilot = true,
            "--interactive" | "-i" => interactive = true,
            "--json" | "-j" => json = true,
            "--print" | "--prompt" | "-p" => {
                prompt = Some(flag_value("-p/--prompt", inline, &mut iter)?);
                prompt_from_flag = true;
            }
            "--remote" => remote = Some(flag_value("--remote", inline, &mut iter)?),
            "--token" => token = Some(flag_value("--token", inline, &mut iter)?),
            "--version" | "-V" => version = true,
            // The flag form of attach normalizes to the subcommand.
            "--attach" => {
                let id = match iter.peek() {
                    Some(next) if !next.starts_with('-') => iter.next().cloned(),
                    _ => None,
                };
                rest.push("attach".to_string());
                if let Some(id) = id {
                    rest.push(id);
                }
            }
            "--single-instance" => {
                return Err(
                    "--single-instance was removed: the unified daemon owns every \
                     session, so a per-project instance lock no longer applies"
                        .into(),
                );
            }
            _ => rest.push(arg.clone()),
        }
    }

    let base = |mode| CliArgs {
        mode,
        project: project.clone(),
        autopilot,
        interactive,
        prompt: prompt.clone(),
        prompt_from_flag,
        json,
        remote: remote.clone(),
        token: token.clone(),
        home: home.clone(),
        config_dir: config_dir.clone(),
        data_dir: data_dir.clone(),
        state_dir: state_dir.clone(),
        cache_dir: cache_dir.clone(),
    };
    let ok = |mode| Ok(base(mode));

    if version {
        return ok(Mode::Version);
    }

    // Help is position-sensitive: bare `--help`/`-h`/`help` is top-level;
    // `help <topic>` and `<command> --help` are the topic form.
    if let Some(first) = rest.first().map(String::as_str) {
        if first == "-h" || first == "--help" || (first == "help" && rest.len() == 1) {
            return ok(Mode::Help(None));
        }
        if first == "help" {
            let topic = &rest[1];
            return if command_index().contains_key(topic.as_str()) {
                ok(Mode::Help(Some(topic.clone())))
            } else {
                Err(format!("unknown help topic '{topic}'"))
            };
        }
        if command_index().contains_key(first)
            && rest[1..].iter().any(|a| a == "-h" || a == "--help")
        {
            return ok(Mode::Help(Some(first.to_string())));
        }
    }

    let Some(cmd) = rest.first().cloned() else {
        // Bare `neenee`: a fresh session (headless decided by the flags
        // and stdin shape in `main.rs`).
        return ok(Mode::Fresh);
    };
    let extra: Vec<String> = rest[1..].to_vec();
    let unexpected = |arg: &str| {
        Err(format!(
            "unexpected argument '{arg}' found for 'neenee {cmd}'"
        ))
    };

    // Retired top-level spellings point at the canonical form (ADR-0116):
    // an error that teaches, rather than two spellings forever.
    match cmd.as_str() {
        "serve" => {
            return Err(
                "'neenee serve' is now 'neenee daemon start' (add --fg to stay in \
                 the foreground)"
                    .into(),
            );
        }
        "stop" => return Err("'neenee stop' is now 'neenee daemon stop'".into()),
        "status" => return Err("'neenee status' is now 'neenee daemon status'".into()),
        "resume" => {
            return Err(
                "'neenee resume' is now 'neenee attach' (the picker opens when no \
                 id is given)"
                    .into(),
            );
        }
        "exec" => return Err("'neenee exec' is now 'neenee run'".into()),
        _ => {}
    }

    // Positional prompt: a multi-word phrase (or any phrase alongside
    // `-p`) is a prompt, not a command. A single unknown word stays an
    // error so typos surface.
    if resolve(&cmd, COMMANDS).is_none() && !cmd.starts_with('-') {
        let positional_prompt = if rest.len() > 1 || cmd.contains(' ') {
            Some(rest.join(" "))
        } else {
            None
        };
        match positional_prompt.or_else(|| prompt.clone()) {
            Some(text) if rest.len() > 1 || cmd.contains(' ') || prompt_from_flag => {
                return ok(Mode::Fresh).map(|mut args| {
                    if args.prompt.is_none() {
                        args.prompt = Some(text);
                    }
                    args
                });
            }
            _ => {
                let tip = suggest_command(&cmd)
                    .map(|s| format!("\n\n  tip: a similar command exists: '{s}'"))
                    .unwrap_or_default();
                return Err(format!("unrecognized command '{cmd}'{tip}"));
            }
        }
    }

    // Reachable only when `cmd` matched a spec above (the match arm's
    // fallback errors out otherwise), so the resolve is infallible here.
    let Some(spec) = resolve(&cmd, COMMANDS) else {
        return Err(format!("unrecognized command '{cmd}'"));
    };
    let mode = match spec.name {
        "run" => {
            let mut parts: Vec<String> = Vec::new();
            if let Some(p) = prompt.as_ref()
                && !p.is_empty()
            {
                parts.push(p.clone());
            }
            parts.extend(extra.iter().cloned());
            let text = parts.join(" ");
            if text.trim().is_empty() {
                return Err("run requires a prompt".into());
            }
            Mode::Run { prompt: text }
        }
        "attach" => match extra.as_slice() {
            [] => Mode::Attach { id: None },
            [id] if !id.starts_with('-') => Mode::Attach {
                id: Some(id.clone()),
            },
            [bad, ..] => return unexpected(bad),
        },
        "session" => {
            if extra.is_empty() {
                // `neenee session` teaches instead of defaulting: the noun
                // has exactly one subcommand now (the listing moved to
                // `daemon status` — the session table *is* the daemon's
                // view), so the bare noun has no obvious default.
                return Err(
                    "neenee session needs a subcommand: `neenee session rm <id>` \
                     (to list sessions, use `neenee daemon status`)"
                        .into(),
                );
            }
            // The retired listing points at its new home, like the retired
            // top-level spellings above.
            if matches!(extra[0].as_str(), "ls" | "list") {
                return Err("'neenee session ls' is now 'neenee daemon status' (the \
                     session table is the daemon's view of what it hosts)"
                    .into());
            }
            let sub = match resolve(&extra[0], SESSION_SUBS) {
                Some(sub) => sub,
                None => return unexpected(&extra[0]),
            };
            let sub_extra = &extra[1..];
            match sub.name {
                "rm" => {
                    let args: &[String] = sub_extra;
                    match args {
                        [id] if !id.starts_with('-') => {
                            Mode::Session(SessionAction::Delete(id.clone()))
                        }
                        [] => return Err("session rm requires a session id".into()),
                        [bad, ..] => return unexpected(bad),
                    }
                }
                _ => unreachable!("session subcommands are closed"),
            }
        }
        "daemon" => {
            if extra.is_empty() {
                // `neenee daemon` defaults to status, like `git remote`.
                Mode::Daemon(DaemonAction::Status {
                    watch: false,
                    json,
                    include_idle: false,
                    diagnostic: false,
                })
            } else {
                let sub = match resolve(&extra[0], DAEMON_SUBS) {
                    Some(sub) => sub,
                    None => return unexpected(&extra[0]),
                };
                let sub_extra = &extra[1..];
                match sub.name {
                    "start" => {
                        let flags = parse_daemon_start_flags(sub_extra).map_err(|e| e.0)?;
                        Mode::Daemon(DaemonAction::Start {
                            foreground: flags.foreground,
                            port: flags.port,
                            public: flags.public,
                            no_local_auth: flags.no_local_auth,
                            idle_exit_minutes: flags.idle_exit_minutes,
                            shutdown_grace_secs: flags.shutdown_grace_secs,
                        })
                    }
                    "stop" => {
                        let args: &[String] = sub_extra;
                        match args {
                            [] => Mode::Daemon(DaemonAction::Stop),
                            [bad, ..] => return unexpected(bad),
                        }
                    }
                    "status" => {
                        let flags = parse_table_flags(sub_extra, false).map_err(|e| e.0)?;
                        Mode::Daemon(DaemonAction::Status {
                            watch: flags.watch,
                            json: flags.json || json,
                            include_idle: flags.include_idle,
                            diagnostic: flags.diagnostic,
                        })
                    }
                    _ => unreachable!("daemon subcommands are closed"),
                }
            }
        }
        "config" => {
            let extra_str: Vec<&str> = extra.iter().map(String::as_str).collect();
            match extra_str.as_slice() {
                [] | ["list"] | ["show"] => Mode::Config(ConfigAction::List),
                ["path"] => Mode::Config(ConfigAction::Path),
                ["check"] => Mode::Config(ConfigAction::Check),
                ["get", key] => Mode::Config(ConfigAction::Get((*key).to_string())),
                ["set", key, value] => Mode::Config(ConfigAction::Set {
                    key: (*key).to_string(),
                    value: (*value).to_string(),
                }),
                ["get"] => return Err("config get requires a key name".into()),
                ["set"] | ["set", _] => {
                    return Err("config set requires <key> and <value>".into());
                }
                [bad, ..] => return unexpected(bad),
            }
        }
        "auth" => {
            let extra_str: Vec<&str> = extra.iter().map(String::as_str).collect();
            match extra_str.as_slice() {
                [] | ["list"] | ["status"] => Mode::Auth(AuthAction::List),
                ["show", provider] => Mode::Auth(AuthAction::Show((*provider).to_string())),
                ["set", provider, key] => Mode::Auth(AuthAction::Set {
                    provider: (*provider).to_string(),
                    key: (*key).to_string(),
                }),
                ["show"] => return Err("auth show requires a provider name".into()),
                ["set"] | ["set", _] => {
                    return Err("auth set requires <provider> and <key>".into());
                }
                [bad, ..] => return unexpected(bad),
            }
        }
        "mcp" => {
            let extra_str: Vec<&str> = extra.iter().map(String::as_str).collect();
            match extra_str.as_slice() {
                // A bare `neenee mcp` teaches the subcommand rather than
                // silently running the only one (ADR-0119's noun-verb
                // shape; `config`/`auth` default to `list` because they
                // have several — `mcp` has exactly one, so the lesson is
                // worth the keystroke).
                [] => return Err("neenee mcp needs a subcommand: `neenee mcp ls`".into()),
                ["ls"] | ["list"] => Mode::Mcp(McpAction::List),
                [bad, ..] => return unexpected(bad),
            }
        }
        "skill" => {
            let extra_str: Vec<&str> = extra.iter().map(String::as_str).collect();
            match extra_str.as_slice() {
                [] => return Err("neenee skill needs a subcommand: `neenee skill ls`".into()),
                ["ls"] | ["list"] => Mode::Skill(SkillAction::List),
                [bad, ..] => return unexpected(bad),
            }
        }
        "doctor" | "dashboard" => {
            if extra.is_empty() {
                match spec.name {
                    "doctor" => Mode::Doctor,
                    "dashboard" => Mode::Dashboard,
                    _ => unreachable!(),
                }
            } else {
                return unexpected(&extra[0]);
            }
        }
        "panel" => {
            // The verb carries two acts with different shapes: `url`
            // prints (scripts, remote forwarding), `open` also launches
            // the browser. The bare noun keeps the printing behaviour it
            // always had — a bare noun that opens a GUI would surprise
            // headless boxes and SSH sessions.
            let extra_str: Vec<&str> = extra.iter().map(String::as_str).collect();
            match extra_str.as_slice() {
                [] | ["url"] => Mode::Panel(PanelAction::Url),
                ["open"] => Mode::Panel(PanelAction::Open),
                [bad, ..] => return unexpected(bad),
            }
        }
        "completions" => match extra.as_slice() {
            [shell] => match Shell::from_name(shell) {
                Some(shell) => Mode::Completions(shell),
                None => {
                    return Err(format!(
                        "unknown shell '{shell}' (expected bash, zsh, or fish)"
                    ));
                }
            },
            [] => return Err("missing shell name (expected bash, zsh, or fish)".into()),
            [bad, ..] => return unexpected(bad),
        },
        #[cfg(debug_assertions)]
        "showcase" => match extra.as_slice() {
            [comp] => Mode::Showcase((*comp).to_string()),
            [] => return Err("showcase requires a component name".into()),
            [bad, ..] => return unexpected(bad),
        },
        "help" => Mode::Help(None),
        _ => unreachable!("closed command set"),
    };

    Ok(base(mode))
}

// ─────────────────────────────────────────────────────────────────────────
// Suggestions
// ─────────────────────────────────────────────────────────────────────────

/// clap-style "did you mean": an exact-prefix match first, then the
/// closest command within a small edit distance.
fn suggest_command(input: &str) -> Option<&'static str> {
    let index = command_index();
    if input.len() >= 2
        && let Some((_, canonical)) = index.iter().find(|(name, _)| name.starts_with(input))
    {
        return Some(canonical);
    }
    let tolerance = if input.len() >= 5 { 2 } else { 1 };
    index
        .iter()
        .filter(|(name, _)| levenshtein(input, name) <= tolerance)
        .min_by_key(|(name, _)| levenshtein(input, name))
        .map(|(_, canonical)| *canonical)
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            curr[j + 1] = (prev[j] + usize::from(ca != cb))
                .min(prev[j + 1] + 1)
                .min(curr[j] + 1);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

// ─────────────────────────────────────────────────────────────────────────
// Help (generated from the spec tables)
// ─────────────────────────────────────────────────────────────────────────

/// Per-command flag tables rendered into help. Kept as data so help and
/// completion share them.
fn command_flags(cmd: &str) -> &'static [(&'static str, &'static str)] {
    match cmd {
        "daemon" => &[
            ("start --fg", "stay in the foreground (default: detach)"),
            (
                "start --port <n>",
                "TCP port (default: NEENEE_PORT, else 9800)",
            ),
            (
                "start --public",
                "bind all interfaces; requires the bearer token",
            ),
            (
                "start --no-local-auth",
                "drop the loopback bearer-token requirement",
            ),
            (
                "start --idle-exit <min>",
                "auto-exit after <min> idle minutes (0 = never)",
            ),
            ("start --grace <secs>", "graceful-drain budget in seconds"),
            ("status --watch", "keep streaming live updates"),
            ("status --json", "emit one JSON frame per update"),
            ("status --all", "include idle sessions"),
            (
                "status --diagnostic",
                "report discovery/lock/socket/log health",
            ),
        ],
        "session" => &[("rm <id>", "terminate a hosted session by id")],
        "panel" => &[
            ("url", "print the panel URL with its token (default)"),
            ("open", "print the URL and launch the platform browser"),
        ],
        "mcp" => &[("ls", "list configured MCP servers")],
        "skill" => &[("ls", "list discovered skills")],
        _ => &[],
    }
}

/// The help text for a topic: `None` is the top-level help, a command
/// name its per-command text. Returns `None` for unknown topics.
pub fn help_text(topic: Option<&str>) -> Option<String> {
    let mut out = String::new();
    match topic {
        None => {
            out.push_str("neenee — an expert AI coding assistant with tool access\n\n");
            out.push_str("Usage: neenee [OPTIONS] [PROMPT]\n");
            out.push_str("       neenee [OPTIONS] <COMMAND>\n\nCommands:\n");
            let width = COMMANDS.iter().map(|s| s.name.len()).max().unwrap_or(0);
            for spec in COMMANDS {
                out.push_str(&format!("  {:<width$}  {}\n", spec.name, spec.about));
            }
            out.push_str("\nOptions:\n");
            out.push_str("  -p, --prompt <prompt>  run the prompt non-interactively (headless)\n");
            out.push_str("  -i, --interactive      force interactive TUI mode\n");
            out.push_str("  -j, --json             emit structured JSON where supported\n");
            out.push_str("  --autopilot, -y, --yolo  run without confirmations or questions\n");
            out.push_str("      --project <path>   operate on the project at <path>\n");
            out.push_str(
                "      --home <dir>       run as a separate instance rooted at <dir>/neenee\n",
            );
            out.push_str("      --remote <addr>    connect to a remote daemon\n");
            out.push_str("      --token <token>    bearer token for daemon connection\n");
            out.push_str(
                "  -h, --help             print help ('neenee help <command>' for more)\n",
            );
            out.push_str("  -V, --version          print the version and exit\n");
            out.push_str("\nWith no command, neenee opens a fresh interactive session.\n");
            out.push_str("Passing a prompt phrase opens a session and runs it interactively;\n");
            out.push_str("piping into neenee (git diff | neenee) runs headless.\n");
        }
        Some(topic) => {
            let spec = resolve(topic, COMMANDS)?;
            out.push_str(&format!("neenee {} — {}\n\n", spec.name, spec.about));
            out.push_str(&format!("Usage: neenee {}", spec.name));
            if let Some(subs) = subs_of(spec.name) {
                out.push_str(" [COMMAND]\n\nCommands:\n");
                let width = subs.iter().map(|s| s.name.len()).max().unwrap_or(0);
                for sub in subs {
                    out.push_str(&format!("  {:<width$}  {}\n", sub.name, sub.about));
                }
            } else {
                out.push('\n');
            }
            let flags = command_flags(spec.name);
            if !flags.is_empty() {
                out.push_str("\nOptions:\n");
                let width = flags.iter().map(|(f, _)| f.len()).max().unwrap_or(0);
                for (flag, about) in flags {
                    out.push_str(&format!("  {:<width$}  {about}\n", flag));
                }
            }
            match spec.name {
                "run" => {
                    out.push_str(
                        "\nThe prompt streams to stdout (tool activity to stderr), exiting 0\n",
                    );
                    out.push_str("on completion — built for pipes, scripts, and CI.\n");
                }
                "attach" => {
                    out.push_str(
                        "\nWith no id the TUI session picker opens (a lone hosted session is\n",
                    );
                    out.push_str(
                        "auto-selected). Spawns the daemon on demand when none is running.\n",
                    );
                }
                "daemon" => {
                    out.push_str(
                        "\nThe daemon hosts every session across every project. `start` runs it\n",
                    );
                    out.push_str(
                        "detached by default (--fg for systemd/tmux-style supervision);\n",
                    );
                    out.push_str(
                        "`stop` drains within the daemon's --grace budget before escalating;\n",
                    );
                    out.push_str("`status` observes without ever spawning one.\n");
                    out.push_str(
                        "\nNEENEE_HOME points the whole instance (config, data, daemon files,\n",
                    );
                    out.push_str(
                        "port default via NEENEE_PORT) at an isolated root — the dev/test\n",
                    );
                    out.push_str("sandbox shape. See docs/reference/paths.md.\n");
                }
                _ => {}
            }
        }
    }
    Some(out)
}

fn subs_of(cmd: &str) -> Option<&'static [Spec]> {
    match cmd {
        "session" => Some(SESSION_SUBS),
        "daemon" => Some(DAEMON_SUBS),
        "config" => Some(CONFIG_SUBS),
        "auth" => Some(AUTH_SUBS),
        "mcp" => Some(MCP_SUBS),
        "skill" => Some(SKILL_SUBS),
        _ => None,
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Shell completions (generated from the same tables)
// ─────────────────────────────────────────────────────────────────────────

/// The static completion script for a shell.
pub fn completion_script(shell: Shell) -> String {
    match shell {
        Shell::Bash => bash_completion(),
        Shell::Zsh => zsh_completion(),
        Shell::Fish => fish_completion(),
    }
}

fn subs_and_flags(cmd: &str) -> (Vec<&'static str>, Vec<&'static str>) {
    let subs: Vec<&'static str> = subs_of(cmd)
        .map(|s| s.iter().map(|x| x.name).collect())
        .unwrap_or_default();
    let flags: Vec<&'static str> = command_flags(cmd)
        .iter()
        .filter_map(|(flag, _)| {
            // Strip the leading subcommand and the value placeholder; keep
            // the flag itself.
            let flag = flag.split(' ').next().unwrap_or(flag);
            flag.starts_with("--").then_some(flag)
        })
        .collect();
    (subs, flags)
}

fn bash_completion() -> String {
    let cmds: Vec<&str> = COMMANDS.iter().map(|s| s.name).collect();
    let mut cases = String::new();
    for spec in COMMANDS {
        let (subs, flags) = subs_and_flags(spec.name);
        let mut words = subs;
        words.extend(flags.iter().copied());
        words.push("--help");
        if !words.is_empty() {
            cases.push_str(&format!(
                "        {})\n            COMPREPLY=($(compgen -W \"{}\" -- \"$cur\")) ;;\n",
                spec.name,
                words.join(" ")
            ));
        }
    }
    format!(
        "# bash completion for neenee — eval \"$(neenee completions bash)\"\n\
         _neenee() {{\n\
         \x20   local cur cmd\n\
         \x20   cur=\"${{COMP_WORDS[COMP_CWORD]}}\"\n\
         \x20   cmd=\"${{COMP_WORDS[1]}}\"\n\
         \x20   if [[ $COMP_CWORD -eq 1 ]]; then\n\
         \x20       COMPREPLY=($(compgen -W \"{} --project --home --remote --token --prompt -p --interactive -i --json -j --yolo -y --autopilot --help --version\" -- \"$cur\"))\n\
         \x20       return 0\n\
         \x20   fi\n\
         \x20   case \"$cmd\" in\n{}\n\
         \x20   esac\n\
         }}\n\
         complete -F _neenee neenee\n",
        cmds.join(" "),
        cases
    )
}

fn zsh_completion() -> String {
    let mut cmds = String::new();
    for spec in COMMANDS {
        cmds.push_str(&format!("        '{}:{}'\n", spec.name, spec.about));
    }
    let mut cases = String::new();
    for spec in COMMANDS {
        let (subs, flags) = subs_and_flags(spec.name);
        if !subs.is_empty() {
            cases.push_str(&format!(
                "        {})\n            _describe 'subcommand' '({})' ;;\n",
                spec.name,
                subs.iter()
                    .map(|s| format!("{}:{}", s, subs_about(spec.name, s)))
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
        }
        if !flags.is_empty() {
            cases.push_str(&format!(
                "        {})\n            _arguments '{}'\n            ;;\n",
                spec.name,
                flags.join("' '")
            ));
        }
    }
    format!(
        "#compdef neenee\n\
         # zsh completion for neenee — save as `_neenee` on $fpath\n\
         _neenee() {{\n\
         \x20   local -a cmds\n\
         \x20   cmds=(\n{}\n\
         \x20   )\n\
         \x20   if (( CURRENT == 2 )); then\n\
         \x20       _describe 'command' cmds\n\
         \x20       return\n\
         \x20   fi\n\
         \x20   case \"$words[2]\" in\n{}\
         \x20   esac\n\
         }}\n\
         _neenee \"$@\"\n",
        cmds, cases
    )
}

fn subs_about(cmd: &str, sub: &str) -> &'static str {
    subs_of(cmd)
        .and_then(|subs| subs.iter().find(|s| s.name == sub))
        .map(|s| s.about)
        .unwrap_or("")
}

fn fish_completion() -> String {
    let cmds: Vec<&str> = COMMANDS.iter().map(|s| s.name).collect();
    let mut out = format!(
        "# fish completion for neenee — save to ~/.config/fish/completions/neenee.fish\n\
         set -l cmds {}\n\
         complete -c neenee -n '__fish_use_subcommand' -f\n\
         complete -c neenee -n '__fish_use_subcommand' -a \"$cmds\"\n\
         complete -c neenee -l home -d 'separate instance root' -r\n",
        cmds.join(" ")
    );
    for spec in COMMANDS {
        let (subs, flags) = subs_and_flags(spec.name);
        for sub in subs {
            out.push_str(&format!(
                "complete -c neenee -n '__fish_seen_subcommand_from {}' -f -a '{}' -d '{}'\n",
                spec.name,
                sub,
                subs_about(spec.name, sub)
            ));
        }
        for flag in flags {
            out.push_str(&format!(
                "complete -c neenee -n '__fish_seen_subcommand_from {}' -l '{}' -d 'flag'\n",
                spec.name,
                flag.trim_start_matches("--")
            ));
        }
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(tokens: &[&str]) -> CliArgs {
        super::parse(&tokens.iter().map(|t| t.to_string()).collect::<Vec<_>>())
            .expect("parse should succeed")
    }

    fn parse_err(tokens: &[&str]) -> String {
        super::parse(&tokens.iter().map(|t| t.to_string()).collect::<Vec<_>>())
            .expect_err("parse should fail")
    }

    #[test]
    fn bare_invocation_is_fresh() {
        let parsed = parse(&[]);
        assert!(matches!(parsed.mode, Mode::Fresh));
        assert!(parsed.prompt.is_none());
    }

    #[test]
    fn multiword_positional_is_a_prompt() {
        let parsed = parse(&["fix", "the", "build"]);
        assert!(matches!(parsed.mode, Mode::Fresh));
        assert_eq!(parsed.prompt.as_deref(), Some("fix the build"));
        assert!(!parsed.prompt_from_flag);
    }

    #[test]
    fn single_unknown_word_stays_an_error() {
        let err = parse_err(&["sessionn"]);
        assert!(err.contains("unrecognized command"), "{err}");
    }

    #[test]
    fn prompt_flag_selects_headless_intent() {
        let parsed = parse(&["-p", "hi"]);
        assert_eq!(parsed.prompt.as_deref(), Some("hi"));
        assert!(parsed.prompt_from_flag);
    }

    #[test]
    fn retired_spellings_teach_the_canonical_form() {
        assert!(parse_err(&["serve"]).contains("daemon start"));
        assert!(parse_err(&["stop"]).contains("daemon stop"));
        assert!(parse_err(&["status"]).contains("daemon status"));
        assert!(parse_err(&["resume"]).contains("attach"));
        assert!(parse_err(&["exec", "x"]).contains("run"));
    }

    #[test]
    fn daemon_start_defaults_to_detached() {
        let parsed = parse(&["daemon", "start"]);
        match parsed.mode {
            Mode::Daemon(DaemonAction::Start { foreground, .. }) => assert!(!foreground),
            other => panic!("unexpected mode: {other:?}"),
        }
    }

    #[test]
    fn daemon_start_fg_stays_in_foreground() {
        let parsed = parse(&["daemon", "start", "--fg"]);
        match parsed.mode {
            Mode::Daemon(DaemonAction::Start { foreground, .. }) => assert!(foreground),
            other => panic!("unexpected mode: {other:?}"),
        }
    }

    #[test]
    fn daemon_start_flags_parse() {
        let parsed = parse(&[
            "daemon",
            "start",
            "--port",
            "8765",
            "--public",
            "--grace",
            "30",
            "--idle-exit",
            "0",
        ]);
        match parsed.mode {
            Mode::Daemon(DaemonAction::Start {
                port,
                public,
                idle_exit_minutes,
                shutdown_grace_secs,
                ..
            }) => {
                assert_eq!(port, Some(8765));
                assert!(public);
                assert_eq!(idle_exit_minutes, Some(0));
                assert_eq!(shutdown_grace_secs, Some(30));
            }
            other => panic!("unexpected mode: {other:?}"),
        }
    }

    #[test]
    fn daemon_start_rejects_unknown_flags() {
        let err = parse_err(&["daemon", "start", "--expose"]);
        assert!(err.contains("--expose"), "{err}");
    }

    #[test]
    fn daemon_defaults_to_status() {
        let parsed = parse(&["daemon"]);
        assert!(matches!(
            parsed.mode,
            Mode::Daemon(DaemonAction::Status { .. })
        ));
    }

    #[test]
    fn daemon_status_diagnostic() {
        let parsed = parse(&["daemon", "status", "--diagnostic"]);
        match parsed.mode {
            Mode::Daemon(DaemonAction::Status { diagnostic, .. }) => assert!(diagnostic),
            other => panic!("unexpected mode: {other:?}"),
        }
    }

    #[test]
    fn bare_session_teaches_the_subcommand() {
        let err = parse_err(&["session"]);
        assert!(err.contains("session rm"), "{err}");
        assert!(err.contains("daemon status"), "{err}");
    }

    #[test]
    fn session_ls_is_retired_with_a_pointer() {
        let err = parse_err(&["session", "ls"]);
        assert!(err.contains("daemon status"), "{err}");
    }

    #[test]
    fn mcp_and_skill_need_a_subcommand() {
        assert!(
            parse_err(&["mcp"]).contains("mcp ls"),
            "must teach `mcp ls`"
        );
        assert!(
            parse_err(&["skill"]).contains("skill ls"),
            "must teach `skill ls`"
        );
    }

    #[test]
    fn mcp_and_skill_ls_parse() {
        assert!(matches!(
            parse(&["mcp", "ls"]).mode,
            Mode::Mcp(McpAction::List)
        ));
        assert!(matches!(
            parse(&["mcp", "list"]).mode,
            Mode::Mcp(McpAction::List)
        ));
        assert!(matches!(
            parse(&["skill", "ls"]).mode,
            Mode::Skill(SkillAction::List)
        ));
        // The bare-noun spelling is retired with an error naming the form.
        assert!(parse_err(&["mcp", "bogus"]).contains("bogus"));
    }

    #[test]
    fn panel_forms() {
        // Bare `panel` and `panel url` print (the historical behaviour —
        // a bare noun opening a GUI would surprise headless boxes).
        assert!(matches!(
            parse(&["panel"]).mode,
            Mode::Panel(PanelAction::Url)
        ));
        assert!(matches!(
            parse(&["panel", "url"]).mode,
            Mode::Panel(PanelAction::Url)
        ));
        assert!(matches!(
            parse(&["panel", "open"]).mode,
            Mode::Panel(PanelAction::Open)
        ));
        assert!(parse_err(&["panel", "bogus"]).contains("bogus"));
    }

    #[test]
    fn session_rm_takes_one_id() {
        let parsed = parse(&["session", "rm", "abc"]);
        assert!(matches!(parsed.mode, Mode::Session(SessionAction::Delete(id)) if id == "abc"));
        assert!(
            parse(&["session", "rm", "abc"]).mode
                == Mode::Session(SessionAction::Delete("abc".into()))
        );
    }

    #[test]
    fn attach_forms() {
        assert!(matches!(parse(&["attach"]).mode, Mode::Attach { id: None }));
        assert!(matches!(
            parse(&["attach", "sess-1"]).mode,
            Mode::Attach { id: Some(ref id) } if id == "sess-1"
        ));
        // The retired flag form normalizes to the subcommand.
        assert!(matches!(
            parse(&["--attach", "sess-1"]).mode,
            Mode::Attach { id: Some(ref id) } if id == "sess-1"
        ));
    }

    #[test]
    fn attach_does_not_swallow_a_following_flag() {
        let parsed = parse(&["--attach", "--project", "/p"]);
        assert!(matches!(parsed.mode, Mode::Attach { id: None }));
        assert_eq!(parsed.project, Some(PathBuf::from("/p")));
    }

    #[test]
    fn single_instance_is_removed_with_an_explanation() {
        let err = parse_err(&["--single-instance"]);
        assert!(err.contains("removed"), "{err}");
    }

    #[test]
    fn run_joins_flag_and_positional_prompt() {
        let parsed = parse(&["run", "fix", "it"]);
        assert!(matches!(parsed.mode, Mode::Run { ref prompt } if prompt == "fix it"));
    }

    #[test]
    fn run_requires_a_prompt() {
        assert!(parse_err(&["run"]).contains("requires a prompt"));
    }

    #[test]
    fn help_topics_resolve_for_every_command() {
        assert!(help_text(None).is_some());
        for spec in COMMANDS {
            let text = help_text(Some(spec.name)).expect("help exists");
            assert!(text.contains(&format!("neenee {}", spec.name)), "{text}");
        }
        assert!(help_text(Some("nope")).is_none());
    }

    #[test]
    fn help_lists_every_canonical_command_and_no_aliases() {
        let text = help_text(None).unwrap();
        for spec in COMMANDS {
            assert!(
                text.lines()
                    .any(|l| l.starts_with(&format!("  {} ", spec.name))),
                "help must list '{name}':\n{text}",
                name = spec.name
            );
        }
        // Aliases parse but never appear as commands: check the command
        // column, not the whole text (an about like "execute" would false- positives).
        let listed = |name: &str| {
            text.lines()
                .any(|l| l == format!("  {name}").as_str() || l.starts_with(&format!("  {name} ")))
        };
        assert!(!listed("skills"), "{text}");
        assert!(!listed("exec"), "{text}");
        assert!(!listed("list"), "{text}");
    }

    #[test]
    fn completions_are_generated_for_every_shell() {
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let script = completion_script(shell);
            assert!(script.contains("daemon"), "{:?}", shell);
            assert!(script.contains("attach"), "{:?}", shell);
        }
    }

    #[test]
    fn flag_value_forms() {
        let parsed = parse(&["daemon", "start", "--port=8123"]);
        assert!(matches!(
            parsed.mode,
            Mode::Daemon(DaemonAction::Start {
                port: Some(8123),
                ..
            })
        ));
    }

    #[test]
    fn invalid_port_is_actionable() {
        let err = parse_err(&["daemon", "start", "--port", "notaport"]);
        assert!(err.contains("not a port number"), "{err}");
    }

    #[test]
    fn missing_flag_value_is_named() {
        let err = parse_err(&["daemon", "start", "--port"]);
        assert!(err.contains("requires a value"), "{err}");
    }

    #[test]
    fn home_flag_parses_in_both_forms() {
        for tokens in [vec!["--home", "/tmp/nn-run"], vec!["--home=/tmp/nn-run"]] {
            let parsed = parse(&tokens);
            assert_eq!(
                parsed.home.as_deref(),
                Some(std::path::Path::new("/tmp/nn-run")),
                "{tokens:?}"
            );
        }
    }

    #[test]
    fn home_flag_coexists_with_commands() {
        let parsed = parse(&["--home", "/tmp/nn-run", "daemon", "status"]);
        assert!(matches!(
            parsed.mode,
            Mode::Daemon(DaemonAction::Status { .. })
        ));
        assert_eq!(
            parsed.home.as_deref(),
            Some(std::path::Path::new("/tmp/nn-run"))
        );
    }

    #[test]
    fn home_flag_requires_a_value() {
        assert!(parse_err(&["--home"]).contains("requires a value"));
    }
}
