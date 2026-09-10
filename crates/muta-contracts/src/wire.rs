//! Unified application wire protocol (ADR-0134, ADR-0158).
//!
//! Provides the fundamental client-daemon wire envelopes:
//! - Handshake & selection: `Select`, `Welcome`, `Pick`
//! - Single-shot control: `ControlReply`
//! - Full-duplex session transport: `Request`, `Response`
//! - Observability & diagnostics: `Monitor`, `Error`

use serde::{Deserialize, Serialize};

/// Current wire protocol number (ADR-0134, ADR-0159).
///
/// v5: `Wire::Welcome.retry_resolutions`, `RoundEvent::RetryResolved`,
/// `AgentEvent::BackgroundJobReady` / `RoundEvent::BackgroundJobReady`
/// (new variants an older peer cannot deserialize — not additive),
/// `TokenUsage.reasoning_tokens`, `ToolOutput::Shell.detached_job_id`
/// (additive sidecars riding the same bump).
///
/// v6 (ADR-0201): the connection vocabulary is re-keyed. `AddProvider` /
/// `EditProvider` / `ConnectProvider` / `DeleteProvider` / `SwitchProvider` /
/// `EditProviderModel` become `AddConnection` / `EditConnection` /
/// `ConnectConnection` / `DeleteConnection` / `SwitchConnection` /
/// `EditConnectionModel`; `RenameConnection` is new; `preset_id` becomes
/// `provider`; `ConnectionDetail` drops `id`; `ModelTargetScope::Preset`
/// becomes `Provider`. A v5 peer cannot deserialize any of these, so the
/// minimum served version moves with it.
///
/// v7 (ADR-0183): the subagent/subagent vocabulary is retired for the homogeneous
/// agent model. Renamed wire/persisted tags: `AgentKind::Subagent` →
/// `AgentKind::Subagent`, `AgentEvent::Subagent` → `AgentEvent::Subagent`,
/// `ToolOutput::Subagent` → `ToolOutput::Subagent`, `SubagentEvent` →
/// `SubagentEvent`, `RoundEvent::SubagentStep` → `RoundEvent::SubagentStep`,
/// `MeshMessage::SubagentEol` → `MeshMessage::SubagentEol`, and
/// `InjectionKind::{SubagentTask, SubagentSteer}` →
/// `InjectionKind::{SubagentTask, SubagentSteer}`. Records carrying the old
/// tags deserialize as unknown payloads (the persistence layer preserves them
/// opaquely) instead of failing the session. Legacy tool-name aliases
/// (`spawn_agent`, `delegate_code`, `delegate_mcp`, …) are deleted; the canonical
/// names are `spawn_agent`, `delegate_code`, and `delegate_mcp`.
/// v8 (ADR-0202, ADR-0204): typed `ToolOutput::WebSearch` and `ToolOutput::WebArticle`
/// variants with structured `WebSearchHit` results.
/// v9 (ADR-0203): `RemoteCatalogSourceOverride` and `RemoteCatalogEndpoint`
/// contracts for connection-local catalog source decoupling.
/// v10 (ADR-0208, ADR-0211): Archivist single-shot control query (`ControlRequest::AskArchivist`),
/// cross-project session history full-text search (`AgentRequest::SearchHistory`,
/// `AgentResponse::HistorySearch`), and provider model effort levels (`ProviderModelInfo.effort_levels`).
/// v11 (ADR-0227): `AgentRequest::RefreshProviderModels` is a unit variant; the
/// `user_initiated` flag is gone because every catalog refresh is user-initiated.
pub const PROTOCOL_VERSION: u32 = 11;

/// Minimum served wire protocol version. Raised to 6 by ADR-0201 (connection
/// vocabulary) and to 7 by ADR-0183 (subagent vocabulary): peers carrying the
/// previous tags cannot deserialize these shapes and must be rejected at
/// handshake rather than misinterpreted.
pub const MIN_PROTOCOL_VERSION: u32 = 7;

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
        retry_resolutions: Vec<crate::RetryResolution>,
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

/// Initial options and postures when creating a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct SessionInitOptions {
    /// `--unattended` / unattended execution posture.
    #[serde(default)]
    pub unattended: bool,
    /// Whether workspace filesystem confinement is enforced (default true).
    #[serde(default = "default_confined")]
    pub confined: bool,
    /// Persona id to staff this session with (ADR-0220). `None` = the default
    /// workspace-scoped coding principal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona: Option<String>,
    /// Resume the most recent matching session instead of creating a new one
    /// (ADR-0226). With `persona`, matches by persona (and workspace when the
    /// persona binds one).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub resume: bool,
}

const fn default_confined() -> bool {
    true
}

impl Default for SessionInitOptions {
    fn default() -> Self {
        Self {
            unattended: false,
            confined: true,
            persona: None,
            resume: false,
        }
    }
}

impl SessionInitOptions {
    pub fn new(unattended: bool, confined: bool) -> Self {
        Self {
            unattended,
            confined,
            persona: None,
            resume: false,
        }
    }

    pub fn with_persona(mut self, persona: Option<String>) -> Self {
        self.persona = persona;
        self
    }

    pub fn with_resume(mut self, resume: bool) -> Self {
        self.resume = resume;
        self
    }

    pub fn is_default(&self) -> bool {
        !self.unattended && self.confined && self.persona.is_none() && !self.resume
    }
}

/// What role the connection wants to assume.
#[derive(Debug, Clone, PartialEq, ts_rs::TS)]
#[ts(rename_all = "snake_case", export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum AttachAction {
    New(Option<SessionInitOptions>),
    Attach(Option<String>),
    Picker,
    Control(ControlRequest),
    Monitor(crate::MonitorAction),
}

impl Serialize for AttachAction {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::New(None) => serializer.serialize_str("new"),
            Self::New(Some(opts)) => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("new", opts)?;
                map.end()
            }
            Self::Attach(id) => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("attach", id)?;
                map.end()
            }
            Self::Picker => serializer.serialize_str("picker"),
            Self::Control(req) => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("control", req)?;
                map.end()
            }
            Self::Monitor(act) => {
                use serde::ser::SerializeMap;
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("monitor", act)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for AttachAction {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum RawAttachAction {
            New(Option<SessionInitOptions>),
            Attach(Option<String>),
            Picker,
            Control(ControlRequest),
            Monitor(crate::MonitorAction),
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum WireHelper {
            Str(String),
            Structured(RawAttachAction),
        }

        match WireHelper::deserialize(deserializer)? {
            WireHelper::Str(s) => match s.as_str() {
                "new" => Ok(AttachAction::New(None)),
                "picker" => Ok(AttachAction::Picker),
                other => Err(serde::de::Error::unknown_variant(other, &["new", "picker"])),
            },
            WireHelper::Structured(raw) => Ok(match raw {
                RawAttachAction::New(opts) => AttachAction::New(opts),
                RawAttachAction::Attach(id) => AttachAction::Attach(id),
                RawAttachAction::Picker => AttachAction::Picker,
                RawAttachAction::Control(c) => AttachAction::Control(c),
                RawAttachAction::Monitor(m) => AttachAction::Monitor(m),
            }),
        }
    }
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        init_options: Option<SessionInitOptions>,
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
    /// Ask the daemon's Archivist (ADR-0208) one question and get the full
    /// answer synchronously in the reply. The Archivist is the muta-level
    /// retrieval agent: cross-project session search, metadata surveys, and
    /// transcript reads — no workspace, no session side effects. The round
    /// borrows the asking session's live provider.
    AskArchivist {
        text: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_action_new_roundtrips() {
        // Bare "new" string (legacy)
        let bare = serde_json::to_string(&AttachAction::New(None)).unwrap();
        assert_eq!(bare, "\"new\"");
        let deserialized: AttachAction = serde_json::from_str("\"new\"").unwrap();
        assert_eq!(deserialized, AttachAction::New(None));

        // Structured new with options
        let opts = SessionInitOptions {
            unattended: true,
            confined: false,
            ..Default::default()
        };
        let with_opts = serde_json::to_string(&AttachAction::New(Some(opts.clone()))).unwrap();
        assert_eq!(with_opts, r#"{"new":{"unattended":true,"confined":false}}"#);
        let parsed: AttachAction = serde_json::from_str(&with_opts).unwrap();
        assert_eq!(parsed, AttachAction::New(Some(opts)));
    }

    #[test]
    fn attach_action_attach_and_picker_roundtrip() {
        let attach = AttachAction::Attach(Some("s-123".into()));
        let ser = serde_json::to_string(&attach).unwrap();
        assert_eq!(ser, r#"{"attach":"s-123"}"#);
        let back: AttachAction = serde_json::from_str(&ser).unwrap();
        assert_eq!(back, attach);

        let picker = AttachAction::Picker;
        let ser = serde_json::to_string(&picker).unwrap();
        assert_eq!(ser, "\"picker\"");
        let back: AttachAction = serde_json::from_str(&ser).unwrap();
        assert_eq!(back, picker);
    }
}
