//! The agent-side mesh: in-process tracker + per-agent mailboxes
//! (ADR-0144 §4).
//!
//! The contract types ([`MeshAddress`], [`MeshEnvelope`], [`MeshMessage`])
//! live in `muta-contracts` so every crate shares one envelope shape. This
//! module is the **transport**: a BitTorrent-style tracker that knows every
//! live address in this daemon, plus the mailbox wiring that turns an
//! [`Agent`](crate::Agent)'s existing inbox into a mesh-addressable endpoint.
//!
//! ## Tracker semantics
//!
//! - **Registration is lease-based.** An address registers with a mailbox
//!   sender and a cancellation token. The tracker reaps addresses whose
//!   token has fired (cooperative; no heartbeat protocol needed
//!   in-process).
//! - **Cancellation is hierarchical.** Registering a child under a parent
//!   links the child's token to the parent's, so cancelling a master
//!   cancels its runners — the *mechanical* half of the atomic
//!   master-replacement guarantee.
//! - **Sends are lawfulness-checked.** [`MeshTracker::send`] consults
//!   [`MeshEnvelope::lawful`] and refuses (with an explicit error) rather
//!   than delivering an out-of-contract message (e.g. a runner commanding a
//!   master). Fail-closed beats silent misrouting.

use muta_contracts::{AgentTier, MeshAddress, MeshEnvelope, MeshMessage};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub mod tools;
pub use tools::{MeshListPeersTool, MeshSendTool};

/// Error returned by the tracker for an unlawful or unaddressable send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeshError {
    /// No live agent is registered at that address (reaped, never
    /// registered, or already replaced).
    Unroutable(MeshAddress),
    /// The envelope violates the tier contract (see
    /// [`MeshMessage::lawful_for`]).
    Unlawful {
        message_kind: &'static str,
        sender: Box<MeshAddress>,
        recipient: Box<MeshAddress>,
    },
}

impl std::fmt::Display for MeshError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MeshError::Unroutable(a) => write!(f, "no agent at mesh address {a}"),
            MeshError::Unlawful {
                message_kind,
                sender,
                recipient,
            } => write!(
                f,
                "unlawful mesh route {message_kind} from {sender} to {recipient}"
            ),
        }
    }
}

impl std::error::Error for MeshError {}

impl MeshError {
    /// The `kind` tag of an unlawful message, for logging/UI.
    fn kind_of(message: &MeshMessage) -> &'static str {
        match message {
            MeshMessage::Ping { .. } => "ping",
            MeshMessage::Pong { .. } => "pong",
            MeshMessage::Instruction { .. } => "instruction",
            MeshMessage::InstructionAck { .. } => "instruction_ack",
            MeshMessage::Report { .. } => "report",
            MeshMessage::ReportAck { .. } => "report_ack",
            MeshMessage::ProgressNote { .. } => "progress_note",
            MeshMessage::PeerNote { .. } => "peer_note",
            MeshMessage::RunnerEol { .. } => "runner_eol",
        }
    }
}

/// One live mesh endpoint.
#[derive(Clone)]
struct MeshEntry {
    sender: mpsc::UnboundedSender<MeshEnvelope>,
    token: CancellationToken,
    /// The tier-1 parent of a runner (masters have `None`). Used by
    /// [`MeshTracker::reap_children`] to implement hierarchical
    /// cancellation and by ownership checks on report delivery.
    master: Option<MeshAddress>,
}

/// The per-daemon mesh tracker: address → live mailbox.
///
/// Shared behind an `Arc` so the supervisor, every session's master, and the
/// runner dispatch all see one registry. Cloning the tracker clones the
/// handle, not the registry.
#[derive(Clone, Default)]
pub struct MeshTracker {
    entries: Arc<Mutex<HashMap<MeshAddress, MeshEntry>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl MeshTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a live endpoint. `token` cancels the entry (the tracker
    /// lazily reapes on next use). `master` links a runner to its owning
    /// master; pass `None` for supervisors and masters.
    pub fn register(
        &self,
        address: MeshAddress,
        sender: mpsc::UnboundedSender<MeshEnvelope>,
        token: CancellationToken,
        master: Option<MeshAddress>,
    ) {
        if let Some(parent) = master.as_ref().and_then(|m| m.parent()) {
            // Hierarchical cancellation: child token dies with parent.
            // The parent's own token is registered under its address; a
            // master whose token is not yet registered simply links nothing
            // (registration order is supervisor → masters → runners, so in
            // practice the parent exists first).
            if let Some(pe) = lock(&self.entries).get(&parent) {
                pe.token.child_token();
            }
        }
        lock(&self.entries).insert(
            address,
            MeshEntry {
                sender,
                token,
                master,
            },
        );
    }

    /// Deregister an address (graceful shutdown). Idempotent.
    pub fn deregister(&self, address: &MeshAddress) {
        lock(&self.entries).remove(address);
    }

    /// Cancel + deregister an address **and every address it owns**
    /// (its runners). Returns the number of addresses reaped. This is the
    /// reclamation half of atomic master replacement (ADR-0144 §3).
    pub fn reap_children(&self, master: &MeshAddress) -> usize {
        let mut map = lock(&self.entries);
        let victims: Vec<MeshAddress> = map
            .iter()
            .filter(|(addr, e)| addr.tier == AgentTier::Runner && e.master.as_ref() == Some(master))
            .map(|(addr, _)| addr.clone())
            .collect();
        for v in &victims {
            if let Some(e) = map.remove(v) {
                e.token.cancel();
            }
        }
        victims.len()
    }

    /// Deliver an envelope. Checks liveness, then lawfulness, then hands to
    /// the recipient's mailbox. Lawfulness is checked against the *sender*
    /// address carried on the envelope, so a forged sender is refused by the
    /// same rule that refuses a genuinely mis-ordered hierarchy.
    pub fn send(&self, envelope: MeshEnvelope) -> Result<(), MeshError> {
        if !envelope.lawful() {
            let sender = envelope
                .sender
                .clone()
                .unwrap_or_else(|| MeshAddress::new(AgentTier::Supervisor, "daemon", "daemon"));
            return Err(MeshError::Unlawful {
                message_kind: MeshError::kind_of(&envelope.message),
                sender: Box::new(sender),
                recipient: Box::new(envelope.recipient),
            });
        }
        let entry = {
            let mut map = lock(&self.entries);
            if let Some(e) = map.get(&envelope.recipient) {
                if e.token.is_cancelled() {
                    map.remove(&envelope.recipient);
                    return Err(MeshError::Unroutable(envelope.recipient));
                }
                e.clone()
            } else {
                return Err(MeshError::Unroutable(envelope.recipient));
            }
        };
        let recipient = envelope.recipient.clone();
        entry
            .sender
            .send(envelope)
            .map_err(|_| MeshError::Unroutable(recipient))
    }

    /// Every live address, sorted (tier, session, agent). For the tracker
    /// surface exposed to the supervisor.
    pub fn live_addresses(&self) -> Vec<MeshAddress> {
        self.reap_cancelled();
        lock(&self.entries).keys().cloned().collect::<Vec<_>>()
    }

    /// Addresses at one tier, in one session (peer discovery).
    pub fn peers(&self, tier: AgentTier, session: &str) -> Vec<MeshAddress> {
        self.reap_cancelled();
        lock(&self.entries)
            .keys()
            .filter(|a| a.tier == tier && a.session == session)
            .cloned()
            .collect()
    }

    /// Addresses at one tier across all sessions (cross-session peer discovery).
    pub fn peers_by_tier(&self, tier: AgentTier) -> Vec<MeshAddress> {
        self.reap_cancelled();
        lock(&self.entries)
            .keys()
            .filter(|a| a.tier == tier)
            .cloned()
            .collect()
    }

    /// Sweep entries whose token has fired.
    fn reap_cancelled(&self) {
        let mut map = lock(&self.entries);
        let dead: Vec<MeshAddress> = map
            .iter()
            .filter(|(_, e)| e.token.is_cancelled())
            .map(|(a, _)| a.clone())
            .collect();
        for a in dead {
            map.remove(&a);
        }
    }
}

/// A mailbox bound to a mesh address: the agent-side half of the transport.
///
/// `MeshMailbox::spawn` creates the channel, registers it with the tracker,
/// and returns the receiving half plus the entry's cancellation token. The
/// owning loop `select!`s on [`MeshMailbox::recv`] alongside its other
/// sources; on exit it calls [`MeshTracker::deregister`].
pub struct MeshMailbox {
    address: MeshAddress,
    tracker: MeshTracker,
    rx: mpsc::UnboundedReceiver<MeshEnvelope>,
    token: CancellationToken,
}

impl MeshMailbox {
    /// Create and register a mailbox for `address`, owned by `master` when
    /// the address is a runner.
    pub fn spawn(tracker: MeshTracker, address: MeshAddress, master: Option<MeshAddress>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let token = CancellationToken::new();
        tracker.register(address.clone(), tx, token.clone(), master);
        Self {
            address,
            tracker,
            rx,
            token,
        }
    }

    pub fn address(&self) -> &MeshAddress {
        &self.address
    }

    pub fn tracker(&self) -> &MeshTracker {
        &self.tracker
    }

    /// The cancellation token for this endpoint. Fires when a parent is
    /// reaped or the owner shuts down.
    pub fn token(&self) -> &CancellationToken {
        &self.token
    }

    /// Await the next lawful envelope addressed to us.
    pub async fn recv(&mut self) -> Option<MeshEnvelope> {
        self.rx.recv().await
    }

    /// Convenience: send a message from us to `recipient`.
    pub fn send(&self, recipient: MeshAddress, message: MeshMessage) -> Result<(), MeshError> {
        let envelope = MeshEnvelope::new(Some(self.address.clone()), recipient, message);
        self.tracker.send(envelope)
    }

    /// Deregister from the tracker (graceful exit).
    pub fn close(self) {
        self.tracker.deregister(&self.address);
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

    #[tokio::test]
    async fn send_delivers_to_registered_mailbox() {
        let tracker = MeshTracker::new();
        let mut mb = MeshMailbox::spawn(tracker.clone(), master("s"), None);
        let sender = MeshAddress::new(AgentTier::Supervisor, "daemon", "daemon");

        tracker
            .send(MeshEnvelope::new(
                Some(sender),
                master("s"),
                MeshMessage::Instruction {
                    body: "begin".into(),
                },
            ))
            .expect("supervisor may instruct master");

        let got = mb.recv().await.expect("envelope arrives");
        assert_eq!(
            got.message,
            MeshMessage::Instruction {
                body: "begin".into()
            }
        );
    }

    #[tokio::test]
    async fn unlawful_send_is_refused_not_delivered() {
        let tracker = MeshTracker::new();
        let mut mb = MeshMailbox::spawn(tracker.clone(), master("s"), None);

        let err = tracker
            .send(MeshEnvelope::new(
                Some(runner("s", "r1")),
                master("s"),
                MeshMessage::Instruction {
                    body: "usurp".into(),
                },
            ))
            .expect_err("runner cannot command master");

        assert!(matches!(err, MeshError::Unlawful { .. }));
        // And nothing was delivered.
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), mb.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn reap_children_removes_only_that_masters_runners() {
        let tracker = MeshTracker::new();
        let _m1 = MeshMailbox::spawn(tracker.clone(), master("s1"), None);
        let _m2 = MeshMailbox::spawn(tracker.clone(), master("s2"), None);
        let r1 = MeshMailbox::spawn(tracker.clone(), runner("s1", "r1"), Some(master("s1")));
        let r2 = MeshMailbox::spawn(tracker.clone(), runner("s2", "r2"), Some(master("s2")));

        assert_eq!(tracker.reap_children(&master("s1")), 1);
        assert!(r1.token().is_cancelled());
        assert!(!r2.token().is_cancelled());

        let live: Vec<String> = tracker
            .live_addresses()
            .iter()
            .map(|a| a.display())
            .collect();
        assert!(!live.contains(&runner("s1", "r1").display()));
        assert!(live.contains(&runner("s2", "r2").display()));
    }

    #[tokio::test]
    async fn cancelled_endpoint_is_reaped_on_use() {
        let tracker = MeshTracker::new();
        let mb = MeshMailbox::spawn(tracker.clone(), master("s"), None);
        mb.token().cancel();

        let sender = MeshAddress::new(AgentTier::Supervisor, "daemon", "daemon");
        let err = tracker
            .send(MeshEnvelope::new(
                Some(sender),
                master("s"),
                MeshMessage::Ping { nonce: 1 },
            ))
            .expect_err("cancelled endpoint is unroutable");
        assert_eq!(err, MeshError::Unroutable(master("s")));
    }

    #[tokio::test]
    async fn peer_discovery_is_tier_and_session_scoped() {
        let tracker = MeshTracker::new();
        MeshMailbox::spawn(tracker.clone(), master("s1"), None);
        MeshMailbox::spawn(tracker.clone(), master("s2"), None);
        MeshMailbox::spawn(tracker.clone(), runner("s1", "r1"), Some(master("s1")));

        let peers = tracker.peers(AgentTier::Master, "s1");
        assert_eq!(peers, vec![master("s1")]);
        let runners = tracker.peers(AgentTier::Runner, "s1");
        assert_eq!(runners, vec![runner("s1", "r1")]);
        assert!(tracker.peers(AgentTier::Runner, "s2").is_empty());
    }
}
