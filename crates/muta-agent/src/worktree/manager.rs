//! Worktree manager providing lifecycle tracking and batch cleanup.

use super::isolated::IsolatedWorkspace;
use muta_contracts::WorktreeMode;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Manager holding and tracking active subagent workspaces.
#[derive(Clone, Default)]
pub struct WorktreeManager {
    workspaces: Arc<Mutex<HashMap<String, Arc<Mutex<IsolatedWorkspace>>>>>,
}

impl WorktreeManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocate a new isolated workspace.
    pub fn create_workspace(
        &self,
        base_dir: &Path,
        mode: WorktreeMode,
        id: &str,
    ) -> Result<Arc<Mutex<IsolatedWorkspace>>, String> {
        let ws = IsolatedWorkspace::create(base_dir, mode, id)?;
        let arc_ws = Arc::new(Mutex::new(ws));
        let mut map = self.workspaces.lock().unwrap_or_else(|e| e.into_inner());
        map.insert(id.to_string(), arc_ws.clone());
        Ok(arc_ws)
    }

    /// Retrieve an existing workspace by ID.
    pub fn get_workspace(&self, id: &str) -> Option<Arc<Mutex<IsolatedWorkspace>>> {
        let map = self.workspaces.lock().unwrap_or_else(|e| e.into_inner());
        map.get(id).cloned()
    }

    /// Remove and trigger cleanup for a workspace.
    pub fn release_workspace(&self, id: &str) -> Result<(), String> {
        let mut map = self.workspaces.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ws) = map.remove(id) {
            let mut guard = ws.lock().unwrap_or_else(|e| e.into_inner());
            guard.cleanup()?;
        }
        Ok(())
    }
}
