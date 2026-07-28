//! Principal profiles: declarative top-level agent roles. The principal-side
//! mirror of [`crate::envoy::EnvoyProfile`] (ADR-0053).
//!
//! ## Why this exists
//!
//! Before ADR-0053 the principal role was *imperative*: `neenee`'s
//! `main.rs` hand-assembled the identity, left the capability scope at the
//! constructor default, left the write/command boundary unrestricted, and
//! seeded the runtime knobs from a single `[principal]` config table. There was
//! no first-class object that *named* a principal role. Meanwhile the envoy
//! side has been declarative since ADR-0011: a role is a `const EnvoyProfile`,
//! and `EnvoyTool` binds it.
//!
//! That asymmetry meant adding a principal instance (a new binary, a new
//! persona) duplicated assembly logic instead of binding a profile, and left
//! the `QUANT` envoy profile semantically homeless (the quant *product* is a
//! principal, but the quant *role description* lived on the envoy side).
//!
//! `PrincipalProfile` closes the gap: a principal role is a value the embedding
//! binds via [`crate::agent::Agent::apply_principal_profile`] (re-exported
//! through the agent crate), exactly as `EnvoyTool` binds an
//! [`crate::EnvoyProfile`]. Both live in core as vocabulary so the engine stays
//! role-agnostic and ADR-0042's role taxonomy is declared in one place.

use crate::{AgentIdentity, OperationScope, ToolSelection};

/// User-tunable principal *runtime* behaviour — the declarative form of the
/// values `neenee`'s `main.rs` used to seed imperatively from the
/// `[principal]` config table. Mirrors the subset of [`crate::EnvoyProfile`]
/// that concerns execution knobs (hard stop, doom-loop guard, model-stdin)
/// rather than capability scope, which lives directly on
/// [`PrincipalProfile::agent_selection`] / [`PrincipalProfile::operation_scope`].
///
/// Defaults match [`crate::DoomGuardConfig`] / the constructor's built-in values
/// so a profile with [`PrincipalRuntimeConfig::default`] is a no-op over the
/// agent constructor's defaults.
///
/// `Copy` because every field is (`DoomGuardConfig` is `Copy`, the two scalars
/// trivially so) — a profile can be read and re-seeded cheaply.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PrincipalRuntimeConfig {
    /// Opt-in hard-stop budget: abort a round after this many ReAct turns.
    /// `0` (the default) means uncapped. Mirrors `[principal] hard_stop_turns`
    /// and `Agent::set_hard_stop_turns`.
    pub hard_stop_turns: usize,
    /// Doom-loop guard config. Mirrors `[principal.nudge]` and
    /// `Agent::set_doom_guard_config`. Default disabled.
    pub nudge: crate::DoomGuardConfig,
    /// Whether the model may supply stdin bytes for a `bash` call it emits.
    /// Mirrors `[principal] allow_model_stdin` and
    /// `Agent::set_allow_model_stdin`. Default `false`.
    pub allow_model_stdin: bool,
    /// Whether an interactive `bash` command skips the inline input panel and
    /// instead runs with stdin closed (fast failure + non-interactive remedy).
    /// Mirrors `[principal] skip_interactive_input` and
    /// `Agent::set_skip_interactive_input`. Default `false`.
    pub skip_interactive_input: bool,
}

/// A declarative principal role: an identity, the capability scope it admits,
/// the write/command boundary it enforces, and its runtime knobs. The
/// principal-side mirror of [`crate::EnvoyProfile`] (ADR-0053).
///
/// A profile is a value the embedding binds after constructing the agent via
/// [`crate::agent::Agent::apply_principal_profile`] (re-exported through the
/// agent crate). The built-in coding principal lives in the application layer
/// (`neenee`'s `identity` module, `principal_code()` — ADR-0054); a future
/// quant/research/ops principal is another value.
///
/// Unlike [`crate::EnvoyProfile`] (a `Copy` `const` of `&'static` slices), this
/// owns `String`s / `Vec`s because [`AgentIdentity`] and [`OperationScope`] do.
/// That is fine — a principal is constructed once at startup, not per-spawn.
///
/// ## Identity is supplied at construction, not applied
///
/// [`AgentIdentity`] feeds the system-prompt preamble and is immutable past the
/// `Agent` constructor, so it is passed to `Agent::new` / `from_toolset`, not
/// set by [`crate::agent::Agent::apply_principal_profile`]. A role whose
/// identity should differ per instance (side conversations, group chat) composes
/// the profile with [`Self::with_identity`] before construction.
#[derive(Debug, Clone)]
pub struct PrincipalProfile {
    /// The profile's name, e.g. `"code"`. For logs / pickers / a future
    /// principal registry.
    pub name: &'static str,
    /// Who this principal is and what it is for (name + mission + optional
    /// persona). Supplied to the `Agent` constructor unchanged.
    pub identity: AgentIdentity,
    /// This principal's capability **name scope** over the pool — the agent
    /// half of the two-selector model (ADR-0041).
    /// [`ToolSelection::unrestricted`] (the default for a coding principal)
    /// admits every capability; a scoped principal (e.g. read-only ops) narrows
    /// this.
    pub agent_selection: ToolSelection,
    /// The hard write/command boundary this principal enforces (ADR-0028).
    /// [`OperationScope::unrestricted`] (the default) leaves both dimensions
    /// open; a sandboxed principal pins `write_paths` / `command_allowlist`.
    pub operation_scope: OperationScope,
    /// Runtime execution knobs (hard stop, doom guard, model stdin).
    pub config: PrincipalRuntimeConfig,
    /// Whether this principal runs unattended (no human confirmations). Default
    /// `false` — a top-level principal is interactive by contract.
    pub unattended: bool,
}

impl PrincipalProfile {
    /// Build a profile from an identity with full default scope and attended
    /// behaviour — the common case for a new coding-class principal. Compose
    /// further with the `with_*` builders.
    pub fn with_identity(name: &'static str, identity: AgentIdentity) -> Self {
        Self {
            name,
            identity,
            agent_selection: ToolSelection::unrestricted(),
            operation_scope: OperationScope::unrestricted(),
            config: PrincipalRuntimeConfig::default(),
            unattended: false,
        }
    }

    /// Narrow the capability scope (the scope axis of ADR-0041). Builder-style.
    pub fn with_selection(mut self, selection: ToolSelection) -> Self {
        self.agent_selection = selection;
        self
    }

    /// Pin a write/command boundary (ADR-0028). Builder-style.
    pub fn with_operation_scope(mut self, scope: OperationScope) -> Self {
        self.operation_scope = scope;
        self
    }

    /// Attach the runtime knobs. Builder-style.
    pub fn with_runtime_config(mut self, config: PrincipalRuntimeConfig) -> Self {
        self.config = config;
        self
    }

    /// Run attended (`false`, the default) or unattended (`true`).
    pub fn with_unattended(mut self, unattended: bool) -> Self {
        self.unattended = unattended;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentIdentity;

    #[test]
    fn with_identity_is_unrestricted_and_attended() {
        let p = PrincipalProfile::with_identity("code", AgentIdentity::new("n", "m"));
        assert_eq!(p.name, "code");
        assert!(!p.unattended);
        // unrestricted selection ⇒ All scope, empty variant pins
        assert_eq!(p.agent_selection.scope, crate::ToolScope::All);
        assert!(p.agent_selection.variants.is_empty());
        // unrestricted operation scope
        assert!(p.operation_scope.paths.is_none());
        assert!(p.operation_scope.commands.is_none());
        // default runtime config
        assert_eq!(p.config.hard_stop_turns, 0);
        assert!(!p.config.allow_model_stdin);
        assert!(!p.config.skip_interactive_input);
        assert_eq!(p.config.nudge, crate::DoomGuardConfig::default());
    }

    #[test]
    fn builders_override_defaults() {
        let p = PrincipalProfile::with_identity("ops", AgentIdentity::default())
            .with_unattended(true)
            .with_runtime_config(PrincipalRuntimeConfig {
                hard_stop_turns: 7,
                ..Default::default()
            });
        assert!(p.unattended);
        assert_eq!(p.config.hard_stop_turns, 7);
    }

    #[test]
    fn runtime_config_is_copy() {
        let c = PrincipalRuntimeConfig::default();
        let _copy = c; // Copy: no move
        let _again = c;
    }
}
