//! Actor handle for controlling and querying subagent lifecycle.

use super::mailbox::ActorMailboxSender;
use muta_contracts::{ActorId, ActorMessage, ActorRole, ActorState, WorktreeMode};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tokio_util::sync::CancellationToken;

/// A lightweight, thread-safe handle to interact with an active Actor.
#[derive(Clone)]
pub struct ActorHandle {
    pub id: ActorId,
    pub parent_id: Option<ActorId>,
    pub role: ActorRole,
    pub worktree_mode: WorktreeMode,
    pub cancel_token: CancellationToken,
    sender: ActorMailboxSender,
    state: Arc<RwLock<ActorState>>,
}

impl ActorHandle {
    pub fn new(
        id: ActorId,
        parent_id: Option<ActorId>,
        role: ActorRole,
        worktree_mode: WorktreeMode,
        sender: ActorMailboxSender,
        cancel_token: CancellationToken,
        initial_state: ActorState,
    ) -> Self {
        Self {
            id,
            parent_id,
            role,
            worktree_mode,
            cancel_token,
            sender,
            state: Arc::new(RwLock::new(initial_state)),
        }
    }

    /// Read the current lifecycle state of the Actor.
    pub fn state(&self) -> ActorState {
        self.state.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Update the lifecycle state (called by the actor runtime).
    pub(crate) fn set_state(&self, new_state: ActorState) {
        let mut w = self.state.write().unwrap_or_else(|e| e.into_inner());
        *w = new_state;
    }

    /// Send a new task payload to the Actor.
    pub fn send_task(&self, prompt: String, target_files: Vec<String>) -> Result<(), String> {
        self.sender.send_message(
            self.parent_id.clone(),
            self.id.clone(),
            ActorMessage::Task {
                prompt,
                target_files,
                metadata: HashMap::new(),
            },
        )
    }

    /// Send mid-flight steering or conversational input to the Actor.
    pub fn send_input(&self, content: String) -> Result<(), String> {
        self.sender.send_message(
            self.parent_id.clone(),
            self.id.clone(),
            ActorMessage::Input { content },
        )
    }

    /// Request cancellation of the Actor's in-flight work.
    pub fn cancel(&self, reason: String) {
        self.cancel_token.cancel();
        let _ = self.sender.send_message(
            self.parent_id.clone(),
            self.id.clone(),
            ActorMessage::Cancel { reason },
        );
        self.set_state(ActorState::Cancelling);
    }

    /// Check if the actor has terminated or cancelled.
    pub fn is_terminated(&self) -> bool {
        matches!(
            self.state(),
            ActorState::Terminated | ActorState::Errored(_)
        )
    }
}
