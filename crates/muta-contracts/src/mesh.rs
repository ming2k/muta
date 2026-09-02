//! The agent mesh: transport-neutral envelopes, addresses, and the
//! tracker contract (ADR-0167).
//!
//! ## Why this exists
//!
//! The station hierarchy needs three communication directions:
//!
//! - **top-down** (`Instruction`): an elder station steers a subordinate.
//! - **bottom-up** (`Report`): a subordinate answers its elder station.
//! - **peer** (`PeerNote`): same-station agents exchange notes without going
//!   through their parent's conversation.
//!
//! The design follows a BitTorrent-style **tracker**: one registry per daemon
//! knows every live [`MeshAddress`]; agents send by address. The in-process
//! transport is an unbounded mpsc per agent, but the contract types here are
//! transport-neutral — a future socket transport carries the same envelopes
//! without touching the agent loop.
//!
//! Direction is **governed by station hierarchy**: [`MeshMessage::route`] encodes who may
//! send what to whom, so a runner can never be commanded by a sibling, and a
//! master never receives a report from a runner it does not own (ownership
//! is checked by the receiving side against the sender's parent identity).

use serde::{Deserialize, Serialize};

use crate::agent_kind::MeshStation;

/// A globally unique, sortable address in the daemon's mesh.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MeshAddress {
    /// Station of the addressed agent — the first routing hop.
    pub station: MeshStation,
    /// Owning session id (runners inherit their master's session).
    pub session: String,
    /// Agent instance id — unique within `(station, session)`.
    pub agent: String,
}

impl MeshAddress {
    pub fn new(station: MeshStation, session: impl Into<String>, agent: impl Into<String>) -> Self {
        Self {
            station,
            session: session.into(),
            agent: agent.into(),
        }
    }

    /// Hypervisor address for the daemon.
    pub fn hypervisor(agent_id: impl Into<String>) -> Self {
        Self::new(MeshStation::Hypervisor, "daemon", agent_id)
    }

    /// Master address for a given session.
    pub fn master(session: impl Into<String>) -> Self {
        let s = session.into();
        Self::new(MeshStation::Session, s.clone(), s)
    }

    /// Runner address for a subordinate within a session.
    pub fn runner(session: impl Into<String>, agent: impl Into<String>) -> Self {
        Self::new(MeshStation::Subtask, session, agent)
    }

    /// The address of this agent's parent in the mesh (same session, one
    /// station up). `None` for the hypervisor — the root has no parent.
    pub fn parent(&self) -> Option<MeshAddress> {
        let station = self.station.parent_station()?;
        Some(Self {
            station,
            session: self.session.clone(),
            agent: self.session.clone(),
        })
    }

    /// Compact debug form `station:session:agent`, used in logs and UI chips.
    pub fn display(&self) -> String {
        format!("{}:{}:{}", self.station.label(), self.session, self.agent)
    }
}

impl std::fmt::Display for MeshAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.display())
    }
}

/// A single mesh message payload. Delivery semantics per variant:
/// acknowledged (`Instruction`/`Report`), fire-and-forget (`ProgressNote`,
/// `PeerNote`), lifecycle (`RunnerEol`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MeshMessage {
    /// Liveness probe. Fire-and-forget; answered by `Pong`.
    Ping { nonce: u64 },
    /// Liveness answer to a `Ping` carrying the same nonce.
    Pong { nonce: u64 },
    /// Elder → subordinate steering. Acknowledged by `InstructionAck`.
    Instruction { body: String },
    /// Subordinate's acknowledgement of an `Instruction`.
    InstructionAck { instruction_id: String },
    /// Subordinate → elder answer. Acknowledged by `ReportAck`.
    Report { body: String },
    /// Elder's acknowledgement of a `Report`.
    ReportAck { report_id: String },
    /// Subordinate → elder fire-and-forget progress note.
    ProgressNote { body: String },
    /// Same-station fire-and-forget note; never crosses station boundaries.
    PeerNote { body: String },
    /// A runner announcing graceful end-of-life to its master.
    RunnerEol { final_note: Option<String> },
}

impl MeshMessage {
    /// The direction class of this message: who may send it to whom.
    pub fn route(&self) -> MeshRoute {
        match self {
            MeshMessage::Instruction { .. } | MeshMessage::InstructionAck { .. } => {
                MeshRoute::Vertical
            }
            MeshMessage::Report { .. } | MeshMessage::ReportAck { .. } => MeshRoute::Vertical,
            MeshMessage::ProgressNote { .. } | MeshMessage::RunnerEol { .. } => MeshRoute::UpOnly,
            MeshMessage::PeerNote { .. } => MeshRoute::Peer,
            MeshMessage::Ping { .. } | MeshMessage::Pong { .. } => MeshRoute::Any,
        }
    }

    /// Whether an agent at station `sender` may lawfully emit this message to an
    /// agent at station `recipient`.
    ///
    /// Rules, by route class:
    /// - `Instruction` (elder → subordinate) and `Report` (subordinate →
    ///   elder) are **strictly one hop**: parent-to-child and child-to-parent
    ///   respectively, never skipping stations and never inverted.
    /// - The acks travel the *reverse* direction of their verb: an
    ///   `InstructionAck` flows up, a `ReportAck` flows down.
    /// - `ProgressNote`/`RunnerEol` flow strictly up (one hop).
    /// - `PeerNote` flows strictly sideways (same station).
    /// - `Ping`/`Pong` are direction-free liveness.
    pub fn lawful_for(&self, sender: MeshStation, recipient: MeshStation) -> bool {
        use MeshMessage::*;
        if sender == recipient {
            // Only liveness and peer traffic is self-addressable (a loopback
            // probe); control verbs never are.
            return matches!(self, Ping { .. } | Pong { .. } | PeerNote { .. });
        }
        match self {
            Instruction { .. } | ReportAck { .. } => sender.may_command(recipient),
            Report { .. } | InstructionAck { .. } => recipient.may_command(sender),
            ProgressNote { .. } | RunnerEol { .. } => recipient.may_command(sender),
            PeerNote { .. } => false,
            Ping { .. } | Pong { .. } => true,
        }
    }
}

/// Direction class of a [`MeshMessage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshRoute {
    /// Elder ⇄ subordinate (both directions lawful, one hop).
    Vertical,
    /// Subordinate → elder only.
    UpOnly,
    /// Same station only.
    Peer,
    /// Any direction (liveness).
    Any,
}

/// The envelope carrying one [`MeshMessage`] with routing and tracing metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeshEnvelope {
    /// Unique message id (used by acks).
    pub id: String,
    /// Optional distributed trace id correlating multi-agent operations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// Optional correlation id linking replies to initiating requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    /// Sender address; `None` only for daemon-originated control traffic.
    pub sender: Option<MeshAddress>,
    /// Intended recipient address.
    pub recipient: MeshAddress,
    /// The payload.
    pub message: MeshMessage,
}

impl MeshEnvelope {
    pub fn new(sender: Option<MeshAddress>, recipient: MeshAddress, message: MeshMessage) -> Self {
        let id = crate::mesh_ids::next_message_id();
        Self {
            id,
            trace_id: None,
            correlation_id: None,
            sender,
            recipient,
            message,
        }
    }

    /// Attach a distributed trace ID for cross-session/cross-agent tracing.
    pub fn with_trace(mut self, trace_id: impl Into<String>) -> Self {
        self.trace_id = Some(trace_id.into());
        self
    }

    /// Attach a correlation ID linking this envelope to a prior request.
    pub fn with_correlation(mut self, correlation_id: impl Into<String>) -> Self {
        self.correlation_id = Some(correlation_id.into());
        self
    }

    /// Lawfulness check bundling the sender's station (when present).
    pub fn lawful(&self) -> bool {
        let Some(sender) = &self.sender else {
            return true;
        };
        self.message
            .lawful_for(sender.station, self.recipient.station)
    }
}

/// Identifiers for mesh envelopes. Module-private helper so the id format can
/// evolve without touching the envelope shape.
pub mod mesh_ids {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(1);

    /// Monotonic in-process message id. Long enough lived for ack correlation
    /// within a daemon; a cross-process transport supplies its own ids.
    pub fn next_message_id() -> String {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("mesh-{n}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn master(session: &str) -> MeshAddress {
        MeshAddress::new(MeshStation::Session, session, session)
    }

    fn runner(session: &str, agent: &str) -> MeshAddress {
        MeshAddress::new(MeshStation::Subtask, session, agent)
    }

    #[test]
    fn addresses_sort_by_station_then_session_then_agent() {
        let sup = MeshAddress::hypervisor("daemon");
        let m1 = master("session-1");
        let r1 = runner("session-1", "runner-a");
        let r2 = runner("session-1", "runner-b");
        let m2 = master("session-2");

        let mut addrs = vec![r2.clone(), m2.clone(), sup.clone(), r1.clone(), m1.clone()];
        addrs.sort();
        assert_eq!(addrs, vec![sup, m1, m2, r1, r2]);
    }

    #[test]
    fn lawful_instruction_flows_down_only() {
        let instr = MeshMessage::Instruction {
            body: "do work".into(),
        };
        assert!(instr.lawful_for(MeshStation::Session, MeshStation::Subtask));
        assert!(!instr.lawful_for(MeshStation::Subtask, MeshStation::Session));
    }

    #[test]
    fn lawful_report_flows_up_only() {
        let report = MeshMessage::Report {
            body: "work done".into(),
        };
        assert!(report.lawful_for(MeshStation::Subtask, MeshStation::Session));
        assert!(!report.lawful_for(MeshStation::Session, MeshStation::Subtask));
    }

    #[test]
    fn peer_notes_never_cross_stations() {
        let note = MeshMessage::PeerNote {
            body: "hi peer".into(),
        };
        assert!(note.lawful_for(MeshStation::Subtask, MeshStation::Subtask));
        assert!(!note.lawful_for(MeshStation::Subtask, MeshStation::Session));
        assert!(!note.lawful_for(MeshStation::Session, MeshStation::Subtask));
    }

    #[test]
    fn runner_eol_flows_up_only() {
        let note = MeshMessage::RunnerEol {
            final_note: Some("done".into()),
        };
        assert!(note.lawful_for(MeshStation::Subtask, MeshStation::Session));
        assert!(!note.lawful_for(MeshStation::Session, MeshStation::Subtask));
        assert!(!note.lawful_for(MeshStation::Subtask, MeshStation::Subtask));
    }
}
