//! Pure, orthogonal application wire protocol (ADR-0134, ADR-0158, ADR-0159).
//!
//! Provides the four fundamental orthogonal primitives:
//! 1. [`Wire::Call`] / [`Wire::Reply`] - Point-to-point RPC with correlation IDs.
//! 2. [`Wire::Patch`] - Versioned domain state synchronization (Replace | Update).
//! 3. [`Wire::Gate`] / [`Wire::GateDecision`] - Server-initiated interactive gates.
//! 4. [`Wire::Stream`] - High-throughput token/output chunks.
//! 5. Control & lifecycle frames: `Select`, `Welcome`, `Pick`, `Error`, `Monitor`.

use serde::{Deserialize, Serialize};

/// Current wire protocol number (ADR-0134, ADR-0159).
pub const PROTOCOL_VERSION: u32 = 4;

/// Minimum served wire protocol version.
pub const MIN_PROTOCOL_VERSION: u32 = 1;

/// Stable machine-readable error codes.
pub const ERR_PROTOCOL_MISMATCH: &str = "protocol_mismatch";
pub const ERR_VERSION_MISMATCH: &str = "version_mismatch";

pub const fn protocol_accepts(client: u32) -> bool {
    matches!(client, MIN_PROTOCOL_VERSION..=PROTOCOL_VERSION)
}

/// The unified wire envelope on every connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Wire {
    /// Handshake frame declaring role, scope, and capabilities.
    Select {
        action: AttachAction,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<std::path::PathBuf>,
        #[serde(default)]
        posture: crate::human_request::HumanChannelPosture,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        protocol: Option<u32>,
    },
    /// Daemon response welcoming an attached connection.
    Welcome {
        session_id: String,
        round_counter: u64,
        messages: Vec<crate::Message>,
        #[serde(default)]
        provider: String,
        #[serde(default)]
        model: String,
        #[serde(default)]
        round_interrupts: Vec<crate::RoundInterrupt>,
        #[serde(default)]
        command_catalog: crate::CommandCatalog,
    },
    /// Daemon response to ambiguous attach / picker.
    Pick {
        sessions: Vec<crate::SessionOverview>,
    },
    /// Reply to single-shot control verb.
    ControlReply {
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },

    // --- Core Orthogonal Primitives (ADR-0159) ---
    /// 1. Unicast Client RPC Request.
    Call {
        id: u64,
        #[serde(flatten)]
        call: RpcCall,
    },
    /// 1. Unicast Server RPC Response.
    Reply {
        id: u64,
        result: Result<serde_json::Value, ProtocolError>,
    },

    /// 2. Versioned State Synchronization.
    Patch {
        session_id: String,
        domain: StateDomain,
        version: u64,
        #[serde(flatten)]
        op: PatchOp,
    },

    /// 3. Interactive Gate Request (Daemon -> Client).
    Gate {
        gate_id: String,
        session_id: String,
        #[serde(flatten)]
        payload: GatePayload,
    },
    /// 3. Interactive Gate Resolution (Client -> Daemon).
    GateDecision {
        gate_id: String,
        decision: GateDecision,
    },

    /// 4. High-frequency Streaming Output Chunk.
    Stream {
        session_id: String,
        #[serde(flatten)]
        chunk: StreamChunk,
    },

    // --- Legacy compatibility envelopes during transition ---
    Request {
        #[serde(flatten)]
        request: crate::AgentRequest,
    },
    Response {
        #[serde(flatten)]
        response: crate::AgentResponse,
    },
    Monitor {
        #[serde(flatten)]
        event: crate::MonitorEvent,
    },
    Error {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
}

/// Point-to-point RPC methods (ADR-0159).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum RpcCall {
    CompleteComposer {
        text: String,
        cursor: usize,
    },
    SwitchProvider {
        provider: String,
        model: Option<String>,
    },
    GetSessionDetail {
        session_id: String,
    },
}

/// Domain classification for state synchronization (ADR-0159).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateDomain {
    Transcript,
    TodoList,
    SecurityTrust,
    Pressure,
    RuntimeMeta,
}

/// State patch operation: full replacement or diff (ADR-0159).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", content = "data", rename_all = "snake_case")]
pub enum PatchOp {
    Replace(serde_json::Value),
    Update(serde_json::Value),
}

/// Interactive gate request payload (ADR-0159).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "gate_type", rename_all = "snake_case")]
pub enum GatePayload {
    Permission {
        tool_name: String,
        arguments: String,
        preview: Option<String>,
    },
    AskUser {
        questions: Vec<crate::UserQuestion>,
    },
}

/// Interactive gate resolution decision (ADR-0159).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum GateDecision {
    Allow,
    Deny { reason: Option<String> },
    Answer { answers: Vec<String> },
}

/// Fast streaming data chunk (ADR-0159).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "stream_kind", rename_all = "snake_case")]
pub enum StreamChunk {
    TokenDelta(String),
    ProcessStdout(String),
    ProcessStderr(String),
}

/// Unified protocol error model (ADR-0159).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProtocolError {
    pub domain: String,
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl ProtocolError {
    pub fn new(
        domain: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            domain: domain.into(),
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }
}

/// What role the connection wants to assume.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum AttachAction {
    New,
    Attach(Option<String>),
    Picker,
    Control(ControlRequest),
    Monitor(crate::MonitorAction),
}

/// Single-shot session-management verbs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "verb", rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum ControlRequest {
    Shutdown,
    CreateSession {
        project: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
    },
    SendPrompt {
        session_id: String,
        text: String,
    },
    Interrupt {
        session_id: String,
    },
    ResolvePermission {
        session_id: String,
        request_id: String,
        decision: crate::PermissionDecision,
    },
    KillSession {
        session_id: String,
    },
    SuspendSession {
        session_id: String,
    },
}
