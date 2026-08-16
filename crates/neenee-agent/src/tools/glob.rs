use async_trait::async_trait;
use neenee_contracts::Tool;
use serde_json::json;

/// Fast file pattern matching using globs.
///
/// Relative patterns and bases resolve against the session's workspace root
/// (captured at factory time), not the daemon process's cwd (ADR-0096).
pub struct GlobTool {
    pub(crate) root: crate::tools::helpers::WorkspaceBase,
}

const GLOB_MAX_RESULTS: usize = 200;

use crate::tools::helpers::{should_skip_path, workspace_base};

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

        // Resolve the base against the session's workspace root so a default
        // `.` (or any relative base) scans the session's project, never the
        // daemon's coincidental process cwd. `join` passes absolute bases
        // through unchanged.
        let base_path = match &self.root {
            Some(root) => root.join(base),
            None => std::path::PathBuf::from(base),
        };
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
neenee_contracts::register_tool!(GlobFactory => |ctx| GlobTool {
    root: workspace_base(ctx),
});
