use async_trait::async_trait;
use muta_contracts::Tool;
use serde_json::json;

use crate::tools::helpers::{
    WorkspaceBase, env_from_root, execution_environment, should_skip_path, workspace_base,
};

/// Fast file pattern matching using globs.
///
/// Relative patterns and bases resolve against the session's workspace root
/// (captured at factory time), not the daemon process's cwd (ADR-0096).
pub struct GlobTool {
    pub(crate) root: WorkspaceBase,
    pub(crate) env: Option<std::sync::Arc<dyn muta_contracts::ExecutionEnvironment>>,
}

impl GlobTool {
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

const GLOB_MAX_RESULTS: usize = 200;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }
    fn description(&self) -> &str {
        "Find files by glob pattern (e.g., '**/*.rs', 'src/**/*.ts'). Returns matching paths."
    }
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern (e.g. '**/*.rs', 'docs/*.md')" },
                "path": { "type": "string", "description": "Base directory to search from (default '.')" }
            },
            "required": ["pattern"]
        })
    }
    async fn call(&self, arguments: &str) -> Result<String, String> {
        let args: serde_json::Value =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {}", e))?;
        let pattern = args["pattern"].as_str().ok_or("Missing 'pattern'")?;
        let base = args["path"].as_str().unwrap_or(".");

        let env = self
            .env
            .clone()
            .unwrap_or_else(|| env_from_root(&self.root));
        // Resolve the base against the session's workspace root so a default
        // `.` (or any relative base) scans the session's project, never the
        // daemon's coincidental process cwd. `join` passes absolute bases
        // through unchanged.
        let base_path = match &self.root {
            Some(root) => root.join(base),
            None => std::path::PathBuf::from(base),
        };
        let metadata = env
            .fs()
            .metadata(&base_path)
            .await
            .map_err(|error| format!("Cannot search glob base '{}': {error}", base))?;
        if !metadata.is_dir {
            return Err(format!("Glob base is not a directory: {base}"));
        }
        reject_pattern_traversal(pattern)?;
        let candidates = if pattern.contains('/') || base != "." {
            vec![base_path.join(pattern).to_string_lossy().to_string()]
        } else {
            vec![
                base_path.join(pattern).to_string_lossy().to_string(),
                base_path
                    .join("**")
                    .join(pattern)
                    .to_string_lossy()
                    .to_string(),
            ]
        };

        let cwd = self
            .root
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let mut results = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for candidate in &candidates {
            for entry in glob::glob(candidate).map_err(|e| format!("Bad glob pattern: {}", e))? {
                let path = match entry {
                    Ok(path) => path,
                    Err(_) => continue,
                };
                if should_skip_path(&path) {
                    continue;
                }
                let display = path
                    .strip_prefix(&cwd)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string();
                if seen.insert(display.clone()) {
                    results.push(display);
                }
                if results.len() >= GLOB_MAX_RESULTS {
                    break;
                }
            }
            if results.len() >= GLOB_MAX_RESULTS {
                break;
            }
        }

        if results.is_empty() {
            Ok("No files matched the pattern.".to_string())
        } else {
            Ok(results.join("\n"))
        }
    }
}

fn reject_pattern_traversal(pattern: &str) -> Result<(), String> {
    if std::path::Path::new(pattern).is_absolute()
        || std::path::Path::new(pattern).components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err("Glob pattern must stay relative to its workspace base".to_string());
    }
    Ok(())
}

muta_contracts::register_tool!(GlobFactory => |ctx| GlobTool {
    root: workspace_base(ctx),
    env: Some(execution_environment(ctx)),
});
