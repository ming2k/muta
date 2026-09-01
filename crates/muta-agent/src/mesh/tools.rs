use async_trait::async_trait;
use serde_json::json;
use std::sync::{Arc, Mutex};

use muta_contracts::{MeshAddress, MeshEnvelope, MeshMessage, MeshStation, Tool};

use super::MeshTracker;

/// Tool allowing agents to send lawful messages over the mesh network.
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
                "recipient_station": {
                    "type": "string",
                    "enum": ["hypervisor", "master", "runner", "session", "subtask"],
                    "description": "Station of the recipient agent"
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
            "required": ["recipient_station", "recipient_session", "recipient_agent", "message_type", "body"]
        })
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let args: serde_json::Value =
            serde_json::from_str(arguments).map_err(|e| format!("Invalid JSON: {e}"))?;

        let station_str = args["recipient_station"]
            .as_str()
            .or_else(|| args["recipient_tier"].as_str())
            .ok_or("Missing 'recipient_station'")?;
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

        let station = match station_str {
            "hypervisor" | "supervisor" => MeshStation::Hypervisor,
            "session" | "master" => MeshStation::Session,
            "subtask" | "runner" => MeshStation::Subtask,
            other => return Err(format!("Unknown recipient_station: '{other}'")),
        };

        let recipient = MeshAddress::new(station, session, agent);
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

        let envelope = MeshEnvelope::new(sender, recipient, message);
        let msg_id = envelope.id.clone();

        self.tracker
            .send(envelope)
            .map_err(|e| format!("Mesh send failed: {e}"))?;

        Ok(json!({
            "status": "delivered",
            "message_id": msg_id
        })
        .to_string())
    }
}

/// Tool for discovering other agents registered in the daemon's mesh.
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
        "Discover active agents registered in the mesh tracker. Allows finding same-station peers (e.g. other masters), subordinate runners, or all registered endpoints."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "scope": {
                    "type": "string",
                    "enum": ["same_station", "subordinates", "masters", "all"],
                    "description": "Filter scope for discovery (default: same_station)"
                }
            }
        })
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
        let scope = args["scope"].as_str().unwrap_or("same_station");

        let sender = self
            .sender_address
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();

        let addresses = match scope {
            "all" => self.tracker.live_addresses(),
            "masters" => self.tracker.peers_by_station(MeshStation::Session),
            "subordinates" => {
                if let Some(s) = sender {
                    if s.station == MeshStation::Session {
                        self.tracker.peers(MeshStation::Subtask, &s.session)
                    } else if s.station == MeshStation::Hypervisor {
                        self.tracker.peers_by_station(MeshStation::Session)
                    } else {
                        vec![]
                    }
                } else {
                    vec![]
                }
            }
            _ => {
                // "same_station" / "same_tier"
                if let Some(s) = sender {
                    if s.station == MeshStation::Session {
                        self.tracker.peers_by_station(MeshStation::Session)
                    } else {
                        self.tracker.peers(s.station, &s.session)
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
                    "station": a.station.label(),
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
    async fn mesh_send_tool_success() {
        let tracker = MeshTracker::new();
        let master_addr = MeshAddress::master("session_1");
        let mut mailbox = MeshMailbox::spawn(tracker.clone(), master_addr.clone(), None);

        let tool = MeshSendTool::new(tracker.clone(), Some(MeshAddress::hypervisor("daemon")));

        let args = json!({
            "recipient_station": "master",
            "recipient_session": "session_1",
            "recipient_agent": "session_1",
            "message_type": "instruction",
            "body": "test instruction"
        })
        .to_string();

        let res = tool.call(&args).await.unwrap();
        assert!(res.contains("delivered"));

        let env = mailbox.recv().await.unwrap();
        assert_eq!(
            env.message,
            MeshMessage::Instruction {
                body: "test instruction".to_string()
            }
        );
    }

    #[tokio::test]
    async fn mesh_list_peers_tool_filtering() {
        let tracker = MeshTracker::new();
        let m1 = MeshAddress::master("s1");
        let m2 = MeshAddress::master("s2");
        let r1 = MeshAddress::runner("s1", "r1");

        let _mb1 = MeshMailbox::spawn(tracker.clone(), m1.clone(), None);
        let _mb2 = MeshMailbox::spawn(tracker.clone(), m2.clone(), None);
        let _mbr1 = MeshMailbox::spawn(tracker.clone(), r1.clone(), Some(m1.clone()));

        let tool = MeshListPeersTool::new(tracker.clone(), Some(m1.clone()));

        let res_str = tool.call(&json!({"scope": "masters"}).to_string()).await.unwrap();
        let res: serde_json::Value = serde_json::from_str(&res_str).unwrap();
        assert_eq!(res["count"], 2);

        let res_sub_str = tool.call(&json!({"scope": "subordinates"}).to_string()).await.unwrap();
        let res_sub: serde_json::Value = serde_json::from_str(&res_sub_str).unwrap();
        assert_eq!(res_sub["count"], 1);
        assert_eq!(res_sub["peers"][0]["agent"], "r1");
    }
}
