//! Isolated workspace execution environment supporting shadow copies and Git worktrees.

use muta_contracts::WorktreeMode;
use std::path::{Path, PathBuf};

/// An isolated working directory context for an Actor or Subagent.
pub struct IsolatedWorkspace {
    /// The unique identifier of the isolation context.
    pub id: String,
    /// Root directory where the agent's operations take place.
    pub root_dir: PathBuf,
    /// The original parent project directory.
    pub base_dir: PathBuf,
    /// Mode of isolation.
    pub mode: WorktreeMode,
    /// Whether this workspace created a temporary directory that needs deletion on drop.
    is_ephemeral: bool,
}

impl IsolatedWorkspace {
    /// Create and initialize an isolated workspace.
    pub fn create(base_dir: &Path, mode: WorktreeMode, id: &str) -> Result<Self, String> {
        match mode {
            WorktreeMode::Inherit => Ok(Self {
                id: id.to_string(),
                root_dir: base_dir.to_path_buf(),
                base_dir: base_dir.to_path_buf(),
                mode,
                is_ephemeral: false,
            }),
            WorktreeMode::Branch => {
                let temp_base = std::env::temp_dir().join("muta_shadow_worktrees");
                let worktree_dir = temp_base.join(format!("muta_{}_{}", id, uuid::Uuid::new_v4()));
                std::fs::create_dir_all(&worktree_dir)
                    .map_err(|e| format!("Failed to create shadow workspace directory: {e}"))?;

                // Shallow-link or copy immediate non-hidden configuration files if existing
                Self::seed_shadow_workspace(base_dir, &worktree_dir)?;

                Ok(Self {
                    id: id.to_string(),
                    root_dir: worktree_dir,
                    base_dir: base_dir.to_path_buf(),
                    mode,
                    is_ephemeral: true,
                })
            }
            WorktreeMode::Share => {
                // If it's a git repo, attempt creating a git worktree; fallback to Branch if not a git repository
                let git_dir = base_dir.join(".git");
                if git_dir.exists() {
                    let temp_base = std::env::temp_dir().join("muta_git_worktrees");
                    let worktree_dir = temp_base.join(format!("muta_wt_{}", id));
                    std::fs::create_dir_all(&temp_base)
                        .map_err(|e| format!("Failed to create git worktree parent: {e}"))?;

                    let output = std::process::Command::new("git")
                        .arg("worktree")
                        .arg("add")
                        .arg("--detach")
                        .arg(&worktree_dir)
                        .arg("HEAD")
                        .current_dir(base_dir)
                        .output();

                    match output {
                        Ok(out) if out.status.success() => Ok(Self {
                            id: id.to_string(),
                            root_dir: worktree_dir,
                            base_dir: base_dir.to_path_buf(),
                            mode,
                            is_ephemeral: true,
                        }),
                        _ => {
                            // Fallback to shadow copy mode
                            Self::create(base_dir, WorktreeMode::Branch, id)
                        }
                    }
                } else {
                    Self::create(base_dir, WorktreeMode::Branch, id)
                }
            }
        }
    }

    /// Seed the shadow workspace with initial files from the base directory.
    fn seed_shadow_workspace(base: &Path, shadow: &Path) -> Result<(), String> {
        // Copy standard project descriptor files (Cargo.toml, package.json, etc.)
        let descriptors = [
            "Cargo.toml",
            "Cargo.lock",
            "package.json",
            "tsconfig.json",
            "pyproject.toml",
            "go.mod",
        ];
        for desc in descriptors {
            let src = base.join(desc);
            if src.is_file() {
                let dest = shadow.join(desc);
                let _ = std::fs::copy(&src, &dest);
            }
        }
        Ok(())
    }

    /// Return the active root directory for tool execution.
    pub fn path(&self) -> &Path {
        &self.root_dir
    }

    /// Compute the list of modified or newly created files compared to base.
    pub fn list_modified_files(&self) -> Vec<PathBuf> {
        if self.mode == WorktreeMode::Inherit {
            return Vec::new();
        }

        let mut modified = Vec::new();
        let walker = walkdir::WalkDir::new(&self.root_dir)
            .into_iter()
            .filter_entry(|e| {
                let name = e.file_name().to_string_lossy();
                name != "target" && name != "node_modules" && name != ".git"
            });

        for entry in walker.flatten() {
            if entry.file_type().is_file() {
                if let Ok(rel) = entry.path().strip_prefix(&self.root_dir) {
                    let base_file = self.base_dir.join(rel);
                    let should_include = if !base_file.exists() {
                        true
                    } else {
                        // Compare file contents
                        let shadow_content = std::fs::read(entry.path()).unwrap_or_default();
                        let base_content = std::fs::read(&base_file).unwrap_or_default();
                        shadow_content != base_content
                    };

                    if should_include {
                        modified.push(rel.to_path_buf());
                    }
                }
            }
        }

        modified
    }

    /// Apply all modified files in the isolated workspace back to the target directory.
    pub fn apply_to_target(&self, target_dir: &Path) -> Result<Vec<PathBuf>, String> {
        if self.mode == WorktreeMode::Inherit {
            return Ok(Vec::new());
        }

        let modified_files = self.list_modified_files();
        let mut applied = Vec::new();

        for rel in &modified_files {
            let src = self.root_dir.join(rel);
            let dst = target_dir.join(rel);

            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create target directory {:?}: {e}", parent))?;
            }

            std::fs::copy(&src, &dst)
                .map_err(|e| format!("Failed to copy {:?} to {:?}: {e}", src, dst))?;
            applied.push(rel.clone());
        }

        Ok(applied)
    }

    /// Clean up ephemeral resources.
    pub fn cleanup(&mut self) -> Result<(), String> {
        if !self.is_ephemeral {
            return Ok(());
        }

        if self.mode == WorktreeMode::Share {
            let _ = std::process::Command::new("git")
                .arg("worktree")
                .arg("remove")
                .arg("--force")
                .arg(&self.root_dir)
                .current_dir(&self.base_dir)
                .output();
        }

        if self.root_dir.exists() {
            let _ = std::fs::remove_dir_all(&self.root_dir);
        }

        self.is_ephemeral = false;
        Ok(())
    }
}

impl Drop for IsolatedWorkspace {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}
