//! Actor-model subsystem for concurrent subagent execution and mailbox communication.

pub mod handle;
pub mod mailbox;
pub mod supervisor;

pub use handle::ActorHandle;
pub use mailbox::{ActorMailbox, ActorMailboxSender};
pub use supervisor::ActorSupervisor;

#[cfg(test)]
mod tests;
