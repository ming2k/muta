//! The agent mesh: transport-neutral envelopes, addresses, and the
//! tracker contract (ADR-0144 §4).
//!
//! ## Why this exists
//!
//! The tier hierarchy needs three communication directions:
//!
//! - **top-down** (`Instruction`): an elder steers a subordinate.
//! - **bottom-up** (`Report`): a subordinate answers its elder.
//! - **peer** (`PeerNote`): same-tier agents exchange notes without going
//!   through their parent's conversation.
//!
//! The design follows a BitTorrent-style **tracker**: one registry per daemon
//! knows every live [`MeshAddress`]; agents send by address. The in-process
//! transport is an unbounded mpsc per agent, but the contract types here are
//! transport-neutral — a future socket transport carries the same envelopes
//! without touching the agent loop.
//!
//! Direction is **signed by tier**: [`MeshMessage::route`] encodes who may
//! send what to whom, so a runner can never be commanded by a sibling, and a
//! master never receives a report from a runner it does not own (ownership
//! is checked by the receiving side against the sender's parent identity).

use serde::{Deserialize, Serialize};

use crate::tier::AgentTier;

/// A globally unique, sortable address in the daemon's mesh.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MeshAddress {
    /// Tier of the addressed agent — the first routing hop.
    pub tier: AgentTier,
    /// Owning session id (runners inherit their master's session).
    pub session: String,
    /// Agent instance id — unique within `(tier, session)`.
    pub agent: String,
}

impl MeshAddress {
    pub fn new(tier: AgentTier, session: impl Into<String>, agent: impl Into<String>) -> Self {
        Self { tier, session: session.into(), agent: agent.into() }
    }

    /// The address of this agent's parent in the mesh (same session, one
    /// tier up). `None` for the supervisor — the root has no parent.
    pub fn parent(&self) -> Option<MeshAddress> {
        let tier = self.tier.parent_tier()?;
        Some(Self { tier, session: self.session.clone(), agent: self.session.clone() })
    }

    /// Compact debug form `tier:session:agent`, used in logs and UI chips.
    pub fn display(&self) -> String {
        format!("{}:{}:{}", self.tier.label(), self.session, self.agent)
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
    /// Same-tier fire-and-forget note; never crosses tier lines.
    PeerNote { body: String },
    /// A runner announcing graceful end-of-life to its master.
    RunnerEol { final_note: Option<String> },
}

impl MeshMessage {
    /// The tier-direction class of this message: who may send it to whom.
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

    /// Whether an agent of tier `sender` may lawfully emit this message to an
    /// agent of tier `recipient`.
    ///
    /// Rules, by route class:
    /// - `Instruction` (elder → subordinate) and `Report` (subordinate →
    ///   elder) are **strictly one hop**: parent-to-child and child-to-parent
    ///   respectively, never skipping tiers and never inverted.
    /// - The acks travel the *reverse* direction of their verb: an
    ///   `InstructionAck` flows up, a `ReportAck` flows down.
    /// - `ProgressNote`/`RunnerEol` flow strictly up (one hop).
    /// - `PeerNote` flows strictly sideways.
    /// - `Ping`/`Pong` are direction-free liveness.
    pub fn lawful_for(&self, sender: AgentTier, recipient: AgentTier) -> bool {
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
    /// Same tier only.
    Peer,
    /// Any direction (liveness).
    Any,
}

/// The envelope carrying one [`MeshMessage`] with routing metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeshEnvelope {
    /// Unique message id (used by acks).
    pub id: String,
    /// Sender address; `None` only for daemon-originated control traffic.
    pub sender: Option<MeshAddress>,
    /// Intended recipient address.
    pub recipient: MeshAddress,
    /// The payload.
    pub message: MeshMessage,
}

impl MeshEnvelope {
    pub fn new(
        sender: Option<MeshAddress>,
        recipient: MeshAddress,
        message: MeshMessage,
    ) -> Self {
        let id = crate::mesh_ids::next_message_id();
        Self { id, sender, recipient, message }
    }

    /// Lawfulness check bundling the sender's tier (when present).
    pub fn lawful(&self) -> bool {
        let Some(sender) = &self.sender else { return true };
        self.message.lawful_for(sender.tier, self.recipient.tier)
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
        MeshAddress::new(AgentTier::Master, session, session)
    }

    fn runner(session: &str, agent: &str) -> MeshAddress {
        MeshAddress::new(AgentTier::Runner, session, agent)
    }

    #[test]
    fn addresses_sort_by_tier_then_session_then_agent() {
        let sup = MeshAddress::new(AgentTier::Supervisor, "daemon", "daemon");
        let m1 = master("a");
        let m2 = master("b");
        let r = runner("a", "r1");
        assert!(sup < m1 && m1 < m2 && m2 < r);
    }

    #[test]
    fn instruction_flows_down_and_report_flows_up() {
        let instr = MeshMessage::Instruction { body: "go".into() };
        assert!(instr.lawful_for(AgentTier::Master, AgentTier::Runner));
        assert!(!instr.lawful_for(AgentTier::Runner, AgentTier::Master));

        let report = MeshMessage::Report { body: "done".into() };
        assert!(report.lawful_for(AgentTier::Runner, AgentTier::Master));
        assert!(!report.lawful_for(AgentTier::Master, AgentTier::Runner));
    }

    #[test]
    fn peer_notes_never_cross_tiers() {
        let note = MeshMessage::PeerNote { body: "hi".into() };
        assert!(note.lawful_for(AgentTier::Runner, AgentTier::Runner));
        assert!(!note.lawful_for(AgentTier::Runner, AgentTier::Master));
        assert!(!note.lawful_for(AgentTier::Master, AgentTier::Runner));
    }

    #[test]
    fn progress_notes_only_go_up() {
        let note = MeshMessage::ProgressNote { body: "halfway".into() };
        assert!(note.lawful_for(AgentTier::Runner, AgentTier::Master));
        assert!(!note.lawful_for(AgentTier::Master, AgentTier::Runner));
        assert!(!note.lawful_for(AgentTier::Runner, AgentTier::Runner));
    }

    #[test]
    fn envelopes_check_lawfulness() {
        let bad = MeshEnvelope::new(
            Some(runner("s", "r1")),
            master("s"),
            MeshMessage::Instruction { body: "usurp".into() },
        );
        assert!(!bad.lawful());

        let good = MeshEnvelope::new(
            Some(master("s")),
            runner("s", "r1"),
            MeshMessage::Instruction { body: "go".into() },
        );
        assert!(good.lawful());
    }

    #[test]
    fn message_ids_are_unique() {
        let a = mesh_ids::next_message_id();
        let b = mesh_ids::next_message_id();
        assert_ne!(a, b);
    }

    #[test]
    fn envelope_round_trips_serde() {
        let e = MeshEnvelope::new(
            Some(master("s")),
            runner("s", "r1"),
            MeshMessage::Report { body: "findings".into() },
        );
        let s = serde_json::to_string(&e).unwrap();
        let back: MeshEnvelope = serde_json::from_str(&s).unwrap();
        assert_eq!(back.id, e.id);
        assert_eq!(back.message, e.message);
    }
}
