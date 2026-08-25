//! Actor inbox messaging channels based on asynchronous Tokio MPSC.

use muta_contracts::{ActorEnvelope, ActorMessage};
use tokio::sync::mpsc;

/// Sender handle to deposit messages into an Actor's inbox.
#[derive(Debug, Clone)]
pub struct ActorMailboxSender {
    tx: mpsc::UnboundedSender<ActorEnvelope>,
}

impl ActorMailboxSender {
    pub fn new(tx: mpsc::UnboundedSender<ActorEnvelope>) -> Self {
        Self { tx }
    }

    /// Deliver an envelope to the actor's inbox.
    pub fn send(&self, envelope: ActorEnvelope) -> Result<(), String> {
        self.tx
            .send(envelope)
            .map_err(|_| "Actor mailbox is closed (recipient terminated)".to_string())
    }

    /// Convenience helper to wrap a message into an envelope and send it.
    pub fn send_message(
        &self,
        sender: Option<String>,
        recipient: String,
        message: ActorMessage,
    ) -> Result<(), String> {
        let env = ActorEnvelope::new(sender, recipient, message);
        self.send(env)
    }
}

/// Actor Mailbox receiver.
pub struct ActorMailbox {
    rx: mpsc::UnboundedReceiver<ActorEnvelope>,
    tx: mpsc::UnboundedSender<ActorEnvelope>,
}

impl ActorMailbox {
    /// Create a new unbounded actor mailbox.
    pub fn new() -> (Self, ActorMailboxSender) {
        let (tx, rx) = mpsc::unbounded_channel();
        let sender = ActorMailboxSender::new(tx.clone());
        (Self { rx, tx }, sender)
    }

    /// Retrieve the cloneable sender handle for this mailbox.
    pub fn sender(&self) -> ActorMailboxSender {
        ActorMailboxSender::new(self.tx.clone())
    }

    /// Asynchronously receive the next envelope from the inbox.
    pub async fn recv(&mut self) -> Option<ActorEnvelope> {
        self.rx.recv().await
    }
}
