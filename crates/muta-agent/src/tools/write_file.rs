use async_trait::async_trait;
use muta_contracts::Tool;
use muta_tool_derive::ToolSchema;
use serde::Deserialize;

use crate::tools::helpers::{
    WorkspaceBase, env_from_root, execution_environment, json_string, resolve_workspace_path,
    workspace_base,
};

#[derive(ToolSchema, Deserialize)]
struct WriteFileArgs {
    #[tool(desc = "Path to the file")]
    path: String,
    #[tool(desc = "Content to write")]
    content: String,
}

/// Write content to a file (overwrites).
///
/// Relative paths resolve against the session's workspace root (captured at
/// factory time), not the daemon process's cwd — under the unified daemon
/// (ADR-0096) those differ whenever the daemon was first spawned from another
/// project, and a write is exactly where that divergence does damage.
pub struct WriteFileTool {
    pub(crate) root: WorkspaceBase,
    pub(crate) env: Option<std::sync::Arc<dyn muta_contracts::ExecutionEnvironment>>,
}

impl WriteFileTool {
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
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }
    fn description(&self) -> &str {
        "Create a new file or overwrite an existing file with the given content."
    }
    fn parameters(&self) -> serde_json::Value {
        WriteFileArgs::parameters_schema()
    }
    fn scope_target(&self, arguments: &str) -> muta_contracts::ScopeTarget {
        muta_contracts::ScopeTarget::Path(std::path::PathBuf::from(json_string(arguments, "path")))
    }
    fn hazard_level(&self) -> muta_contracts::HazardLevel {
        muta_contracts::HazardLevel::FileModification
    }
    fn permission_submission(
        &self,
        arguments: &str,
    ) -> Option<muta_contracts::ToolPermissionSubmission> {
        let path = json_string(arguments, "path");
        Some(muta_contracts::ToolPermissionSubmission {
            hazard_level: muta_contracts::HazardLevel::FileModification,
            label: format!("Write file `{path}`"),
            description: format!("Creates or overwrites file `{path}` with new content."),
            scope: path.clone(),
            payload: muta_contracts::ToolPermissionPayload::FileEdit {
                paths: vec![path],
                operation: "write_file".to_string(),
            },
        })
    }
    async fn call(&self, arguments: &str) -> Result<String, String> {
        self.call_structured(arguments).await.map(|o| o.to_text())
    }

    async fn call_structured(&self, arguments: &str) -> Result<muta_contracts::ToolOutput, String> {
        let args: WriteFileArgs =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {}", e))?;
        let path = &args.path;
        let content = &args.content;

        let env = self
            .env
            .clone()
            .unwrap_or_else(|| env_from_root(&self.root));
        let resolved = resolve_workspace_path(&self.root, path);

        // Syntax defense guard: verify syntactic integrity before committing changes to disk.
        if let super::syntax_guard::SyntaxCheckResult::Invalid(err) =
            super::syntax_guard::verify_syntax(&resolved, content)
        {
            return Err(format!(
                "Syntax check failed for '{}': {err}. The file was NOT written. Please fix the syntax error and try again.",
                path
            ));
        }

        // Write atomically (temp file + fsync + rename) so an interrupted write
        // never leaves a half-written, corrupt file in place of the original.
        env.fs()
            .write(&resolved, content.as_bytes())
            .await
            .map_err(|e| format!("Failed to write '{}': {}", path, e))?;

        Ok(muta_contracts::ToolOutput::Patch {
            path: path.to_string(),
            op: muta_contracts::PatchOp::Create,
            old: String::new(),
            new: content.to_string(),
            start_line: 0,
        })
    }
}
muta_contracts::register_tool!(WriteFileFactory => |ctx| WriteFileTool {
    root: workspace_base(ctx),
    env: Some(execution_environment(ctx)),
});
