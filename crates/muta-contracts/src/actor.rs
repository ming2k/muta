//! Actor-model messaging and Subagent isolation contracts.
//!
//! Provides the core domain types for asynchronous Subagent lifecycle,
//! inbox messaging, hierarchical cancellation, and isolated worktree execution.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Unique identifier for an Actor / Subagent instance.
pub type ActorId = String;

/// The specialized operational role of an Actor.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorRole {
    /// The root coordinating agent interacting with the human user.
    Principal,
    /// Read-only research and code exploration specialist.
    Research,
    /// Code reviewer focused on correctness, style, and verification.
    CodeReview,
    /// Deep implementation specialist running in an isolated worktree.
    Coder,
    /// High-level strategic planning and task breakdown specialist.
    Planner,
    /// Integration and external MCP tool interaction specialist.
    McpSpecialist,
    /// User-defined customized role with a descriptive name.
    Custom(String),
}

impl ActorRole {
    /// Canonical name string for the role.
    pub fn name(&self) -> &str {
        match self {
            Self::Principal => "principal",
            Self::Research => "research",
            Self::CodeReview => "code_review",
            Self::Coder => "coder",
            Self::Planner => "planner",
            Self::McpSpecialist => "mcp_specialist",
            Self::Custom(name) => name.as_str(),
        }
    }

    /// Concise role description for UI and status presentation.
    pub fn description(&self) -> &str {
        match self {
            Self::Principal => "Principal coordinator agent",
            Self::Research => "Read-only codebase & documentation researcher",
            Self::CodeReview => "Code quality, correctness & safety reviewer",
            Self::Coder => "Autonomous implementation & refactoring specialist",
            Self::Planner => "High-level goal decomposition & step planner",
            Self::McpSpecialist => "Dynamic integration & external API specialist",
            Self::Custom(_) => "Specialized autonomous subagent",
        }
    }
}

/// Lifecycle state of an Actor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorState {
    /// Ready and waiting for incoming messages or assignments.
    Idle,
    /// Actively executing instructions or driving LLM rounds.
    Running,
    /// Blocked awaiting human approval or explicit question input.
    WaitingInput,
    /// Blocked awaiting responses from child subagents.
    WaitingDependents,
    /// Cancellation requested, shutting down in-flight operations.
    Cancelling,
    /// Cleanly finished execution and exited.
    Terminated,
    /// Failed with an unrecoverable runtime error.
    Errored(String),
}

/// Worktree isolation mode for an Actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeMode {
    /// Inherit the primary agent's working directory directly.
    #[default]
    Inherit,
    /// Create a fully isolated copy/shadow workspace branched from parent.
    Branch,
    /// Share the parent git repository storage but use an independent git worktree.
    Share,
}

/// Typed message payload sent between Actors or from the Supervisor.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActorMessage {
    /// Initiate a new task or prompt execution.
    Task {
        prompt: String,
        #[serde(default)]
        target_files: Vec<String>,
        #[serde(default)]
        metadata: HashMap<String, String>,
    },
    /// Provide steering, mid-flight guidance, or interactive input.
    Input { content: String },
    /// Request graceful cancellation of in-flight work.
    Cancel { reason: String },
    /// Health-check ping.
    Ping,
    /// Arbitrary domain message payload.
    Custom {
        custom_type: String,
        payload: String,
    },
}

/// An envelope wrapping an [`ActorMessage`] with routing metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActorEnvelope {
    pub id: String,
    pub sender: Option<ActorId>,
    pub recipient: ActorId,
    pub timestamp: u64,
    pub message: ActorMessage,
}

impl ActorEnvelope {
    pub fn new(sender: Option<ActorId>, recipient: ActorId, message: ActorMessage) -> Self {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            sender,
            recipient,
            timestamp,
            message,
        }
    }
}

/// Broadcast and status events emitted by Actors during their lifecycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActorEvent {
    /// Actor changed its lifecycle state.
    StateChanged {
        id: ActorId,
        old_state: ActorState,
        new_state: ActorState,
    },
    /// Actor reported incremental progress or milestone.
    Progress {
        id: ActorId,
        message: String,
        percent: Option<u8>,
    },
    /// Actor completed a task round with a final result.
    Result {
        id: ActorId,
        output: String,
        success: bool,
        duration_ms: u64,
    },
    /// A new child actor was spawned.
    Spawned {
        id: ActorId,
        parent_id: Option<ActorId>,
        role: ActorRole,
        worktree_mode: WorktreeMode,
    },
    /// Actor has terminated.
    Terminated { id: ActorId, reason: String },
}
