//! The protocol-client facade (ADR-0197 M3 / D4): every frontend-facing
//! client surface of the daemon behind one crate, so frontends depend on
//! `muta-contracts` (protocol types) plus **this crate** (transport and
//! lifecycle) and never on `muta-runtime` internals.
//!
//! This is a deliberate thin facade, not a fork: the client implementation
//! lives in `muta_runtime::client` (client and server share one wire
//! implementation so the protocol cannot drift — see that module's docs).
//! What this crate owns is the *boundary*: the compiler-visible edge a TUI,
//! dashboard, or headless frontend is allowed to cross, and the
//! boundary-enforcement test below, which fails the build if a frontend
//! reaches past it into `muta-runtime` directly.
//!
//! Re-exported here so the facade stays complete:
//! - discovery, daemon spawn/attach, control verbs, monitor stream
//!   (`muta_runtime::client`);
//! - the control-plane request type ([`ControlRequest`], defined in
//!   `muta_contracts::wire` — pure protocol);
//! - frontend-support helpers that are runtime-authored but
//!   frontend-consumed: tracing init, the backend slash-command catalog
//!   compiler, and slash/`@` completion (`muta_runtime::startup`,
//!   `muta_runtime::input_completion`);
//! - the clipboard SPI ([`UiBridge`], [`CopyOutcome`]) frontends implement
//!   for the runtime to call back into.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub use muta_contracts::wire::ControlRequest;
pub use muta_runtime::client::{
    AttachAction, DaemonInfo, Handshake, RemoteDaemon, connect, control, control_with_reply,
    discover, ensure_daemon, incompatibility_error, monitor_stream, set_posture,
    upsert_session_row, versions_compatible,
};
pub use muta_runtime::input_completion::{complete_for_frontend_test, complete_slash_items};
pub use muta_runtime::startup::{command_catalog, init_tracing};
pub use muta_runtime::{CopyOutcome, UiBridge};

#[cfg(test)]
mod boundary {
    /// D4's compiler-enforced protocol boundary: `mutx` must depend on
    /// `muta-contracts` and this facade — never on `muta-runtime` directly.
    /// A dependency-graph lint (cargo-deny) cannot scope a ban to one
    /// edge of the workspace, so the edge is pinned here, where any
    /// violation fails the suite with the rule stated inline.
    #[test]
    fn mutx_depends_on_the_facade_not_on_runtime_internals() {
        let manifest = include_str!("../../../apps/tui/crates/mutx/Cargo.toml");
        const BANNED: &[(&str, &str)] = &[
            (
                "muta-runtime",
                "ADR-0197 D4: mutx must not depend on `muta-runtime` directly; \
                 route daemon transport/lifecycle through `muta-client`",
            ),
            (
                "muta-providers",
                "ADR-0197 edge debt (cleared): model provider specs and OAuth login \
                 profiles are contract data — use `muta_contracts::model_providers` \
                 and `muta_contracts::provider_auth`",
            ),
            (
                "muta-persistence",
                "ADR-0197 edge debt (cleared): the daemon is the SSOT for the \
                 shared SQLite store — history and route settings travel over \
                 the protocol; filesystem paths come from `muta-paths`",
            ),
        ];
        for line in manifest.lines() {
            let dep = line.trim();
            for (crate_name, rule) in BANNED {
                assert!(!dep.starts_with(crate_name), "{rule}");
            }
        }
        assert!(
            manifest.contains("muta-client"),
            "the facade crate is the frontend's transport dependency"
        );
        assert!(
            manifest.contains("muta-contracts"),
            "the protocol vocabulary stays a direct dependency"
        );
    }
}
