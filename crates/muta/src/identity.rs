//! This CLI's identity + principal profile.
//!
//! Lives in the application layer (`muta`), NOT in `muta-runtime`.
//! The server layer stays application-neutral — a future sibling binary
//! brings its own identity/principal. The server's `/btw` side
//! session reuses the primary agent's identity via `Agent::identity()`,
//! so it never asks the server to name a product.

use muta_contracts::{AgentIdentity, PrincipalProfile};

/// The product's default instance name. A self-reference anchor the model
/// uses in the system prompt (intro line, responding when called by name).
/// Not "the principal's name" — the role ("code") is carried by
/// [`PrincipalProfile::name`].
const MUTA_NAME: &str = "muta";

/// What this CLI's agent is for.
const MUTA_MISSION: &str = "an expert AI coding assistant with tool access";

/// The composed identity: name + mission, default tone (no persona override).
pub fn muta_identity() -> AgentIdentity {
    AgentIdentity::new(MUTA_NAME, MUTA_MISSION)
}

/// The built-in **coding principal** profile (ADR-0053): the declarative
/// form of the role this binary historically assembled inline. Bound via
/// `agent.apply_principal_profile(&principal_code())` after construction.
///
/// Scope and operation boundary are unrestricted (a coding principal may
/// use every capability and write anywhere in the workspace) and the
/// runtime config is the default — the binary still overlays the live
/// `[principal]` config table afterwards so per-installation knobs win.
/// A future `muta-quant` binary brings its own `PrincipalProfile` value
/// instead of forking the server.
pub fn principal_code() -> PrincipalProfile {
    PrincipalProfile::with_identity("code", muta_identity())
}

/// The daemon has no terminal or browser clipboard of its own. Clipboard
/// effects belong to a connected app; until the wire protocol carries that
/// request back to the initiating client, report the boundary explicitly.
pub struct DaemonUiBridge;

#[async_trait::async_trait]
impl muta_runtime::UiBridge for DaemonUiBridge {
    async fn copy_to_clipboard(&self, _text: &str) -> Result<muta_runtime::CopyOutcome, String> {
        Err("clipboard export is a client capability; use mutx's local copy action".to_string())
    }
}

// Principal role presets (`architect`, `reviewer`, `security`) and the
// `/principal` / `@principal:` switching mechanism are declared in
// `muta-contracts` as shared vocabulary (`PrincipalRole`,
// `PrincipalProfile::for_role`) and applied via `Agent::apply_principal_role`,
// so this binary does not need its own role registry — both frontends share one.
