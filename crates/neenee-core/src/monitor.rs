//! Daemon-observability wire contracts (ADR-0093): the [`MonitorAction`]
//! handshake selector and the [`MonitorEvent`] stream a `neenee-server` daemon
//! publishes about every session it hosts.
//!
//! These types are the read-only control-plane counterpart of the
//! session-scoped `AgentRequest`/`AgentResponse` protocol: a control panel (or
//! any other observer) connects, selects `Monitor`, receives one
//! [`MonitorEvent::Snapshot`], and then follows `MonitorEvent::Diff`s. They
//! carry **no conversation content** — only ids, titles/previews, status, and
//! accounting — so a dashboard never deserializes a transcript.
//!
//! The types are pure contracts (ADR-0057): no I/O, no derivations. The
//! session status machine that produces [`SessionStatus`] values from the
//! `AgentResponse` stream lives in `neenee_transport::monitor`.

use serde::{Deserialize, Serialize};

/// Handshake action selecting a daemon-observability stream instead of a
/// session attach (ADR-0093 §2). Sent as the first frame:
/// `{"type":"Select","action":{"monitor":{"watch":…,"include_idle":…}}}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MonitorAction {
    /// Keep the connection open and stream `MonitorEvent::Diff`s after the
    /// initial snapshot (`neenee status --watch`, live control panels). When
    /// `false` the server sends the snapshot and closes the connection.
    #[serde(default)]
    pub watch: bool,
    /// Include live sessions that are simply idle (no round running and
    /// nothing blocked). Defaults to `false` so a busy dashboard stays a
    /// zero-statement surface: an all-idle daemon reports an empty list.
    #[serde(default)]
    pub include_idle: bool,
}

/// How the session behind a [`MonitoredSession`] row is hosted. Under
/// ADR-0096's unified ownership every session is daemon-held, so this is
/// always [`Hosted`](Self::Hosted); the field is kept on the wire (with its
/// serde default) so rows produced before the distinction was removed still
/// deserialize.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionHosting {
    /// The session's driver lives inside the serving host process (an
    /// `attach`-created or lazily resumed session). The host owns its
    /// lifecycle and can serve full `Attach` clients for it.
    #[default]
    Hosted,
}

impl SessionHosting {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hosted => "hosted",
        }
    }
}

impl std::fmt::Display for SessionHosting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A stream frame about the daemon as a whole.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MonitorEvent {
    /// The full current state, sent exactly once as the first frame after the
    /// monitor handshake. Sessions are sorted by `updated_at`, newest first.
    Snapshot(MonitorSnapshot),
    /// One hosted session was created or re-hosted (lazy resume). Carries its
    /// complete row so a consumer needs no back-reference.
    SessionAdded(MonitoredSession),
    /// One hosted session's row changed in place.
    SessionUpdated(MonitoredSession),
    /// A hosted session shut down. Consumers drop the row. (Session teardown
    /// is not yet emitted by the host — hosted sessions live for the daemon's
    /// lifetime — but the variant is part of the contract so panels written
    /// against it handle teardown when it lands.)
    SessionRemoved { session_id: String },
}

/// The daemon-level snapshot: who is serving and what is happening right now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitorSnapshot {
    pub project_root: String,
    /// Unix seconds when the daemon process started (from the discovery
    /// record; `0` when the registry was not created by a daemon, e.g. an
    /// in-TUI `/serve` prehost).
    pub daemon_started_at: u64,
    pub sessions: Vec<MonitoredSession>,
}

/// One row of the control panel: a hosted session's identity, status, and
/// accounting. Deliberately a superset of nothing — every field is cheap and
/// content-free (see module docs).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonitoredSession {
    pub id: String,
    /// Stored AI/manual title, falling back to the first-prompt preview.
    pub overview: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub message_count: usize,
    /// Who owns the session's driver. Defaults to `Hosted` — the only value
    /// since ADR-0096 — so producers written against ADR-0093 stay valid.
    #[serde(default)]
    pub hosting: SessionHosting,
    /// Derived lifecycle status (ADR-0093 §3): the panel's primary sort key.
    pub status: SessionStatus,
    /// 1-based index of the current (or most recently completed) user round.
    pub round: u64,
    /// 0-based index of the model request within the current round, when one
    /// has started.
    pub turn: Option<usize>,
    /// Output tokens generated by the current/most-recent round so far.
    pub output_tokens: u64,
    /// Wall-clock milliseconds since the current round started (frozen at the
    /// final duration once the round terminates).
    pub elapsed_ms: u64,
    /// Currently executing tool, if any.
    pub current_tool: Option<String>,
    /// Latest one-line activity string (e.g. "waiting for model").
    pub activity: Option<String>,
    /// Current AI-visible context size, when reported.
    pub context_tokens: Option<usize>,
    /// One-line error/notice text for `Failed` / `NeedsApproval` / `NeedsInput`.
    pub note: Option<String>,
    /// Absolute project workspace path this session belongs to (ADR-0096's
    /// two-level indexing projected down to the row). Empty for producers
    /// that predate the field (e.g. `/serve` prehosts) — display code must
    /// tolerate it. Content-free in the monitor sense: it is addressing
    /// metadata, not conversation.
    #[serde(default)]
    pub project_root: String,
    /// The session's declared work-in-progress (ADR-0097 §5), when it has
    /// registered one: the paths it is mid-edit on plus a one-line summary.
    /// Coordination metadata, not conversation; absent (`None`) means "no
    /// declared WIP", which is what a consumer needs to answer `check_wip`.
    #[serde(default)]
    pub wip: Option<WipStatus>,
}

/// A session's declared work-in-progress (ADR-0097 §5): the paths it is
/// mid-edit on plus a one-line summary, so peers in the same workspace can
/// avoid colliding verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WipStatus {
    /// Paths the session is actively editing (as declared; workspace-relative
    /// or absolute, normalized at comparison time).
    pub paths: Vec<String>,
    /// One-line description of the in-flight work (e.g. "refactoring the
    /// retry loop — tree doesn't build").
    pub summary: String,
}

/// One overlapping WIP found by a `check_wip` query (ADR-0097 §5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WipConflict {
    /// Session id holding the conflicting WIP.
    pub session: String,
    /// The WIP's declared paths.
    pub paths: Vec<String>,
    /// The WIP's one-line summary.
    pub summary: String,
    /// The subset of `paths` that overlaps the query's paths (empty when the
    /// query named no paths — the conflict is then whole-workspace).
    pub overlap: Vec<String>,
}

/// What a `check_wip` verdict advises the asking session to do (ADR-0097 §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WipAdvice {
    /// No conflicting WIP — proceed, including whole-tree verification.
    Proceed,
    /// Conflicting WIP exists — narrow to non-overlapping paths and skip
    /// global verification (no full test suite / no direct run).
    ProceedScoped,
    /// A conflicting WIP directly overlaps what the session is about to do —
    /// wait or ask the human rather than plough ahead.
    Defer,
}

impl WipAdvice {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proceed => "proceed",
            Self::ProceedScoped => "proceed_scoped",
            Self::Defer => "defer",
        }
    }
}

impl std::fmt::Display for WipAdvice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl MonitoredSession {
    /// A zeroed row for one session id — the seed a tracker starts from
    /// before any event has been folded in.
    pub fn empty(id: String) -> Self {
        Self {
            id,
            overview: String::new(),
            created_at: 0,
            updated_at: 0,
            message_count: 0,
            hosting: SessionHosting::Hosted,
            status: SessionStatus::Idle,
            round: 0,
            turn: None,
            output_tokens: 0,
            elapsed_ms: 0,
            current_tool: None,
            activity: None,
            context_tokens: None,
            note: None,
            project_root: String::new(),
            wip: None,
        }
    }
}

/// Display-level lifecycle status of a hosted session, derived from its
/// response stream. This is the multi-session analogue of the single-session
/// [`ParentStatus`](crate::ParentStatus) badge (ADR-0017): a coarse,
/// panel-facing classification, not the protocol state — the round lifecycle
/// itself stays binary (`RoundLifecycle`, ADR-0078).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    /// No round running, nothing waiting on a human.
    Idle,
    /// A round is actively producing model output or running tools.
    Running,
    /// Blocked on a tool-permission decision.
    NeedsApproval,
    /// Blocked on an `ask_user` question or interactive-command input.
    NeedsInput,
    /// The current round ended via interruption (Esc); the prompt may resume.
    Interrupted,
    /// The current round ended with a turn-level error.
    Failed,
}

impl SessionStatus {
    /// Whether a panel row in this status describes ongoing or blocked work —
    /// the default (non-`include_idle`) filter for monitor snapshots.
    pub fn is_active(self) -> bool {
        !matches!(self, Self::Idle)
    }

    /// The wire string, also used directly by the `neenee status` table.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::NeedsApproval => "needs-approval",
            Self::NeedsInput => "needs-input",
            Self::Interrupted => "interrupted",
            Self::Failed => "failed",
        }
    }
}

impl std::fmt::Display for SessionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitor_action_defaults_are_off() {
        let action: MonitorAction = serde_json::from_str("{}").unwrap();
        assert!(!action.watch);
        assert!(!action.include_idle);
    }

    #[test]
    fn monitor_action_roundtrips() {
        let action = MonitorAction {
            watch: true,
            include_idle: true,
        };
        let json = serde_json::to_string(&action).unwrap();
        let back: MonitorAction = serde_json::from_str(&json).unwrap();
        assert_eq!(back, action);
    }

    #[test]
    fn session_status_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&SessionStatus::NeedsApproval).unwrap(),
            "\"needs_approval\""
        );
        assert_eq!(SessionStatus::NeedsApproval.as_str(), "needs-approval");
        assert_eq!(SessionStatus::NeedsInput.to_string(), "needs-input");
    }

    #[test]
    fn session_status_is_active_gates_idle_only() {
        assert!(!SessionStatus::Idle.is_active());
        for status in [
            SessionStatus::Running,
            SessionStatus::NeedsApproval,
            SessionStatus::NeedsInput,
            SessionStatus::Interrupted,
            SessionStatus::Failed,
        ] {
            assert!(status.is_active(), "{status} should be active");
        }
    }

    #[test]
    fn monitor_event_uses_kind_tag() {
        let event = MonitorEvent::SessionRemoved {
            session_id: "s-1".into(),
        };
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(json, r#"{"kind":"session_removed","session_id":"s-1"}"#);
        let back: MonitorEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, event);
    }

    #[test]
    fn hosting_defaults_to_hosted_for_older_producers() {
        let json = r#"{"id":"s","overview":"","created_at":0,"updated_at":0,"message_count":0,"status":"idle","round":0,"turn":null,"output_tokens":0,"elapsed_ms":0,"current_tool":null,"activity":null,"context_tokens":null,"note":null}"#;
        let row: MonitoredSession = serde_json::from_str(json).unwrap();
        assert_eq!(row.hosting, SessionHosting::Hosted);
    }

    #[test]
    fn snapshot_roundtrips_with_a_full_row() {
        let snapshot = MonitorSnapshot {
            project_root: "/tmp/proj".into(),
            daemon_started_at: 1_700_000_000,
            sessions: vec![MonitoredSession {
                id: "s-1".into(),
                overview: "fix the flaky test".into(),
                created_at: 1,
                updated_at: 2,
                message_count: 7,
                hosting: SessionHosting::Hosted,
                status: SessionStatus::Running,
                round: 3,
                turn: Some(1),
                output_tokens: 512,
                elapsed_ms: 9_000,
                current_tool: Some("bash".into()),
                activity: Some("running bash".into()),
                context_tokens: Some(48_000),
                note: None,
                project_root: "/tmp/proj".into(),
                wip: None,
            }],
        };
        let json = serde_json::to_string(&MonitorEvent::Snapshot(snapshot.clone())).unwrap();
        let back: MonitorEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back, MonitorEvent::Snapshot(snapshot));
    }
}
