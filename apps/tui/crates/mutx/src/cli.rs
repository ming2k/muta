//! Command-line contract for the `mutx` terminal application.
//!
//! `mutx` is a client of the Muta daemon. It owns interactive and headless
//! terminal workflows; daemon lifecycle, configuration, credentials, MCP,
//! skills, and daemon administration belong to the `muta` core command.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// What the user asked the terminal application to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    /// Bare `mutx` / `mutx "prompt"`: open an interactive session.
    Fresh,
    /// `mutx run <prompt>`: execute a headless one-shot.
    Run { prompt: String },
    /// `mutx attach [id]`: join a hosted session, using the picker without id.
    Attach { id: Option<String> },
    /// Open the full-screen session dashboard.
    Dashboard,
    /// `mutx completions <shell>`.
    Completions(Shell),
    /// `--version` / `-V`.
    Version,
    /// `--help` / `-h` / `help [topic]`.
    Help(Option<String>),
    /// Render one UI component standalone (debug builds only).
    #[cfg(debug_assertions)]
    Showcase(String),
}

/// A shell whose completion script `mutx completions` can print.
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

/// The parsed command line: the [`Mode`] plus terminal-app global options.
#[derive(Debug, Clone)]
pub struct CliArgs {
    pub mode: Mode,
    /// `--project <path>`: operate on the project at `<path>`.
    pub project: Option<PathBuf>,
    /// `--yolo` / `-y` / `--autopilot`: auto-approve all tool permissions.
    pub yolo: bool,
    /// `--interactive` / `-i`: force the TUI when headless would apply.
    pub interactive: bool,
    /// `-p`/`--prompt`/`--print` or a positional prompt phrase.
    pub prompt: Option<String>,
    /// Whether the prompt came from `-p`/`--prompt` rather than positionally.
    pub prompt_from_flag: bool,
    /// `-j`/`--json`: structured output where supported.
    pub json: bool,
    /// `--remote <addr>` / `--token <token>`: daemon endpoint override.
    pub remote: Option<String>,
    pub token: Option<String>,
    /// `--home <dir>`: instance root, equivalent to `MUTA_HOME`.
    pub home: Option<PathBuf>,
    /// Per-category path overrides, equivalent to the matching `MUTA_*_DIR`.
    pub config_dir: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub cache_dir: Option<PathBuf>,
}

struct Spec {
    name: &'static str,
    about: &'static str,
}

const COMMANDS: &[Spec] = &[
    Spec {
        name: "run",
        about: "execute a prompt non-interactively (headless one-shot)",
    },
    Spec {
        name: "attach",
        about: "attach the TUI to a hosted session (picker when no id)",
    },
    Spec {
        name: "dashboard",
        about: "open the full-screen session dashboard",
    },
    Spec {
        name: "completions",
        about: "print a shell completion script",
    },
    #[cfg(debug_assertions)]
    Spec {
        name: "showcase",
        about: "render a single UI component standalone (debug only)",
    },
    Spec {
        name: "help",
        about: "print help for a command",
    },
];

const CORE_COMMANDS: &[&str] = &[
    "daemon", "session", "config", "auth", "mcp", "skill", "doctor",
];

fn command_index() -> BTreeMap<&'static str, &'static str> {
    COMMANDS.iter().map(|spec| (spec.name, spec.name)).collect()
}

fn resolve(word: &str) -> Option<&'static Spec> {
    COMMANDS.iter().find(|spec| spec.name == word)
}

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

fn split_flag(arg: &str) -> (&str, Option<&str>) {
    match arg.split_once('=') {
        Some((name, value)) if name.starts_with("--") => (name, Some(value)),
        _ => (arg, None),
    }
}

fn flag_value<'a, I: Iterator<Item = &'a String>>(
    flag: &str,
    inline: Option<&str>,
    iter: &mut I,
) -> Result<String, FlagError> {
    if let Some(value) = inline {
        return Ok(value.to_string());
    }
    iter.next()
        .cloned()
        .ok_or_else(|| FlagError::new(flag, "requires a value"))
}

/// Parse the command line. The caller owns error rendering and exit policy.
pub fn parse(args: &[String]) -> Result<CliArgs, String> {
    let mut project = None;
    let mut yolo = false;
    let mut interactive = false;
    let mut prompt = None;
    let mut prompt_from_flag = false;
    let mut json = false;
    let mut version = false;
    let mut remote = None;
    let mut token = None;
    let mut home = None;
    let mut config_dir = None;
    let mut data_dir = None;
    let mut state_dir = None;
    let mut cache_dir = None;
    let mut rest = Vec::new();

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
            "--yolo" | "-y" | "--autopilot" => yolo = true,
            "--interactive" | "-i" => interactive = true,
            "--json" | "-j" => json = true,
            "--print" | "--prompt" | "-p" => {
                prompt = Some(flag_value("-p/--prompt", inline, &mut iter)?);
                prompt_from_flag = true;
            }
            "--remote" => remote = Some(flag_value("--remote", inline, &mut iter)?),
            "--token" => token = Some(flag_value("--token", inline, &mut iter)?),
            "--version" | "-V" => version = true,
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
                    "--single-instance was removed: the unified daemon owns every session".into(),
                );
            }
            _ => rest.push(arg.clone()),
        }
    }

    let base = |mode| CliArgs {
        mode,
        project: project.clone(),
        yolo,
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
            && rest[1..].iter().any(|arg| arg == "-h" || arg == "--help")
        {
            return ok(Mode::Help(Some(first.to_string())));
        }
    }

    let Some(cmd) = rest.first().cloned() else {
        return ok(Mode::Fresh);
    };
    let extra = &rest[1..];
    let unexpected = |arg: &str| {
        Err(format!(
            "unexpected argument '{arg}' found for 'mutx {cmd}'"
        ))
    };

    if CORE_COMMANDS.contains(&cmd.as_str()) {
        return Err(format!(
            "'{cmd}' is a muta service command; run `muta {cmd}` instead"
        ));
    }

    // A multi-word unknown phrase is an interactive prompt. A single unknown
    // word remains an error so command typos do not silently reach the model.
    if resolve(&cmd).is_none() && !cmd.starts_with('-') {
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
                    .map(|name| format!("\n\n  tip: a similar command exists: '{name}'"))
                    .unwrap_or_default();
                return Err(format!("unrecognized command '{cmd}'{tip}"));
            }
        }
    }

    let Some(spec) = resolve(&cmd) else {
        return Err(format!("unrecognized command '{cmd}'"));
    };
    let mode = match spec.name {
        "run" => {
            let mut parts = Vec::new();
            if let Some(value) = prompt.as_ref().filter(|value| !value.is_empty()) {
                parts.push(value.clone());
            }
            parts.extend(extra.iter().cloned());
            let text = parts.join(" ");
            if text.trim().is_empty() {
                return Err("run requires a prompt".into());
            }
            Mode::Run { prompt: text }
        }
        "attach" => match extra {
            [] => Mode::Attach { id: None },
            [id] if !id.starts_with('-') => Mode::Attach {
                id: Some(id.clone()),
            },
            [bad, ..] => return unexpected(bad),
        },
        "dashboard" => match extra {
            [] => Mode::Dashboard,
            [bad, ..] => return unexpected(bad),
        },
        "completions" => match extra {
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
        "showcase" => match extra {
            [component] => Mode::Showcase(component.clone()),
            [] => return Err("showcase requires a component name".into()),
            [bad, ..] => return unexpected(bad),
        },
        "help" => Mode::Help(None),
        _ => unreachable!("closed command set"),
    };

    Ok(base(mode))
}

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
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0; b.len() + 1];
    for (i, left) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, right) in b.iter().enumerate() {
            current[j + 1] = (previous[j] + usize::from(left != right))
                .min(previous[j + 1] + 1)
                .min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

/// Top-level or per-command help text.
pub fn help_text(topic: Option<&str>) -> Option<String> {
    let mut out = String::new();
    match topic {
        None => {
            out.push_str("mutx — Muta's terminal application\n\n");
            out.push_str("Usage: mutx [OPTIONS] [PROMPT]\n");
            out.push_str("       mutx [OPTIONS] <COMMAND>\n\nCommands:\n");
            let width = COMMANDS
                .iter()
                .map(|spec| spec.name.len())
                .max()
                .unwrap_or(0);
            for spec in COMMANDS {
                out.push_str(&format!("  {:<width$}  {}\n", spec.name, spec.about));
            }
            out.push_str("\nOptions:\n");
            out.push_str("  -p, --prompt <prompt>  run the prompt non-interactively (headless)\n");
            out.push_str("  -i, --interactive      force interactive TUI mode\n");
            out.push_str("  -j, --json             emit structured JSON where supported\n");
            out.push_str("  --autopilot, -y, --yolo  run without confirmations or questions\n");
            out.push_str("      --project <path>   operate on the project at <path>\n");
            out.push_str("      --home <dir>       use an instance rooted at <dir>/muta\n");
            out.push_str("      --remote <addr>    connect to a remote Muta daemon\n");
            out.push_str("      --token <token>    bearer token for daemon connection\n");
            out.push_str("  -h, --help             print help ('mutx help <command>' for more)\n");
            out.push_str("  -V, --version          print the version and exit\n");
            out.push_str("\nWith no command, mutx opens a fresh interactive session.\n");
            out.push_str("It checks the Muta daemon first and starts `muta` when needed.\n");
            out.push_str("Daemon and service administration remains under the `muta` command.\n");
        }
        Some(topic) => {
            let spec = resolve(topic)?;
            out.push_str(&format!("mutx {} — {}\n\n", spec.name, spec.about));
            out.push_str(&format!("Usage: mutx {}\n", spec.name));
            match spec.name {
                "run" => {
                    out.push_str(
                        "\nThe prompt streams to stdout (tool activity to stderr), exiting 0\n",
                    );
                    out.push_str("on completion. The Muta daemon starts on demand.\n");
                }
                "attach" => {
                    out.push_str(
                        "\nWith no id the TUI session picker opens (a lone hosted session is\n",
                    );
                    out.push_str("auto-selected). The Muta daemon starts on demand.\n");
                }
                _ => {}
            }
        }
    }
    Some(out)
}

/// A static completion script generated from the same command table.
pub fn completion_script(shell: Shell) -> String {
    let commands = COMMANDS
        .iter()
        .map(|spec| spec.name)
        .collect::<Vec<_>>()
        .join(" ");
    match shell {
        Shell::Bash => format!(
            "# bash completion for mutx — eval \"$(mutx completions bash)\"\n\
             _mutx() {{\n\
             \x20   local cur\n\
             \x20   cur=\"${{COMP_WORDS[COMP_CWORD]}}\"\n\
             \x20   if [[ $COMP_CWORD -eq 1 ]]; then\n\
             \x20       COMPREPLY=($(compgen -W \"{commands} --project --home --remote --token --prompt -p --interactive -i --json -j --yolo -y --autopilot --help --version\" -- \"$cur\"))\n\
             \x20   fi\n\
             }}\n\
             complete -F _mutx mutx\n"
        ),
        Shell::Zsh => {
            let entries = COMMANDS
                .iter()
                .map(|spec| format!("        '{}:{}'", spec.name, spec.about))
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "#compdef mutx\n\
                 _arguments '1:command:(({entries}))' '*::argument:->args'\n"
            )
        }
        Shell::Fish => format!(
            "# fish completion for mutx\n\
             set -l cmds {commands}\n\
             complete -c mutx -n '__fish_use_subcommand' -f -a \"$cmds\"\n\
             complete -c mutx -l home -d 'separate Muta instance root' -r\n"
        ),
    }
}

#[cfg(test)]
mod tests {
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
    fn bare_invocation_starts_a_fresh_tui_session() {
        assert!(matches!(parse(&[]).unwrap().mode, Mode::Fresh));
    }

    #[test]
    fn client_commands_remain_on_the_mutx_surface() {
        assert!(matches!(
            parse(&["attach"]).unwrap().mode,
            Mode::Attach { id: None }
        ));
        assert!(matches!(
            parse(&["run", "hello"]).unwrap().mode,
            Mode::Run { .. }
        ));
    }

    #[test]
    fn core_commands_point_to_muta() {
        for command in CORE_COMMANDS {
            let error = parse(&[command]).unwrap_err();
            assert!(error.contains("muta service command"), "{command}: {error}");
        }
    }

    #[test]
    fn positional_prompt_and_path_overrides_are_preserved() {
        let parsed = parse(&["--home", "/tmp/muta-test", "fix", "the", "build"]).unwrap();
        assert!(matches!(parsed.mode, Mode::Fresh));
        assert_eq!(parsed.prompt.as_deref(), Some("fix the build"));
        assert_eq!(
            parsed.home.as_deref(),
            Some(std::path::Path::new("/tmp/muta-test"))
        );
    }
}
