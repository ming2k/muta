use async_trait::async_trait;
use serde_json::json;
use std::sync::{Arc, Mutex};

use muta_contracts::{AgentTier, MeshAddress, MeshEnvelope, MeshMessage, Tool};

use super::MeshTracker;

/// Tool allowing agents to send lawful messages over the three-tier mesh network.
pub struct MeshSendTool {
    tracker: MeshTracker,
    sender_address: Arc<Mutex<Option<MeshAddress>>>,
}

impl MeshSendTool {
    pub fn new(tracker: MeshTracker, sender: Option<MeshAddress>) -> Self {
        Self {
            tracker,
            sender_address: Arc::new(Mutex::new(sender)),
        }
    }

    pub fn bind_sender(&self, address: MeshAddress) {
        *self
            .sender_address
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(address);
    }
}

#[async_trait]
impl Tool for MeshSendTool {
    fn name(&self) -> &str {
        "mesh_send"
    }

    fn description(&self) -> &str {
        "Send a message across the agent mesh network (top-down instruction, bottom-up report, progress note, peer note, or ping) to another agent in the hierarchy."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "recipient_tier": {
                    "type": "string",
                    "enum": ["supervisor", "master", "runner"],
                    "description": "Tier of the recipient agent"
                },
                "recipient_session": {
                    "type": "string",
                    "description": "Session ID of the recipient"
                },
                "recipient_agent": {
                    "type": "string",
                    "description": "Agent ID of the recipient within the session"
                },
                "message_type": {
                    "type": "string",
                    "enum": ["instruction", "report", "progress_note", "peer_note", "ping"],
                    "description": "Type of message to send"
                },
                "body": {
                    "type": "string",
                    "description": "Content of the message"
                }
            },
            "required": ["recipient_tier", "recipient_session", "recipient_agent", "message_type", "body"]
        })
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let args: serde_json::Value =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {e}"))?;

        let tier_str = args["recipient_tier"]
            .as_str()
            .ok_or("Missing 'recipient_tier'")?;
        let session = args["recipient_session"]
            .as_str()
            .ok_or("Missing 'recipient_session'")?;
        let agent = args["recipient_agent"]
            .as_str()
            .ok_or("Missing 'recipient_agent'")?;
        let message_type = args["message_type"]
            .as_str()
            .ok_or("Missing 'message_type'")?;
        let body = args["body"].as_str().ok_or("Missing 'body'")?;

        let tier = match tier_str {
            "supervisor" => AgentTier::Supervisor,
            "master" => AgentTier::Master,
            "runner" => AgentTier::Runner,
            other => return Err(format!("Unknown recipient_tier: '{other}'")),
        };

        let recipient = MeshAddress::new(tier, session, agent);
        let message = match message_type {
            "instruction" => MeshMessage::Instruction {
                body: body.to_string(),
            },
            "report" => MeshMessage::Report {
                body: body.to_string(),
            },
            "progress_note" => MeshMessage::ProgressNote {
                body: body.to_string(),
            },
            "peer_note" => MeshMessage::PeerNote {
                body: body.to_string(),
            },
            "ping" => MeshMessage::Ping {
                nonce: fastrand::u64(..),
            },
            other => return Err(format!("Unknown message_type: '{other}'")),
        };

        let sender = self
            .sender_address
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();

        let envelope = MeshEnvelope::new(sender, recipient.clone(), message);
        let envelope_id = envelope.id.clone();

        self.tracker
            .send(envelope)
            .map_err(|e| format!("Failed to deliver mesh envelope: {e}"))?;

        Ok(json!({
            "status": "delivered",
            "envelope_id": envelope_id,
            "recipient": recipient.display()
        })
        .to_string())
    }
}

/// Tool allowing agents to discover active peers and subordinates in the BitTorrent-style mesh tracker.
pub struct MeshListPeersTool {
    tracker: MeshTracker,
    sender_address: Arc<Mutex<Option<MeshAddress>>>,
}

impl MeshListPeersTool {
    pub fn new(tracker: MeshTracker, sender: Option<MeshAddress>) -> Self {
        Self {
            tracker,
            sender_address: Arc::new(Mutex::new(sender)),
        }
    }

    pub fn bind_sender(&self, address: MeshAddress) {
        *self
            .sender_address
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(address);
    }
}

#[async_trait]
impl Tool for MeshListPeersTool {
    fn name(&self) -> &str {
        "mesh_list_peers"
    }

    fn description(&self) -> &str {
        "Discover active agents registered in the mesh tracker. Allows finding same-tier peers (e.g. other masters), subordinate runners, or all registered endpoints."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "scope": {
                    "type": "string",
                    "enum": ["same_tier", "subordinates", "masters", "all"],
                    "description": "Filter scope for discovery (default: same_tier)"
                }
            }
        })
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
        let scope = args["scope"].as_str().unwrap_or("same_tier");

        let sender = self
            .sender_address
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();

        let addresses = match scope {
            "all" => self.tracker.live_addresses(),
            "masters" => self.tracker.peers_by_tier(AgentTier::Master),
            "subordinates" => {
                if let Some(s) = sender {
                    if s.tier == AgentTier::Master {
                        self.tracker.peers(AgentTier::Runner, &s.session)
                    } else if s.tier == AgentTier::Supervisor {
                        self.tracker.peers_by_tier(AgentTier::Master)
                    } else {
                        vec![]
                    }
                } else {
                    vec![]
                }
            }
            _ => {
                // "same_tier"
                if let Some(s) = sender {
                    if s.tier == AgentTier::Master {
                        self.tracker.peers_by_tier(AgentTier::Master)
                    } else {
                        self.tracker.peers(s.tier, &s.session)
                    }
                } else {
                    self.tracker.live_addresses()
                }
            }
        };

        let result: Vec<serde_json::Value> = addresses
            .into_iter()
            .map(|a| {
                json!({
                    "tier": a.tier.label(),
                    "session": a.session,
                    "agent": a.agent,
                    "address": a.display()
                })
            })
            .collect();

        Ok(json!({
            "count": result.len(),
            "peers": result
        })
        .to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::MeshMailbox;

    #[tokio::test]
    async fn mesh_tools_discovery_and_sending() {
        let tracker = MeshTracker::new();

        let master_a = MeshAddress::master("session-a");
        let master_b = MeshAddress::master("session-b");
        let runner_a1 = MeshAddress::runner("session-a", "runner-1");

        let _mb_a = MeshMailbox::spawn(tracker.clone(), master_a.clone(), None);
        let mut mb_b = MeshMailbox::spawn(tracker.clone(), master_b.clone(), None);
        let mut mb_r =
            MeshMailbox::spawn(tracker.clone(), runner_a1.clone(), Some(master_a.clone()));

        let send_tool = MeshSendTool::new(tracker.clone(), Some(master_a.clone()));
        let list_tool = MeshListPeersTool::new(tracker.clone(), Some(master_a.clone()));

        // Discover masters (peers)
        let peers_json = list_tool.call(r#"{"scope":"masters"}"#).await.unwrap();
        assert!(peers_json.contains("session-a"));
        assert!(peers_json.contains("session-b"));

        // Master A sends peer note to Master B
        let send_peer = send_tool
            .call(
                r#"{
            "recipient_tier": "master",
            "recipient_session": "session-b",
            "recipient_agent": "session-b",
            "message_type": "peer_note",
            "body": "hello peer master"
        }"#,
            )
            .await
            .unwrap();
        assert!(send_peer.contains("delivered"));

        let received_by_b = mb_b
            .recv()
            .await
            .expect("master b should receive peer note");
        assert_eq!(
            received_by_b.message,
            MeshMessage::PeerNote {
                body: "hello peer master".to_string()
            }
        );

        // Master A sends instruction to subordinate runner
        let send_instr = send_tool
            .call(
                r#"{
            "recipient_tier": "runner",
            "recipient_session": "session-a",
            "recipient_agent": "runner-1",
            "message_type": "instruction",
            "body": "search the code"
        }"#,
            )
            .await
            .unwrap();
        assert!(send_instr.contains("delivered"));

        let received_by_r = mb_r
            .recv()
            .await
            .expect("runner should receive instruction");
        assert_eq!(
            received_by_r.message,
            MeshMessage::Instruction {
                body: "search the code".to_string()
            }
        );
    }
}
