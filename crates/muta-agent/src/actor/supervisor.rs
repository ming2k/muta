//! Hierarchical Actor supervisor managing active subagents, cancellation, and routing.

use super::handle::ActorHandle;
use muta_contracts::{ActorEvent, ActorId};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// The central supervisor registry for a session's Actor hierarchy.
#[derive(Clone, Default)]
pub struct ActorSupervisor {
    actors: Arc<Mutex<HashMap<ActorId, ActorHandle>>>,
    event_tx: Option<mpsc::UnboundedSender<ActorEvent>>,
}

impl ActorSupervisor {
    pub fn new(event_tx: Option<mpsc::UnboundedSender<ActorEvent>>) -> Self {
        Self {
            actors: Arc::new(Mutex::new(HashMap::new())),
            event_tx,
        }
    }

    /// Register a newly spawned Actor with the supervisor.
    pub fn register(&self, handle: ActorHandle) {
        let mut map = self.actors.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(ActorEvent::Spawned {
                id: handle.id.clone(),
                parent_id: handle.parent_id.clone(),
                role: handle.role.clone(),
                worktree_mode: handle.worktree_mode,
            });
        }
        map.insert(handle.id.clone(), handle);
    }

    /// Get a handle to an active Actor by its ID.
    pub fn get(&self, id: &str) -> Option<ActorHandle> {
        let map = self.actors.lock().unwrap_or_else(|e| e.into_inner());
        map.get(id).cloned()
    }

    /// Remove a terminated actor from the active registry.
    pub fn remove(&self, id: &str) -> Option<ActorHandle> {
        let mut map = self.actors.lock().unwrap_or_else(|e| e.into_inner());
        map.remove(id)
    }

    /// List all currently registered active actors.
    pub fn list_active(&self) -> Vec<ActorHandle> {
        let map = self.actors.lock().unwrap_or_else(|e| e.into_inner());
        map.values().cloned().collect()
    }

    /// Broadcast cancellation to all registered actors (hierarchical teardown).
    pub fn cancel_all(&self, reason: &str) {
        let active = self.list_active();
        for actor in active {
            actor.cancel(reason.to_string());
        }
    }

    /// Emit an actor event up to the session event stream.
    pub fn emit_event(&self, event: ActorEvent) {
        if let Some(ref tx) = self.event_tx {
            let _ = tx.send(event);
        }
    }
}
