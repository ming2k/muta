use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

use muta_agent::mesh::{MeshListPeersTool, MeshSendTool, MeshTracker};
use muta_agent::{Agent, AgentIdentity};
use muta_contracts::{MeshAddress, MeshEnvelope, MeshMessage, MonitorAction, Tool};

use crate::registry::SessionRegistry;

/// The single Supervisor agent instance per muta daemon (Tier 0).
///
/// Responsible for orchestrating sessions, tracking progress across projects,
/// joint debugging / cross-session coordination (联调), and dispatching top-down
/// mesh instructions to session masters.
pub struct Supervisor {
    agent: Arc<Agent>,
    registry: SessionRegistry,
    tracker: Arc<MeshTracker>,
    address: MeshAddress,
}

impl Supervisor {
    /// Create the singleton supervisor for the daemon.
    pub fn new(
        provider: Arc<dyn muta_contracts::Provider>,
        registry: SessionRegistry,
        tracker: Arc<MeshTracker>,
    ) -> Self {
        let address = MeshAddress::supervisor("supervisor");

        let send_tool = Arc::new(MeshSendTool::new((*tracker).clone(), Some(address.clone())));
        let list_peers_tool = Arc::new(MeshListPeersTool::new(
            (*tracker).clone(),
            Some(address.clone()),
        ));
        let list_sessions_tool = Arc::new(SupervisorListSessionsTool::new(registry.clone()));
        let inspect_session_tool = Arc::new(SupervisorInspectSessionTool::new(registry.clone()));
        let instruct_session_tool = Arc::new(SupervisorInstructSessionTool::new(
            tracker.clone(),
            address.clone(),
        ));
        let coordinate_tool = Arc::new(SupervisorCoordinateDebugTool::new(
            registry.clone(),
            tracker.clone(),
            address.clone(),
        ));

        let tools: Vec<Arc<dyn Tool>> = vec![
            send_tool,
            list_peers_tool,
            list_sessions_tool,
            inspect_session_tool,
            instruct_session_tool,
            coordinate_tool,
        ];

        let identity = AgentIdentity::new(
            "supervisor",
            "the single top-level supervisor for Muta — orchestrating sessions, tracking progress across projects, and coordinating joint debugging and multi-session workflows",
        );

        let agent = Arc::new(Agent::new(provider, tools, identity));
        agent.set_tier(muta_contracts::AgentTier::Supervisor);

        Self {
            agent,
            registry,
            tracker,
            address,
        }
    }

    pub fn agent(&self) -> &Arc<Agent> {
        &self.agent
    }

    pub fn address(&self) -> &MeshAddress {
        &self.address
    }

    pub fn tracker(&self) -> &Arc<MeshTracker> {
        &self.tracker
    }

    pub fn registry(&self) -> &SessionRegistry {
        &self.registry
    }
}

/// Tool for Supervisor to list and monitor all hosted sessions across the daemon.
pub struct SupervisorListSessionsTool {
    registry: SessionRegistry,
}

impl SupervisorListSessionsTool {
    pub fn new(registry: SessionRegistry) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for SupervisorListSessionsTool {
    fn name(&self) -> &str {
        "supervisor_list_sessions"
    }

    fn description(&self) -> &str {
        "List all hosted and active sessions in the Muta daemon, including session IDs, project roots, status, token usage, and active masters."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {}
        })
    }

    async fn call(&self, _arguments: &str) -> Result<String, String> {
        let snapshot = self
            .registry
            .monitor_snapshot(MonitorAction {
                watch: false,
                include_idle: true,
            })
            .await;
        let sessions: Vec<serde_json::Value> = snapshot
            .sessions
            .iter()
            .map(|s| {
                json!({
                    "session_id": s.id,
                    "overview": s.overview,
                    "project_root": s.project_root,
                    "status": s.status.as_str(),
                    "output_tokens": s.output_tokens,
                    "round": s.round
                })
            })
            .collect();

        Ok(json!({
            "total_hosted_sessions": sessions.len(),
            "sessions": sessions
        })
        .to_string())
    }
}

/// Tool for Supervisor to inspect a session's detailed state and messages.
pub struct SupervisorInspectSessionTool {
    registry: SessionRegistry,
}

impl SupervisorInspectSessionTool {
    pub fn new(registry: SessionRegistry) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for SupervisorInspectSessionTool {
    fn name(&self) -> &str {
        "supervisor_inspect_session"
    }

    fn description(&self) -> &str {
        "Inspect a session in detail: status, recent turns, activity, and token usage."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "The ID of the session to inspect"
                }
            },
            "required": ["session_id"]
        })
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let args: serde_json::Value =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {e}"))?;
        let session_id = args["session_id"].as_str().ok_or("Missing 'session_id'")?;

        let snapshot = self
            .registry
            .monitor_snapshot(MonitorAction {
                watch: false,
                include_idle: true,
            })
            .await;
        let session = snapshot
            .sessions
            .into_iter()
            .find(|s| s.id == session_id)
            .ok_or_else(|| format!("Session '{session_id}' not found"))?;

        Ok(json!({
            "session_id": session.id,
            "overview": session.overview,
            "project_root": session.project_root,
            "status": session.status.as_str(),
            "output_tokens": session.output_tokens,
            "round": session.round,
            "activity": session.activity
        })
        .to_string())
    }
}

/// Tool for Supervisor to send top-down instructions or guidance to a session Master over the mesh.
pub struct SupervisorInstructSessionTool {
    tracker: Arc<MeshTracker>,
    supervisor_address: MeshAddress,
}

impl SupervisorInstructSessionTool {
    pub fn new(tracker: Arc<MeshTracker>, supervisor_address: MeshAddress) -> Self {
        Self {
            tracker,
            supervisor_address,
        }
    }
}

#[async_trait]
impl Tool for SupervisorInstructSessionTool {
    fn name(&self) -> &str {
        "supervisor_instruct_session"
    }

    fn description(&self) -> &str {
        "Send top-down instruction or guidance directly to a session's Master agent over the mesh."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "The target session ID whose Master receives the instruction"
                },
                "instruction": {
                    "type": "string",
                    "description": "The top-down guidance or task instruction"
                }
            },
            "required": ["session_id", "instruction"]
        })
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let args: serde_json::Value =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {e}"))?;
        let session_id = args["session_id"].as_str().ok_or("Missing 'session_id'")?;
        let instruction = args["instruction"]
            .as_str()
            .ok_or("Missing 'instruction'")?;

        let recipient = MeshAddress::master(session_id);
        let envelope = MeshEnvelope::new(
            Some(self.supervisor_address.clone()),
            recipient.clone(),
            MeshMessage::Instruction {
                body: instruction.to_string(),
            },
        );

        let id = envelope.id.clone();
        self.tracker
            .send(envelope)
            .map_err(|e| format!("Failed to deliver instruction to session master: {e}"))?;

        Ok(json!({
            "status": "instructed",
            "envelope_id": id,
            "session_id": session_id,
            "recipient": recipient.display()
        })
        .to_string())
    }
}

/// Tool for Supervisor to coordinate joint debugging across multiple sessions (联调).
pub struct SupervisorCoordinateDebugTool {
    registry: SessionRegistry,
    tracker: Arc<MeshTracker>,
    supervisor_address: MeshAddress,
}

impl SupervisorCoordinateDebugTool {
    pub fn new(
        registry: SessionRegistry,
        tracker: Arc<MeshTracker>,
        supervisor_address: MeshAddress,
    ) -> Self {
        Self {
            registry,
            tracker,
            supervisor_address,
        }
    }
}

#[async_trait]
impl Tool for SupervisorCoordinateDebugTool {
    fn name(&self) -> &str {
        "supervisor_coordinate_debug"
    }

    fn description(&self) -> &str {
        "Coordinate joint debugging (联调) across multiple sessions: select participants, align progress, and dispatch coordination instructions."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "session_ids": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "List of session IDs involved in joint debugging. Empty means all hosted sessions."
                },
                "instruction": {
                    "type": "string",
                    "description": "Optional joint debugging instruction to broadcast to all participating masters."
                }
            }
        })
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));

        let requested_sessions: Option<Vec<String>> = args["session_ids"].as_array().map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        });

        let instruction = args["instruction"].as_str();

        let snapshot = self
            .registry
            .monitor_snapshot(MonitorAction {
                watch: false,
                include_idle: true,
            })
            .await;
        let participating_sessions: Vec<_> = snapshot
            .sessions
            .iter()
            .filter(|s| {
                if let Some(target) = &requested_sessions {
                    target.contains(&s.id)
                } else {
                    true
                }
            })
            .collect();

        let mut dispatched_instructions = 0;
        if let Some(instr) = instruction {
            for session in &participating_sessions {
                let recipient = MeshAddress::master(&session.id);
                let envelope = MeshEnvelope::new(
                    Some(self.supervisor_address.clone()),
                    recipient,
                    MeshMessage::Instruction {
                        body: format!("[Joint Debugging / 联调]: {instr}"),
                    },
                );
                if self.tracker.send(envelope).is_ok() {
                    dispatched_instructions += 1;
                }
            }
        }

        Ok(json!({
            "participating_sessions_count": participating_sessions.len(),
            "instructions_dispatched": dispatched_instructions,
            "status": "coordinated"
        })
        .to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::HostParams;
    use crate::ui_bridge::{CopyOutcome, UiBridge};
    use muta_contracts::{AgentTier, MasterPreset, Message, ModelRequest, Provider, Role};

    struct DummyUi;
    #[async_trait::async_trait]
    impl UiBridge for DummyUi {
        async fn copy_to_clipboard(&self, _text: &str) -> Result<CopyOutcome, String> {
            Ok(CopyOutcome::Native)
        }
    }

    struct DummyProvider;
    #[async_trait::async_trait]
    impl Provider for DummyProvider {
        async fn chat(&self, _request: ModelRequest) -> Result<muta_contracts::ProviderCompletion, String> {
            Ok(muta_contracts::ProviderCompletion::message(Message::new(
                Role::Assistant,
                "supervisor response",
            )))
        }
        async fn stream_chat(
            &self,
            _request: ModelRequest,
        ) -> Result<futures::stream::BoxStream<'static, Result<String, String>>, String> {
            use futures::stream;
            Ok(Box::pin(stream::once(async {
                Ok("supervisor response".to_string())
            })))
        }
    }

    #[tokio::test]
    async fn supervisor_construction_and_tools() {
        let params = HostParams {
            identity: AgentIdentity::new("muta", "coding"),
            master: MasterPreset::developer(),
            ui: Arc::new(DummyUi),
        };
        let registry = SessionRegistry::new(params);
        let tracker = Arc::new(MeshTracker::new());
        let provider = Arc::new(DummyProvider);

        let supervisor = Supervisor::new(provider, registry.clone(), tracker.clone());

        assert_eq!(supervisor.address().tier, AgentTier::Supervisor);
        assert_eq!(supervisor.address().agent, "supervisor");

        // List sessions tool
        let list_tool = SupervisorListSessionsTool::new(registry.clone());
        let list_out = list_tool.call("{}").await.unwrap();
        assert!(list_out.contains("total_hosted_sessions"));

        // Joint coordinate debug tool
        let coord_tool = SupervisorCoordinateDebugTool::new(
            registry.clone(),
            tracker.clone(),
            supervisor.address().clone(),
        );
        let coord_out = coord_tool.call("{}").await.unwrap();
        assert!(coord_out.contains("participating_sessions_count"));
    }
}
