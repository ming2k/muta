//! Shared helpers for built-in tools.

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
/// Leading `~` is expanded to the user's home directory. Absolute paths pass
/// through unchanged (`Path::join` semantics); a relative path is anchored to
/// the session's project root, never to the daemon's coincidental process cwd.
/// Model-facing argument text is untouched — only filesystem access goes
/// through the resolved value, so prompt/UI rendering keeps showing what the
/// model actually sent.
pub(crate) fn resolve_workspace_path(base: &WorkspaceBase, path: &str) -> PathBuf {
    let expanded = muta_contracts::execution::expand_tilde(Path::new(path));
    if expanded.is_absolute() {
        expanded
    } else {
        match base {
            Some(root) => root.join(expanded),
            None => expanded,
        }
    }
}

/// Directories that are almost never interesting to search or list and can be
/// enormous: VCS metadata, dependency trees, and build output. These are shared
/// by `find_files` and `search_text` so discovery and content search prune the
/// same set of directories and never disagree about the searchable tree.
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
    "proc",
    "sys",
    "dev",
    "run",
];

/// Extract a string field from JSON arguments for `permission_scope`.
pub(crate) fn json_string(arguments: &str, key: &str) -> String {
    serde_json::from_str::<serde_json::Value>(arguments)
        .ok()
        .and_then(|value| value.get(key)?.as_str().map(str::to_string))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "*".to_string())
}

/// Deserialize either a single string or an array of strings into `Option<Vec<String>>`.
///
/// Models frequently supply `"patterns": "*.rs"` or `"include": "*.rs"` instead
/// of wrapping a single item in an array. This provides robust parameter ergonomics
/// without sacrificing type safety.
pub(crate) fn deserialize_optional_string_or_vec<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct StringOrVec;

    impl<'de> serde::de::Visitor<'de> for StringOrVec {
        type Value = Option<Vec<String>>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a string or an array of strings")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(vec![trimmed.to_string()]))
            }
        }

        fn visit_seq<S>(self, mut seq: S) -> Result<Self::Value, S::Error>
        where
            S: serde::de::SeqAccess<'de>,
        {
            let mut vec = Vec::new();
            while let Some(elem) = seq.next_element::<String>()? {
                let trimmed = elem.trim();
                if !trimmed.is_empty() {
                    vec.push(trimmed.to_string());
                }
            }
            if vec.is_empty() {
                Ok(None)
            } else {
                Ok(Some(vec))
            }
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(None)
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            Ok(None)
        }
    }

    deserializer.deserialize_any(StringOrVec)
}
