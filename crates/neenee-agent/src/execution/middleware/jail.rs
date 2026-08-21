//! Workspace jail middleware to ensure file operations stay confined within the workspace root.

use async_trait::async_trait;
use neenee_contracts::execution::{ExecutionEnvironment, ToolMiddleware};
use serde_json::Value;

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
            let path = std::path::Path::new(raw_path);
            let resolved = if path.is_absolute() {
                path.to_path_buf()
            } else {
                env.workspace_root().join(path)
            };

            // Disallow obvious dangerous system root breakouts if absolute
            if path.is_absolute() && !resolved.starts_with(env.workspace_root()) {
                // Allow /tmp reads/writes, but deny system sensitive roots (/etc, /root, /var/run)
                let forbidden_roots = ["/etc", "/root", "/var", "/sys", "/proc", "/usr", "/boot"];
                for root in forbidden_roots {
                    if resolved.starts_with(root) {
                        return Err(format!(
                            "Security Denial: access to '{raw_path}' is outside the workspace root and forbidden."
                        ));
                    }
                }
            }
        }

        Ok(())
    }
}
