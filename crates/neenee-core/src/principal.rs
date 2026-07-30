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

use crate::{AgentIdentity, CommandScope, OperationScope, ToolSelection};

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
    /// Whether this principal runs on autopilot (no human confirmations). Default
    /// `false` — a top-level principal is interactive by contract.
    pub autopilot: bool,
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
            autopilot: false,
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

    /// Run attended (`false`, the default) or autopilot (`true`).
    pub fn with_autopilot(mut self, autopilot: bool) -> Self {
        self.autopilot = autopilot;
        self
    }
}

/// A named principal *role* a user can switch the live agent into at runtime
/// via `@principal:{role}` or `/principal <role>` (plan §3.3). Each role
/// composes a focused persona onto the product's base identity and narrows the
/// capability scope / operation boundary to match that role's contract.
///
/// The roles live in `neenee-core` (shared vocabulary) rather than the
/// application layer so both the CLI and the server offer the same set without
/// duplicating definitions. They are *presets* over a base
/// [`AgentIdentity`]: the product name ("neenee") is preserved and only the
/// mission/persona shifts, so a switched role is still recognizably the same
/// product, just wearing a different hat.
///
/// Use [`PrincipalProfile::for_role`] to materialize a role onto a base
/// identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrincipalRole {
    /// The default coding principal — full capabilities, unrestricted writes,
    /// the product's own mission. Identical in effect to the profile the
    /// embedding binds at startup, so switching back to `code` after another
    /// role restores the baseline.
    Code,
    /// Architect: design and review focus. Full read access, write tools
    /// retained but the persona steers toward analysis, tradeoff evaluation,
    /// and written design rationale before any change.
    Architect,
    /// Reviewer: read-only code review. Read/search/inspect tools only — no
    /// `write_file`, `edit_file`, or `bash`. The persona is a meticulous
    /// reviewer who reports findings and proposed diffs without applying them.
    Reviewer,
    /// Security auditor: read-only, command-confined. Read/search tools plus a
    /// narrow command allowlist (`git`, `rg`, `cargo audit`-class inspection).
    /// The persona focuses on vulnerability and supply-chain review.
    Security,
}

impl PrincipalRole {
    /// Every role in its canonical display order, for pickers and `/help`.
    pub const ALL: &[PrincipalRole] = &[
        PrincipalRole::Code,
        PrincipalRole::Architect,
        PrincipalRole::Reviewer,
        PrincipalRole::Security,
    ];

    /// The stable string name used in `@principal:{name}` / `/principal <name>`.
    pub fn as_str(self) -> &'static str {
        match self {
            PrincipalRole::Code => "code",
            PrincipalRole::Architect => "architect",
            PrincipalRole::Reviewer => "reviewer",
            PrincipalRole::Security => "security",
        }
    }

    /// Parse a role name (case-insensitive). Returns `None` for an unknown
    /// name so the caller can surface a clear "unknown role" error listing
    /// [`PrincipalRole::ALL`].
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "code" | "coder" | "default" => Some(PrincipalRole::Code),
            "architect" | "architecture" => Some(PrincipalRole::Architect),
            "reviewer" | "review" => Some(PrincipalRole::Reviewer),
            "security" | "audit" | "auditor" => Some(PrincipalRole::Security),
            _ => None,
        }
    }

    /// A short human description of what this role does, for confirmations.
    pub fn description(self) -> &'static str {
        match self {
            PrincipalRole::Code => "the default coding principal (full capabilities)",
            PrincipalRole::Architect => "architecture & design focus (analysis-first)",
            PrincipalRole::Reviewer => "read-only code review",
            PrincipalRole::Security => "read-only security audit (command-confined)",
        }
    }
}

impl PrincipalProfile {
    /// Materialize a [`PrincipalRole`] onto a product's base [`AgentIdentity`].
    ///
    /// The base identity's `name` is preserved (the agent is still called
    /// "neenee"); only the mission — and, for focused roles, a persona
    /// override — shifts to match the role. Capability scope and operation
    /// boundary narrow per the role's contract (see [`PrincipalRole`]).
    ///
    /// Runtime config and the attended flag are left at defaults; the live
    /// `[principal]` config overlay and the current attended setting are not
    /// disturbed by a role switch.
    pub fn for_role(role: PrincipalRole, base: &AgentIdentity) -> Self {
        match role {
            PrincipalRole::Code => Self::with_identity("code", base.clone()),
            PrincipalRole::Architect => {
                let identity = AgentIdentity::new(
                    base.name.clone(),
                    "an expert software architect — evaluates design tradeoffs, \
                     proposes structure, and writes design rationale before changing code",
                );
                Self::with_identity("architect", identity)
            }
            PrincipalRole::Reviewer => {
                // Read-only inspection tools. `bash` is excluded: a reviewer
                // reports findings, it does not execute arbitrary commands.
                let identity = AgentIdentity::new(
                    base.name.clone(),
                    "a meticulous code reviewer — reports findings and proposed \
                     diffs without applying changes",
                );
                Self::with_identity("reviewer", identity).with_selection(
                    ToolSelection::only([
                        "read_text",
                        "grep",
                        "glob",
                        "list_dir",
                        "read_image",
                        "webfetch",
                        "websearch",
                        "todo",
                        "ask_user",
                    ]),
                )
            }
            PrincipalRole::Security => {
                // Read-only, plus a confined command allowlist for audit-style
                // inspection (version control, search, dependency audit).
                let identity = AgentIdentity::new(
                    base.name.clone(),
                    "a security auditor — reviews for vulnerabilities and \
                     supply-chain risk without modifying the project",
                );
                let scope = OperationScope {
                    paths: None,
                    commands: Some(CommandScope::new([
                        "git".to_string(),
                        "rg".to_string(),
                        "cargo".to_string(),
                        "npm".to_string(),
                        "ls".to_string(),
                        "cat".to_string(),
                        "find".to_string(),
                        "file".to_string(),
                    ])),
                };
                Self::with_identity("security", identity)
                    .with_selection(ToolSelection::only([
                        "read_text",
                        "grep",
                        "glob",
                        "list_dir",
                        "read_image",
                        "bash",
                        "webfetch",
                        "websearch",
                        "todo",
                        "ask_user",
                    ]))
                    .with_operation_scope(scope)
            }
        }
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
        assert!(!p.autopilot);
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
            .with_autopilot(true)
            .with_runtime_config(PrincipalRuntimeConfig {
                hard_stop_turns: 7,
                ..Default::default()
            });
        assert!(p.autopilot);
        assert_eq!(p.config.hard_stop_turns, 7);
    }

    #[test]
    fn runtime_config_is_copy() {
        let c = PrincipalRuntimeConfig::default();
        let _copy = c; // Copy: no move
        let _again = c;
    }

    #[test]
    fn role_round_trips_through_parse() {
        for role in PrincipalRole::ALL {
            let parsed = PrincipalRole::parse(role.as_str());
            assert_eq!(parsed, Some(*role), "{} should parse back", role.as_str());
        }
        // Aliases.
        assert_eq!(PrincipalRole::parse("Coder"), Some(PrincipalRole::Code));
        assert_eq!(PrincipalRole::parse("REVIEW"), Some(PrincipalRole::Reviewer));
        assert_eq!(PrincipalRole::parse("auditor"), Some(PrincipalRole::Security));
        // Unknown.
        assert!(PrincipalRole::parse("wizard").is_none());
    }

    #[test]
    fn for_role_preserves_product_name() {
        let base = AgentIdentity::new("neenee", "an expert AI coding assistant");
        for role in PrincipalRole::ALL {
            let profile = PrincipalProfile::for_role(*role, &base);
            // The product name survives a role switch; only the mission shifts.
            assert_eq!(profile.identity.name, "neenee", "{:?} kept the name", role);
            assert!(!profile.identity.mission.is_empty());
            assert_eq!(profile.name, role.as_str());
        }
    }

    #[test]
    fn code_role_is_unrestricted_baseline() {
        let base = AgentIdentity::new("neenee", "coding assistant");
        let code = PrincipalProfile::for_role(PrincipalRole::Code, &base);
        assert_eq!(code.agent_selection.scope, crate::ToolScope::All);
        assert!(code.operation_scope.paths.is_none());
        assert!(code.operation_scope.commands.is_none());
    }

    #[test]
    fn reviewer_role_is_read_only() {
        let base = AgentIdentity::new("neenee", "coding assistant");
        let reviewer = PrincipalProfile::for_role(PrincipalRole::Reviewer, &base);
        // Scoped: write/edit/bash are NOT admitted.
        let crate::ToolScope::Only(names) = &reviewer.agent_selection.scope else {
            panic!("reviewer must be scoped, not unrestricted");
        };
        assert!(!names.contains("write_file"));
        assert!(!names.contains("edit_file"));
        assert!(!names.contains("bash"));
        assert!(names.contains("read_text"));
    }

    #[test]
    fn security_role_confines_commands() {
        let base = AgentIdentity::new("neenee", "coding assistant");
        let security = PrincipalProfile::for_role(PrincipalRole::Security, &base);
        // bash IS admitted (for audit commands) but the command scope narrows it.
        let crate::ToolScope::Only(names) = &security.agent_selection.scope else {
            panic!("security must be scoped");
        };
        assert!(names.contains("bash"));
        let commands = security.operation_scope.commands.as_ref().unwrap();
        assert!(commands.allows("git log"));
        assert!(commands.allows("cargo audit"));
        assert!(!commands.allows("rm -rf /"));
    }
}
