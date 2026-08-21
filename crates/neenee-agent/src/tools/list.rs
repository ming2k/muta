use async_trait::async_trait;
use neenee_contracts::Tool;
use serde_json::json;

use crate::tools::helpers::{
    WorkspaceBase, env_from_root, execution_environment, should_skip_path, workspace_base,
};

/// List directory contents.
///
/// The listed directory (default `.`) resolves against the session's
/// workspace root (captured at factory time), not the daemon process's cwd
/// (ADR-0096).
pub struct ListDirTool {
    pub(crate) root: WorkspaceBase,
    pub(crate) env: Option<std::sync::Arc<dyn neenee_contracts::ExecutionEnvironment>>,
}

impl ListDirTool {
    pub fn new(root: WorkspaceBase) -> Self {
        Self { root, env: None }
    }

    pub fn with_env(env: std::sync::Arc<dyn neenee_contracts::ExecutionEnvironment>) -> Self {
        let root = Some(env.workspace_root().to_path_buf());
        Self {
            root,
            env: Some(env),
        }
    }
}

#[async_trait]
impl Tool for ListDirTool {
    fn name(&self) -> &str {
        "list_dir"
    }
    fn description(&self) -> &str {
        "List files and directories. Supports glob patterns."
    }
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory path to list (default '.')" },
                "pattern": { "type": "string", "description": "Optional glob pattern to filter results (e.g., '*.rs')" },
                "recursive": { "type": "boolean", "description": "Whether to list recursively (default false)" },
                "max_results": { "type": "integer", "description": "Max entries to return (default 100)" }
            },
            "required": []
        })
    }
    async fn call(&self, arguments: &str) -> Result<String, String> {
        let args: serde_json::Value =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {}", e))?;
        let path = args["path"].as_str().unwrap_or(".");
        let pattern = args["pattern"].as_str();
        let recursive = args["recursive"].as_bool().unwrap_or(false);
        let max_results = args["max_results"].as_u64().unwrap_or(100) as usize;

        let env = self.env.clone().unwrap_or_else(|| env_from_root(&self.root));
        // Resolve the listed directory against the session's workspace root
        // so a default `.` lists the session's project, never the daemon's
        // coincidental process cwd. `join` passes absolute paths through.
        let resolved = match &self.root {
            Some(root) => root.join(path),
            None => std::path::PathBuf::from(path),
        };
        let path = resolved.to_string_lossy().to_string();
        // Display strips the workspace root (not the process cwd) so results
        // stay relative to what the model asked about.
        let display_base = self
            .root
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

        let mut results = Vec::new();

        if let Some(glob_pat) = pattern {
            let full_pattern = if recursive {
                format!("{}/**/{}\0{}/{}", path, glob_pat, path, glob_pat)
            } else {
                format!("{}/{}\0{}/{}", path, glob_pat, path, glob_pat)
            };
            // Split and deduplicate
            let patterns: Vec<&str> = full_pattern.split('\0').collect();
            for pat in patterns {
                for entry in glob::glob(pat).map_err(|e| format!("Bad glob pattern: {}", e))? {
                    let path = entry.map_err(|e| format!("Glob error: {}", e))?;
                    if results.len() >= max_results {
                        break;
                    }
                    let display = path.strip_prefix(&display_base).unwrap_or(&path);
                    results.push(display.to_string_lossy().to_string());
                }
                if results.len() >= max_results {
                    break;
                }
            }
        } else if recursive {
            for entry in walkdir::WalkDir::new(&path)
                .max_depth(10)
                .into_iter()
                // Prune ignored dirs (build output / deps) the same way grep and
                // glob do, so the three tools agree about the tree.
                .filter_entry(|e| {
                    if e.depth() == 0 {
                        return true;
                    }
                    let name = e.file_name().to_string_lossy();
                    !name.starts_with('.') && !should_skip_path(e.path())
                })
                .filter_map(|e| e.ok())
            {
                if results.len() >= max_results {
                    break;
                }
                let p = entry.path();
                let display = p.strip_prefix(&display_base).unwrap_or(p);
                results.push(display.to_string_lossy().to_string());
            }
        } else {
            let entries = env
                .fs()
                .list_dir(&resolved)
                .await
                .map_err(|e| format!("Failed to read dir '{}': {}", path, e))?;
            for entry in entries {
                if results.len() >= max_results {
                    break;
                }
                let suffix = if entry.is_dir { "/" } else { "" };
                results.push(format!("{}{}", entry.name, suffix));
            }
        }

        if results.is_empty() {
            Ok("No files found.".to_string())
        } else {
            Ok(results.join("\n"))
        }
    }

    async fn call_structured(
        &self,
        arguments: &str,
    ) -> Result<neenee_contracts::ToolOutput, String> {
        let out = self.call(arguments).await?;
        Ok(neenee_contracts::ToolOutput::Listing {
            entries: out.split('\n').map(str::to_string).collect(),
        })
    }
}
neenee_contracts::register_tool!(ListDirFactory => |ctx| ListDirTool {
    root: workspace_base(ctx),
    env: Some(execution_environment(ctx)),
});
