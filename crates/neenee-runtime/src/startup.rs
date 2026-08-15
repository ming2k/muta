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
            /// by `BuiltinCmd::from_alias` so old invocations keep working
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
/// **presentation-only** counterpart of `BuiltinCmd::from_alias`.
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigAction {
    List,
    Get(String),
    Set { key: String, value: String },
    Path,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthAction {
    List,
    Set { provider: String, key: String },
    Show(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpAction {
    List,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillAction {
    List,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionAction {
    List {
        watch: bool,
        json: bool,
        include_idle: bool,
    },
    Attach(Option<String>),
    Delete(String),
    Dashboard,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonAction {
    Start {
        port: u16,
        public: bool,
        detach: bool,
        idle_exit_minutes: Option<u64>,
        shutdown_grace_secs: Option<u64>,
    },
    Stop,
    Status {
        watch: bool,
        json: bool,
        include_idle: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartupMode {
    Fresh,
    /// `neenee "prompt"`: launch interactive TUI and automatically send the initial prompt.
    FreshWithPrompt(String),
    /// `neenee -p "prompt"` / `neenee run "prompt"` / stdin piping: non-interactive headless run.
    Headless {
        prompt: String,
        json: bool,
    },
    Resume(Option<String>),
    /// `neenee resume` with no id: pop the sessions picker overlay so the
    /// user can choose which session to resume. Distinct from
    /// `Resume(None)` (which would auto-resume the most-recent session) so
    /// the two stay explicit.
    Picker,
    Doctor,
    /// Attach the TUI to a session hosted by the session daemon
    /// (`neenee attach [id]`). The id is the session to attach to; `None`
    /// attaches to whatever the daemon hosts.
    Attach(Option<String>),
    /// `neenee serve [...]` / `neenee daemon start [...]`: run the session daemon in foreground/background.
    Serve {
        port: u16,
        public: bool,
        detach: bool,
        idle_exit_minutes: Option<u64>,
        shutdown_grace_secs: Option<u64>,
    },
    /// `neenee stop` / `neenee daemon stop`: ask running daemon to shut down gracefully.
    Stop,
    /// `neenee status [--watch] [--json] [--all]`: observe the session daemon.
    Status {
        watch: bool,
        json: bool,
        include_idle: bool,
    },
    /// `neenee dashboard`: open the full-screen session dashboard directly.
    Dashboard,
    /// `neenee config [list|get|set|path]`
    Config(ConfigAction),
    /// `neenee auth [list|set|show]`
    Auth(AuthAction),
    /// `neenee mcp [list]`
    Mcp(McpAction),
    /// `neenee skill [list]`
    Skill(SkillAction),
    /// `neenee session [list|attach|delete|dashboard]`
    Session(SessionAction),
    /// `neenee daemon [start|stop|status]`
    Daemon(DaemonAction),
    /// `--version` / `-V`: print binary name and version.
    Version,
    /// `--help` / `-h` / `help [command]`: print help.
    Help(Option<String>),
    /// `neenee completions <shell>`: print static completion script.
    Completions(String),
    /// Render a single UI component in isolation for interactive development.
    #[cfg(debug_assertions)]
    Showcase(String),
}

/// The parsed command line: the startup mode plus the global options.
#[derive(Debug, Clone)]
pub struct CliArgs {
    pub mode: StartupMode,
    /// `--project <path>`: operate on the project at `<path>`.
    pub project: Option<PathBuf>,
    /// `--autopilot` / `--yolo` / `-y`: no confirmations or questions this session.
    pub autopilot: bool,
    /// `--single-instance`: require the exclusive per-project lock.
    pub single_instance: bool,
    /// `--interactive` / `-i`: force interactive TUI mode even when a prompt is supplied.
    pub interactive: bool,
    /// `--remote <address>`: address for remote daemon connection.
    pub remote: Option<String>,
    /// `--token <token>`: authorization bearer token for remote daemon connection.
    pub token: Option<String>,
}

/// The subcommands, in help order. `showcase` exists only in debug builds
/// and is handled separately; keep the completion scripts in sync.
const COMMANDS: &[&str] = &[
    "run",
    "exec",
    "session",
    "daemon",
    "config",
    "auth",
    "mcp",
    "skill",
    "skills",
    "resume",
    "attach",
    "serve",
    "stop",
    "status",
    "dashboard",
    "doctor",
    "completions",
    "help",
];

fn top_level_help() -> String {
    #[cfg(debug_assertions)]
    let showcase_line = "  showcase <n>  render a single UI component standalone (debug only)\n";
    #[cfg(not(debug_assertions))]
    let showcase_line = "";
    format!(
        "\
neenee — an expert AI coding assistant with tool access

Usage: neenee [OPTIONS] [PROMPT]
       neenee [OPTIONS] <COMMAND>

Commands:
  run <prompt>   execute a prompt non-interactively (headless / one-shot)
  session        manage sessions (list, attach, delete, dashboard)
  daemon         manage the session daemon (start, stop, status)
  config         inspect or update configuration (list, get, set, path)
  auth           manage provider credentials and API keys (list, set, show)
  mcp            inspect configured MCP servers (list)
  skill          list available skills
  status         show sessions needing attention (alias for 'session list')
  dashboard      open the full-screen session dashboard
  doctor         verify stored session integrity
  completions    print a shell completion script (bash, zsh, fish)
  help           print this help, or a command's help
{showcase_line}
Legacy aliases:
  resume [id]    resume a hosted session (picker when no id is given)
  attach [id]    attach the TUI to a hosted session
  serve          run the session daemon (foreground; --detach backgrounds it)
  stop           stop the session daemon gracefully

Options:
  -p, --print, --prompt <prompt>  run prompt non-interactively (headless)
  -i, --interactive               force interactive TUI mode
  -j, --json                      emit structured JSON in headless mode
  -y, --yolo, --autopilot         run without confirmations or questions
      --project <path>            operate on the project at <path>
      --remote <addr>             connect to a remote daemon
      --token <token>             bearer token for daemon connection
      --single-instance           require the exclusive per-project lock
  -h, --help                      print help ('neenee help <command>' for command help)
  -V, --version                   print the version and exit

With no command or options, neenee opens a fresh interactive session.
Passing a prompt (e.g. 'neenee \"fix the build\"') opens a session and runs it.
Piping into neenee (e.g. 'git diff | neenee') runs in non-interactive headless mode.
"
    )
}

const RUN_HELP: &str = "\
neenee run <prompt> — execute a prompt non-interactively (headless)

Streams assistant responses to stdout and tool activities to stderr,
exiting with code 0 on completion. Ideal for pipes, scripts, and CI.

Usage: neenee run [OPTIONS] <prompt>

Options:
  -y, --yolo, --autopilot  auto-approve tool execution and permissions
  -j, --json               emit structured JSON output
  -i, --interactive        switch to interactive TUI mode with this prompt
      --project <path>     operate on the project at <path>
";

const SESSION_HELP: &str = "\
neenee session [COMMAND] — manage daemon sessions

Usage: neenee session [COMMAND]

Commands:
  list, ls [OPTIONS]  list all daemon-hosted sessions (--watch, --json, --all)
  attach [id]         attach the TUI to a session
  delete <id>         delete a session by id
  dashboard           open the full-screen session dashboard
";

const DAEMON_HELP: &str = "\
neenee daemon [COMMAND] — manage the background session daemon

Usage: neenee daemon [COMMAND]

Commands:
  start [OPTIONS]     start the daemon (--port, --public, --detach, --idle-exit, --grace)
  stop                gracefully stop the running daemon
  status [OPTIONS]    show daemon session status (--watch, --json, --all)
";

const CONFIG_HELP: &str = "\
neenee config [COMMAND] — inspect or modify neenee configuration

Usage: neenee config [COMMAND]

Commands:
  list, show          show current configuration
  get <key>           get a configuration value (e.g. default_provider)
  set <key> <value>   update a configuration value and save to config.toml
  path                print the configuration file path
";

const AUTH_HELP: &str = "\
neenee auth [COMMAND] — manage API keys and provider credentials

Usage: neenee auth [COMMAND]

Commands:
  list, status        list configured provider keys and auth status
  set <provider> <k>  set API key for a provider (e.g. openai, anthropic, google, deepseek)
  show <provider>     show credential status for a provider
";

const MCP_HELP: &str = "\
neenee mcp [COMMAND] — inspect configured Model Context Protocol servers

Usage: neenee mcp [COMMAND]

Commands:
  list, ls            list all configured MCP servers and their commands
";

const SKILL_HELP: &str = "\
neenee skill [COMMAND] — inspect available skills and tools

Usage: neenee skill [COMMAND]

Commands:
  list, ls            list all discovered skills, descriptions, and locations
";

const RESUME_HELP: &str = "\
neenee resume [id] — resume a hosted session

With no id, opens the session picker. A session stays hosted by the session
daemon after you detach; resume re-attaches to it.

Usage: neenee resume [id] [--project <path>]
";

const ATTACH_HELP: &str = "\
neenee attach [id] — attach the TUI to a hosted session

With no id, attaches to whatever the daemon hosts (a lone session is
auto-selected; several produce a pick list). Spawns the session daemon on
demand when none is running.

Usage: neenee attach [id] [--project <path>]
";

const SERVE_HELP: &str = "\
neenee serve — run the session daemon

Runs in the foreground by default; Ctrl-C or SIGTERM stops it gracefully: it
stops accepting, closes live connections, tears every hosted session down
(SessionEnd hooks fire), and exits within its grace budget — a second
Ctrl-C skips the wait. Equivalent to the `neenee-server` binary. Exits on
its own after 5 idle minutes (zero sessions, zero clients) unless disabled.

Usage: neenee serve [OPTIONS]

Options:
      --port <n>          also listen on TCP port <n> (default: OS-assigned)
      --public            bind all interfaces and require a bearer token
      --detach            fork into the background and return
      --idle-exit <min>   auto-exit after <min> idle minutes (0 = never;
                          default: [daemon] idle_exit_minutes, else 5)
      --grace <secs>      graceful-shutdown budget before a forced exit
                          (default: [daemon] shutdown_grace_secs, else 10)
  -h, --help              print this help and exit
";

const STOP_HELP: &str = "\
neenee stop — stop the session daemon gracefully

Asks the running daemon to shut down through its control plane (the same
drain as Ctrl-C or SIGTERM): listeners close, live connections are closed,
each hosted session's SessionEnd hooks fire, the discovery record is
removed. Stopping a daemon that is not running is a success.

Usage: neenee stop
";

const STATUS_HELP: &str = "\
neenee status — show the daemon's sessions needing attention

Prints one snapshot and exits by default. Never spawns a daemon: observing
only makes sense against a running one.

Usage: neenee status [OPTIONS]

Options:
      --watch   keep streaming live updates
      --json    emit one JSON frame per update
      --all     include idle sessions (default: only sessions needing attention)
  -h, --help    print this help and exit
";

const DASHBOARD_HELP: &str = "\
neenee dashboard — open the full-screen session dashboard

Attaches to the most-recently-active hosted session as the carrier and
raises the dashboard over it. Never spawns a daemon.

Usage: neenee dashboard [--project <path>]
";

const DOCTOR_HELP: &str = "\
neenee doctor — verify stored session integrity

Checks the on-disk session store and exits. Never takes the per-project
lock, so it can run alongside a live instance.

Usage: neenee doctor [--project <path>]
";

const COMPLETIONS_HELP: &str = "\
neenee completions <shell> — print a shell completion script

Usage: neenee completions <bash|zsh|fish>

Examples:
  bash:  eval \"$(neenee completions bash)\"
  zsh:   neenee completions zsh > \"${fpath[1]}/_neenee\"
  fish:  neenee completions fish > ~/.config/fish/completions/neenee.fish
";

#[cfg(debug_assertions)]
const SHOWCASE_HELP: &str = "\
neenee showcase <name> — render a single UI component standalone

A Storybook for TUI components: wires one component's model and renderer to
a real terminal, with no agent, session, or network. Debug builds only.

Usage: neenee showcase <name>
";

/// The help text for a topic: `None` (or `help`) is the top-level help, a
/// command name its per-command text. Returns `None` for unknown topics.
pub fn help_text(topic: Option<&str>) -> Option<String> {
    let text = match topic {
        None | Some("help") => top_level_help(),
        Some("run") | Some("exec") => RUN_HELP.to_string(),
        Some("session") => SESSION_HELP.to_string(),
        Some("daemon") => DAEMON_HELP.to_string(),
        Some("config") => CONFIG_HELP.to_string(),
        Some("auth") => AUTH_HELP.to_string(),
        Some("mcp") => MCP_HELP.to_string(),
        Some("skill") | Some("skills") => SKILL_HELP.to_string(),
        Some("resume") => RESUME_HELP.to_string(),
        Some("attach") => ATTACH_HELP.to_string(),
        Some("serve") => SERVE_HELP.to_string(),
        Some("stop") => STOP_HELP.to_string(),
        Some("status") => STATUS_HELP.to_string(),
        Some("dashboard") => DASHBOARD_HELP.to_string(),
        Some("doctor") => DOCTOR_HELP.to_string(),
        Some("completions") => COMPLETIONS_HELP.to_string(),
        #[cfg(debug_assertions)]
        Some("showcase") => SHOWCASE_HELP.to_string(),
        Some(_) => return None,
    };
    Some(text)
}

const BASH_COMPLETION: &str = r#"# bash completion for neenee — eval "$(neenee completions bash)"
_neenee() {
    local cur cmd
    cur="${COMP_WORDS[COMP_CWORD]}"
    cmd="${COMP_WORDS[1]}"
    if [[ $COMP_CWORD -eq 1 ]]; then
        COMPREPLY=($(compgen -W "run session daemon config auth mcp skill resume attach serve status dashboard doctor completions help --prompt --print -p --interactive -i --json -j --yolo -y --project --remote --token --autopilot --single-instance --help --version" -- "$cur"))
        return 0
    fi
    case "$cmd" in
        session)     COMPREPLY=($(compgen -W "list ls attach delete kill dashboard --watch --json --all --help" -- "$cur")) ;;
        daemon)      COMPREPLY=($(compgen -W "start serve stop status --port --public --detach --idle-exit --grace --watch --json --all --help" -- "$cur")) ;;
        config)      COMPREPLY=($(compgen -W "list show get set path --help" -- "$cur")) ;;
        auth)        COMPREPLY=($(compgen -W "list status get set show --help" -- "$cur")) ;;
        mcp)         COMPREPLY=($(compgen -W "list ls --help" -- "$cur")) ;;
        skill|skills) COMPREPLY=($(compgen -W "list ls --help" -- "$cur")) ;;
        serve)       COMPREPLY=($(compgen -W "--port --public --detach --idle-exit --grace --help" -- "$cur")) ;;
        status)      COMPREPLY=($(compgen -W "--watch --json --all --help" -- "$cur")) ;;
        completions) COMPREPLY=($(compgen -W "bash zsh fish" -- "$cur")) ;;
        help)        COMPREPLY=($(compgen -W "run session daemon config auth mcp skill resume attach serve status dashboard doctor completions" -- "$cur")) ;;
    esac
}
complete -F _neenee neenee
"#;

const ZSH_COMPLETION: &str = r#"#compdef neenee
# zsh completion for neenee — save as `_neenee` in a directory on $fpath
_neenee() {
    local -a cmds
    cmds=(
        'run:execute a prompt non-interactively (headless)'
        'session:manage sessions (list, attach, delete, dashboard)'
        'daemon:manage the session daemon'
        'config:inspect or modify configuration'
        'auth:manage provider credentials'
        'mcp:inspect MCP servers'
        'skill:inspect discovered skills'
        'resume:resume a hosted session (picker when no id is given)'
        'attach:attach the TUI to a hosted session'
        'serve:run the session daemon'
        'status:show sessions needing attention'
        'dashboard:open the full-screen session dashboard'
        'doctor:verify stored session integrity'
        'completions:print a shell completion script'
        'help:print help'
    )
    if (( CURRENT == 2 )); then
        _describe 'command' cmds
        _arguments \
            '--project[operate on the project at <path>]:path:_files -/' \
            '--autopilot[run without confirmations this session]' \
            '--single-instance[require the exclusive per-project lock]' \
            '(-h --help)'{-h,--help}'[print help]' \
            '(-V --version)'{-V,--version}'[print the version]'
        return
    fi
    case "$words[2]" in
        serve|daemon)
            _arguments '--port[listen on TCP port <n>]:port:' '--public[bind all interfaces and require a bearer token]' '--detach[fork into the background]' ;;
        status|session)
            _arguments '--watch[keep streaming live updates]' '--json[emit one JSON frame per update]' '--all[include idle sessions]' ;;
        completions)
            _describe 'shell' '("bash:bash completion" "zsh:zsh completion" "fish:fish completion")' ;;
        help)
            _describe 'command' cmds ;;
    esac
}
_neenee "$@"
"#;

const FISH_COMPLETION: &str = r#"# fish completion for neenee — save to ~/.config/fish/completions/neenee.fish
set -l cmds run session daemon config auth mcp skill resume attach serve status dashboard doctor completions help
complete -c neenee -n '__fish_use_subcommand' -f
complete -c neenee -n '__fish_use_subcommand' -a "$cmds"
complete -c neenee -n '__fish_use_subcommand' -l project -r -F -d 'operate on the project at <path>'
complete -c neenee -n '__fish_use_subcommand' -l autopilot -d 'run without confirmations this session'
complete -c neenee -n '__fish_use_subcommand' -l single-instance -d 'require the exclusive per-project lock'
complete -c neenee -n '__fish_use_subcommand' -s h -l help -d 'print help'
complete -c neenee -n '__fish_use_subcommand' -s V -l version -d 'print the version'
complete -c neenee -n '__fish_seen_subcommand_from serve daemon' -l port -r -d 'listen on TCP port <n>'
complete -c neenee -n '__fish_seen_subcommand_from serve daemon' -l public -d 'bind all interfaces and require a bearer token'
complete -c neenee -n '__fish_seen_subcommand_from serve daemon' -l detach -d 'fork into the background'
complete -c neenee -n '__fish_seen_subcommand_from status session' -l watch -d 'keep streaming live updates'
complete -c neenee -n '__fish_seen_subcommand_from status session' -l json -d 'emit one JSON frame per update'
complete -c neenee -n '__fish_seen_subcommand_from status session' -l all -d 'include idle sessions'
complete -c neenee -n '__fish_seen_subcommand_from completions' -f -a 'bash zsh fish'
"#;

/// The static completion script for a shell, or `None` for an unknown shell.
pub fn completion_script(shell: &str) -> Option<&'static str> {
    match shell {
        "bash" => Some(BASH_COMPLETION),
        "zsh" => Some(ZSH_COMPLETION),
        "fish" => Some(FISH_COMPLETION),
        _ => None,
    }
}

/// clap-style "did you mean": an exact-prefix match first, then the closest
/// command within a small edit distance. Returns `None` when nothing is
/// close enough to be a confident suggestion.
fn suggest_command(input: &str) -> Option<&'static str> {
    if input.len() >= 2
        && let Some(prefix) = COMMANDS.iter().find(|c| c.starts_with(input))
    {
        return Some(prefix);
    }
    let tolerance = if input.len() >= 5 { 2 } else { 1 };
    COMMANDS
        .iter()
        .copied()
        .filter(|c| levenshtein(input, c) <= tolerance)
        .min_by_key(|c| levenshtein(input, c))
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

/// Parse the command line into a [`CliArgs`]. Returns a short, actionable
/// error string on misuse; the caller owns the exit policy (GNU convention:
/// print the error plus a pointer to `--help` on stderr, exit 2).
pub fn parse_args(args: Vec<String>) -> Result<CliArgs, String> {
    let mut iter = args.into_iter().peekable();
    let mut project: Option<PathBuf> = None;
    let mut autopilot = false;
    let mut single_instance = false;
    let mut interactive = false;
    let mut json = false;
    let mut version = false;
    let mut remote: Option<String> = None;
    let mut token: Option<String> = None;
    let mut prompt_flag: Option<String> = None;
    // `Some(inner)` once `--attach` is seen; `inner` is the optional session id.
    let mut attach: Option<Option<String>> = None;
    let mut rest = Vec::new();

    while let Some(arg) = iter.next() {
        if arg == "--project" {
            project = Some(PathBuf::from(
                iter.next().ok_or("--project requires a value")?,
            ));
        } else if let Some(value) = arg.strip_prefix("--project=") {
            project = Some(PathBuf::from(value));
        } else if arg == "--attach" {
            // `--attach <id>`: next token is session id when not a flag
            let id = match iter.peek() {
                Some(next) if !next.starts_with('-') => iter.next(),
                _ => None,
            };
            attach = Some(id);
        } else if let Some(value) = arg.strip_prefix("--attach=") {
            attach = Some(Some(value.to_string()));
        } else if arg == "--autopilot" || arg == "--yolo" || arg == "-y" {
            autopilot = true;
        } else if arg == "--single-instance" {
            single_instance = true;
        } else if arg == "--interactive" || arg == "-i" {
            interactive = true;
        } else if arg == "--json" || arg == "-j" {
            json = true;
        } else if arg == "--remote" {
            remote = Some(iter.next().ok_or("--remote requires a value")?);
        } else if let Some(value) = arg.strip_prefix("--remote=") {
            remote = Some(value.to_string());
        } else if arg == "--token" {
            token = Some(iter.next().ok_or("--token requires a value")?);
        } else if let Some(value) = arg.strip_prefix("--token=") {
            token = Some(value.to_string());
        } else if arg == "--print" || arg == "--prompt" || arg == "-p" {
            let p = match iter.peek() {
                Some(next) if !next.starts_with('-') => iter.next(),
                _ => None,
            };
            prompt_flag = Some(p.unwrap_or_default());
        } else if let Some(value) = arg
            .strip_prefix("--print=")
            .or_else(|| arg.strip_prefix("--prompt="))
            .or_else(|| arg.strip_prefix("-p="))
        {
            prompt_flag = Some(value.to_string());
        } else if arg == "--version" || arg == "-V" {
            version = true;
        } else {
            rest.push(arg);
        }
    }

    let ok = |mode| {
        Ok(CliArgs {
            mode,
            project: project.clone(),
            autopilot,
            single_instance,
            interactive,
            remote: remote.clone(),
            token: token.clone(),
        })
    };

    // `--version` short-circuits every other mode
    if version {
        return ok(StartupMode::Version);
    }

    // Help is position-sensitive: bare `--help`/`-h` (or `help`) is top-level
    if let Some(first) = rest.first().map(String::as_str) {
        if first == "-h" || first == "--help" || (first == "help" && rest.len() == 1) {
            return ok(StartupMode::Help(None));
        }
        if first == "help" {
            let topic = &rest[1];
            return match help_text(Some(topic)) {
                Some(_) => ok(StartupMode::Help(Some(topic.clone()))),
                None => Err(format!("unknown help topic '{topic}'")),
            };
        }
        if is_command(first) && rest[1..].iter().any(|a| a == "-h" || a == "--help") {
            return ok(StartupMode::Help(Some(first.to_string())));
        }
    }

    if let Some(id) = attach {
        if rest.is_empty() {
            return ok(StartupMode::Attach(id));
        }
        return Err(format!("--attach cannot be combined with '{}'", rest[0]));
    }

    let first = rest.first().map(String::as_str);

    let Some(cmd) = first else {
        if let Some(prompt) = prompt_flag {
            if prompt.trim().is_empty() {
                return Err("missing prompt for -p/--prompt".to_string());
            }
            if interactive {
                return ok(StartupMode::FreshWithPrompt(prompt));
            } else {
                return ok(StartupMode::Headless { prompt, json });
            }
        }
        return ok(StartupMode::Fresh);
    };

    let extra = &rest[1..];
    let unexpected = |arg: &str| {
        Err(format!(
            "unexpected argument '{arg}' found for 'neenee {cmd}'"
        ))
    };

    match cmd {
        "run" | "exec" => {
            let mut prompt_parts = Vec::new();
            if let Some(p) = prompt_flag
                && !p.is_empty()
            {
                prompt_parts.push(p);
            }
            for arg in extra {
                prompt_parts.push(arg.clone());
            }
            let prompt = prompt_parts.join(" ");
            if prompt.trim().is_empty() {
                return Err(format!("{cmd} requires a prompt"));
            }
            if interactive {
                ok(StartupMode::FreshWithPrompt(prompt))
            } else {
                ok(StartupMode::Headless { prompt, json })
            }
        }
        "config" => {
            let extra_str: Vec<&str> = extra.iter().map(String::as_str).collect();
            match extra_str.as_slice() {
                [] | ["list"] | ["show"] => ok(StartupMode::Config(ConfigAction::List)),
                ["path"] => ok(StartupMode::Config(ConfigAction::Path)),
                ["get", key] => ok(StartupMode::Config(ConfigAction::Get((*key).to_string()))),
                ["set", key, value] => ok(StartupMode::Config(ConfigAction::Set {
                    key: (*key).to_string(),
                    value: (*value).to_string(),
                })),
                ["get"] => Err("config get requires a key name".to_string()),
                ["set"] | ["set", _] => Err("config set requires <key> and <value>".to_string()),
                [bad, ..] => unexpected(bad),
            }
        }
        "auth" => {
            let extra_str: Vec<&str> = extra.iter().map(String::as_str).collect();
            match extra_str.as_slice() {
                [] | ["list"] | ["status"] => ok(StartupMode::Auth(AuthAction::List)),
                ["show", provider] => {
                    ok(StartupMode::Auth(AuthAction::Show((*provider).to_string())))
                }
                ["set", provider, key] => ok(StartupMode::Auth(AuthAction::Set {
                    provider: (*provider).to_string(),
                    key: (*key).to_string(),
                })),
                ["show"] => Err("auth show requires a provider name".to_string()),
                ["set"] | ["set", _] => Err("auth set requires <provider> and <key>".to_string()),
                [bad, ..] => unexpected(bad),
            }
        }
        "mcp" => {
            let extra_str: Vec<&str> = extra.iter().map(String::as_str).collect();
            match extra_str.as_slice() {
                [] | ["list"] | ["ls"] => ok(StartupMode::Mcp(McpAction::List)),
                [bad, ..] => unexpected(bad),
            }
        }
        "skill" | "skills" => {
            let extra_str: Vec<&str> = extra.iter().map(String::as_str).collect();
            match extra_str.as_slice() {
                [] | ["list"] | ["ls"] => ok(StartupMode::Skill(SkillAction::List)),
                [bad, ..] => unexpected(bad),
            }
        }
        "session" => {
            if extra.is_empty() {
                return ok(StartupMode::Session(SessionAction::List {
                    watch: false,
                    json,
                    include_idle: false,
                }));
            }
            let sub = &extra[0];
            let sub_extra = &extra[1..];
            match sub.as_str() {
                "ls" | "list" => {
                    let mut watch = false;
                    let mut json_out = json;
                    let mut include_idle = false;
                    for flag in sub_extra {
                        match flag.as_str() {
                            "--watch" => watch = true,
                            "--json" => json_out = true,
                            "--all" => include_idle = true,
                            other => return unexpected(other),
                        }
                    }
                    ok(StartupMode::Session(SessionAction::List {
                        watch,
                        json: json_out,
                        include_idle,
                    }))
                }
                "attach" => match sub_extra {
                    [] => ok(StartupMode::Session(SessionAction::Attach(None))),
                    [id] if !id.starts_with('-') => {
                        ok(StartupMode::Session(SessionAction::Attach(Some(id.clone()))))
                    }
                    [bad, ..] => unexpected(bad),
                },
                "delete" | "kill" | "rm" => match sub_extra {
                    [id] if !id.starts_with('-') => {
                        ok(StartupMode::Session(SessionAction::Delete(id.clone())))
                    }
                    [] => Err("session delete requires a session id".to_string()),
                    [bad, ..] => unexpected(bad),
                },
                "dashboard" => match sub_extra {
                    [] => ok(StartupMode::Session(SessionAction::Dashboard)),
                    [bad, ..] => unexpected(bad),
                },
                other => unexpected(other),
            }
        }
        "daemon" => {
            if extra.is_empty() {
                return ok(StartupMode::Daemon(DaemonAction::Status {
                    watch: false,
                    json,
                    include_idle: false,
                }));
            }
            let sub = &extra[0];
            let sub_extra = &extra[1..];
            match sub.as_str() {
                "start" | "serve" => {
                    let mut port: u16 = 0;
                    let mut public = false;
                    let mut detach = false;
                    let mut idle_exit: Option<u64> = None;
                    let mut grace_secs: Option<u64> = None;
                    let mut flags = sub_extra.iter();
                    while let Some(flag) = flags.next() {
                        match flag.as_str() {
                            "--port" => {
                                let value = flags.next().ok_or("--port requires a value")?;
                                port = value
                                    .parse()
                                    .map_err(|_| format!("invalid --port value '{value}'"))?;
                            }
                            "--public" => public = true,
                            "--detach" => detach = true,
                            "--idle-exit" => {
                                let value = flags.next().ok_or("--idle-exit requires a value")?;
                                idle_exit = Some(value.parse().map_err(|_| {
                                    format!(
                                        "invalid --idle-exit value '{value}' (minutes, 0 = never)"
                                    )
                                })?);
                            }
                            "--grace" => {
                                let value = flags.next().ok_or("--grace requires a value")?;
                                grace_secs = Some(value.parse().map_err(|_| {
                                    format!("invalid --grace value '{value}' (seconds)")
                                })?);
                            }
                            other => return unexpected(other),
                        }
                    }
                    ok(StartupMode::Daemon(DaemonAction::Start {
                        port,
                        public,
                        detach,
                        idle_exit_minutes: idle_exit,
                        shutdown_grace_secs: grace_secs,
                    }))
                }
                "stop" => match sub_extra {
                    [] => ok(StartupMode::Daemon(DaemonAction::Stop)),
                    [bad, ..] => unexpected(bad),
                },
                "status" => {
                    let mut watch = false;
                    let mut json_out = json;
                    let mut include_idle = false;
                    for flag in sub_extra {
                        match flag.as_str() {
                            "--watch" => watch = true,
                            "--json" => json_out = true,
                            "--all" => include_idle = true,
                            other => return unexpected(other),
                        }
                    }
                    ok(StartupMode::Daemon(DaemonAction::Status {
                        watch,
                        json: json_out,
                        include_idle,
                    }))
                }
                other => unexpected(other),
            }
        }
        "resume" => match extra {
            [] => ok(StartupMode::Picker),
            [id] if !id.starts_with('-') => ok(StartupMode::Resume(Some(id.clone()))),
            [bad, ..] => unexpected(bad),
        },
        "attach" => match extra {
            [] => ok(StartupMode::Attach(None)),
            [id] if !id.starts_with('-') => ok(StartupMode::Attach(Some(id.clone()))),
            [bad, ..] => unexpected(bad),
        },
        "dashboard" | "doctor" => match extra {
            [] => ok(if cmd == "dashboard" {
                StartupMode::Dashboard
            } else {
                StartupMode::Doctor
            }),
            [bad, ..] => unexpected(bad),
        },
        "status" => {
            let mut watch = false;
            let mut json_out = json;
            let mut include_idle = false;
            for flag in extra {
                match flag.as_str() {
                    "--watch" => watch = true,
                    "--json" => json_out = true,
                    "--all" => include_idle = true,
                    other => return unexpected(other),
                }
            }
            ok(StartupMode::Status {
                watch,
                json: json_out,
                include_idle,
            })
        }
        "serve" => {
            let mut port: u16 = 0;
            let mut public = false;
            let mut detach = false;
            let mut idle_exit: Option<u64> = None;
            let mut grace_secs: Option<u64> = None;
            let mut flags = extra.iter();
            while let Some(flag) = flags.next() {
                match flag.as_str() {
                    "--port" => {
                        let value = flags.next().ok_or("--port requires a value")?;
                        port = value
                            .parse()
                            .map_err(|_| format!("invalid --port value '{value}'"))?;
                    }
                    "--public" => public = true,
                    "--detach" => detach = true,
                    "--idle-exit" => {
                        let value = flags.next().ok_or("--idle-exit requires a value")?;
                        idle_exit = Some(value.parse().map_err(|_| {
                            format!("invalid --idle-exit value '{value}' (minutes, 0 = never)")
                        })?);
                    }
                    "--grace" => {
                        let value = flags.next().ok_or("--grace requires a value")?;
                        grace_secs = Some(value.parse().map_err(|_| {
                            format!("invalid --grace value '{value}' (seconds)")
                        })?);
                    }
                    other => return unexpected(other),
                }
            }
            ok(StartupMode::Serve {
                port,
                public,
                detach,
                idle_exit_minutes: idle_exit,
                shutdown_grace_secs: grace_secs,
            })
        }
        "stop" => match extra {
            [] => ok(StartupMode::Stop),
            _ => unexpected(extra.first().map(String::as_str).unwrap_or("")),
        },
        "completions" => match extra {
            [shell] if completion_script(shell).is_some() => {
                ok(StartupMode::Completions(shell.clone()))
            }
            [shell] if !shell.starts_with('-') => Err(format!(
                "unknown shell '{shell}' (expected bash, zsh, or fish)"
            )),
            [bad, ..] => unexpected(bad),
            [] => Err("missing shell name (expected bash, zsh, or fish)".to_string()),
        },
        #[cfg(debug_assertions)]
        "showcase" => match extra {
            [component] if !component.starts_with('-') => {
                ok(StartupMode::Showcase(component.clone()))
            }
            [bad, ..] => unexpected(bad),
            [] => Err("showcase requires a component name".to_string()),
        },
        other if other.starts_with('-') => Err(format!("unexpected argument '{other}' found")),
        other => {
            let is_multi_word = rest.len() > 1 || other.contains(' ') || other.contains('\n');
            if is_multi_word || prompt_flag.is_some() {
                let prompt = rest.join(" ");
                if interactive {
                    ok(StartupMode::FreshWithPrompt(prompt))
                } else if prompt_flag.is_some() || json {
                    ok(StartupMode::Headless { prompt, json })
                } else {
                    ok(StartupMode::FreshWithPrompt(prompt))
                }
            } else {
                let tip = suggest_command(other)
                    .map(|s| format!("\n\n  tip: a similar command exists: '{s}'"))
                    .unwrap_or_default();
                Err(format!("unrecognized command '{other}'{tip}"))
            }
        }
    }
}

fn is_command(word: &str) -> bool {
    COMMANDS.contains(&word) || (cfg!(debug_assertions) && word == "showcase")
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
/// `RUST_LOG=neenee=debug,neenee_runtime=trace`), because
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
            "neenee={lvl},neenee_contracts={lvl},neenee_runtime={lvl}"
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

    fn parse(tokens: &[&str]) -> CliArgs {
        parse_args(args(tokens)).expect("expected the args to parse")
    }

    fn parse_err(tokens: &[&str]) -> String {
        parse_args(args(tokens)).expect_err("expected the args to be rejected")
    }

    #[test]
    fn no_args_is_fresh() {
        let parsed = parse(&[]);
        assert!(matches!(parsed.mode, StartupMode::Fresh));
        assert!(parsed.project.is_none());
        assert!(!parsed.autopilot);
        assert!(!parsed.single_instance);
    }

    #[test]
    fn version_flags_short_circuit_to_version_mode() {
        assert!(matches!(parse(&["--version"]).mode, StartupMode::Version));
        assert!(matches!(parse(&["-V"]).mode, StartupMode::Version));
        // Version wins even over an explicit subcommand.
        assert!(matches!(
            parse(&["--version", "resume"]).mode,
            StartupMode::Version
        ));
    }

    #[test]
    fn help_forms_resolve_to_help_mode() {
        assert!(matches!(parse(&["--help"]).mode, StartupMode::Help(None)));
        assert!(matches!(parse(&["-h"]).mode, StartupMode::Help(None)));
        assert!(matches!(parse(&["help"]).mode, StartupMode::Help(None)));
        assert!(matches!(
            parse(&["help", "serve"]).mode,
            StartupMode::Help(Some(topic)) if topic == "serve"
        ));
        assert!(matches!(
            parse(&["serve", "--help"]).mode,
            StartupMode::Help(Some(topic)) if topic == "serve"
        ));
        assert!(matches!(
            parse(&["status", "-h"]).mode,
            StartupMode::Help(Some(topic)) if topic == "status"
        ));
    }

    #[test]
    fn help_unknown_topic_errors() {
        let error = parse_args(args(&["help", "nope"])).unwrap_err();
        assert!(error.contains("unknown help topic 'nope'"), "{error}");
    }

    #[test]
    fn every_command_has_a_help_text() {
        for command in COMMANDS {
            assert!(
                help_text(Some(command)).is_some(),
                "command '{command}' lacks a help text"
            );
        }
        #[cfg(debug_assertions)]
        assert!(help_text(Some("showcase")).is_some());
        assert!(help_text(Some("frobnicate")).is_none());
    }

    #[test]
    fn unknown_command_suggests_a_similar_one() {
        let error = parse_args(args(&["statsu"])).unwrap_err();
        assert!(error.contains("unrecognized command 'statsu'"), "{error}");
        assert!(error.contains("'status'"), "{error}");
        // A distant typo gets no suggestion rather than a misleading one.
        let error = parse_args(args(&["frobnicate"])).unwrap_err();
        assert!(error.contains("unrecognized command"), "{error}");
        assert!(!error.contains("tip:"), "{error}");
    }

    #[test]
    fn unknown_option_is_not_reported_as_a_command() {
        let error = parse_args(args(&["--frobnicate"])).unwrap_err();
        assert!(
            error.contains("unexpected argument '--frobnicate'"),
            "{error}"
        );
        assert!(!error.contains("unrecognized command"), "{error}");
    }

    #[test]
    fn known_commands_reject_unexpected_arguments() {
        let error = parse_args(args(&["dashboard", "--watch"])).unwrap_err();
        assert!(error.contains("for 'neenee dashboard'"), "{error}");
        let error = parse_args(args(&["status", "--bogus"])).unwrap_err();
        assert!(error.contains("for 'neenee status'"), "{error}");
        let error = parse_args(args(&["serve", "--bogus"])).unwrap_err();
        assert!(error.contains("for 'neenee serve'"), "{error}");
    }

    #[test]
    fn completions_forms() {
        assert!(matches!(
            parse(&["completions", "bash"]).mode,
            StartupMode::Completions(ref shell) if shell == "bash"
        ));
        let error = parse_args(args(&["completions"])).unwrap_err();
        assert!(error.contains("missing shell"), "{error}");
        let error = parse_args(args(&["completions", "tcsh"])).unwrap_err();
        assert!(error.contains("unknown shell 'tcsh'"), "{error}");
    }

    #[test]
    fn completion_scripts_exist_for_each_shell() {
        for shell in ["bash", "zsh", "fish"] {
            let script = completion_script(shell).expect("script must exist");
            assert!(script.contains("neenee"), "{shell}");
        }
        assert!(completion_script("tcsh").is_none());
    }

    #[test]
    fn resume_forms_are_unchanged() {
        assert!(matches!(parse(&["resume"]).mode, StartupMode::Picker));
        assert!(matches!(
            parse(&["resume", "abc"]).mode,
            StartupMode::Resume(Some(id)) if id == "abc"
        ));
    }

    #[test]
    fn attach_bare_means_any_session() {
        assert!(matches!(
            parse(&["--attach"]).mode,
            StartupMode::Attach(None)
        ));
    }

    #[test]
    fn attach_with_id_both_styles() {
        assert!(matches!(
            parse(&["--attach", "sess-1"]).mode,
            StartupMode::Attach(Some(id)) if id == "sess-1"
        ));
        assert!(matches!(
            parse(&["--attach=sess-2"]).mode,
            StartupMode::Attach(Some(id)) if id == "sess-2"
        ));
    }

    #[test]
    fn attach_does_not_swallow_a_following_flag() {
        // `--attach --project /p` must parse as Attach(None) + project, not
        // as an attach id of "--project".
        let parsed = parse(&["--attach", "--project", "/p"]);
        assert!(matches!(parsed.mode, StartupMode::Attach(None)));
        assert_eq!(parsed.project, Some(PathBuf::from("/p")));
    }

    #[test]
    fn attach_combines_with_project_flag() {
        let parsed = parse(&["--project", "/p", "--attach", "s"]);
        assert!(matches!(
            parsed.mode,
            StartupMode::Attach(Some(id)) if id == "s"
        ));
        assert_eq!(parsed.project, Some(PathBuf::from("/p")));
    }

    #[test]
    fn project_flag_still_works_without_attach() {
        let parsed = parse(&["--project=/q"]);
        assert!(matches!(parsed.mode, StartupMode::Fresh));
        assert_eq!(parsed.project, Some(PathBuf::from("/q")));
    }

    #[test]
    fn serve_subcommand_forms() {
        assert!(matches!(
            parse(&["serve"]).mode,
            StartupMode::Serve {
                port: 0,
                public: false,
                detach: false,
                idle_exit_minutes: None,
                shutdown_grace_secs: None
            }
        ));
        assert!(matches!(
            parse(&["serve", "--port", "8765", "--public", "--detach"]).mode,
            StartupMode::Serve {
                port: 8765,
                public: true,
                detach: true,
                idle_exit_minutes: None,
                shutdown_grace_secs: None
            }
        ));
        let parsed = parse(&["serve", "--idle-exit", "0", "--grace", "30"]);
        assert!(matches!(
            parsed.mode,
            StartupMode::Serve {
                idle_exit_minutes: Some(0),
                shutdown_grace_secs: Some(30),
                ..
            }
        ));
        let parsed = parse(&["--project", "/p", "serve"]);
        assert!(matches!(parsed.mode, StartupMode::Serve { .. }));
        assert_eq!(parsed.project, Some(PathBuf::from("/p")));
    }

    #[test]
    fn stop_subcommand_parses_and_rejects_extras() {
        assert!(matches!(parse(&["stop"]).mode, StartupMode::Stop));
        assert!(!parse_err(&["stop", "--force"]).is_empty());
        assert!(!parse_err(&["stop", "now"]).is_empty());
    }

    #[test]
    fn dashboard_subcommand_form() {
        assert!(matches!(parse(&["dashboard"]).mode, StartupMode::Dashboard));
        let parsed = parse(&["--project", "/p", "dashboard"]);
        assert!(matches!(parsed.mode, StartupMode::Dashboard));
        assert_eq!(parsed.project, Some(PathBuf::from("/p")));
    }

    #[test]
    fn status_subcommand_forms() {
        assert!(matches!(
            parse(&["status"]).mode,
            StartupMode::Status {
                watch: false,
                json: false,
                include_idle: false
            }
        ));
        assert!(matches!(
            parse(&["status", "--watch", "--json", "--all"]).mode,
            StartupMode::Status {
                watch: true,
                json: true,
                include_idle: true
            }
        ));
        let parsed = parse(&["--project", "/p", "status", "--watch"]);
        assert!(matches!(
            parsed.mode,
            StartupMode::Status { watch: true, .. }
        ));
        assert_eq!(parsed.project, Some(PathBuf::from("/p")));
    }

    #[test]
    fn run_subcommand_parses_prompt() {
        let parsed = parse(&["run", "explain", "this", "code"]);
        assert!(matches!(
            parsed.mode,
            StartupMode::Headless { ref prompt, json: false } if prompt == "explain this code"
        ));

        let parsed_json = parse(&["run", "-j", "fix bug"]);
        assert!(matches!(
            parsed_json.mode,
            StartupMode::Headless { ref prompt, json: true } if prompt == "fix bug"
        ));

        let parsed_interactive = parse(&["run", "-i", "my prompt"]);
        assert!(matches!(
            parsed_interactive.mode,
            StartupMode::FreshWithPrompt(ref prompt) if prompt == "my prompt"
        ));
    }

    #[test]
    fn print_prompt_flag_parses() {
        let parsed = parse(&["-p", "check code"]);
        assert!(matches!(
            parsed.mode,
            StartupMode::Headless { ref prompt, json: false } if prompt == "check code"
        ));

        let parsed_prompt = parse(&["--prompt=fix lint"]);
        assert!(matches!(
            parsed_prompt.mode,
            StartupMode::Headless { ref prompt, json: false } if prompt == "fix lint"
        ));
    }

    #[test]
    fn positional_multi_word_prompt_parses_to_fresh_with_prompt() {
        let parsed = parse(&["fix", "the", "build"]);
        assert!(matches!(
            parsed.mode,
            StartupMode::FreshWithPrompt(ref prompt) if prompt == "fix the build"
        ));
    }

    #[test]
    fn config_subcommand_forms() {
        assert!(matches!(
            parse(&["config"]).mode,
            StartupMode::Config(ConfigAction::List)
        ));
        assert!(matches!(
            parse(&["config", "path"]).mode,
            StartupMode::Config(ConfigAction::Path)
        ));
        assert!(matches!(
            parse(&["config", "get", "default_provider"]).mode,
            StartupMode::Config(ConfigAction::Get(ref key)) if key == "default_provider"
        ));
        assert!(matches!(
            parse(&["config", "set", "default_model", "gpt-4o"]).mode,
            StartupMode::Config(ConfigAction::Set { ref key, ref value }) if key == "default_model" && value == "gpt-4o"
        ));
    }

    #[test]
    fn auth_subcommand_forms() {
        assert!(matches!(
            parse(&["auth"]).mode,
            StartupMode::Auth(AuthAction::List)
        ));
        assert!(matches!(
            parse(&["auth", "list"]).mode,
            StartupMode::Auth(AuthAction::List)
        ));
        assert!(matches!(
            parse(&["auth", "show", "openai"]).mode,
            StartupMode::Auth(AuthAction::Show(ref p)) if p == "openai"
        ));
        assert!(matches!(
            parse(&["auth", "set", "openai", "sk-123456"]).mode,
            StartupMode::Auth(AuthAction::Set { ref provider, ref key }) if provider == "openai" && key == "sk-123456"
        ));
    }

    #[test]
    fn mcp_and_skill_subcommands() {
        assert!(matches!(
            parse(&["mcp"]).mode,
            StartupMode::Mcp(McpAction::List)
        ));
        assert!(matches!(
            parse(&["mcp", "list"]).mode,
            StartupMode::Mcp(McpAction::List)
        ));
        assert!(matches!(
            parse(&["skill", "list"]).mode,
            StartupMode::Skill(SkillAction::List)
        ));
        assert!(matches!(
            parse(&["skills"]).mode,
            StartupMode::Skill(SkillAction::List)
        ));
    }

    #[test]
    fn session_subcommand_forms() {
        assert!(matches!(
            parse(&["session"]).mode,
            StartupMode::Session(SessionAction::List { watch: false, json: false, include_idle: false })
        ));
        assert!(matches!(
            parse(&["session", "ls", "--watch"]).mode,
            StartupMode::Session(SessionAction::List { watch: true, json: false, include_idle: false })
        ));
        assert!(matches!(
            parse(&["session", "attach", "s123"]).mode,
            StartupMode::Session(SessionAction::Attach(Some(ref id))) if id == "s123"
        ));
        assert!(matches!(
            parse(&["session", "delete", "s123"]).mode,
            StartupMode::Session(SessionAction::Delete(ref id)) if id == "s123"
        ));
        assert!(matches!(
            parse(&["session", "dashboard"]).mode,
            StartupMode::Session(SessionAction::Dashboard)
        ));
    }

    #[test]
    fn daemon_subcommand_forms() {
        assert!(matches!(
            parse(&["daemon", "start", "--port", "9000"]).mode,
            StartupMode::Daemon(DaemonAction::Start { port: 9000, .. })
        ));
        assert!(matches!(
            parse(&["daemon", "stop"]).mode,
            StartupMode::Daemon(DaemonAction::Stop)
        ));
        assert!(matches!(
            parse(&["daemon", "status", "--watch"]).mode,
            StartupMode::Daemon(DaemonAction::Status { watch: true, .. })
        ));
    }
}
