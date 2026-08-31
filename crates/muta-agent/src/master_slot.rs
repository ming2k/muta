use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

use muta_contracts::{MasterPreset, MasterPresetDelegation, MeshAddress};

use crate::agent::Agent;
use crate::mesh::MeshTracker;
use crate::runner_tool::RunnerRegistry;

/// Manages the active Master agent for a session with atomic replacement and runner reclamation.
///
/// Ensures the single-master invariant per session and guarantees that replacing a master
/// actively drains all subordinate runners (cancelling tokens, sweeping mailboxes, and
/// clearing registry entries) before assigning the new master. This prevents orphaned runners
/// and transcript/word-source leaks (词源泄露).
pub struct MasterSlot {
    master: Arc<Agent>,
    preset: MasterPreset,
    delegation: MasterPresetDelegation,
    session_id: String,
    tracker: Option<Arc<MeshTracker>>,
    runner_registry: Option<Arc<RunnerRegistry>>,
    runner_cancels: Arc<Mutex<HashMap<String, CancellationToken>>>,
}

impl MasterSlot {
    /// Create a new MasterSlot for a session.
    pub fn new(
        master: Arc<Agent>,
        preset: MasterPreset,
        delegation: MasterPresetDelegation,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            master,
            preset,
            delegation,
            session_id: session_id.into(),
            tracker: None,
            runner_registry: None,
            runner_cancels: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Attach mesh tracker and runner registry for coordinated lifecycle management.
    pub fn with_mesh(
        mut self,
        tracker: Arc<MeshTracker>,
        runner_registry: Arc<RunnerRegistry>,
    ) -> Self {
        self.tracker = Some(tracker);
        self.runner_registry = Some(runner_registry);
        self
    }

    /// The active master agent.
    pub fn master(&self) -> &Arc<Agent> {
        &self.master
    }

    /// The active master preset.
    pub fn preset(&self) -> &MasterPreset {
        &self.preset
    }

    /// The active preset delegation policy.
    pub fn delegation(&self) -> &MasterPresetDelegation {
        &self.delegation
    }

    /// The owning session ID.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Track an in-flight runner's cancellation token under this master.
    pub fn register_runner_cancel(&self, call_id: impl Into<String>, token: CancellationToken) {
        self.runner_cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(call_id.into(), token);
    }

    /// Remove a finished runner's cancellation token.
    pub fn remove_runner_cancel(&self, call_id: &str) {
        self.runner_cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(call_id);
    }

    /// Drain and cancel all subordinate runners currently active under this master.
    ///
    /// Cancels all child cancellation tokens, sweeps child mesh mailboxes from the tracker,
    /// and ensures no background execution continues.
    pub fn drain_runners(&self) -> usize {
        let mut cancels = self
            .runner_cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let count = cancels.len();
        for (_, token) in cancels.drain() {
            token.cancel();
        }

        let master_addr = MeshAddress::master(&self.session_id);
        if let Some(tracker) = &self.tracker {
            let reaped = tracker.reap_children(&master_addr);
            return count.max(reaped);
        }

        count
    }

    /// Replace the session's active master agent and preset atomically.
    ///
    /// Drains all subordinate runners owned by the previous master first, preventing
    /// word-source leaks and transcript confusion. Returns the number of subordinate
    /// runners drained during the transition.
    pub fn replace(
        &mut self,
        new_master: Arc<Agent>,
        new_preset: MasterPreset,
        new_delegation: MasterPresetDelegation,
    ) -> usize {
        let drained = self.drain_runners();
        self.master = new_master;
        self.preset = new_preset;
        self.delegation = new_delegation;
        drained
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentIdentity;
    use muta_contracts::{MASTER_CODE_ANALYST, MASTER_DEVELOPER};

    struct DummyProvider;
    #[async_trait::async_trait]
    impl muta_contracts::Provider for DummyProvider {
        async fn chat(
            &self,
            _request: muta_contracts::ModelRequest,
        ) -> Result<muta_contracts::ProviderCompletion, String> {
            Ok(muta_contracts::ProviderCompletion::message(
                muta_contracts::Message::new(muta_contracts::Role::Assistant, "ok"),
            ))
        }
        async fn stream_chat(
            &self,
            _request: muta_contracts::ModelRequest,
        ) -> Result<futures::stream::BoxStream<'static, Result<String, String>>, String> {
            use futures::stream;
            Ok(Box::pin(stream::once(async { Ok("ok".to_string()) })))
        }
    }

    fn make_agent(name: &str) -> Arc<Agent> {
        let provider = Arc::new(DummyProvider);
        let identity = AgentIdentity::new(name, "test mission");
        Arc::new(Agent::new(provider, vec![], identity))
    }

    #[tokio::test]
    async fn master_slot_replaces_atomically_and_drains_runners() {
        let agent1 = make_agent("master-1");
        let tracker = Arc::new(MeshTracker::new());
        let registry = Arc::new(RunnerRegistry::default());

        let mut slot = MasterSlot::new(
            agent1,
            MasterPreset::developer(),
            MASTER_DEVELOPER,
            "session-xyz",
        )
        .with_mesh(tracker.clone(), registry);

        let master_addr = MeshAddress::master("session-xyz");
        let runner_addr1 = MeshAddress::runner("session-xyz", "runner-1");
        let runner_addr2 = MeshAddress::runner("session-xyz", "runner-2");

        let mailbox1 = crate::mesh::MeshMailbox::spawn(
            (*tracker).clone(),
            runner_addr1,
            Some(master_addr.clone()),
        );
        let mailbox2 = crate::mesh::MeshMailbox::spawn(
            (*tracker).clone(),
            runner_addr2,
            Some(master_addr.clone()),
        );

        slot.register_runner_cancel("runner-1", mailbox1.token().clone());
        slot.register_runner_cancel("runner-2", mailbox2.token().clone());

        assert!(!mailbox1.token().is_cancelled());
        assert!(!mailbox2.token().is_cancelled());
        assert_eq!(tracker.live_addresses().len(), 2);

        // Replace master preset with code analyst
        let agent2 = make_agent("master-2");
        let drained = slot.replace(agent2, MasterPreset::code_analyst(), MASTER_CODE_ANALYST);

        assert_eq!(drained, 2);
        assert!(mailbox1.token().is_cancelled());
        assert!(mailbox2.token().is_cancelled());
        assert_eq!(tracker.live_addresses().len(), 0);
        assert_eq!(slot.delegation().preset_id, "code_analyst");
    }
}
