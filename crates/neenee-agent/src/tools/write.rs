use async_trait::async_trait;
use neenee_contracts::Tool;
use serde_json::json;

use crate::tools::helpers::{json_string, save_file_atomic};

/// Write content to a file (overwrites).
pub struct WriteFileTool;

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }
    fn description(&self) -> &str {
        "Create a new file or replace an entire existing file's contents. \
         Use this when creating a file or when the change spans most of the \
         file; for a small localized change to an existing file prefer \
         edit_file. Writes are atomic (temp + rename) so an interrupted turn \
         never leaves a corrupt or half-written file. Do not write files \
         through the shell (echo >, printf >, cat >)."
    }
    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file" },
                "content": { "type": "string", "description": "Content to write" }
            },
            "required": ["path", "content"]
        })
    }
    fn scope_target(&self, arguments: &str) -> neenee_contracts::ScopeTarget {
        neenee_contracts::ScopeTarget::Path(std::path::PathBuf::from(json_string(
            arguments, "path",
        )))
    }
    async fn call(&self, arguments: &str) -> Result<String, String> {
        self.call_structured(arguments).await.map(|o| o.to_text())
    }

    async fn call_structured(
        &self,
        arguments: &str,
    ) -> Result<neenee_contracts::ToolOutput, String> {
        let args: serde_json::Value =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {}", e))?;
        let path = args["path"].as_str().ok_or("Missing 'path'")?;
        let content = args["content"].as_str().ok_or("Missing 'content'")?;

        // Write atomically (temp file + fsync + rename) so an interrupted write
        // never leaves a half-written, corrupt file in place of the original.
        save_file_atomic(std::path::Path::new(path), content.as_bytes())
            .map_err(|e| format!("Failed to write '{}': {}", path, e))?;
        Ok(neenee_contracts::ToolOutput::Patch {
            path: path.to_string(),
            op: neenee_contracts::PatchOp::Create,
            old: String::new(),
            new: content.to_string(),
            start_line: 0,
        })
    }
}
neenee_contracts::register_tool!(WriteFileFactory => WriteFileTool);
