//! This CLI's identity + principal profile.
//!
//! Lives in the application layer (`neenee`), NOT in `neenee-transport`.
//! The server layer stays application-neutral — a future sibling binary
//! brings its own identity/principal. The server's `/btw` side
//! session reuses the primary agent's identity via `Agent::identity()`,
//! so it never asks the server to name a product.

use neenee_agent::{AgentIdentity, PrincipalProfile};

/// The product's default instance name. A self-reference anchor the model
/// uses in the system prompt (intro line, responding when called by name).
/// Not "the principal's name" — the role ("code") is carried by
/// [`PrincipalProfile::name`].
const NEENEE_NAME: &str = "neenee";

/// What this CLI's agent is for.
const NEENEE_MISSION: &str = "an expert AI coding assistant with tool access";

/// The composed identity: name + mission, default tone (no persona override).
pub fn neenee_identity() -> AgentIdentity {
    AgentIdentity::new(NEENEE_NAME, NEENEE_MISSION)
}

/// The built-in **coding principal** profile (ADR-0053): the declarative
/// form of the role this binary historically assembled inline. Bound via
/// `agent.apply_principal_profile(&principal_code())` after construction.
///
/// Scope and operation boundary are unrestricted (a coding principal may
/// use every capability and write anywhere in the workspace) and the
/// runtime config is the default — the binary still overlays the live
/// `[principal]` config table afterwards so per-installation knobs win.
/// A future `neenee-quant` binary brings its own `PrincipalProfile` value
/// instead of forking the server.
pub fn principal_code() -> PrincipalProfile {
    PrincipalProfile::with_identity("code", neenee_identity())
}

// Principal role presets (`architect`, `reviewer`, `security`) and the
// `/principal` / `@principal:` switching mechanism are declared in
// `neenee-core` as shared vocabulary (`PrincipalRole`,
// `PrincipalProfile::for_role`) and applied via `Agent::apply_principal_role`,
// so this binary does not need its own role registry — both frontends share one.
