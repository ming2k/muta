use async_trait::async_trait;
use muta_contracts::Tool;
use muta_tool_derive::ToolSchema;
use serde::Deserialize;

use crate::tools::helpers::{
    WorkspaceBase, env_from_root, execution_environment, resolve_workspace_path, workspace_base,
};

#[derive(ToolSchema, Deserialize)]
#[serde(deny_unknown_fields)]
struct GetOutlineArgs {
    #[tool(
        desc = "Path to the source file (e.g. .rs, .ts, .py); relative paths use the primary workspace"
    )]
    path: String,
}

/// Inspect the top-level AST symbols of a source file (ADR-0211).
pub struct GetOutlineTool {
    pub(crate) root: WorkspaceBase,
    pub(crate) env: Option<std::sync::Arc<dyn muta_contracts::ExecutionEnvironment>>,
}

impl GetOutlineTool {
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
impl Tool for GetOutlineTool {
    fn name(&self) -> &str {
        "get_outline"
    }

    fn description(&self) -> &str {
        "Inspect the top-level AST symbols (functions, structs, traits, classes, interfaces) of a source file without reading the full implementation body."
    }

    fn parameters(&self) -> serde_json::Value {
        GetOutlineArgs::parameters_schema()
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let args: GetOutlineArgs = serde_json::from_str(arguments)
            .map_err(|e| format!("Invalid arguments for get_outline: {e}"))?;

        let env = self
            .env
            .clone()
            .unwrap_or_else(|| env_from_root(&self.root));

        let resolved = resolve_workspace_path(&self.root, &args.path);

        if env.fs().is_dir(&resolved).await {
            return Err(format!(
                "'{}' is a directory, not a source file.",
                args.path
            ));
        }

        let ext = resolved
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        if crate::syntax::SupportedLanguage::from_extension(&ext).is_none() {
            return Err(format!(
                "Unsupported file type for AST outline (extension: '{ext}'). Supported: rs, ts, js, py, c, cpp, go."
            ));
        }

        let content = env
            .fs()
            .read_to_string(&resolved)
            .await
            .map_err(|e| format!("Failed to read file '{}': {e}", resolved.display()))?;

        let symbols = crate::syntax::extract_symbols(&ext, &content);
        if symbols.is_empty() {
            return Ok(format!(
                "No top-level exported/public symbols found in '{}'.",
                args.path
            ));
        }

        Ok(format!(
            "Outline for '{}':\n{}",
            args.path,
            symbols.join("\n")
        ))
    }
}

muta_contracts::register_tool!(GetOutlineFactory => |ctx| GetOutlineTool {
    root: workspace_base(ctx),
    env: Some(execution_environment(ctx)),
});

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn get_outline_extracts_symbols_from_file() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("service.rs");
        std::fs::write(&file, "pub struct MyService;\npub fn run_service() {}\n").unwrap();

        let tool = GetOutlineTool::new(Some(dir.path().to_path_buf()));
        let res = tool.call(r#"{"path": "service.rs"}"#).await.unwrap();

        assert!(res.contains("Outline for 'service.rs':"));
        assert!(res.contains("pub struct MyService"));
        assert!(res.contains("pub fn run_service()"));
    }
}
