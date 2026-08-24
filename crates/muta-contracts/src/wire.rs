//! The transport envelope of the daemon control plane (ADR-0134).
//!
//! [`Wire`] is the first-class protocol surface between a client (CLI,
//! TUI, Web app) and the daemon: the `Select` handshake frame and every
//! frame after it. These types lived in `muta-runtime`'s `serve` module
//! from ADR-0096 until ADR-0134 moved them here, next to the payload types
//! ([`crate::events`], [`crate::monitor`]), so the whole wire surface —
//! envelope, payloads, and the protocol-number constants below — has one
//! home and one serde source of truth.
//!
//! # Protocol numbering (ADR-0134)
//!
//! Compatibility between a client and a daemon is governed by a wire
//! **protocol number**, not by the product version. The daemon serves a
//! client whose number falls in the window
//! [`MIN_PROTOCOL_VERSION`, [`PROTOCOL_VERSION`]]; see [`protocol_accepts`].
//! The number is a plain monotonically increasing integer, *not* a semver:
//! semver implies a compatibility interval that a wire format almost never
//! has.
//!
//! Bump discipline (mechanically enforced by `scripts/check-wire-compat.sh`
//! in CI):
//!
//! - **Do not bump** for additive changes — a new `#[serde(default)]`
//!   optional field, a new enum variant an older peer can never receive.
//!   The crate's serde discipline (`default` + `skip_serializing_if` on
//!   every optional field, "absent on older daemons" everywhere) already
//!   makes those changes compatible in both directions.
//! - **Bump [`PROTOCOL_VERSION`]** when an older peer would fail to
//!   deserialize or silently misinterpret a frame — renamed fields,
//!   changed field types, removed variants, re-tagged enums.
//! - **Raise [`MIN_PROTOCOL_VERSION`]** when support for an older protocol
//!   number is deliberately dropped.
//!
//! A forgotten bump is a silent corruption bug — the exact hazard version
//! negotiation exists to prevent — so CI treats any change to this module
//! (or to the generated TypeScript mirror) without a bump as an error
//! unless the PR is explicitly marked `wire-compatible`.

/// The wire protocol this build speaks. Served to and accepted from
/// clients; see the [module documentation][self] for the bump discipline.
pub const PROTOCOL_VERSION: u32 = 2;

/// The oldest wire protocol number this daemon still serves. Raises only
/// when support for an older protocol is deliberately dropped; clients
/// older than this are refused with an upgrade instruction.
pub const MIN_PROTOCOL_VERSION: u32 = 1;

/// Stable machine-readable error code (carried on `Wire::Error.code`) for
/// a refused wire-protocol negotiation (ADR-0134). Mirrors
/// `ERR_VERSION_MISMATCH`, which covers the legacy product-version path.
pub const ERR_PROTOCOL_MISMATCH: &str = "protocol_mismatch";

/// Stable machine-readable error code for the legacy exact-equality
/// product-version refusal (ADR-0100 rule 4), sent to clients that predate
/// the protocol field.
pub const ERR_VERSION_MISMATCH: &str = "version_mismatch";

/// Whether this build serves a client speaking wire protocol number
/// `client`. The window is inclusive on both ends: a client on
/// [`PROTOCOL_VERSION`] is the current protocol, a client on
/// [`MIN_PROTOCOL_VERSION`] is the oldest still served.
pub const fn protocol_accepts(client: u32) -> bool {
    matches!(client, MIN_PROTOCOL_VERSION..=PROTOCOL_VERSION)
}

/// The first frame on every connection, and every frame after it. serde
/// tags envelopes on `"type"`; `Request` / `Response` / `Monitor` payloads
/// are flattened so a payload variant appears as a key next to `"type"`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum Wire {
    Select {
        action: AttachAction,
        /// The attaching client's working directory — the project scope for
        /// `New` creation, auto-attach, and lazy resume (ADR-0096). Optional
        /// for wire compatibility: a client predating the field sends none
        /// and the daemon falls back to its own process cwd, its behavior
        /// before the field existed. Ignored for Monitor / Control
        /// actions, which the daemon serves without consulting a project
        /// scope.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<std::path::PathBuf>,
        /// The client's **product** build version (`CARGO_PKG_VERSION`).
        /// Diagnostic identity since ADR-0134 — the wire gate is
        /// `protocol` — but still *enforced* against clients that send no
        /// protocol number, preserving ADR-0100 rule 4's exact equality
        /// for pre-protocol clients. Absent on frames from clients
        /// predating the field; the daemon tolerates them the same way it
        /// tolerates any unknown sender: by serving them (a same-build
        /// client always sends it; only a genuinely old client omits it,
        /// and refusing on absence would brick version-pinned clients
        /// against their own daemon).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        /// The client's **wire protocol** number (ADR-0134). When present,
        /// it is the authority: the daemon serves any number in
        /// [`MIN_PROTOCOL_VERSION`, `PROTOCOL_VERSION`] regardless of the
        /// product version, and refuses anything outside the window with
        /// `ERR_PROTOCOL_MISMATCH` before any session work. When absent
        /// (a pre-protocol client), the daemon falls back to exact
        /// product-version equality on `version` above.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        protocol: Option<u32>,
    },
    Welcome {
        session_id: String,
        round_counter: u64,
        messages: Vec<crate::Message>,
        /// The provider instance id the session is currently serving (its
        /// own pin when set, else the config default at bind time). Drives
        /// the TUI hint-bar's `@<instance>` suffix and the picker's active
        /// highlight. Empty when no provider is configured.
        #[serde(default)]
        provider: String,
        /// The wire model id the session is currently serving. Empty when no
        /// model resolves.
        #[serde(default)]
        model: String,
        /// Durable round-interrupt records (C11), re-projected into the
        /// transcript on the client side. Absent on older daemons.
        #[serde(default)]
        round_interrupts: Vec<crate::RoundInterrupt>,
        /// Backend-owned slash-command vocabulary for completion and help.
        /// Includes project commands visible to this session.
        #[serde(default)]
        command_catalog: crate::CommandCatalog,
    },
    Pick {
        sessions: Vec<crate::SessionOverview>,
    },
    Error {
        message: String,
        /// Stable machine-readable reason (ADR-0105) so a client can render
        /// targeted guidance instead of string-sniffing. Defined values:
        /// [`ERR_VERSION_MISMATCH`], [`ERR_PROTOCOL_MISMATCH`]. Absent on
        /// older daemons.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
    },
    Request {
        #[serde(flatten)]
        request: crate::AgentRequest,
    },
    Response {
        #[serde(flatten)]
        response: crate::AgentResponse,
    },
    /// Daemon-observability stream frame (ADR-0093). Server → client only;
    /// the first frame after a `Select{Monitor}` handshake is always
    /// `MonitorEvent::Snapshot`, followed by diffs while `watch` holds.
    Monitor {
        #[serde(flatten)]
        event: crate::MonitorEvent,
    },
    /// Reply to a `Select{action: Control(..)}` verb (ADR-0096): either the
    /// created/confirmed session id or an error message.
    ControlReply {
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

/// What a client wants from the daemon, declared on the `Select` frame.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum AttachAction {
    New,
    Attach(Option<String>),
    /// Open the sessions picker over a throwaway carrier session
    /// (ADR-0116): the daemon assembles a `Picker` start — no restore, no
    /// hooks — and the client's TUI raises the picker modal; `/sessions
    /// <id>` switches to the real session. The endpoint for `muta
    /// attach` with no id, so choosing is interactive instead of a printed
    /// list on stderr.
    Picker,
    /// Observe the whole host instead of attaching to one session
    /// (ADR-0093): the server answers with a snapshot frame and, when
    /// `watch` is set, streams diffs until the client disconnects.
    Monitor(crate::MonitorAction),
    /// Issue a session-management verb (ADR-0096): create, prompt, interrupt,
    /// answer a permission, or kill — without attaching as a session client.
    Control(ControlRequest),
}

/// Session-management verbs for the control plane (ADR-0096). Each maps to
/// a registry operation; the reply is `Wire::ControlReply`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq, ts_rs::TS)]
#[serde(tag = "verb", rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum ControlRequest {
    /// Create a session for a project; optionally send an opening prompt.
    CreateSession {
        project: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        // Skipped when `None`: absent on the wire, never an explicit `null`.
        #[ts(optional)]
        prompt: Option<String>,
    },
    /// Send a prompt to a hosted session as a new round.
    SendPrompt { session_id: String, text: String },
    /// Interrupt the current round of a hosted session.
    Interrupt { session_id: String },
    /// Answer a pending permission request on a hosted session.
    ResolvePermission {
        session_id: String,
        request_id: String,
        decision: crate::PermissionDecision,
    },
    /// Tear down a hosted session.
    KillSession { session_id: String },
    /// Park a hosted session in memory without ending it: tear the driver
    /// down but do **not** fire `SessionEnd` hooks or broadcast `Exit` —
    /// the transcript is durable, so the next attach rebuilds it through
    /// the standard lazy-resume path. The memory-reclamation half of
    /// [`ControlRequest::KillSession`]: "free the RAM, keep the session".
    /// Rejects a session with an attached client (someone is watching it)
    /// or an active round — those must detach / be interrupted first.
    SuspendSession { session_id: String },
    /// Stop the daemon itself (ADR-0100): stop accepting new attaches, drain
    /// live connections, tear every hosted session down through the same
    /// graceful path as SIGINT/SIGTERM, and exit 0. Gives scripts, the TUI,
    /// and the upgrade flow a clean remote stop that previously required
    /// `kill <pid>`. There is deliberately no force flag: a second `muta
    /// stop` (or any signal) escalates naturally through the same gate.
    Shutdown,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window must be well-formed: the minimum never exceeds the
    /// current number, and this build always accepts its own protocol.
    /// (The well-formedness half is const so a bad edit fails compilation,
    /// not just a test run.)
    #[test]
    fn protocol_window_is_well_formed() {
        const { assert!(MIN_PROTOCOL_VERSION <= PROTOCOL_VERSION) };
        assert!(protocol_accepts(PROTOCOL_VERSION));
        assert!(protocol_accepts(MIN_PROTOCOL_VERSION));
        assert!(!protocol_accepts(PROTOCOL_VERSION + 1));
        if MIN_PROTOCOL_VERSION > 1 {
            assert!(!protocol_accepts(MIN_PROTOCOL_VERSION - 1));
        }
    }

    /// The `Select` handshake shape is pinned: the protocol number rides an
    /// optional field next to the advisory product version, so a
    /// pre-protocol daemon simply never sees it (unknown fields are
    /// ignored) and a pre-protocol client never sends it.
    #[test]
    fn select_roundtrips_the_protocol_field() {
        let frame = Wire::Select {
            action: AttachAction::Monitor(crate::MonitorAction {
                watch: false,
                include_idle: false,
            }),
            project: None,
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            protocol: Some(PROTOCOL_VERSION),
        };
        let json = serde_json::to_string(&frame).unwrap();
        assert!(
            json.contains(&format!("\"protocol\":{PROTOCOL_VERSION}")),
            "protocol number is on the wire: {json}"
        );
        assert!(json.contains(&format!("\"version\":\"{}\"", env!("CARGO_PKG_VERSION"))));
        let back: Wire = serde_json::from_str(&json).unwrap();
        match back {
            Wire::Select { protocol, .. } => assert_eq!(protocol, Some(PROTOCOL_VERSION)),
            other => panic!("expected Select, got {other:?}"),
        }

        // And the legacy shape — no protocol, no version — still parses.
        let legacy: Wire = serde_json::from_str(r#"{"type":"Select","action":"new"}"#).unwrap();
        match legacy {
            Wire::Select {
                protocol, version, ..
            } => {
                assert_eq!(protocol, None);
                assert_eq!(version, None);
            }
            other => panic!("expected Select, got {other:?}"),
        }
    }
}
