//! Session grouping (ADR-0226): the partition that lists/resumes/searches key
//! on is **derived** from two concrete fields — an optional workspace and an
//! optional user-named space — never a stored opaque scope. A persona is
//! orthogonal to grouping.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The grouping a session belongs to. Transient query/layout value, derived
/// from `workspace` ?? `space` ?? Personal; it is never persisted as an opaque
/// key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionGrouping {
    pub workspace: Option<PathBuf>,
    pub space: Option<String>,
}

impl SessionGrouping {
    /// No workspace, no named space: the implicit Personal space.
    pub fn personal() -> Self {
        Self {
            workspace: None,
            space: None,
        }
    }

    /// A workspace grouping (workspace-first: the space is ignored).
    pub fn workspace(root: impl Into<PathBuf>) -> Self {
        Self {
            workspace: Some(root.into()),
            space: None,
        }
    }

    /// A user-named conversation space (workspace-free).
    pub fn named(space: impl Into<String>) -> Self {
        Self {
            workspace: None,
            space: Some(space.into()),
        }
    }

    pub fn is_personal(&self) -> bool {
        self.workspace.is_none() && self.space.is_none()
    }

    pub fn workspace_root(&self) -> Option<&Path> {
        self.workspace.as_deref()
    }

    pub fn space_name(&self) -> Option<&str> {
        self.space.as_deref()
    }

    /// Human-facing label: the workspace path, the space name, or `Personal`.
    pub fn label(&self) -> String {
        match (&self.workspace, &self.space) {
            (Some(root), _) => root.to_string_lossy().into_owned(),
            (None, Some(space)) => space.clone(),
            (None, None) => "Personal".to_string(),
        }
    }

    /// Stable key for the on-disk bucket (`projects/<hash>`).
    pub fn bucket_key(&self) -> String {
        match (&self.workspace, &self.space) {
            (Some(root), _) => root.to_string_lossy().into_owned(),
            (None, Some(space)) => format!("space:{space}"),
            (None, None) => "personal".to_string(),
        }
    }

    /// The `(workspace_root, space)` pair persisted on the session row. Compare
    /// null-safely (`workspace_root IS ? AND space IS ?`). Workspace-first: a
    /// workspace grouping carries no space.
    pub fn query_parts(&self) -> (Option<String>, Option<String>) {
        match &self.workspace {
            Some(root) => (Some(root.to_string_lossy().into_owned()), None),
            None => (None, self.space.clone()),
        }
    }
}

/// The optional filesystem binding for an agent's tools. Independent of
/// [`SessionGrouping`]: a named space may carry one, a workspace grouping does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceBinding {
    pub root: PathBuf,
    #[serde(default)]
    pub additional_roots: Vec<PathBuf>,
}

impl WorkspaceBinding {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            additional_roots: Vec::new(),
        }
    }

    pub fn with_additional_roots(mut self, roots: impl IntoIterator<Item = PathBuf>) -> Self {
        self.additional_roots = roots.into_iter().collect();
        self
    }

    pub fn roots(&self) -> impl Iterator<Item = &Path> {
        std::iter::once(self.root.as_path())
            .chain(self.additional_roots.iter().map(PathBuf::as_path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouping_is_derived_from_workspace_then_space_then_personal() {
        assert!(SessionGrouping::personal().is_personal());
        assert_eq!(SessionGrouping::personal().label(), "Personal");
        assert_eq!(
            SessionGrouping::workspace("/a/b").workspace_root(),
            Some(Path::new("/a/b"))
        );
        assert_eq!(
            SessionGrouping::named("philosophy").space_name(),
            Some("philosophy")
        );
    }

    #[test]
    fn workspace_wins_over_space() {
        let g = SessionGrouping {
            workspace: Some(PathBuf::from("/repo")),
            space: Some("ignored".into()),
        };
        let (workspace, space) = g.query_parts();
        assert_eq!(workspace.as_deref(), Some("/repo"));
        assert_eq!(space, None);
    }

    #[test]
    fn bucket_keys_are_distinct_per_grouping_kind() {
        assert_eq!(SessionGrouping::personal().bucket_key(), "personal");
        assert_eq!(
            SessionGrouping::named("philosophy").bucket_key(),
            "space:philosophy"
        );
        assert_eq!(SessionGrouping::workspace("/repo").bucket_key(), "/repo");
    }
}
