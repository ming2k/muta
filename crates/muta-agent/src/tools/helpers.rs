//! Shared helpers for built-in tools.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

/// The directory workspace-relative tool operations resolve against.
///
/// Tools are session-scoped under the unified daemon (ADR-0096): one process
/// hosts sessions for many projects, so the daemon's process cwd is whatever
/// directory the first client spawned it from. The assembling bootstrap
/// therefore registers the session's project root as a
/// [`WorkspaceRoot`](muta_contracts::WorkspaceRoot) service on the
/// [`ToolContext`](muta_contracts::ToolContext), and each path-taking tool
/// captures it at factory time into its `root` field.
///
/// `None` (unit tests, a context built without the service) means "use the
/// process cwd" — the historical behaviour, still correct wherever one
/// process serves exactly one project.
pub(crate) type WorkspaceBase = Option<PathBuf>;

/// Capture the workspace base from a tool-assembly context. Factories call
/// this once at build time; the returned value is immutable for the tool's
/// lifetime, matching the session whose bootstrap assembled it.
pub(crate) fn workspace_base(ctx: &muta_contracts::tool_registry::ToolContext) -> WorkspaceBase {
    ctx.workspace_root().map(Path::to_path_buf)
}

/// Capture or synthesize an [`ExecutionEnvironment`](muta_contracts::execution::ExecutionEnvironment)
/// from the tool build context.
pub(crate) fn execution_environment(
    ctx: &muta_contracts::tool_registry::ToolContext,
) -> std::sync::Arc<dyn muta_contracts::execution::ExecutionEnvironment> {
    if let Some(env) =
        ctx.get::<std::sync::Arc<dyn muta_contracts::execution::ExecutionEnvironment>>()
    {
        return env.clone();
    }
    let root = ctx
        .workspace_root()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    std::sync::Arc::new(crate::execution::LocalExecutionEnvironment::new(root))
}

/// Synthesize an [`ExecutionEnvironment`](muta_contracts::execution::ExecutionEnvironment)
/// from an optional workspace root base.
pub(crate) fn env_from_root(
    root: &WorkspaceBase,
) -> std::sync::Arc<dyn muta_contracts::execution::ExecutionEnvironment> {
    let base = root.clone().unwrap_or_else(|| PathBuf::from("."));
    std::sync::Arc::new(crate::execution::LocalExecutionEnvironment::new(base))
}

/// Resolve a user-supplied path argument against the tool's workspace base.
///
/// Absolute paths pass through unchanged (`Path::join` semantics); a relative
/// path is anchored to the session's project root, never to the daemon's
/// coincidental process cwd. Model-facing argument text is untouched — only
/// filesystem access goes through the resolved value, so prompt/UI rendering
/// keeps showing what the model actually sent.
pub(crate) fn resolve_workspace_path(base: &WorkspaceBase, path: &str) -> PathBuf {
    match base {
        Some(root) => root.join(path),
        None => PathBuf::from(path),
    }
}

/// Directories that are almost never interesting to search or list and can be
/// enormous: VCS metadata, dependency trees, and build output. These are shared
/// by `grep`, `glob`, and `list_dir` so the three tools prune the *same* set
/// of directories and never disagree about what exists in a tree (previously
/// grep skipped 4 dirs, glob skipped 10, and list skipped none).
pub(crate) const IGNORED_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "__pycache__",
    ".next",
    "dist",
    "build",
    ".venv",
    "venv",
    ".cache",
    ".mypy_cache",
    ".pytest_cache",
    ".tox",
    "coverage",
    ".gradle",
    ".idea",
    ".vscode",
];

/// True if `path` has any component matching [`IGNORED_DIRS`].
pub(crate) fn should_skip_path(path: &std::path::Path) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_str()
            .is_some_and(|name| IGNORED_DIRS.contains(&name))
    })
}

/// Extract a string field from JSON arguments for `permission_scope`.
pub(crate) fn json_string(arguments: &str, key: &str) -> String {
    serde_json::from_str::<serde_json::Value>(arguments)
        .ok()
        .and_then(|value| value.get(key)?.as_str().map(str::to_string))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "*".to_string())
}

/// Write `bytes` to `path` atomically: serialize to a sibling temp file,
/// `fsync` it, then `rename` over `path`.
///
/// A direct `std::fs::write` overwrites the target in place, so a crash or
/// signal mid-write leaves a **partially-written, corrupt** file. For a
/// code-editing agent that is a real data-loss vector: an interrupted
/// `edit_file` could destroy the very file it was fixing. The temp-then-rename
/// pattern keeps the previous contents intact and readable right up until the
/// (atomic) rename commits the new version.
///
/// The temp file lives next to the target (`<path>.<pid>.tmp`) so the rename
/// is on the same filesystem (POSIX requires same-filesystem for atomic
/// `rename(2)`). The temp file is best-effort removed on failure; leaving it
/// behind is harmless since the next successful write overwrites it.
#[allow(dead_code)]
pub(crate) fn save_file_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // A unique-ish temp name: incorporating the PID avoids collisions between
    // concurrent writers without pulling in a UUID/timestamp dependency.
    let temporary = atomic_temp_path(path);
    let result = (|| -> std::io::Result<()> {
        let mut file = File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        // Best-effort: don't litter on failure. Presence is harmless (the next
        // write overwrites it) but tidy is tidy.
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// The sibling temp-file path used by [`save_file_atomic`].
fn atomic_temp_path(path: &Path) -> std::path::PathBuf {
    let pid = std::process::id();
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(format!(".{pid}.tmp"));
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_round_trips_and_replaces() {
        let dir = std::env::temp_dir().join(format!("muta-atomic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("target.txt");

        save_file_atomic(&target, b"first").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"first");

        // A second write fully replaces the first (no append, no remnant).
        save_file_atomic(&target, b"second").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"second");

        // No temp file is left behind.
        assert!(
            !dir.read_dir().unwrap().any(|e| {
                e.map(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
                    .unwrap_or(false)
            }),
            "temp file leaked"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn atomic_write_creates_parent_dirs() {
        let dir = std::env::temp_dir().join(format!("muta-atomic-nested-{}", std::process::id()));
        let target = dir.join("nested/deep/file.txt");
        save_file_atomic(&target, b"hi").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"hi");
        std::fs::remove_dir_all(&dir).ok();
    }
}
