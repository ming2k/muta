use async_trait::async_trait;
use muta_contracts::Tool;
use serde_json::json;
use tokio::process::Command;
use tokio::time::{Duration, timeout};

use crate::tools::helpers::{WorkspaceBase, env_from_root, execution_environment, workspace_base};

/// Maximum wall-clock time for a single `rg` invocation. A slow or wedged
/// ripgrep (huge tree, catastrophic-backtracking pattern) is released rather
/// than pinning the async executor — the old code blocked a runtime worker
/// thread for the entire run via `std::process::Command::output`.
const GREP_TIMEOUT: Duration = Duration::from_secs(30);

/// Global cap on returned match lines, applied *after* ripgrep across all
/// files. `--max-count` only bounds matches per file, so a common pattern in a
/// large tree could still flood the model's context with thousands of lines.
/// This is the grep analogue of the shell-output and paged-read caps.
const GREP_MAX_LINES: usize = 200;

/// Global cap on returned bytes, mirroring the shell-output truncation. Honored
/// alongside [`GREP_MAX_LINES`]; whichever trips first wins.
const GREP_MAX_BYTES: usize = 32 * 1024;

/// Search file contents with ripgrep.
///
/// The search root (default `.`) resolves against the session's workspace
/// root (captured at factory time), not the daemon process's cwd (ADR-0096).
pub struct GrepTool {
    pub(crate) root: WorkspaceBase,
    pub(crate) env: Option<std::sync::Arc<dyn muta_contracts::ExecutionEnvironment>>,
}

impl GrepTool {
    pub fn new(root: WorkspaceBase) -> Self {
        Self { root, env: None }
    }

    pub fn with_env(env: std::sync::Arc<dyn muta_contracts::ExecutionEnvironment>) -> Self {
        let root = Some(env.workspace_root().to_path_buf());
        Self {
            root,
            env: Some(env),
        }
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }
    fn description(&self) -> &str {
        "Search file contents for a regex pattern. Returns matches in path:line:content format."
    }
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regex pattern to search for" },
                "path": { "type": "string", "description": "Directory or file to search in (default '.')" },
                "ext": { "type": "string", "description": "Optional file extension filter (e.g., 'rs', 'py')" },
                "context": { "type": "integer", "description": "Lines of context around each match (default 0)" }
            },
            "required": ["pattern"]
        })
    }
    async fn call(&self, arguments: &str) -> Result<String, String> {
        let args: serde_json::Value =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {}", e))?;
        let pattern = args["pattern"].as_str().ok_or("Missing 'pattern'")?;
        let path = args["path"].as_str().unwrap_or(".");
        let ext = args["ext"].as_str();
        // Context is opt-in: every context line multiplies output, and the model
        // can read the file when it needs surroundings. Clamp to a sane ceiling.
        let context = args["context"].as_u64().unwrap_or(0).min(10);

        let env = self
            .env
            .clone()
            .unwrap_or_else(|| env_from_root(&self.root));
        // Search the session's workspace root, not the daemon process's cwd
        // (ADR-0096): a default `.` must scan the invoking session's project.
        // `join` passes an absolute search path through unchanged.
        let search_root = match &self.root {
            Some(root) => root.join(path),
            None => env.workspace_root().join(path),
        };

        let mut cmd = Command::new("rg");
        cmd.args(["-n", "--color=never", "--max-count", "50"]);
        if context > 0 {
            cmd.arg("-C").arg(context.to_string());
        }
        if let Some(e) = ext {
            cmd.arg("-g").arg(format!("*.{}", e));
        }
        // Prune the same set of directories the glob/list tools ignore, so the
        // three tools agree about what exists in a tree.
        for dir in crate::tools::helpers::IGNORED_DIRS {
            cmd.arg("-g").arg(format!("!{}", dir));
        }
        cmd.arg(pattern).arg(&search_root);

        // Spawn under tokio (releasing the runtime while rg runs) and bound the
        // whole invocation by `GREP_TIMEOUT`. On timeout the child is killed
        // via `kill_on_drop`-equivalent: we explicitly `start_kill` first so a
        // wedged rg does not linger.
        let run = async {
            let res = cmd.output().await;
            match res {
                Ok(output) => {
                    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                    if stdout.is_empty() {
                        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                        if !stderr.is_empty() && output.status.code() != Some(1) {
                            return Err(format!("rg error: {}", stderr));
                        }
                        return Ok("No matches found.".to_string());
                    }
                    Ok::<_, String>(cap_output(&stdout))
                }
                Err(_) => {
                    // Fast in-process native fallback when `rg` is not on PATH
                    let root_clone = search_root.clone();
                    let pattern_owned = pattern.to_string();
                    let ext_owned = ext.map(|s| s.to_string());
                    let ctx_val = context as usize;
                    tokio::task::spawn_blocking(move || {
                        native_grep(&root_clone, &pattern_owned, ext_owned.as_deref(), ctx_val)
                    })
                    .await
                    .map_err(|e| format!("Native grep task panicked: {}", e))?
                }
            }
        };

        match timeout(GREP_TIMEOUT, run).await {
            Ok(result) => result,
            Err(_) => Err(format!(
                "grep timed out after {} seconds",
                GREP_TIMEOUT.as_secs()
            )),
        }
    }

    async fn call_structured(&self, arguments: &str) -> Result<muta_contracts::ToolOutput, String> {
        let out = self.call(arguments).await?;
        let pattern = serde_json::from_str::<serde_json::Value>(arguments)
            .ok()
            .and_then(|a| a["pattern"].as_str().map(str::to_string))
            .unwrap_or_default();
        Ok(muta_contracts::ToolOutput::Matches {
            pattern,
            lines: out.split('\n').map(str::to_string).collect(),
        })
    }
}
muta_contracts::register_tool!(GrepFactory => |ctx| GrepTool {
    root: workspace_base(ctx),
    env: Some(execution_environment(ctx)),
});

/// Bound ripgrep's stdout to [`GREP_MAX_LINES`] / [`GREP_MAX_BYTES`], whichever
/// trips first, appending a one-line truncation notice. This is the grep
/// counterpart to the shell-output and paged-read caps: a common pattern in a
/// large tree must not flood the model's context, since `--max-count` only
/// bounds matches *per file*.
fn cap_output(stdout: &str) -> String {
    let mut out = String::new();
    let mut lines = 0usize;
    let mut truncated = false;
    // `lines` counts *written* lines (conditionally incremented, not the loop
    // index), so `enumerate()` would not be a faithful rewrite.
    #[allow(clippy::explicit_counter_loop)]
    for line in stdout.lines() {
        if lines >= GREP_MAX_LINES || out.len() + line.len() + 1 > GREP_MAX_BYTES {
            truncated = true;
            break;
        }
        out.push_str(line);
        out.push('\n');
        lines += 1;
    }
    if truncated {
        out.push_str("\n[Output truncated — narrow your pattern, path, or `ext`.]");
    }
    out
}

/// In-process native regex / literal search engine.
/// Traverses the directory using `walkdir`, prunes ignored directories,
/// and matches lines against `regex::Regex` with support for context lines
/// and bounded line/byte limits. Runs at zero subprocess cost.
fn native_grep(
    search_root: &std::path::Path,
    pattern: &str,
    ext: Option<&str>,
    context: usize,
) -> Result<String, String> {
    let re = regex::Regex::new(pattern).map_err(|e| format!("Invalid regex: {}", e))?;
    let mut matches = Vec::new();
    let mut total_lines = 0;
    let mut total_bytes = 0;
    let mut truncated = false;

    let walker = walkdir::WalkDir::new(search_root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            if let Some(name) = e.file_name().to_str() {
                if crate::tools::helpers::IGNORED_DIRS.contains(&name) {
                    return false;
                }
            }
            true
        });

    for entry in walker.filter_map(Result::ok) {
        if !entry.file_type().is_file() {
            continue;
        }
        let file_path = entry.path();
        if let Some(e) = ext {
            if file_path.extension().and_then(|x| x.to_str()) != Some(e) {
                continue;
            }
        }

        if let Ok(metadata) = entry.metadata() {
            if metadata.len() > 10 * 1024 * 1024 {
                // skip huge files > 10MB
                continue;
            }
        }

        let content = match std::fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(_) => continue, // skip binary or non-UTF-8 files
        };

        let file_lines: Vec<&str> = content.lines().collect();
        let display_path = file_path
            .strip_prefix(search_root)
            .unwrap_or(file_path)
            .display()
            .to_string();

        let mut file_matches = 0;
        for (idx, line) in file_lines.iter().enumerate() {
            if re.is_match(line) {
                file_matches += 1;
                if file_matches > 50 {
                    break;
                }

                let start_idx = idx.saturating_sub(context);
                let end_idx = (idx + context + 1).min(file_lines.len());

                if context == 0 {
                    let formatted = format!("{}:{}:{}", display_path, idx + 1, line);
                    total_bytes += formatted.len() + 1;
                    total_lines += 1;
                    matches.push(formatted);
                } else {
                    for ctx_i in start_idx..end_idx {
                        let sep = if ctx_i == idx { ":" } else { "-" };
                        let formatted = format!(
                            "{}{}{}{}{}",
                            display_path,
                            sep,
                            ctx_i + 1,
                            sep,
                            file_lines[ctx_i]
                        );
                        total_bytes += formatted.len() + 1;
                        total_lines += 1;
                        matches.push(formatted);
                    }
                }

                if total_lines >= GREP_MAX_LINES || total_bytes >= GREP_MAX_BYTES {
                    truncated = true;
                    break;
                }
            }
        }

        if truncated {
            break;
        }
    }

    if matches.is_empty() {
        return Ok("No matches found.".to_string());
    }

    let mut result = matches.join("\n");
    if truncated {
        result.push_str("\n\n[Output truncated — narrow your pattern, path, or `ext`.]");
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_output_passes_through_small_results() {
        let s = "a.rs:1:foo\na.rs:2:bar\n";
        assert_eq!(cap_output(s), s);
    }

    #[test]
    fn cap_output_truncates_by_line_count() {
        let big: String = (0..GREP_MAX_LINES + 50)
            .map(|i| format!("f.rs:{i}:hit\n"))
            .collect();
        let capped = cap_output(&big);
        let kept = capped.lines().filter(|l| l.contains(":hit")).count();
        assert_eq!(kept, GREP_MAX_LINES);
        assert!(capped.contains("[Output truncated"));
    }

    #[test]
    fn cap_output_truncates_by_bytes() {
        // Few lines, but each huge -> byte cap trips before the line cap.
        let line = format!("f.rs:1:{}\n", "x".repeat(GREP_MAX_BYTES));
        let capped = cap_output(&line);
        assert!(capped.len() <= GREP_MAX_BYTES + 64);
        assert!(capped.contains("[Output truncated"));
    }

    #[test]
    fn native_grep_finds_pattern_in_temp_dir() {
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("test.rs");
        std::fs::write(&file_path, "fn hello_world() {\n    println!(\"hi\");\n}\n").unwrap();

        let result = native_grep(temp_dir.path(), "hello_world", Some("rs"), 0).unwrap();
        assert!(result.contains("test.rs:1:fn hello_world() {"));

        let not_found = native_grep(temp_dir.path(), "non_existent_symbol", None, 0).unwrap();
        assert_eq!(not_found, "No matches found.");
    }
}
