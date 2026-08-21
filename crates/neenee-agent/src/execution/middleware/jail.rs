//! Workspace jail middleware to ensure file operations stay confined within the workspace root.

use async_trait::async_trait;
use neenee_contracts::execution::{ExecutionEnvironment, ToolMiddleware};
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

/// Middleware that inspects path arguments in tool calls and rejects paths outside the workspace root.
#[derive(Debug, Default, Clone)]
pub struct WorkspaceJailMiddleware;

#[async_trait]
impl ToolMiddleware for WorkspaceJailMiddleware {
    async fn pre_execute(
        &self,
        tool: &str,
        arguments: &Value,
        env: &dyn ExecutionEnvironment,
    ) -> Result<(), String> {
        let path_str = match tool {
            "read_text" | "read_text_terse" | "write_file" | "edit_file" | "list_dir" => {
                arguments.get("path").and_then(|v| v.as_str())
            }
            _ => None,
        };

        if let Some(raw_path) = path_str {
            // `PathBuf::join` preserves the workspace prefix for relative
            // paths and applies the platform's native replacement rules for
            // absolute/rooted paths (drive and UNC prefixes on Windows).
            // Normalize lexically before containment so `../` cannot spoof a
            // workspace prefix. The execution environment contract supplies
            // a canonical workspace root; no host filesystem calls belong in
            // this middleware because the backing FS may be remote or virtual.
            let root = lexical_normalize(env.workspace_root());
            let resolved = lexical_normalize(&env.workspace_root().join(Path::new(raw_path)));
            if !resolved.starts_with(&root) {
                return Err(format!(
                    "Security Denial: access to '{raw_path}' is outside the workspace root and forbidden."
                ));
            }
        }

        Ok(())
    }
}

/// Normalize `.` and `..` without consulting a host filesystem.
///
/// This deliberately works in terms of `Path::components`, so prefixes and
/// root components retain their native Windows/Unix meaning.
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() && !normalized.has_root() {
                    normalized.push(component.as_os_str());
                }
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}
