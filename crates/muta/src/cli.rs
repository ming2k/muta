//! The `muta` command line — parsed where it belongs (ADR-0116).
//!
//! For most of the project's life this lived in `muta-runtime::startup`
//! (`parse_args`), which put a frontend concern inside the session-runtime
//! library and let two flag tables (`serve` vs `daemon start`) drift
//! independently. The vocabulary also accreted: `serve`/`daemon start`,
//! `status`/`daemon status`, `attach`/`resume`, `stop`/`daemon stop` —
//! four noun-verb spellings for one daemon. ADR-0116 fixes both:
//!
//! - **One noun per resource, one verb per action.** The daemon is managed
//!   by `muta daemon start|stop|status|token`; sessions by `muta session rm`
//!   (listing is `daemon status`). Interactive run/attach/dashboard commands
//!   belong exclusively to `mutx`. The former top-level spellings (`serve`, `stop`,
//!   `status`, `resume`, `exec`) are removed outright: no alias, no
//!   teaching error — an unknown word is an unrecognized command.
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
    Session(SessionAction),
    Daemon(DaemonAction),
    Config(ConfigAction),
    Auth(AuthAction),
    /// `muta mcp ls` — list configured MCP servers.
    Mcp(McpAction),
    /// `muta skill ls` — list discovered skills.
    Skill(SkillAction),
    Doctor,
    /// `muta completions <shell>`.
    Completions(Shell),
    /// `--version` / `-V`.
    Version,
    /// `--help` / `-h` / `help [topic]`.
    Help(Option<String>),
}

/// `muta mcp …`
#[derive(Debug, Clone, PartialEq)]
pub enum McpAction {
    /// `muta mcp ls` — list configured MCP servers.
    List,
    /// `muta mcp add <name> [--url <url> | -- <command> [args…]]` — register a
    /// server in the user-level config (`[mcp.<name>]`, `~/.config/muta/config.toml`).
    Add {
        name: String,
        url: Option<String>,
        command: Vec<String>,
        environment: Vec<(String, String)>,
        read_only: bool,
        disabled: bool,
        allow_tools: Vec<String>,
        deny_tools: Vec<String>,
    },
    /// `muta mcp rm <name>` — remove a server from the user-level config.
    Remove { name: String },
    /// `muta mcp enable <name>` / `muta mcp disable <name>`.
    SetEnabled { name: String, enabled: bool },
    /// `muta mcp get <name>` — print one server's effective TOML entry.
    Get { name: String },
    /// `muta mcp probe <name>` — connect once, list the advertised tools.
    Probe { name: String },
    /// `muta mcp import (- | <file>)` — read `[mcp.*]` TOML (e.g. the output of
    /// `aegis-mcp print-config`) and merge it into the user-level config.
    Import { source: String },
}

/// `muta skill …`
#[derive(Debug, Clone, PartialEq)]
pub enum SkillAction {
    /// `muta skill ls` — list discovered skills.
    List,
}

/// `muta session …`
#[derive(Debug, Clone, PartialEq)]
pub enum SessionAction {
    /// `muta session rm <id>` — terminate a hosted session. Listing is
    /// `daemon status`: the session table is the daemon's view.
    Delete(String),
}

/// `muta daemon …`
#[derive(Debug, Clone, PartialEq)]
pub enum DaemonAction {
    /// `muta daemon start` — start the session daemon.
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
    /// `muta daemon stop` — graceful, budget-aware drain.
    Stop,
    /// `muta daemon token` — print the local daemon's bearer token.
    Token,
    /// `muta daemon status` — the daemon's session table and endpoints.
    Status {
        watch: bool,
        json: bool,
        include_idle: bool,
        diagnostic: bool,
    },
}

/// `muta config …`
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigAction {
    List,
    Path,
    Get(String),
    Set {
        key: String,
        value: String,
    },
    /// `muta config check` — validate `config.toml` against the schema:
    /// hard errors, typo'd keys, and dead legacy spellings.
    Check,
}

/// `muta auth …`
#[derive(Debug, Clone, PartialEq)]
pub enum AuthAction {
    List,
    Show(String),
    Set { provider: String, key: String },
}

/// A shell whose completion script `muta completions` can print.
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
    Spec {
        name: "token",
        names: &["token"],
        about: "print the local daemon bearer token",
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

const MCP_SUBS: &[Spec] = &[
    Spec {
        name: "ls",
        names: &["ls", "list"],
        about: "list configured MCP servers",
    },
    Spec {
        name: "add",
        names: &["add"],
        about: "register an MCP server in the user config",
    },
    Spec {
        name: "rm",
        names: &["rm", "remove"],
        about: "remove an MCP server from the user config",
    },
    Spec {
        name: "enable",
        names: &["enable"],
        about: "enable a configured MCP server",
    },
    Spec {
        name: "disable",
        names: &["disable"],
        about: "disable a configured MCP server without removing it",
    },
    Spec {
        name: "get",
        names: &["get"],
        about: "print one server's config entry",
    },
    Spec {
        name: "probe",
        names: &["probe"],
        about: "connect to a server once and list its tools",
    },
    Spec {
        name: "import",
        names: &["import"],
        about: "import [mcp.*] TOML (e.g. `aegis-mcp print-config`) into the user config",
    },
];

const SKILL_SUBS: &[Spec] = &[Spec {
    name: "ls",
    names: &["ls", "list"],
    about: "list discovered skills",
}];

const COMMANDS: &[Spec] = &[
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
    Spec {
        name: "token",
        names: &["token"],
        about: "print the local daemon bearer token",
    },
    Spec {
        name: "session",
        names: &["session"],
        about: "manage sessions (rm; listing is `status`)",
    },
    Spec {
        name: "daemon",
        names: &["daemon"],
        about: "manage the session daemon (start, stop, status, token)",
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
        about: "manage MCP servers (ls, add, rm, probe, import)",
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

/// `muta mcp add <name> [--url <url>] [--env K=V]… [--read-only] [--disabled]
///                        [--allow-tools a,b] [--deny-tools a,b] -- <cmd> [args…]`
///
/// The `--` separator protects a server command's own flags from ours; the
/// positional `<name>` is required, and either `--url` or a command after
/// `--` must declare a transport.
fn parse_mcp_add(rest: &[&str]) -> Result<McpAction, String> {
    let mut url = None;
    let mut environment = Vec::new();
    let mut read_only = false;
    let mut disabled = false;
    let mut allow_tools = Vec::new();
    let mut deny_tools = Vec::new();
    let mut command: Vec<String> = Vec::new();
    let mut name: Option<String> = None;

    let owned: Vec<String> = rest.iter().map(|s| s.to_string()).collect();
    let mut iter = owned.iter();
    while let Some(arg) = iter.next() {
        let arg = arg.as_str();
        // Everything after `--` is the server command, verbatim.
        if arg == "--" {
            command = iter.by_ref().cloned().collect();
            break;
        }
        let (flag, inline) = split_flag(arg);
        match flag {
            "--url" => {
                url = Some(flag_value("--url", inline, &mut iter).map_err(|e| e.0)?);
            }
            "--env" => {
                let pair = flag_value("--env", inline, &mut iter).map_err(|e| e.0)?;
                let (key, value) = pair
                    .split_once('=')
                    .ok_or_else(|| format!("--env: expected KEY=VALUE, got '{pair}'"))?;
                if key.is_empty() {
                    return Err("--env: empty variable name".into());
                }
                environment.push((key.to_string(), value.to_string()));
            }
            "--read-only" => read_only = true,
            "--disabled" => disabled = true,
            "--allow-tools" => {
                let list = flag_value("--allow-tools", inline, &mut iter).map_err(|e| e.0)?;
                allow_tools = split_tool_list(&list)?;
            }
            "--deny-tools" => {
                let list = flag_value("--deny-tools", inline, &mut iter).map_err(|e| e.0)?;
                deny_tools = split_tool_list(&list)?;
            }
            _ if arg.starts_with('-') && arg != "-" => {
                return Err(format!("unknown flag '{arg}' for `muta mcp add`"));
            }
            _ => {
                if name.is_some() {
                    return Err(format!(
                        "unexpected '{arg}': server arguments go after `--` (muta mcp add <name> -- <command> [args…])"
                    ));
                }
                name = Some(arg.to_string());
            }
        }
    }

    let Some(name) = name else {
        return Err("muta mcp add requires a server name".into());
    };
    if url.is_none() && command.is_empty() {
        return Err(format!(
            "muta mcp add {name}: declare a transport — `--url <endpoint>` or `-- <command> [args…]`"
        ));
    }
    if url.is_some() && !command.is_empty() {
        return Err(format!(
            "muta mcp add {name}: `--url` and a command are mutually exclusive"
        ));
    }
    Ok(McpAction::Add {
        name,
        url,
        command,
        environment,
        read_only,
        disabled,
        allow_tools,
        deny_tools,
    })
}

/// `--allow-tools a, b` → `["a", "b"]`; trims whitespace, rejects empties.
fn split_tool_list(list: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for item in list.split(',') {
        let item = item.trim();
        if item.is_empty() {
            return Err("tool lists must be comma-separated names without empty items".into());
        }
        out.push(item.to_string());
    }
    if out.is_empty() {
        return Err("tool list cannot be empty".into());
    }
    Ok(out)
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
    let mut json = false;
    let mut version = false;
    let mut rest: Vec<String> = Vec::new();

    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        let (name, inline) = split_flag(arg);
        match name {
            "--project" => {
                project = Some(PathBuf::from(flag_value("--project", inline, &mut iter)?));
            }
            "--home" => {
                return Err(
                    "--home was removed: set the MUTA_HOME environment variable instead (e.g. MUTA_HOME=/tmp/dev)"
                        .into(),
                );
            }
            "--config-dir" | "--data-dir" | "--state-dir" | "--cache-dir" => {
                return Err(format!(
                    "{name} was removed: use matching MUTA_*_DIR environment variables instead"
                ));
            }
            "--json" | "-j" => json = true,
            "--version" | "-V" => version = true,
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
        return ok(Mode::Daemon(DaemonAction::Start {
            foreground: true,
            port: None,
            public: false,
            no_local_auth: false,
            idle_exit_minutes: None,
            shutdown_grace_secs: None,
        }));
    };

    if cmd.starts_with('-') {
        let flags = parse_daemon_start_flags(&rest).map_err(|e| e.0)?;
        return ok(Mode::Daemon(DaemonAction::Start {
            foreground: flags.foreground,
            port: flags.port,
            public: flags.public,
            no_local_auth: flags.no_local_auth,
            idle_exit_minutes: flags.idle_exit_minutes,
            shutdown_grace_secs: flags.shutdown_grace_secs,
        }));
    }

    let extra: Vec<String> = rest[1..].to_vec();
    let unexpected = |arg: &str| {
        Err(format!(
            "unexpected argument '{arg}' found for 'muta {cmd}'"
        ))
    };

    if resolve(&cmd, COMMANDS).is_none() && !cmd.starts_with('-') {
        let tip = suggest_command(&cmd)
            .map(|s| format!("\n\n  tip: a similar command exists: '{s}'"))
            .unwrap_or_default();
        return Err(format!("unrecognized command '{cmd}'{tip}"));
    }

    // Reachable only when `cmd` matched a spec above (the match arm's
    // fallback errors out otherwise), so the resolve is infallible here.
    let Some(spec) = resolve(&cmd, COMMANDS) else {
        return Err(format!("unrecognized command '{cmd}'"));
    };
    let mode = match spec.name {
        "start" => {
            let flags = parse_daemon_start_flags(&extra).map_err(|e| e.0)?;
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
            let args: &[String] = &extra;
            match args {
                [] => Mode::Daemon(DaemonAction::Stop),
                [bad, ..] => return unexpected(bad),
            }
        }
        "status" => {
            let flags = parse_table_flags(&extra, false).map_err(|e| e.0)?;
            Mode::Daemon(DaemonAction::Status {
                watch: flags.watch,
                json: flags.json || json,
                include_idle: flags.include_idle,
                diagnostic: flags.diagnostic,
            })
        }
        "token" => match extra.as_slice() {
            [] => Mode::Daemon(DaemonAction::Token),
            [bad, ..] => return unexpected(bad),
        },
        "session" => {
            if extra.is_empty() {
                return Err("muta session needs a subcommand: `muta session rm <id>` \
                     (to list sessions, use `muta status`)"
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
                // `muta daemon` defaults to status, like `git remote`.
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
                    "token" => match sub_extra {
                        [] => Mode::Daemon(DaemonAction::Token),
                        [bad, ..] => return unexpected(bad),
                    },
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
                // A bare `muta mcp` teaches the subcommand rather than
                // silently running the only one (ADR-0119's noun-verb
                // shape; `config`/`auth` default to `list` because they
                // have several — `mcp` now does too, but the lesson
                // stays worth the keystroke).
                [] => return Err("muta mcp needs a subcommand: `muta mcp ls`".into()),
                ["ls"] | ["list"] => Mode::Mcp(McpAction::List),
                ["add", rest @ ..] => Mode::Mcp(parse_mcp_add(rest)?),
                ["rm", name] | ["remove", name] => Mode::Mcp(McpAction::Remove {
                    name: (*name).to_string(),
                }),
                ["rm"] | ["remove"] => {
                    return Err("muta mcp rm requires a server name".into());
                }
                ["enable", name] => Mode::Mcp(McpAction::SetEnabled {
                    name: (*name).to_string(),
                    enabled: true,
                }),
                ["disable", name] => Mode::Mcp(McpAction::SetEnabled {
                    name: (*name).to_string(),
                    enabled: false,
                }),
                ["enable"] | ["disable"] => {
                    return Err("muta mcp enable/disable requires a server name".into());
                }
                ["get", name] => Mode::Mcp(McpAction::Get {
                    name: (*name).to_string(),
                }),
                ["get"] => return Err("muta mcp get requires a server name".into()),
                ["probe", name] => Mode::Mcp(McpAction::Probe {
                    name: (*name).to_string(),
                }),
                ["probe"] => return Err("muta mcp probe requires a server name".into()),
                ["import", source] => Mode::Mcp(McpAction::Import {
                    source: (*source).to_string(),
                }),
                ["import"] => {
                    return Err(
                        "muta mcp import requires a source: `-` for stdin or a file path".into(),
                    );
                }
                [bad, ..] => return unexpected(bad),
            }
        }
        "skill" => {
            let extra_str: Vec<&str> = extra.iter().map(String::as_str).collect();
            match extra_str.as_slice() {
                [] => return Err("muta skill needs a subcommand: `muta skill ls`".into()),
                ["ls"] | ["list"] => Mode::Skill(SkillAction::List),
                [bad, ..] => return unexpected(bad),
            }
        }
        "doctor" => {
            if extra.is_empty() {
                Mode::Doctor
            } else {
                return unexpected(&extra[0]);
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
        "start" => &[
            ("--fg", "stay in the foreground (default: detach)"),
            ("--port <n>", "TCP port (default: MUTA_PORT, else 9800)"),
            ("--public", "bind all interfaces; requires the bearer token"),
            (
                "--no-local-auth",
                "drop the loopback bearer-token requirement",
            ),
            (
                "--idle-exit <min>",
                "auto-exit after <min> idle minutes (0 = never)",
            ),
            ("--grace <secs>", "graceful-drain budget in seconds"),
        ],
        "status" => &[
            ("--watch", "keep streaming live updates"),
            ("--json", "emit one JSON frame per update"),
            ("--all", "include idle sessions"),
            ("--diagnostic", "report discovery/lock/socket/log health"),
        ],
        "daemon" => &[
            ("start --fg", "stay in the foreground (default: detach)"),
            (
                "start --port <n>",
                "TCP port (default: MUTA_PORT, else 9800)",
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
        "mcp" => &[
            ("ls", "list configured MCP servers"),
            (
                "add <name> -- <cmd> [args…]",
                "register a stdio server in the user config",
            ),
            (
                "add <name> --url <endpoint>",
                "register a Streamable HTTP server",
            ),
            ("rm <name>", "remove a server from the user config"),
            (
                "enable/disable <name>",
                "toggle a server without removing it",
            ),
            ("get <name>", "print one server's config entry"),
            ("probe <name>", "connect once and list the advertised tools"),
            (
                "import (- | <file>)",
                "merge [mcp.*] TOML (e.g. `aegis-mcp print-config`)",
            ),
        ],
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
            out.push_str("muta — AI harness session daemon and control plane\n\n");
            out.push_str("Usage: muta [OPTIONS]\n       muta [OPTIONS] <COMMAND>\n\nCommands:\n");
            let width = COMMANDS.iter().map(|s| s.name.len()).max().unwrap_or(0);
            for spec in COMMANDS {
                out.push_str(&format!("  {:<width$}  {}\n", spec.name, spec.about));
            }
            out.push_str("\nOptions:\n");
            out.push_str("  -j, --json             emit structured JSON where supported\n");
            out.push_str("      --project <path>   operate on the project at <path>\n");
            out.push_str("  -h, --help             print help ('muta help <command>' for more)\n");
            out.push_str("  -V, --version          print the version and exit\n");
            out.push_str("\nEnvironment:\n");
            out.push_str(
                "  MUTA_HOME              instance root for isolated execution (<dir>/muta)\n",
            );
            out.push_str(
                "  MUTA_PORT              override default daemon TCP port (default: 9800)\n",
            );
        }
        Some(topic) => {
            let spec = resolve(topic, COMMANDS)?;
            out.push_str(&format!("muta {} — {}\n\n", spec.name, spec.about));
            out.push_str(&format!("Usage: muta {}", spec.name));
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
            if spec.name == "start" || spec.name == "daemon" {
                out.push_str(
                    "\nThe daemon hosts every session across every project. `start` runs it\n",
                );
                out.push_str("detached by default (--fg for systemd/tmux-style supervision);\n");
                out.push_str(
                    "`stop` drains within the daemon's --grace budget before escalating;\n",
                );
                out.push_str("`status` observes without ever spawning one.\n");
                out.push_str(
                    "\nMUTA_HOME points the whole instance (config, data, daemon files,\n",
                );
                out.push_str("port default via MUTA_PORT) at an isolated root — the dev/test\n");
                out.push_str("sandbox shape. See docs/reference/paths.md.\n");
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
        "# bash completion for muta — eval \"$(muta completions bash)\"\n\
         _muta() {{\n\
         \x20   local cur cmd\n\
         \x20   cur=\"${{COMP_WORDS[COMP_CWORD]}}\"\n\
         \x20   cmd=\"${{COMP_WORDS[1]}}\"\n\
         \x20   if [[ $COMP_CWORD -eq 1 ]]; then\n\
         \x20       COMPREPLY=($(compgen -W \"{} --project --json -j --help --version\" -- \"$cur\"))\n\
         \x20       return 0\n\
         \x20   fi\n\
         \x20   case \"$cmd\" in\n{}\n\
         \x20   esac\n\
         }}\n\
         complete -F _muta muta\n",
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
        "#compdef muta\n\
         # zsh completion for muta — save as `_muta` on $fpath\n\
         _muta() {{\n\
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
         _muta \"$@\"\n",
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
        "# fish completion for muta — save to ~/.config/fish/completions/muta.fish\n\
         set -l cmds {}\n\
         complete -c muta -n '__fish_use_subcommand' -f\n\
         complete -c muta -n '__fish_use_subcommand' -a \"$cmds\"\n",
        cmds.join(" ")
    );
    for spec in COMMANDS {
        let (subs, flags) = subs_and_flags(spec.name);
        for sub in subs {
            out.push_str(&format!(
                "complete -c muta -n '__fish_seen_subcommand_from {}' -f -a '{}' -d '{}'\n",
                spec.name,
                sub,
                subs_about(spec.name, sub)
            ));
        }
        for flag in flags {
            out.push_str(&format!(
                "complete -c muta -n '__fish_seen_subcommand_from {}' -l '{}' -d 'flag'\n",
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
mod surface_tests {
    use super::*;

    fn parse(tokens: &[&str]) -> Result<CliArgs, String> {
        super::parse(
            &tokens
                .iter()
                .map(|token| token.to_string())
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn bare_invocation_starts_foreground_daemon() {
        assert!(matches!(
            parse(&[]).unwrap().mode,
            Mode::Daemon(DaemonAction::Start {
                foreground: true,
                ..
            })
        ));
    }

    #[test]
    fn top_level_daemon_verbs_are_canonical() {
        assert!(matches!(
            parse(&["start"]).unwrap().mode,
            Mode::Daemon(DaemonAction::Start { .. })
        ));
        assert!(matches!(
            parse(&["stop"]).unwrap().mode,
            Mode::Daemon(DaemonAction::Stop)
        ));
        assert!(matches!(
            parse(&["status"]).unwrap().mode,
            Mode::Daemon(DaemonAction::Status { .. })
        ));
        assert!(matches!(
            parse(&["token"]).unwrap().mode,
            Mode::Daemon(DaemonAction::Token)
        ));
    }

    #[test]
    fn legacy_daemon_commands_remain_compatible() {
        assert!(matches!(
            parse(&["daemon", "start"]).unwrap().mode,
            Mode::Daemon(DaemonAction::Start { .. })
        ));
        assert!(matches!(
            parse(&["daemon", "token"]).unwrap().mode,
            Mode::Daemon(DaemonAction::Token)
        ));
    }

    #[test]
    fn tui_commands_are_not_accepted_by_muta() {
        for command in ["run", "attach", "dashboard", "showcase"] {
            assert!(parse(&[command]).is_err(), "{command}");
        }
    }

    #[test]
    fn mcp_verbs_parse() {
        assert!(matches!(
            parse(&["mcp", "ls"]).unwrap().mode,
            Mode::Mcp(McpAction::List)
        ));
        assert!(matches!(
            parse(&["mcp", "add", "aegis", "--", "/usr/bin/aegis-mcp"]).unwrap().mode,
            Mode::Mcp(McpAction::Add { ref name, ref command, url: None, .. })
                if name == "aegis" && command == &["/usr/bin/aegis-mcp".to_string()]
        ));
        // Server flags survive verbatim behind `--`.
        let expected: Vec<String> = ["npx", "--yes", "@ctx/mcp", "--port", "3"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let parsed = parse(&[
            "mcp", "add", "ctx", "--", "npx", "--yes", "@ctx/mcp", "--port", "3",
        ])
        .unwrap();
        assert!(matches!(
            parsed.mode,
            Mode::Mcp(McpAction::Add { ref command, .. }) if *command == expected
        ));
        assert!(matches!(
            parse(&["mcp", "add", "ctx", "--url", "https://example.com/mcp"])
                .unwrap()
                .mode,
            Mode::Mcp(McpAction::Add { ref url, .. }) if url.as_deref() == Some("https://example.com/mcp")
        ));
        assert!(matches!(
            parse(&["mcp", "rm", "aegis"]).unwrap().mode,
            Mode::Mcp(McpAction::Remove { ref name }) if name == "aegis"
        ));
        assert!(matches!(
            parse(&["mcp", "disable", "aegis"]).unwrap().mode,
            Mode::Mcp(McpAction::SetEnabled { enabled: false, .. })
        ));
        assert!(matches!(
            parse(&["mcp", "probe", "aegis"]).unwrap().mode,
            Mode::Mcp(McpAction::Probe { ref name }) if name == "aegis"
        ));
        assert!(matches!(
            parse(&["mcp", "import", "-"]).unwrap().mode,
            Mode::Mcp(McpAction::Import { ref source }) if source == "-"
        ));
    }

    #[test]
    fn mcp_add_rejects_incomplete_or_ambiguous_input() {
        // No transport declared.
        assert!(parse(&["mcp", "add", "aegis"]).is_err());
        // Both transports at once.
        assert!(parse(&["mcp", "add", "aegis", "--url", "https://x/mcp", "--", "cmd"]).is_err());
        // Unknown flag.
        assert!(parse(&["mcp", "add", "aegis", "--bogus", "--", "cmd"]).is_err());
        // Stray positional beyond the name.
        assert!(parse(&["mcp", "add", "aegis", "stray", "--", "cmd"]).is_err());
        // Malformed env pair.
        assert!(parse(&["mcp", "add", "aegis", "--env", "NOEQUALS", "--", "cmd"]).is_err());
    }
}
