use std::sync::Arc;

use async_trait::async_trait;
use muta_contracts::{ExecutionEnvironment, Tool, ToolOutput};
use serde_json::json;

use crate::tools::helpers::{
    IGNORED_DIRS, WorkspaceBase, env_from_root, execution_environment, should_skip_path,
    workspace_base,
};

const DEFAULT_FIND_LIMIT: usize = 200;
const MAX_FIND_LIMIT: usize = 1000;

/// Fast, native file and directory exploration using glob patterns and directory recursion.
pub struct FindTool {
    pub(crate) root: WorkspaceBase,
    pub(crate) env: Option<Arc<dyn ExecutionEnvironment>>,
}

impl FindTool {
    pub fn new(root: WorkspaceBase) -> Self {
        Self { root, env: None }
    }

    pub fn with_env(env: Arc<dyn ExecutionEnvironment>) -> Self {
        let root = Some(env.workspace_root().to_path_buf());
        Self {
            root,
            env: Some(env),
        }
    }
}

#[async_trait]
impl Tool for FindTool {
    fn name(&self) -> &str {
        "find"
    }

    fn description(&self) -> &str {
        "Search for files by glob pattern or directory path. Returns matching relative paths. Respects standard ignore rules."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern to match (e.g. '*.rs', 'src/**/*.ts', 'Cargo.toml'). Default matches all."
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search from (default '.')"
                },
                "max_depth": {
                    "type": "integer",
                    "description": "Maximum directory depth to search (default 10, use 1 for top-level only)"
                },
                "type": {
                    "type": "string",
                    "enum": ["file", "dir", "any"],
                    "description": "Filter by entry type: 'file', 'dir', or 'any' (default 'any')"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of results to return (default 200, max 1000)"
                }
            }
        })
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let args: serde_json::Value =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {}", e))?;

        let raw_pattern = args["pattern"].as_str();
        let base_dir = args["path"].as_str().unwrap_or(".");
        let max_depth = args["max_depth"].as_u64().unwrap_or(10) as usize;
        let kind_filter = args["type"].as_str().unwrap_or("any");
        let limit = args["limit"]
            .as_u64()
            .unwrap_or(DEFAULT_FIND_LIMIT as u64)
            .min(MAX_FIND_LIMIT as u64) as usize;

        let env = self
            .env
            .clone()
            .unwrap_or_else(|| env_from_root(&self.root));
        let display_base = env.workspace_root().to_path_buf();
        let search_root = display_base.join(base_dir);

        if !search_root.exists() {
            return Err(format!("Directory not found: {}", base_dir));
        }

        let compiled_glob = raw_pattern.and_then(|p| glob::Pattern::new(p).ok());

        let mut results = Vec::new();
        let mut truncated = false;

        let walker = walkdir::WalkDir::new(&search_root)
            .max_depth(max_depth)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                if e.depth() == 0 {
                    return true;
                }
                if let Some(name) = e.file_name().to_str() {
                    if IGNORED_DIRS.contains(&name) || should_skip_path(e.path()) {
                        return false;
                    }
                }
                true
            });

        for entry in walker.filter_map(Result::ok) {
            if entry.depth() == 0 {
                continue;
            }

            let file_type = entry.file_type();
            let is_dir = file_type.is_dir();
            let is_file = file_type.is_file();

            match kind_filter {
                "file" if !is_file => continue,
                "dir" if !is_dir => continue,
                _ => {}
            }

            let path = entry.path();
            let rel_path = path.strip_prefix(&search_root).unwrap_or(path);
            let rel_str = rel_path.to_string_lossy();

            let matched = if let Some(ref glob_pat) = compiled_glob {
                let file_name = entry.file_name().to_string_lossy();
                glob_pat.matches(&rel_str) || glob_pat.matches(&file_name)
            } else if let Some(pat) = raw_pattern {
                let file_name = entry.file_name().to_string_lossy();
                rel_str.contains(pat) || file_name.contains(pat)
            } else {
                true
            };

            if matched {
                let display = path.strip_prefix(&display_base).unwrap_or(path);
                let suffix = if is_dir { "/" } else { "" };
                results.push(format!("{}{}", display.to_string_lossy(), suffix));

                if results.len() >= limit {
                    truncated = true;
                    break;
                }
            }
        }

        if results.is_empty() {
            return Ok("No files found matching criteria.".to_string());
        }

        results.sort();

        let mut output = results.join("\n");
        if truncated {
            output.push_str(&format!(
                "\n\n[Results truncated at limit of {} items — narrow your pattern or path]",
                limit
            ));
        }

        Ok(output)
    }

    async fn call_structured(&self, arguments: &str) -> Result<ToolOutput, String> {
        let text = self.call(arguments).await?;
        Ok(ToolOutput::Listing {
            entries: text.lines().map(str::to_string).collect(),
        })
    }
}

muta_contracts::register_tool!(FindFactory => |ctx| FindTool {
    root: workspace_base(ctx),
    env: Some(execution_environment(ctx)),
});

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn find_tool_lists_and_filters_files() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub fn lib() {}").unwrap();
        std::fs::write(root.join("README.md"), "# Hello").unwrap();

        let tool = FindTool::new(Some(root.to_path_buf()));

        let res = tool.call(r#"{"pattern":"*.rs"}"#).await.unwrap();
        assert!(res.contains("src/main.rs"));
        assert!(res.contains("src/lib.rs"));
        assert!(!res.contains("README.md"));

        let res_dirs = tool.call(r#"{"type":"dir"}"#).await.unwrap();
        assert!(res_dirs.contains("src/"));
        assert!(!res_dirs.contains("src/main.rs"));
    }
}
