//! Backend-owned composer completion.
//!
//! Frontends send the current input and cursor, then render the returned edit
//! candidates. Command matching, intent steering, content-admitted workspace
//! commands, project-file discovery, and explicit-path expansion all live here
//! so terminal and browser apps cannot drift into separate completion products.

use muta_contracts::{
    AgentResponse, CommandCatalog, CommandSpec, InputCompletion, InputCompletionKind,
};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use tokio::sync::OnceCell;

const MAX_PATH_COMPLETIONS: usize = 200;

pub struct InputCompletionEngine {
    catalog: CommandCatalog,
    project_root: PathBuf,
    project_entries: OnceCell<Vec<String>>,
}

impl InputCompletionEngine {
    pub fn new(catalog: CommandCatalog, project_root: PathBuf) -> Self {
        Self {
            catalog,
            project_root,
            project_entries: OnceCell::new(),
        }
    }

    /// Produce one race-safe protocol response. `cursor` is a Unicode-scalar
    /// index, matching the wire contract in `muta-contracts`.
    pub async fn complete(&self, request_id: u64, input: String, cursor: usize) -> AgentResponse {
        let items = match char_to_byte(&input, cursor) {
            Some(cursor_byte) => self.complete_items(&input, cursor_byte).await,
            None => Vec::new(),
        };
        AgentResponse::InputCompletions {
            request_id,
            input,
            cursor,
            items,
        }
    }

    async fn complete_items(&self, input: &str, cursor_byte: usize) -> Vec<InputCompletion> {
        if input.starts_with('/') {
            return self.complete_slash(input, cursor_byte);
        }
        let Some((at_start, cursor_end)) = mention_range_at(input, cursor_byte) else {
            return Vec::new();
        };
        let query = &input[at_start + 1..cursor_end];
        if is_explicit_path_prefix(query) {
            self.complete_explicit_path(input, query, at_start, cursor_end)
        } else {
            self.complete_project_path(input, query, at_start, cursor_end)
                .await
        }
    }

    fn complete_slash(&self, input: &str, cursor_byte: usize) -> Vec<InputCompletion> {
        // Slash commands occupy the whole composer. Matching against the
        // prefix through the cursor keeps mid-input caret behavior stable.
        let current = input[..cursor_byte].to_lowercase();
        let Some(trigger) = current.strip_prefix('/') else {
            return Vec::new();
        };
        let replace_end = input.chars().count();

        if current.contains(char::is_whitespace) {
            let command_name = current.split_whitespace().next().unwrap_or_default();
            if let Some(spec) = self.catalog.find(command_name) {
                let canonical_input = self
                    .catalog
                    .alias(command_name)
                    .map(|alias| current.replacen(command_name, &alias.target, 1))
                    .unwrap_or_else(|| current.clone());
                return spec
                    .usage
                    .iter()
                    .filter(|usage| usage.contains(' ') && !usage.contains('<'))
                    .filter(|usage| usage.to_lowercase().starts_with(&canonical_input))
                    .map(|usage| slash_item(usage, &spec.summary, replace_end, spec, false))
                    .collect();
            }
        }

        let trigger_suggestion = self
            .catalog
            .suggestions
            .iter()
            .find(|suggestion| suggestion.trigger.eq_ignore_ascii_case(trigger))
            .map(|suggestion| {
                let command = self.catalog.find(&suggestion.target).cloned();
                InputCompletion {
                    label: suggestion.target.clone(),
                    description: suggestion.reason.clone(),
                    insert_text: suggestion.target.clone(),
                    replace_start: 0,
                    replace_end,
                    kind: InputCompletionKind::Intent,
                    command,
                }
            });
        let trigger_target = trigger_suggestion.as_ref().map(|item| item.label.clone());

        let exact = self
            .catalog
            .commands
            .iter()
            .filter(|spec| spec.name.to_lowercase().starts_with(&current))
            .map(|spec| slash_item(&spec.name, &spec.summary, replace_end, spec, false));

        let intent = (!trigger.is_empty())
            .then(|| {
                self.catalog.commands.iter().filter_map(|spec| {
                    if spec.name.to_lowercase().starts_with(&current)
                        || trigger_target.as_deref() == Some(spec.name.as_str())
                    {
                        return None;
                    }
                    let matched = spec.intent_keywords.iter().find(|keyword| {
                        keyword.eq_ignore_ascii_case(trigger)
                            || (trigger.len() >= 3 && keyword.to_lowercase().starts_with(trigger))
                    })?;
                    Some(slash_item(
                        &spec.name,
                        &format!("(via '{matched}') {}", spec.summary),
                        replace_end,
                        spec,
                        true,
                    ))
                })
            })
            .into_iter()
            .flatten();

        trigger_suggestion
            .into_iter()
            .chain(exact)
            .chain(intent)
            .collect()
    }

    async fn complete_project_path(
        &self,
        input: &str,
        query: &str,
        at_start: usize,
        cursor_end: usize,
    ) -> Vec<InputCompletion> {
        let root = self.project_root.clone();
        let entries = self
            .project_entries
            .get_or_init(|| async move {
                tokio::task::spawn_blocking(move || scan_project_files(&root))
                    .await
                    .unwrap_or_default()
            })
            .await;
        let mut items = entries
            .iter()
            .filter(|path| path_query_match(path, query))
            .take(MAX_PATH_COMPLETIONS)
            .map(|path| {
                path_item(
                    input,
                    path,
                    at_start,
                    cursor_end,
                    if path.ends_with('/') {
                        InputCompletionKind::PathDir
                    } else {
                        InputCompletionKind::PathFile
                    },
                )
            })
            .collect::<Vec<_>>();
        sort_path_completions(&mut items);
        items
    }

    fn complete_explicit_path(
        &self,
        input: &str,
        query: &str,
        at_start: usize,
        cursor_end: usize,
    ) -> Vec<InputCompletion> {
        let Some((dir, name_prefix)) = resolve_explicit_dir(query, &self.project_root) else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut items = entries
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_str()?.to_string();
                if name == ".git" || !name.to_lowercase().starts_with(&name_prefix.to_lowercase()) {
                    return None;
                }
                let is_dir = entry.file_type().ok()?.is_dir();
                let mut label = entry.path().to_str()?.to_string();
                if is_dir && !label.ends_with('/') {
                    label.push('/');
                }
                Some(path_item(
                    input,
                    &label,
                    at_start,
                    cursor_end,
                    InputCompletionKind::PathExplicit,
                ))
            })
            .take(MAX_PATH_COMPLETIONS)
            .collect::<Vec<_>>();
        sort_path_completions(&mut items);
        items
    }
}

/// Synchronous adapter used by frontend unit tests to exercise the daemon's
/// completion implementation without standing up a session driver. Product
/// clients never call this; they use `CompleteInput` over the control plane.
#[doc(hidden)]
pub fn complete_for_frontend_test(
    catalog: CommandCatalog,
    project_root: PathBuf,
    input: &str,
    cursor: usize,
) -> Vec<InputCompletion> {
    let engine = InputCompletionEngine::new(catalog, project_root);
    let Some(cursor_byte) = char_to_byte(input, cursor) else {
        return Vec::new();
    };
    if input.starts_with('/') {
        return engine.complete_slash(input, cursor_byte);
    }
    let Some((at_start, cursor_end)) = mention_range_at(input, cursor_byte) else {
        return Vec::new();
    };
    let query = &input[at_start + 1..cursor_end];
    if is_explicit_path_prefix(query) {
        engine.complete_explicit_path(input, query, at_start, cursor_end)
    } else {
        let mut items = scan_project_files(&engine.project_root)
            .iter()
            .filter(|path| path_query_match(path, query))
            .take(MAX_PATH_COMPLETIONS)
            .map(|path| {
                path_item(
                    input,
                    path,
                    at_start,
                    cursor_end,
                    if path.ends_with('/') {
                        InputCompletionKind::PathDir
                    } else {
                        InputCompletionKind::PathFile
                    },
                )
            })
            .collect::<Vec<_>>();
        sort_path_completions(&mut items);
        items
    }
}

fn slash_item(
    label: &str,
    description: &str,
    replace_end: usize,
    command: &CommandSpec,
    intent: bool,
) -> InputCompletion {
    InputCompletion {
        label: label.to_string(),
        description: description.to_string(),
        insert_text: label.to_string(),
        replace_start: 0,
        replace_end,
        kind: if intent {
            InputCompletionKind::Intent
        } else {
            InputCompletionKind::Slash
        },
        command: Some(command.clone()),
    }
}

fn path_item(
    input: &str,
    label: &str,
    at_start_byte: usize,
    replace_end_byte: usize,
    kind: InputCompletionKind,
) -> InputCompletion {
    let (replace_start_byte, mut insert_text) = match kind {
        // A project directory keeps the `@` trigger so accepting it can ask
        // the backend for the next path segment.
        InputCompletionKind::PathDir => (at_start_byte, format!("@{label}")),
        // Files and explicit paths are terminal mentions: consume the `@`
        // and separate the resolved path from following prose.
        InputCompletionKind::PathFile | InputCompletionKind::PathExplicit => {
            (at_start_byte, label.to_string())
        }
        InputCompletionKind::Slash | InputCompletionKind::Intent => {
            (at_start_byte, label.to_string())
        }
    };
    if matches!(
        kind,
        InputCompletionKind::PathFile | InputCompletionKind::PathExplicit
    ) {
        let needs_space = input
            .get(replace_end_byte..)
            .and_then(|suffix| suffix.chars().next())
            .map(|character| !character.is_whitespace())
            .unwrap_or(true);
        if needs_space {
            insert_text.push(' ');
        }
    }
    InputCompletion {
        label: label.to_string(),
        description: String::new(),
        insert_text,
        replace_start: input[..replace_start_byte].chars().count(),
        replace_end: input[..replace_end_byte].chars().count(),
        kind,
        command: None,
    }
}

fn char_to_byte(input: &str, char_index: usize) -> Option<usize> {
    if char_index == input.chars().count() {
        Some(input.len())
    } else {
        input.char_indices().nth(char_index).map(|(byte, _)| byte)
    }
}

fn mention_range_at(input: &str, cursor_byte: usize) -> Option<(usize, usize)> {
    if cursor_byte > input.len() || !input.is_char_boundary(cursor_byte) {
        return None;
    }
    let mut chars_before = input[..cursor_byte].char_indices().collect::<Vec<_>>();
    while let Some((idx, character)) = chars_before.pop() {
        if character.is_whitespace() {
            return None;
        }
        if character == '@' {
            let preceded_by_space = chars_before
                .last()
                .map(|(_, previous)| previous.is_whitespace())
                .unwrap_or(true);
            return preceded_by_space.then_some((idx, cursor_byte));
        }
    }
    None
}

fn path_query_match(path: &str, query: &str) -> bool {
    if query.is_empty() {
        !path.trim_end_matches('/').contains('/')
    } else if let Some(prefix) = query.strip_suffix('/').filter(|_| query.contains('/')) {
        path.to_lowercase().starts_with(&prefix.to_lowercase())
    } else {
        path.to_lowercase().contains(&query.to_lowercase())
    }
}

fn is_explicit_path_prefix(query: &str) -> bool {
    let normalized = query.replace('\\', "/");
    normalized.starts_with("../")
        || normalized == ".."
        || normalized.starts_with("./")
        || normalized == "."
        || normalized.starts_with("~/")
        || normalized == "~"
        || normalized.starts_with('/')
        || Path::new(query).is_absolute()
}

fn resolve_explicit_dir(query: &str, cwd: &Path) -> Option<(PathBuf, String)> {
    let normalized = query.replace('\\', "/");
    let home = || dirs::home_dir().or_else(|| std::env::var_os("HOME").map(PathBuf::from));
    let (expanded, names_directory) = if normalized == "~" {
        (home()?, true)
    } else if let Some(rest) = normalized.strip_prefix("~/") {
        (home()?.join(rest), normalized.ends_with('/'))
    } else {
        (
            cwd.join(Path::new(&normalized)),
            normalized.ends_with('/') || matches!(normalized.as_str(), "." | ".."),
        )
    };
    let (dir, name_prefix) = if names_directory {
        (expanded, String::new())
    } else {
        (
            expanded.parent()?.to_path_buf(),
            expanded.file_name()?.to_string_lossy().into_owned(),
        )
    };
    Some((dir.canonicalize().unwrap_or(dir), name_prefix))
}

fn scan_project_files(root: &Path) -> Vec<String> {
    try_ripgrep_scan(root).unwrap_or_else(|| manual_walk(root))
}

fn try_ripgrep_scan(root: &Path) -> Option<Vec<String>> {
    let output = std::process::Command::new("rg")
        .args([
            "--files",
            "--hidden",
            "--glob=!.git",
            "--color=never",
            "--no-messages",
        ])
        .current_dir(root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let files = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| line.replace('\\', "/"))
        .collect::<Vec<_>>();
    let mut dirs = BTreeSet::new();
    for path in &files {
        let mut current = String::new();
        let parts = path.split('/').collect::<Vec<_>>();
        for part in &parts[..parts.len().saturating_sub(1)] {
            if !current.is_empty() {
                current.push('/');
            }
            current.push_str(part);
            dirs.insert(format!("{current}/"));
        }
    }
    let mut entries = files;
    entries.extend(dirs);
    sort_paths(&mut entries);
    entries.dedup();
    Some(entries)
}

fn manual_walk(root: &Path) -> Vec<String> {
    let mut output = Vec::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((directory, prefix)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if name == ".git" {
                continue;
            }
            let path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}{name}")
            };
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                let directory_path = format!("{path}/");
                stack.push((entry.path(), directory_path.clone()));
                output.push(directory_path);
            } else {
                output.push(path);
            }
        }
    }
    sort_paths(&mut output);
    output
}

fn sort_paths(paths: &mut [String]) {
    paths.sort_by(|left, right| {
        right
            .ends_with('/')
            .cmp(&left.ends_with('/'))
            .then_with(|| left.to_lowercase().cmp(&right.to_lowercase()))
    });
}

fn sort_path_completions(items: &mut [InputCompletion]) {
    items.sort_by(|left, right| {
        right
            .label
            .ends_with('/')
            .cmp(&left.label.ends_with('/'))
            .then_with(|| left.label.to_lowercase().cmp(&right.label.to_lowercase()))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> CommandCatalog {
        crate::startup::command_catalog(&[("/project-check".into(), "Check project".into())])
    }

    #[tokio::test]
    async fn slash_matching_and_intent_are_backend_owned() {
        let engine = InputCompletionEngine::new(catalog(), PathBuf::from("."));
        let AgentResponse::InputCompletions { items, .. } =
            engine.complete(7, "/mod".into(), 4).await
        else {
            panic!("unexpected response")
        };
        assert!(items.iter().any(|item| item.label == "/models"));

        let AgentResponse::InputCompletions { items, .. } =
            engine.complete(8, "/theme".into(), 6).await
        else {
            panic!("unexpected response")
        };
        assert_eq!(
            items.first().map(|item| item.label.as_str()),
            Some("/settings")
        );
    }

    #[tokio::test]
    async fn project_paths_are_resolved_by_the_daemon() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("src")).unwrap();
        std::fs::write(temp.path().join("src/main.rs"), "fn main() {}").unwrap();
        let engine = InputCompletionEngine::new(catalog(), temp.path().to_path_buf());
        let AgentResponse::InputCompletions { items, .. } =
            engine.complete(1, "look @main".into(), 10).await
        else {
            panic!("unexpected response")
        };
        assert!(items.iter().any(|item| item.label == "src/main.rs"));
    }

    #[test]
    fn wire_offsets_are_unicode_scalar_indices() {
        let item = path_item(
            "中 @src",
            "src/main.rs",
            4,
            8,
            InputCompletionKind::PathFile,
        );
        assert_eq!(item.replace_start, 2);
        assert_eq!(item.replace_end, 6);
        assert_eq!(item.insert_text, "src/main.rs ");
    }
}
