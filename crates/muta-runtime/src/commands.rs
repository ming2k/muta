//! Project and user-defined slash command templates.
//!
//! Commands are markdown files stored in:
//!   - Project-local: `.muta/commands/` (highest priority; loaded only while
//!     the exact Rules-domain content is attested)
//!   - User-global (XDG): `$XDG_DATA_HOME/muta/commands/`

use muta_contracts::WorkspaceTrustState;
use muta_persistence::paths;
use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomCommand {
    pub name: String,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub usage: Option<String>,
    pub intent_keywords: Vec<String>,
    pub source: PathBuf,
    pub template: String,
}

#[derive(Debug, Deserialize, Default)]
struct Frontmatter {
    name: Option<String>,
    summary: Option<String>,
    description: Option<String>,
    usage: Option<String>,
    intent_keywords: Option<Vec<String>>,
    aliases: Option<Vec<String>>,
}

/// A project-local command that overrode a same-named user-global command
/// during discovery (the project directory has higher priority).
///
/// Surfaced to the user as a warning notice: a cloned or vendored repo can
/// shadow the user's own `/<name>` command merely by reusing its name, and a
/// silent override would make that prompt-text injection invisible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowedCommand {
    /// The command name claimed by both scopes (no leading `/`).
    pub name: String,
    /// Path of the winning project-local command file.
    pub winner_source: PathBuf,
}

/// Outcome of a command scan: the commands to register plus any project-over-
/// user shadowing observed (empty when the project directory was not scanned).
#[derive(Debug, Default)]
pub struct CommandDiscovery {
    pub commands: Vec<CustomCommand>,
    pub shadowed: Vec<ShadowedCommand>,
}

/// Discover commands for a session rooted at `project_root`, gating the
/// *project-local* directory (`.muta/commands`) behind content-bound Rules
/// trust. User-global commands (`$XDG_DATA_HOME/muta/commands`) always
/// load — they live on the user's own machine and are trusted
/// unconditionally, mirroring the global-config rule.
///
/// Unless `trust_state` is [`WorkspaceTrustState::Trusted`],
/// project-local slash commands are not discovered, completed, or runnable.
///
/// The project root is passed explicitly (not derived from the process cwd):
/// under the unified daemon (ADR-0096) one process hosts sessions for many
/// projects, so the cwd belongs to whichever client spawned it.
pub fn discover_commands_with_trust(
    project_root: &Path,
    trust_state: WorkspaceTrustState,
) -> CommandDiscovery {
    merge_command_scopes(
        &project_commands_dir(project_root),
        &paths::get().user_commands_dir(),
        trust_state,
    )
}

/// Merge the project and user command scopes (testable core of
/// [`discover_commands_with_trust`]). Order encodes priority: project (highest)
/// → user. A project command claiming a name a user command also holds is a
/// shadow event the user must see — a cloned repo could be overriding their
/// own `/<name>`.
fn merge_command_scopes(
    project_dir: &Path,
    user_dir: &Path,
    trust_state: WorkspaceTrustState,
) -> CommandDiscovery {
    let user_commands = discover_commands_in(&[user_dir.to_path_buf()]);
    if !trust_state.is_trusted() {
        return CommandDiscovery {
            commands: user_commands,
            shadowed: Vec::new(),
        };
    }
    let project_commands = discover_commands_in(&[project_dir.to_path_buf()]);
    let user_names: HashSet<&str> = user_commands.iter().map(|c| c.name.as_str()).collect();
    let project_names: HashSet<String> = project_commands.iter().map(|c| c.name.clone()).collect();
    let shadowed = project_commands
        .iter()
        .filter(|c| user_names.contains(c.name.as_str()))
        .map(|c| ShadowedCommand {
            name: c.name.clone(),
            winner_source: c.source.clone(),
        })
        .collect();
    let mut commands = project_commands;
    commands.extend(
        user_commands
            .into_iter()
            .filter(|c| !project_names.contains(&c.name)),
    );
    CommandDiscovery { commands, shadowed }
}

/// Scan only the project-local commands directory. Used for contribution
/// presence checks and live, content-attested dispatch.
pub fn discover_project_commands(project_root: &Path) -> Vec<CustomCommand> {
    discover_commands_in(std::slice::from_ref(&project_commands_dir(project_root)))
}

/// Whether the project tree declares any project-local slash commands.
pub fn project_commands_present(project_root: &Path) -> bool {
    !discover_project_commands(project_root).is_empty()
}

/// Scan `dirs` in priority order (first dir wins a name clash; the loser is
/// silently dropped — shadow reporting, where relevant, is the caller's job).
fn discover_commands_in(dirs: &[PathBuf]) -> Vec<CustomCommand> {
    let mut commands = Vec::new();
    let mut seen_names = HashSet::new();

    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut paths = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("md"))
            .collect::<Vec<_>>();
        paths.sort();

        for path in paths {
            let Some(command) = parse_command_file(&path) else {
                continue;
            };
            if seen_names.insert(command.name.clone()) {
                commands.push(command);
            }
        }
    }

    commands
}

fn parse_command_file(path: &Path) -> Option<CustomCommand> {
    let raw = std::fs::read_to_string(path).ok()?;
    let (frontmatter, body) = split_frontmatter(&raw)?;
    let meta: Frontmatter = serde_yaml::from_str(frontmatter).unwrap_or_default();
    let name = meta.name.unwrap_or_else(|| {
        path.file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
    });
    let name = name.trim().trim_start_matches('/').to_ascii_lowercase();
    if !valid_command_name(&name) || body.trim().is_empty() {
        return None;
    }

    let summary = meta.summary.clone().or_else(|| meta.description.clone());
    let mut intent_keywords = meta.intent_keywords.unwrap_or_default();
    if let Some(aliases) = meta.aliases {
        intent_keywords.extend(aliases);
    }

    Some(CustomCommand {
        name,
        summary,
        description: meta.description,
        usage: meta.usage,
        intent_keywords,
        source: path.to_path_buf(),
        template: body.trim().to_string(),
    })
}

pub fn expand_command(command: &CustomCommand, arguments: &str) -> String {
    let positional = split_arguments(arguments);
    let mut expanded = command.template.replace("$ARGUMENTS", arguments.trim());
    for index in (1..=9).rev() {
        expanded = expanded.replace(
            &format!("${index}"),
            positional.get(index - 1).map(String::as_str).unwrap_or(""),
        );
    }
    expanded
}

fn valid_command_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, '-' | '_'))
}

fn split_frontmatter(text: &str) -> Option<(&str, &str)> {
    let trimmed = text.trim_start();
    if !trimmed.starts_with("---") {
        return Some(("", trimmed));
    }
    let after_open = &trimmed[3..];
    let close_idx = after_open.find("---")?;
    Some((after_open[..close_idx].trim(), &after_open[close_idx + 3..]))
}

fn split_arguments(input: &str) -> Vec<String> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    for value in input.chars() {
        if escaped {
            current.push(value);
            escaped = false;
            continue;
        }
        if value == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if matches!(value, '\'' | '"') {
            if quote == Some(value) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(value);
            } else {
                current.push(value);
            }
            continue;
        }
        if value.is_whitespace() && quote.is_none() {
            if !current.is_empty() {
                arguments.push(std::mem::take(&mut current));
            }
        } else {
            current.push(value);
        }
    }
    if escaped {
        current.push('\\');
    }
    if !current.is_empty() {
        arguments.push(current);
    }
    arguments
}

fn project_commands_dir(project_root: &Path) -> PathBuf {
    project_root.join(".muta/commands")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_raw_and_positional_arguments() {
        let command = CustomCommand {
            name: "review".to_string(),
            summary: None,
            description: None,
            usage: None,
            intent_keywords: Vec::new(),
            source: PathBuf::from("review.md"),
            template: "Review $1 against $2. Full: $ARGUMENTS".to_string(),
        };

        assert_eq!(
            expand_command(&command, "\"working tree\" main"),
            "Review working tree against main. Full: \"working tree\" main"
        );
    }

    #[test]
    fn parses_frontmatter_and_rejects_invalid_names() {
        let root = std::env::temp_dir().join(format!("muta-command-{}", uuid::Uuid::new_v4()));
        let project = root.join("project");
        let user = root.join("user");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(
            project.join("review.md"),
            "---\ndescription: Review changes\n---\nInspect $ARGUMENTS",
        )
        .unwrap();
        std::fs::write(user.join("review.md"), "lower priority").unwrap();
        std::fs::write(project.join("bad name.md"), "ignored").unwrap();

        let commands = discover_commands_in(&[project, user]);
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].name, "review");
        assert_eq!(commands[0].description.as_deref(), Some("Review changes"));
        assert_eq!(commands[0].template, "Inspect $ARGUMENTS");

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn extension_quarantine_skips_project_command_dir() {
        // Extension quarantine must omit the project dir (so a cloned or
        // vendored repo cannot inject `/<name>` prompt templates), while still
        // loading user-global ones.
        let root = std::env::temp_dir().join(format!("muta-trust-cmd-{}", uuid::Uuid::new_v4()));
        let project = root.join("project");
        let user = root.join("user");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(project.join("danger.md"), "pwn $ARGUMENTS").unwrap();
        std::fs::write(user.join("safe.md"), "safe $ARGUMENTS").unwrap();

        // Quarantined: project command must not appear (user commands do).
        let quarantined =
            merge_command_scopes(&project, &user, WorkspaceTrustState::Quarantined);
        assert_eq!(quarantined.commands.len(), 1);
        assert_eq!(
            quarantined.commands[0].name, "safe",
            "project command hidden while extensions are quarantined"
        );
        assert!(
            quarantined.shadowed.is_empty(),
            "no shadow report when the project dir was never scanned"
        );

        // Trusted: both project and user commands appear.
        let trusted = merge_command_scopes(&project, &user, WorkspaceTrustState::Trusted);
        assert_eq!(trusted.commands.len(), 2);
        assert!(
            trusted.commands.iter().any(|c| c.name == "danger"),
            "project command visible when trusted"
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_command_shadowing_user_command_is_reported_once() {
        // A project command that reuses a user command's name wins by
        // priority — and must produce exactly one shadow record so the
        // runtime can warn about the silent override.
        let root = std::env::temp_dir().join(format!("muta-shadow-cmd-{}", uuid::Uuid::new_v4()));
        let project = root.join("project");
        let user = root.join("user");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(project.join("review.md"), "project version").unwrap();
        std::fs::write(user.join("review.md"), "user version").unwrap();
        std::fs::write(user.join("unique.md"), "only mine").unwrap();

        let discovery = merge_command_scopes(&project, &user, WorkspaceTrustState::Trusted);
        assert_eq!(discovery.commands.len(), 2, "clash must not duplicate");
        let review = discovery
            .commands
            .iter()
            .find(|c| c.name == "review")
            .unwrap();
        assert_eq!(review.template, "project version", "project scope wins");
        assert_eq!(
            discovery.shadowed.len(),
            1,
            "exactly one shadow record per shadowed name"
        );
        let shadow = &discovery.shadowed[0];
        assert_eq!(shadow.name, "review");
        assert_eq!(shadow.winner_source, project.join("review.md"));

        std::fs::remove_dir_all(root).unwrap();
    }
}
