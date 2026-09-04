//! Unified application wire protocol (ADR-0134, ADR-0158).
//!
//! Provides the fundamental client-daemon wire envelopes:
//! - Handshake & selection: `Select`, `Welcome`, `Pick`
//! - Single-shot control: `ControlReply`
//! - Full-duplex session transport: `Request`, `Response`
//! - Observability & diagnostics: `Monitor`, `Error`

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
#[allow(clippy::large_enum_variant)]
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

    /// Full-duplex client agent request envelope.
    Request {
        #[serde(flatten)]
        request: crate::AgentRequest,
    },
    /// Full-duplex daemon agent response envelope.
    Response {
        #[serde(flatten)]
        response: crate::AgentResponse,
    },
    /// Daemon observability event envelope.
    Monitor {
        #[serde(flatten)]
        event: crate::MonitorEvent,
    },
    /// Connection-level error envelope.
    Error {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
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
