use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

use muta_agent::mesh::{MeshListPeersTool, MeshSendTool, MeshTracker};
use muta_agent::{Agent, AgentIdentity};
use muta_contracts::{MeshAddress, MeshEnvelope, MeshMessage, MonitorAction, Tool};

use crate::registry::SessionRegistry;

/// The single Hypervisor station per muta daemon (staffed by a Master agent).
///
/// Responsible for orchestrating sessions, tracking progress across projects,
/// joint debugging / cross-session coordination (联调), and dispatching top-down
/// mesh instructions to session masters.
pub struct Hypervisor {
    agent: Arc<Agent>,
    registry: SessionRegistry,
    tracker: Arc<MeshTracker>,
    address: MeshAddress,
}

impl Hypervisor {
    /// Create the singleton hypervisor station for the daemon.
    pub fn new(
        provider: Arc<dyn muta_contracts::Provider>,
        registry: SessionRegistry,
        tracker: Arc<MeshTracker>,
    ) -> Self {
        let address = MeshAddress::hypervisor("hypervisor");

        let send_tool = Arc::new(MeshSendTool::new((*tracker).clone(), Some(address.clone())));
        let list_peers_tool = Arc::new(MeshListPeersTool::new(
            (*tracker).clone(),
            Some(address.clone()),
        ));
        let list_sessions_tool = Arc::new(HypervisorListSessionsTool::new(registry.clone()));
        let inspect_session_tool = Arc::new(HypervisorInspectSessionTool::new(registry.clone()));
        let instruct_session_tool = Arc::new(HypervisorInstructSessionTool::new(
            tracker.clone(),
            address.clone(),
        ));
        let coordinate_tool = Arc::new(HypervisorCoordinateDebugTool::new(
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
            "hypervisor",
            "the single daemon-level hypervisor for Muta — orchestrating sessions, tracking progress across projects, and coordinating joint debugging and multi-session workflows",
        );

        let agent = Arc::new(Agent::new(provider, tools, identity));
        agent.set_kind(muta_contracts::AgentKind::Master);

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

/// Tool for Hypervisor to list and monitor all hosted sessions across the daemon.
pub struct HypervisorListSessionsTool {
    registry: SessionRegistry,
}

impl HypervisorListSessionsTool {
    pub fn new(registry: SessionRegistry) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for HypervisorListSessionsTool {
    fn name(&self) -> &str {
        "hypervisor_list_sessions"
    }

    fn description(&self) -> &str {
        "List all active and hosted sessions in the daemon with their statuses, message counts, token usage, and working memory digests."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "include_idle": {
                    "type": "boolean",
                    "description": "Whether to include idle/sleeping sessions (default: true)"
                }
            }
        })
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
        let include_idle = args["include_idle"].as_bool().unwrap_or(true);

        let snapshot = self
            .registry
            .monitor_snapshot(MonitorAction {
                watch: false,
                include_idle,
            })
            .await;

        let sessions: Vec<serde_json::Value> = snapshot
            .sessions
            .iter()
            .map(|s| {
                json!({
                    "session_id": s.id,
                    "status": format!("{:?}", s.status),
                    "message_count": s.message_count,
                    "round": s.round,
                    "output_tokens": s.output_tokens,
                    "digest_title": s.digest.as_ref().map(|d| d.title.clone()),
                    "digest_intent": s.digest.as_ref().map(|d| d.intent.clone()),
                    "overview": s.overview
                })
            })
            .collect();

        Ok(json!({
            "total_hosted_sessions": snapshot.sessions.len(),
            "sessions": sessions
        })
        .to_string())
    }
}

/// Tool for Hypervisor to inspect a session's detailed state and messages.
pub struct HypervisorInspectSessionTool {
    registry: SessionRegistry,
}

impl HypervisorInspectSessionTool {
    pub fn new(registry: SessionRegistry) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl Tool for HypervisorInspectSessionTool {
    fn name(&self) -> &str {
        "hypervisor_inspect_session"
    }

    fn description(&self) -> &str {
        "Inspect the detailed state, transcript history, and working memory of a specific hosted session."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "The ID of the session to inspect"
                },
                "tail_messages": {
                    "type": "integer",
                    "description": "Number of most recent messages to retrieve (default: 10)"
                }
            },
            "required": ["session_id"]
        })
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let args: serde_json::Value =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {e}"))?;

        let session_id = args["session_id"]
            .as_str()
            .ok_or("Missing 'session_id' argument")?;
        let tail_messages = args["tail_messages"].as_u64().unwrap_or(10) as usize;

        let host = self
            .registry
            .get(session_id)
            .await
            .ok_or_else(|| format!("Session '{session_id}' not found in registry"))?;

        let history = host.session.full_transcript().await;
        let total_messages = history.len();
        let start_idx = total_messages.saturating_sub(tail_messages);
        let recent_slice = &history[start_idx..];

        let messages: Vec<serde_json::Value> = recent_slice
            .iter()
            .map(|m| {
                json!({
                    "role": format!("{:?}", m.role),
                    "content": m.content
                })
            })
            .collect();

        let (digest, _) = host.session.digest().await;

        Ok(json!({
            "session_id": session_id,
            "total_messages": total_messages,
            "recent_messages_count": messages.len(),
            "recent_messages": messages,
            "digest": digest
        })
        .to_string())
    }
}

/// Tool for Hypervisor to send top-down instructions or guidance to a session Master over the mesh.
pub struct HypervisorInstructSessionTool {
    tracker: Arc<MeshTracker>,
    hypervisor_address: MeshAddress,
}

impl HypervisorInstructSessionTool {
    pub fn new(tracker: Arc<MeshTracker>, hypervisor_address: MeshAddress) -> Self {
        Self {
            tracker,
            hypervisor_address,
        }
    }
}

#[async_trait]
impl Tool for HypervisorInstructSessionTool {
    fn name(&self) -> &str {
        "hypervisor_instruct_session"
    }

    fn description(&self) -> &str {
        "Send top-down instructions or steering guidance from the Hypervisor to a session Master over the agent mesh network."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "The ID of the target session"
                },
                "instruction": {
                    "type": "string",
                    "description": "The directive or guidance for the session Master"
                }
            },
            "required": ["session_id", "instruction"]
        })
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let args: serde_json::Value =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {e}"))?;

        let session_id = args["session_id"]
            .as_str()
            .ok_or("Missing 'session_id' argument")?;
        let instruction = args["instruction"]
            .as_str()
            .ok_or("Missing 'instruction' argument")?;

        let recipient = MeshAddress::master(session_id);
        let envelope = MeshEnvelope::new(
            Some(self.hypervisor_address.clone()),
            recipient,
            MeshMessage::Instruction {
                body: instruction.to_string(),
            },
        );

        let msg_id = envelope.id.clone();
        self.tracker
            .send(envelope)
            .map_err(|e| format!("Failed to send instruction via mesh: {e}"))?;

        Ok(json!({
            "status": "delivered",
            "message_id": msg_id,
            "target_session": session_id
        })
        .to_string())
    }
}

/// Tool for Hypervisor to coordinate joint debugging across multiple sessions (联调).
pub struct HypervisorCoordinateDebugTool {
    registry: SessionRegistry,
    tracker: Arc<MeshTracker>,
    hypervisor_address: MeshAddress,
}

impl HypervisorCoordinateDebugTool {
    pub fn new(
        registry: SessionRegistry,
        tracker: Arc<MeshTracker>,
        hypervisor_address: MeshAddress,
    ) -> Self {
        Self {
            registry,
            tracker,
            hypervisor_address,
        }
    }
}

#[async_trait]
impl Tool for HypervisorCoordinateDebugTool {
    fn name(&self) -> &str {
        "hypervisor_coordinate_debug"
    }

    fn description(&self) -> &str {
        "Coordinate joint debugging (联调) across multiple sessions — aggregating cross-session logs and dispatching alignment directives."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "target_sessions": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional list of session IDs to involve in joint debugging (if omitted, all hosted sessions are considered)"
                },
                "instruction": {
                    "type": "string",
                    "description": "Optional alignment directive to broadcast to all participating session Masters"
                }
            }
        })
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
        let requested_sessions: Option<Vec<String>> =
            args["target_sessions"].as_array().map(|arr| {
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
                    Some(self.hypervisor_address.clone()),
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
    use muta_contracts::{MasterPreset, MeshStation, Message, ModelRequest, Provider, Role};

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
        async fn chat(
            &self,
            _request: ModelRequest,
        ) -> Result<muta_contracts::ProviderCompletion, muta_contracts::ProviderError> {
            Ok(muta_contracts::ProviderCompletion::message(Message::new(
                Role::Assistant,
                "hypervisor response",
            )))
        }
        async fn stream_chat(
            &self,
            _request: ModelRequest,
        ) -> Result<
            futures::stream::BoxStream<'static, Result<String, muta_contracts::ProviderError>>,
            muta_contracts::ProviderError,
        > {
            use futures::stream;
            Ok(Box::pin(stream::once(async {
                Ok("hypervisor response".to_string())
            })))
        }
    }

    #[tokio::test]
    async fn hypervisor_construction_and_tools() {
        let params = HostParams {
            identity: AgentIdentity::new("muta", "coding"),
            master: MasterPreset::developer(),
            ui: Arc::new(DummyUi),
        };
        let registry = SessionRegistry::new(params);
        let tracker = Arc::new(MeshTracker::new());
        let provider = Arc::new(DummyProvider);

        let hypervisor = Hypervisor::new(provider, registry.clone(), tracker.clone());

        assert_eq!(hypervisor.address().station, MeshStation::Hypervisor);
        assert_eq!(hypervisor.address().agent, "hypervisor");

        // List sessions tool
        let list_tool = HypervisorListSessionsTool::new(registry.clone());
        let list_out = list_tool.call("{}").await.unwrap();
        assert!(list_out.contains("total_hosted_sessions"));

        // Joint coordinate debug tool
        let coord_tool = HypervisorCoordinateDebugTool::new(
            registry.clone(),
            tracker.clone(),
            hypervisor.address().clone(),
        );
        let coord_out = coord_tool.call("{}").await.unwrap();
        assert!(coord_out.contains("participating_sessions_count"));
    }
}
