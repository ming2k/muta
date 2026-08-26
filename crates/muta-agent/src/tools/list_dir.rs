use std::sync::Arc;

use async_trait::async_trait;
use muta_contracts::{ExecutionEnvironment, Tool, ToolAccesses};
use serde::Deserialize;
use serde_json::json;

use crate::tools::file_search::{
    MAX_SEARCH_LIMIT, resolve_search_root, search_limit, search_path_argument,
};
use crate::tools::helpers::{WorkspaceBase, env_from_root, execution_environment};

/// Shallow directory browsing with no search semantics.
pub struct ListDirTool {
    env: Arc<dyn ExecutionEnvironment>,
}

impl ListDirTool {
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
struct ListDirArgs {
    #[serde(default = "default_path")]
    path: String,
    limit: Option<u64>,
}

fn default_path() -> String {
    ".".to_string()
}

#[async_trait]
impl Tool for ListDirTool {
    fn name(&self) -> &str {
        "list_dir"
    }

    fn description(&self) -> &str {
        "List the immediate children of a directory (non-recursive)."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory to list; relative paths use the primary workspace (default '.')"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_SEARCH_LIMIT,
                    "description": "Maximum entries (default 200)"
                }
            },
            "additionalProperties": false
        })
    }

    fn accesses(&self, arguments: &str) -> ToolAccesses {
        ToolAccesses::read_tree(search_path_argument(arguments))
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let args: ListDirArgs = serde_json::from_str(arguments)
            .map_err(|error| format!("Invalid arguments: {error}"))?;
        let limit = search_limit(args.limit)?;
        let workspace = self.env.workspace_root();
        let directory = resolve_search_root(workspace, self.env.additional_roots(), &args.path)?;
        let metadata = self
            .env
            .fs()
            .metadata(&directory)
            .await
            .map_err(|error| format!("Cannot list '{}': {error}", args.path))?;
        if !metadata.is_dir {
            return Err(format!("List path is not a directory: {}", args.path));
        }

        let mut entries = self
            .env
            .fs()
            .list_dir(&directory)
            .await
            .map_err(|error| format!("Failed to read directory '{}': {error}", args.path))?
            .into_iter()
            .map(|entry| {
                let suffix = if entry.is_dir { "/" } else { "" };
                format!("{}{suffix}", entry.name)
            })
            .collect::<Vec<_>>();
        entries.sort();

        let truncated = entries.len() > limit;
        entries.truncate(limit);
        if entries.is_empty() {
            return Ok("Directory is empty.".to_string());
        }
        let mut output = entries.join("\n");
        if truncated {
            output.push_str(&format!(
                "\n\n[Results truncated at {limit} entries — use find_files to narrow the search.]"
            ));
        }
        Ok(output)
    }

    async fn call_structured(&self, arguments: &str) -> Result<muta_contracts::ToolOutput, String> {
        let output = self.call(arguments).await?;
        Ok(muta_contracts::ToolOutput::Listing {
            entries: output.lines().map(str::to_string).collect(),
        })
    }
}

muta_contracts::register_tool!(ListDirFactory => |ctx| ListDirTool {
    env: execution_environment(ctx),
});

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lists_only_immediate_children_in_stable_order() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("z-dir/nested")).unwrap();
        std::fs::write(tmp.path().join("a-file"), "a").unwrap();
        let tool = ListDirTool::new(Some(tmp.path().to_path_buf()));

        let output = tool.call("{}").await.unwrap();
        assert_eq!(output, "a-file\nz-dir/");
        assert!(!output.contains("nested"));
    }

    #[tokio::test]
    async fn rejects_retired_search_parameters() {
        let tmp = tempfile::tempdir().unwrap();
        let tool = ListDirTool::new(Some(tmp.path().to_path_buf()));
        let error = tool.call(r#"{"recursive":true}"#).await.unwrap_err();
        assert!(error.contains("unknown field"), "{error}");
    }
}
