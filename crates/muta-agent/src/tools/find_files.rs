use std::sync::Arc;

use async_trait::async_trait;
use muta_contracts::{ExecutionEnvironment, Tool, ToolAccesses, ToolOutput};
use serde::Deserialize;
use serde_json::json;

use crate::tools::file_search::{
    build_file_walker, build_include_matcher, include_allows, resolve_search_root, search_limit,
    search_path_argument,
};
use crate::tools::helpers::{WorkspaceBase, env_from_root, execution_environment};

/// Recursive file discovery over ripgrep-compatible ignore and glob rules.
pub struct FindFilesTool {
    env: Arc<dyn ExecutionEnvironment>,
}

impl FindFilesTool {
    pub fn new(root: WorkspaceBase) -> Self {
        Self {
            env: env_from_root(&root),
        }
    }

    pub fn with_env(env: Arc<dyn ExecutionEnvironment>) -> Self {
        Self { env }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FindFilesArgs {
    patterns: Vec<String>,
    #[serde(default = "default_path")]
    path: String,
    #[serde(default)]
    exclude: Vec<String>,
    max_depth: Option<usize>,
    limit: Option<u64>,
}

fn default_path() -> String {
    ".".to_string()
}

#[async_trait]
impl Tool for FindFilesTool {
    fn name(&self) -> &str {
        "find_files"
    }

    fn description(&self) -> &str {
        "Find files whose paths match one or more globs. Patterns are ORed; project ignore rules apply."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "patterns": {
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 1,
                    "description": "Path globs relative to path; pass alternatives as separate array items (OR)"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search; relative paths use the primary workspace (default '.')"
                },
                "exclude": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Path globs to exclude from the result"
                },
                "max_depth": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Optional maximum depth below path"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 1000,
                    "description": "Maximum results (default 200)"
                }
            },
            "required": ["patterns"],
            "additionalProperties": false
        })
    }

    fn accesses(&self, arguments: &str) -> ToolAccesses {
        ToolAccesses::search_tree(search_path_argument(arguments))
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let args: FindFilesArgs = serde_json::from_str(arguments)
            .map_err(|error| format!("Invalid arguments: {error}"))?;
        if args.patterns.is_empty() {
            return Err("'patterns' must contain at least one glob".to_string());
        }
        if args.max_depth == Some(0) {
            return Err("'max_depth' must be at least 1".to_string());
        }
        let limit = search_limit(args.limit)?;
        let workspace = self.env.workspace_root().to_path_buf();
        let search_root = resolve_search_root(&workspace, self.env.additional_roots(), &args.path)?;
        let metadata = self
            .env
            .fs()
            .metadata(&search_root)
            .await
            .map_err(|error| format!("Cannot search '{}': {error}", args.path))?;
        if !metadata.is_dir {
            return Err(format!("Search path is not a directory: {}", args.path));
        }

        // Include globs filter *after* the walker's project-ignore pruning, so
        // a whitelisting pattern (`*.log`) cannot resurrect a gitignored file.
        let include = build_include_matcher(&search_root, &args.patterns, &args.exclude)?;
        let walker = build_file_walker(&search_root, &args.exclude, args.max_depth)?.build();
        let output = tokio::task::spawn_blocking(move || {
            let mut results = Vec::new();
            let mut first_error = None;
            for entry in walker {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        first_error.get_or_insert_with(|| error.to_string());
                        continue;
                    }
                };
                if entry.depth() == 0 || !entry.file_type().is_some_and(|kind| kind.is_file()) {
                    continue;
                }
                if !include_allows(&include, true, &search_root, entry.path()) {
                    continue;
                }
                let display = entry
                    .path()
                    .strip_prefix(&workspace)
                    .unwrap_or_else(|_| entry.path())
                    .to_string_lossy()
                    .to_string();
                results.push(display);
                if results.len() > limit {
                    break;
                }
            }
            (results, first_error)
        })
        .await
        .map_err(|error| format!("File search task failed: {error}"))?;

        let (mut results, first_error) = output;
        let truncated = results.len() > limit;
        results.truncate(limit);
        if results.is_empty() {
            return match first_error {
                Some(error) => Err(format!("File search failed: {error}")),
                None => Ok("No files matched.".to_string()),
            };
        }

        let mut text = results.join("\n");
        if truncated {
            text.push_str(&format!(
                "\n\n[Results truncated at {limit} files — narrow patterns or path.]"
            ));
        } else if let Some(error) = first_error {
            text.push_str(&format!("\n\n[Some paths could not be read: {error}]"));
        }
        Ok(text)
    }

    async fn call_structured(&self, arguments: &str) -> Result<ToolOutput, String> {
        let text = self.call(arguments).await?;
        Ok(ToolOutput::Listing {
            entries: text.lines().map(str::to_string).collect(),
        })
    }
}

muta_contracts::register_tool!(FindFilesFactory => |ctx| FindFilesTool {
    env: execution_environment(ctx),
});

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn finds_multiple_patterns_and_respects_ignore_rules() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join(".agents")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join("README.md"), "# Hello").unwrap();
        std::fs::write(root.join("ignored.log"), "ignored").unwrap();
        std::fs::write(root.join(".gitignore"), "ignored.log\n").unwrap();
        std::fs::write(root.join(".agents/settings.toml"), "enabled = true").unwrap();
        std::fs::write(root.join("target/generated.rs"), "generated").unwrap();

        let tool = FindFilesTool::new(Some(root.to_path_buf()));
        let result = tool
            .call(r#"{"patterns":["*.rs","*.md",".agents/**","*.log"],"limit":20}"#)
            .await
            .unwrap();

        assert!(result.contains("src/main.rs"));
        assert!(result.contains("README.md"));
        assert!(result.contains(".agents/settings.toml"));
        assert!(!result.contains("ignored.log"));
        assert!(!result.contains("target/generated.rs"));
    }

    #[tokio::test]
    async fn rejects_invalid_globs_instead_of_falling_back_to_substrings() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = FindFilesTool::new(Some(tmp.path().to_path_buf()));
        let error = tool.call(r#"{"patterns":["[broken"]}"#).await.unwrap_err();
        assert!(error.contains("Invalid include glob"), "{error}");
    }
}
