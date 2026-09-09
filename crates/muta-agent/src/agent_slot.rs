use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

use muta_contracts::{AgentPreset, AgentPresetDelegation, MeshAddress};

use crate::agent::Agent;
use crate::mesh::MeshTracker;
use crate::subagent_tool::SubagentRegistry;

/// Manages the active root agent for a session with atomic replacement and subagent reclamation.
///
/// Ensures the single-root invariant per session and guarantees that replacing a root agent
/// actively drains all subordinate subagents (cancelling tokens, sweeping mailboxes, and
/// clearing registry entries) before assigning the new agent. This prevents orphaned subagents
/// and transcript/word-source leaks.
pub struct AgentSlot {
    agent: Arc<Agent>,
    preset: AgentPreset,
    delegation: AgentPresetDelegation,
    session_id: String,
    tracker: Option<Arc<MeshTracker>>,
    subagent_registry: Option<Arc<SubagentRegistry>>,
    subagent_cancels: Arc<Mutex<HashMap<String, CancellationToken>>>,
}

impl AgentSlot {
    /// Create a new AgentSlot for a session.
    pub fn new(
        agent: Arc<Agent>,
        preset: AgentPreset,
        delegation: AgentPresetDelegation,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            agent,
            preset,
            delegation,
            session_id: session_id.into(),
            tracker: None,
            subagent_registry: None,
            subagent_cancels: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Attach mesh tracker and sub-agent registry for coordinated lifecycle management.
    pub fn with_mesh(
        mut self,
        tracker: Arc<MeshTracker>,
        subagent_registry: Arc<SubagentRegistry>,
    ) -> Self {
        self.tracker = Some(tracker);
        self.subagent_registry = Some(subagent_registry);
        self
    }

    /// The active session root agent.
    pub fn agent(&self) -> &Arc<Agent> {
        &self.agent
    }

    /// The active agent preset.
    pub fn preset(&self) -> &AgentPreset {
        &self.preset
    }

    /// The active preset delegation policy.
    pub fn delegation(&self) -> &AgentPresetDelegation {
        &self.delegation
    }

    /// The owning session ID.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Track an in-flight subagent's cancellation token under this root agent.
    pub fn register_subagent_cancel(&self, call_id: impl Into<String>, token: CancellationToken) {
        self.subagent_cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(call_id.into(), token);
    }

    /// Remove a finished subagent's cancellation token.
    pub fn remove_subagent_cancel(&self, call_id: &str) {
        self.subagent_cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(call_id);
    }

    /// Drain and cancel all subordinate subagents currently active under this root agent.
    ///
    /// Cancels all child cancellation tokens, sweeps child mesh mailboxes from the tracker,
    /// and ensures no background execution continues.
    pub fn drain_subagents(&self) -> usize {
        let mut cancels = self
            .subagent_cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let count = cancels.len();
        for (_, token) in cancels.drain() {
            token.cancel();
        }

        let root_addr = MeshAddress::session_root(&self.session_id);
        if let Some(tracker) = &self.tracker {
            let reaped = tracker.reap_children(&root_addr);
            return count.max(reaped);
        }

        count
    }

    /// Replace the session's active root agent and preset atomically.
    ///
    /// Drains all subordinate subagents owned by the previous agent first, preventing
    /// word-source leaks and transcript confusion. Returns the number of subordinate
    /// subagents drained during the transition.
    pub fn replace(
        &mut self,
        new_agent: Arc<Agent>,
        new_preset: AgentPreset,
        new_delegation: AgentPresetDelegation,
    ) -> usize {
        let drained = self.drain_subagents();
        self.agent = new_agent;
        self.preset = new_preset;
        self.delegation = new_delegation;
        drained
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentIdentity;
    use muta_contracts::{AGENT_CODE_ANALYST, AGENT_DEVELOPER};

    struct DummyProvider;
    #[async_trait::async_trait]
    impl muta_contracts::Provider for DummyProvider {
        async fn chat(
            &self,
            _request: muta_contracts::ModelRequest,
        ) -> Result<muta_contracts::ProviderCompletion, muta_contracts::ProviderError> {
            Ok(muta_contracts::ProviderCompletion::message(
                muta_contracts::Message::new(muta_contracts::Role::Assistant, "ok"),
            ))
        }
        async fn stream_chat(
            &self,
            _request: muta_contracts::ModelRequest,
        ) -> Result<
            futures::stream::BoxStream<'static, Result<String, muta_contracts::ProviderError>>,
            muta_contracts::ProviderError,
        > {
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
    async fn agent_slot_replaces_atomically_and_drains_subagents() {
        let agent1 = make_agent("agent-1");
        let tracker = Arc::new(MeshTracker::new());
        let registry = Arc::new(SubagentRegistry::default());

        let mut slot = AgentSlot::new(
            agent1,
            AgentPreset::developer(),
            AGENT_DEVELOPER,
            "session-xyz",
        )
        .with_mesh(tracker.clone(), registry);

        let root_addr = MeshAddress::session_root("session-xyz");
        let subagent_addr1 = MeshAddress::subagent("session-xyz", "subagent-1");
        let subagent_addr2 = MeshAddress::subagent("session-xyz", "subagent-2");

        let mailbox1 = crate::mesh::MeshMailbox::spawn(
            (*tracker).clone(),
            subagent_addr1,
            Some(root_addr.clone()),
        );
        let mailbox2 = crate::mesh::MeshMailbox::spawn(
            (*tracker).clone(),
            subagent_addr2,
            Some(root_addr.clone()),
        );

        slot.register_subagent_cancel("subagent-1", mailbox1.token().clone());
        slot.register_subagent_cancel("subagent-2", mailbox2.token().clone());

        assert!(!mailbox1.token().is_cancelled());
        assert!(!mailbox2.token().is_cancelled());
        assert_eq!(tracker.live_addresses().len(), 2);

        // Replace preset with code analyst
        let agent2 = make_agent("agent-2");
        let drained = slot.replace(agent2, AgentPreset::code_analyst(), AGENT_CODE_ANALYST);

        assert_eq!(drained, 2);
        assert!(mailbox1.token().is_cancelled());
        assert!(mailbox2.token().is_cancelled());
        assert_eq!(tracker.live_addresses().len(), 0);
        assert_eq!(slot.delegation().preset_id, "code_analyst");
    }
}
