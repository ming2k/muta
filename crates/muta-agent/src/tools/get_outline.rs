use async_trait::async_trait;
use muta_contracts::Tool;
use muta_tool_derive::ToolSchema;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::tools::helpers::{
    WorkspaceBase, env_from_root, execution_environment, resolve_workspace_path, workspace_base,
};

/// Maximum number of top-level symbols rendered in one outline. A structural
/// summary is bounded evidence, not a full repository dump (ADR-0214 §3).
const MAX_OUTLINE_SYMBOLS: usize = 200;

/// Maximum rendered outline size in bytes. This is an output budget; the file
/// itself may be larger and is never truncated on disk.
const MAX_OUTLINE_BYTES: usize = 16 * 1024;

/// Maximum source size eligible for outlining. Parsing is work, so the input
/// size is bounded independently of the rendered-output budget (ADR-0214 §3).
const MAX_SOURCE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(ToolSchema, Deserialize)]
#[serde(deny_unknown_fields)]
struct GetOutlineArgs {
    #[tool(
        desc = "Path to the source file (e.g. .rs, .ts, .py); relative paths use the primary workspace"
    )]
    path: String,
}

/// Inspect the top-level AST symbols of a source file (ADR-0211 / ADR-0214).
///
/// The outline is a syntactic summary of the bytes read at call time. Every
/// result carries a content-addressed version identity so callers can tell
/// whether later evidence describes the same snapshot, and every result is
/// bounded and discloses truncation.
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

/// Content-addressed version identity for a source snapshot.
///
/// Deterministic over the bytes actually read, so the same content always
/// yields the same short digest. This is provenance, not a security boundary.
fn content_version(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(12);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in digest.iter().take(6) {
        hex.push(HEX[(byte >> 4) as usize] as char);
        hex.push(HEX[(byte & 0x0f) as usize] as char);
    }
    hex
}

/// Error for a source that exceeds the input budget.
fn oversized_source(path: &str, len: u64) -> String {
    format!(
        "'{path}' is {len} bytes, above the {MAX_SOURCE_BYTES} byte outline input budget. \
         No structural analysis was performed. Read a narrower file or use search/text tools instead."
    )
}

#[async_trait]
impl Tool for GetOutlineTool {
    fn name(&self) -> &str {
        "get_outline"
    }

    fn description(&self) -> &str {
        "Fast overview of top-level code symbols (functions, structs, traits, classes) in a source file without reading full implementations. Best for locating targets in large files before using read_text. Supported: rs, ts, js, py, c, cpp, go."
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
                "Unsupported file type for AST outline (extension: '{ext}'). Supported: rs, ts, js, py, c, cpp, go. No structural analysis was performed for this snapshot."
            ));
        }

        // Bound the input, not just the rendered output. Metadata is a cheap
        // first check; some backends may report it loosely, so the post-read
        // length below is the backstop.
        if let Ok(metadata) = env.fs().metadata(&resolved).await {
            if metadata.len > MAX_SOURCE_BYTES {
                return Err(oversized_source(&args.path, metadata.len));
            }
        }

        // Read the current bytes once and derive every field — version, size,
        // and symbols — from that same snapshot. A concurrent change afterward
        // remains possible; the result describes the bytes read here.
        let bytes = env
            .fs()
            .read(&resolved)
            .await
            .map_err(|e| format!("Failed to read file '{}': {e}", resolved.display()))?;
        if bytes.len() as u64 > MAX_SOURCE_BYTES {
            return Err(oversized_source(&args.path, bytes.len() as u64));
        }
        let version = content_version(&bytes);
        let content = String::from_utf8_lossy(&bytes);

        let symbols = crate::syntax::extract_symbols(&ext, &content);
        if symbols.is_empty() {
            return Ok(format!(
                "Outline for '{}' (version {version}, {} bytes): no top-level symbols found. \
                 This file may be unsupported, empty, or contain only private/local items. \
                 The outline is a syntactic summary, not a complete AST or semantic analysis.",
                args.path,
                bytes.len()
            ));
        }

        let total = symbols.len();
        let mut rendered = String::new();
        let mut shown = 0usize;
        let mut truncated = false;
        for symbol in symbols.iter().take(MAX_OUTLINE_SYMBOLS) {
            if rendered.len() + symbol.len() + 1 > MAX_OUTLINE_BYTES {
                truncated = true;
                break;
            }
            rendered.push_str(symbol);
            rendered.push('\n');
            shown += 1;
        }
        if shown < total {
            truncated = true;
        }

        let mut header = format!(
            "Outline for '{}' (version {version}, {} bytes, {shown} of {total} symbols",
            args.path,
            bytes.len()
        );
        if truncated {
            header.push_str("; output truncated");
        }
        header.push_str("):\n");

        let mut out = header;
        out.push_str(&rendered);
        if truncated {
            out.push_str(&format!(
                "\n[Outlined only the first {shown} of {total} symbols within the {MAX_OUTLINE_SYMBOLS}-symbol / {MAX_OUTLINE_BYTES}-byte budget. Query a narrower path or read the file for the remainder.]"
            ));
        }
        Ok(out)
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

        assert!(res.contains("Outline for 'service.rs'"));
        assert!(res.contains("version "));
        assert!(res.contains("bytes"));
        assert!(res.contains("2 of 2 symbols"));
        assert!(res.contains("pub struct MyService"));
        assert!(res.contains("pub fn run_service"));
    }

    /// ADR-0214: the result must identify the source version it describes, and
    /// that identity must change when the bytes on disk change.
    #[tokio::test]
    async fn get_outline_version_tracks_content_changes() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("lib.rs");
        let tool = GetOutlineTool::new(Some(dir.path().to_path_buf()));

        std::fs::write(&file, "pub struct BeforeX;\n").unwrap();
        let first = tool.call(r#"{"path": "lib.rs"}"#).await.unwrap();

        // Same bytes -> same version (deterministic).
        let repeat = tool.call(r#"{"path": "lib.rs"}"#).await.unwrap();
        assert_eq!(
            first.lines().next(),
            repeat.lines().next(),
            "identical content must yield the same version identity"
        );

        // Same-size content replacement still changes the version.
        std::fs::write(&file, "pub struct AfterXX;\n").unwrap();
        let second = tool.call(r#"{"path": "lib.rs"}"#).await.unwrap();
        assert_ne!(first, second);
        assert!(second.contains("pub struct AfterXX"));
    }

    /// ADR-0214: unsupported formats must fail loudly rather than being
    /// reported as successfully parsed.
    #[tokio::test]
    async fn get_outline_rejects_unsupported_language() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "hello\n").unwrap();
        let tool = GetOutlineTool::new(Some(dir.path().to_path_buf()));
        let err = tool.call(r#"{"path": "notes.txt"}"#).await.unwrap_err();
        assert!(err.contains("Unsupported file type"));
    }

    /// ADR-0214: structure output has a finite budget and discloses truncation
    /// rather than silently clipping or emitting everything.
    #[tokio::test]
    async fn get_outline_discloses_truncation() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("big.rs");
        let mut source = String::new();
        for i in 0..(MAX_OUTLINE_SYMBOLS + 25) {
            source.push_str(&format!("pub struct Item{i};\n"));
        }
        std::fs::write(&file, source).unwrap();

        let tool = GetOutlineTool::new(Some(dir.path().to_path_buf()));
        let out = tool.call(r#"{"path": "big.rs"}"#).await.unwrap();

        assert!(out.contains("output truncated"));
        assert!(out.contains(&format!(
            "{MAX_OUTLINE_SYMBOLS} of {} symbols",
            MAX_OUTLINE_SYMBOLS + 25
        )));
        assert!(!out.contains("pub struct Item200"));
    }

    /// ADR-0214: the input itself is bounded, not only the rendered output.
    #[tokio::test]
    async fn get_outline_rejects_oversized_source() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("huge.rs");
        let mut source = String::new();
        while source.len() as u64 <= MAX_SOURCE_BYTES {
            source.push_str("pub struct Pad;\n");
        }
        std::fs::write(&file, source).unwrap();

        let tool = GetOutlineTool::new(Some(dir.path().to_path_buf()));
        let err = tool.call(r#"{"path": "huge.rs"}"#).await.unwrap_err();
        assert!(err.contains("input budget"), "got: {err}");
    }
}
