//! The agent mesh tracker: delivery, lifecycle, and supervision (ADR-0167).
//!
//! A single [`MeshTracker`] per daemon coordinates agent addresses.
//!
//! Key invariants:
//! - **Addresses are unique and stable.** [`MeshAddress`] identifies an agent
//!   by `(station, session, agent)`.
//! - **Registration is RAII-managed.** [`MeshMailbox::spawn`] registers on
//!   creation and unregisters on drop.
//! - **Sends are lawfulness-checked.** [`MeshTracker::send`] consults
//!   [`MeshEnvelope::lawful`] and refuses (with an explicit error) rather
//!   than delivering an out-of-contract message (e.g. a runner commanding a
//!   master). Fail-closed beats silent misrouting.

use muta_contracts::{MeshAddress, MeshEnvelope, MeshMessage, MeshStation};
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
    /// The envelope violates the station contract (see
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

/// Registration entry in the tracker.
#[derive(Clone)]
struct Entry {
    sender: mpsc::UnboundedSender<MeshEnvelope>,
    token: CancellationToken,
    /// The parent master of a runner (masters have `None`). Used by
    /// [`MeshTracker::reap_children`].
    master: Option<MeshAddress>,
}

/// The daemon-level coordinator for agent communication.
#[derive(Clone, Default)]
pub struct MeshTracker {
    entries: Arc<Mutex<HashMap<MeshAddress, Entry>>>,
}

impl MeshTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new mailbox at `address`. Drops any existing mailbox at
    /// that address (re-registering an address cancels the old occupant).
    pub fn register(
        &self,
        address: MeshAddress,
        sender: mpsc::UnboundedSender<MeshEnvelope>,
        token: CancellationToken,
        master: Option<MeshAddress>,
    ) {
        let mut map = lock(&self.entries);
        if let Some(old) = map.insert(
            address,
            Entry {
                sender,
                token,
                master,
            },
        ) {
            old.token.cancel();
        }
    }

    /// Unregister an address explicitly (also happens automatically when the
    /// [`MeshMailbox`] drops).
    pub fn unregister(&self, address: &MeshAddress) {
        lock(&self.entries).remove(address);
    }

    /// Cancel and remove all runner endpoints registered under `master`
    /// (its runners). Returns the number of addresses reaped.
    pub fn reap_children(&self, master: &MeshAddress) -> usize {
        let mut map = lock(&self.entries);
        let victims: Vec<MeshAddress> = map
            .iter()
            .filter(|(addr, e)| addr.station == MeshStation::Subtask && e.master.as_ref() == Some(master))
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
                .unwrap_or_else(|| MeshAddress::hypervisor("daemon"));
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

    /// Every live address, sorted.
    pub fn live_addresses(&self) -> Vec<MeshAddress> {
        self.reap_cancelled();
        lock(&self.entries).keys().cloned().collect::<Vec<_>>()
    }

    /// Addresses at one station, in one session (peer discovery).
    pub fn peers(&self, station: MeshStation, session: &str) -> Vec<MeshAddress> {
        self.reap_cancelled();
        lock(&self.entries)
            .keys()
            .filter(|a| a.station == station && a.session == session)
            .cloned()
            .collect()
    }

    /// Addresses at one station across all sessions (cross-session peer discovery).
    pub fn peers_by_station(&self, station: MeshStation) -> Vec<MeshAddress> {
        self.reap_cancelled();
        lock(&self.entries)
            .keys()
            .filter(|a| a.station == station)
            .cloned()
            .collect()
    }

    /// Sweep entries whose token has fired.
    fn reap_cancelled(&self) {
        let mut map = lock(&self.entries);
        map.retain(|_, entry| !entry.token.is_cancelled());
    }
}

/// An agent's inbound mesh mailbox. Drops unregister the address.
pub struct MeshMailbox {
    tracker: MeshTracker,
    address: MeshAddress,
    receiver: mpsc::UnboundedReceiver<MeshEnvelope>,
    token: CancellationToken,
}

impl MeshMailbox {
    /// Spawn a mailbox registered with `tracker`. If `master` is supplied,
    /// this mailbox will be reaped when `tracker.reap_children(master)` is
    /// called.
    pub fn spawn(
        tracker: MeshTracker,
        address: MeshAddress,
        master: Option<MeshAddress>,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let token = CancellationToken::new();
        tracker.register(address.clone(), tx, token.clone(), master);
        Self {
            tracker,
            address,
            receiver: rx,
            token,
        }
    }

    /// This agent's unique address in the mesh.
    pub fn address(&self) -> &MeshAddress {
        &self.address
    }

    /// Cancellation token for this agent's lifetime. Fires when the mailbox
    /// drops, the master reaps its children, or a replacement agent is
    /// registered at the same address.
    pub fn token(&self) -> &CancellationToken {
        &self.token
    }

    /// Await the next envelope delivered to this mailbox.
    pub async fn recv(&mut self) -> Option<MeshEnvelope> {
        self.receiver.recv().await
    }

    /// Try to receive an envelope without waiting.
    pub fn try_recv(&mut self) -> Result<MeshEnvelope, mpsc::error::TryRecvError> {
        self.receiver.try_recv()
    }
}

impl Drop for MeshMailbox {
    fn drop(&mut self) {
        self.token.cancel();
        self.tracker.unregister(&self.address);
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn master(session: &str) -> MeshAddress {
        MeshAddress::master(session)
    }

    fn runner(session: &str, agent: &str) -> MeshAddress {
        MeshAddress::runner(session, agent)
    }

    #[tokio::test]
    async fn send_delivers_to_registered_mailbox() {
        let tracker = MeshTracker::new();
        let mut mb = MeshMailbox::spawn(tracker.clone(), master("s"), None);
        let sender = MeshAddress::hypervisor("daemon");

        tracker
            .send(MeshEnvelope::new(
                Some(sender),
                master("s"),
                MeshMessage::Instruction {
                    body: "begin".into(),
                },
            ))
            .expect("hypervisor may instruct master");

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

        let sender = MeshAddress::hypervisor("daemon");
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
    async fn peer_discovery_is_station_and_session_scoped() {
        let tracker = MeshTracker::new();
        let _m1 = MeshMailbox::spawn(tracker.clone(), master("s1"), None);
        let _m2 = MeshMailbox::spawn(tracker.clone(), master("s2"), None);
        let _r1 = MeshMailbox::spawn(tracker.clone(), runner("s1", "r1"), Some(master("s1")));

        let peers = tracker.peers(MeshStation::Session, "s1");
        assert_eq!(peers, vec![master("s1")]);
        let runners = tracker.peers(MeshStation::Subtask, "s1");
        assert_eq!(runners, vec![runner("s1", "r1")]);
        assert!(tracker.peers(MeshStation::Subtask, "s2").is_empty());
    }
}
