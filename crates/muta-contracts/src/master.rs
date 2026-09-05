//! Principal profiles: declarative top-level agent roles. The principal-side
//! mirror of [`crate::runner::RunnerPreset`] (ADR-0053).
//!
//! ## Why this exists
//!
//! Before ADR-0053 the principal role was *imperative*: `muta`'s
//! `main.rs` hand-assembled the identity, left the capability scope at the
//! constructor default, left the write/command boundary unrestricted, and
//! seeded the runtime knobs from a single `[master]` config table. There was
//! no first-class object that *named* a principal role. Meanwhile the envoy
//! side has been declarative since ADR-0011: a role is a `const EnvoyProfile`,
//! and `EnvoyTool` binds it.
//!
//! That asymmetry meant adding a principal instance (a new binary, a new
//! persona) duplicated assembly logic instead of binding a profile.
//!
//! `MasterPreset` closes the gap: a principal role is a value the embedding
//! binds via `Agent::apply_master_preset` (re-exported
//! through the agent crate), exactly as `EnvoyTool` binds an
//! [`crate::RunnerPreset`]. Both live in core as vocabulary so the engine stays
//! role-agnostic and ADR-0042's role taxonomy is declared in one place.

use crate::{AgentIdentity, CommandScope, OperationScope, ToolScope, ToolSelection};

/// User-tunable principal *runtime* behaviour — the declarative form of the
/// values `muta`'s `main.rs` used to seed imperatively from the
/// `[master]` config table. Mirrors the subset of [`crate::RunnerPreset`]
/// that concerns execution knobs (hard stop, doom-loop guard, model-stdin)
/// rather than capability scope, which lives directly on
/// [`MasterPreset::agent_selection`] / [`MasterPreset::operation_scope`].
///
/// Defaults match [`crate::DoomGuardConfig`] / the constructor's built-in values
/// so a profile with [`MasterRuntimeConfig::default`] is a no-op over the
/// agent constructor's defaults.
///
/// `Copy` because every field is (`DoomGuardConfig` is `Copy`, the two scalars
/// trivially so) — a profile can be read and re-seeded cheaply.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MasterRuntimeConfig {
    /// Opt-in hard-stop budget: abort a round after this many ReAct turns.
    /// `0` (the default) means uncapped. Mirrors `[master] hard_stop_turns`
    /// and `Agent::set_hard_stop_turns`.
    pub hard_stop_turns: usize,
    /// Doom-loop guard config. Mirrors `[principal.doom_guard]` (the
    /// historical `nudge` spelling) and
    /// `Agent::set_doom_guard_config`. Default disabled.
    pub nudge: crate::DoomGuardConfig,
    /// Whether the model may supply stdin bytes for an `execute_command` call it emits.
    /// Mirrors `[master] allow_model_stdin` and
    /// `Agent::set_allow_model_stdin`. Default `false`.
    pub allow_model_stdin: bool,
    /// Whether an interactive `execute_command` call skips the inline input panel and
    /// instead runs with stdin closed (fast failure + non-interactive remedy).
    /// Mirrors `[master] skip_interactive_input` and
    /// `Agent::set_skip_interactive_input`. Default `false`.
    pub skip_interactive_input: bool,
}

/// A declarative principal role: an identity, the capability scope it admits,
/// the write/command boundary it enforces, and its runtime knobs. The
/// principal-side mirror of [`crate::RunnerPreset`] (ADR-0053).
///
/// A profile is a value the embedding binds after constructing the agent via
/// `Agent::apply_master_preset` (re-exported through the
/// agent crate). The built-in coding principal lives in the application layer
/// (`muta`'s `identity` module, `master_developer()` — ADR-0054); a future
/// quant/research/ops principal is another value.
///
/// Unlike [`crate::RunnerPreset`] (a `Copy` `const` of `&'static` slices), this
/// owns `String`s / `Vec`s because [`AgentIdentity`] and [`OperationScope`] do.
/// That is fine — a principal is constructed once at startup, not per-spawn.
///
/// ## Identity is supplied at construction, not applied
///
/// [`AgentIdentity`] feeds the system-prompt preamble and is immutable past the
/// `Agent` constructor, so it is passed to `Agent::new` / `from_toolset`, not
/// set by `Agent::apply_master_preset`. A role whose
/// identity should differ per instance (side conversations, group chat) composes
/// the profile with [`Self::with_identity`] before construction.
#[derive(Debug, Clone)]
pub struct MasterPreset {
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
    pub config: MasterRuntimeConfig,
    /// Whether this principal runs in delegated autonomous execution mode
    /// (auto-approves all tool permissions). Default `false` — a top-level
    /// principal is interactive by contract.
    pub delegated: bool,
}

impl MasterPreset {
    /// Build a profile from an identity with full default scope and attended
    /// behaviour — the common case for a new coding-class principal. Compose
    /// further with the `with_*` builders.
    pub fn with_identity(name: &'static str, identity: AgentIdentity) -> Self {
        Self {
            name,
            identity,
            agent_selection: ToolSelection::unrestricted(),
            operation_scope: OperationScope::unrestricted(),
            config: MasterRuntimeConfig::default(),
            delegated: false,
        }
    }

    /// Preset for standard developer master (native tools, full delegation).
    pub fn developer() -> Self {
        Self::with_identity(
            "developer",
            AgentIdentity::new(
                "developer",
                "an expert AI software engineer with native tool access",
            ),
        )
    }

    /// Preset for code analyst master (sandbox execution, contained delegation).
    pub fn code_analyst() -> Self {
        Self::with_identity(
            "code_analyst",
            AgentIdentity::new(
                "code_analyst",
                "a careful AI code analyst performing contained inspection and testing in sandbox",
            ),
        )
        .with_selection(MASTER_CODE_ANALYST.selection())
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
    pub fn with_runtime_config(mut self, config: MasterRuntimeConfig) -> Self {
        self.config = config;
        self
    }

    /// Run attended (`false`, the default) or in delegated autonomous
    /// execution mode (`true`).
    pub fn with_delegated(mut self, delegated: bool) -> Self {
        self.delegated = delegated;
        self
    }
}

/// A named principal *role* a user can switch the live agent into at runtime
/// via `@principal:{role}` or `/principal <role>` (plan §3.3). Each role
/// composes a focused persona onto the product's base identity and narrows the
/// capability scope / operation boundary to match that role's contract.
///
/// The roles live in `muta-contracts` (shared vocabulary) rather than the
/// application layer so both the CLI and the server offer the same set without
/// duplicating definitions. They are *presets* over a base
/// [`AgentIdentity`]: the product name ("muta") is preserved and only the
/// mission/persona shifts, so a switched role is still recognizably the same
/// product, just wearing a different hat.
///
/// Use [`MasterPreset::for_role`] to materialize a role onto a base
/// identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MasterPresetId {
    /// The default coding principal — full capabilities, unrestricted writes,
    /// the product's own mission. Identical in effect to the profile the
    /// embedding binds at startup, so switching back to `code` after another
    /// role restores the baseline.
    Code,
    /// Code analyst: deep codebase analysis and sandbox testing without host execution.
    CodeAnalyst,
    /// Architect: design and review focus. Full read access, write tools
    /// retained but the persona steers toward analysis, tradeoff evaluation,
    /// and written design rationale before any change.
    Architect,
    /// Reviewer: read-only code review. Read/search/inspect tools only — no
    /// `write_file`, `edit_text`, or `execute_command`. The persona is a meticulous
    /// reviewer who reports findings and proposed diffs without applying them.
    Reviewer,
    /// Security auditor: read-only, command-confined. Read/search tools plus a
    /// narrow command allowlist (`git`, `rg`, `cargo audit`-class inspection).
    /// The persona focuses on vulnerability and supply-chain review.
    Security,
}

impl MasterPresetId {
    /// Every role in its canonical display order, for pickers and `/help`.
    pub const ALL: &[MasterPresetId] = &[
        MasterPresetId::Code,
        MasterPresetId::CodeAnalyst,
        MasterPresetId::Architect,
        MasterPresetId::Reviewer,
        MasterPresetId::Security,
    ];

    /// The stable string name used in `@master:{name}` / `/master <name>`.
    pub fn as_str(self) -> &'static str {
        match self {
            MasterPresetId::Code => "code",
            MasterPresetId::CodeAnalyst => "code_analyst",
            MasterPresetId::Architect => "architect",
            MasterPresetId::Reviewer => "reviewer",
            MasterPresetId::Security => "security",
        }
    }

    /// Parse a role name (case-insensitive). Returns `None` for an unknown
    /// name so the caller can surface a clear "unknown role" error listing
    /// [`MasterPresetId::ALL`].
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "code" | "coder" | "developer" | "dev" | "default" => Some(MasterPresetId::Code),
            "code_analyst" | "analyst" | "analysis" => Some(MasterPresetId::CodeAnalyst),
            "architect" | "architecture" => Some(MasterPresetId::Architect),
            "reviewer" | "review" => Some(MasterPresetId::Reviewer),
            "security" | "audit" | "auditor" => Some(MasterPresetId::Security),
            _ => None,
        }
    }

    /// A short human description of what this role does, for confirmations.
    pub fn description(self) -> &'static str {
        match self {
            MasterPresetId::Code => "the default developer master (full native capabilities)",
            MasterPresetId::CodeAnalyst => {
                "code analyst (read-only analysis & sandboxed execution)"
            }
            MasterPresetId::Architect => "architecture & design focus (analysis-first)",
            MasterPresetId::Reviewer => "read-only code review",
            MasterPresetId::Security => "read-only security audit (command-confined)",
        }
    }
}

/// The developer master preset: native toolchain authority (ADR-0144 §3).
///
/// This is the *default* master — identical in capability to the historical
/// `Code` principal: unrestricted scope, host command execution, the full runner
/// catalog. What it adds over `for_role(Code, …)` is the explicit runner
/// delegation set, which the master-facing runner dispatch consults to decide
/// which [`crate::RunnerPreset`]s may be loaded.
pub const MASTER_DEVELOPER: MasterPresetDelegation = MasterPresetDelegation {
    preset_id: "developer",
    // Runner presets a developer master may load: the full catalog, since the
    // developer owns host execution and may spawn write-capable runners.
    runner_presets: &[
        crate::runner::RUNNER_EXPLORE.name,
        crate::runner::RUNNER_TITLE.name,
        crate::runner::RUNNER_CODE.name,
        crate::runner::RUNNER_MCP_SPECIALIST.name,
        crate::runner::RUNNER_SKILL.name,
    ],
    // Declared tools: everything (the host `execute_command` variant included).
    tool_scope: ToolScope::All,
};

/// The code-analyst master preset: no host command execution (ADR-0144 §3).
///
/// The analyst pins `execute_command` to its workspace-contained variant:
/// suitable for `cargo metadata`-class probes and basic functional tests, never host
/// writes. Its runner delegation is restricted to read-only presets — an
/// analyst must not be able to spawn a write-capable runner and thereby
/// regain the write authority its own preset denies.
pub const MASTER_CODE_ANALYST: MasterPresetDelegation = MasterPresetDelegation {
    preset_id: "code_analyst",
    runner_presets: &[
        crate::runner::RUNNER_EXPLORE.name,
        crate::runner::RUNNER_TITLE.name,
        crate::runner::RUNNER_SKILL.name,
    ],
    tool_scope: ToolScope::All,
};

/// The delegation face of a master preset (ADR-0144 §3): which runner
/// presets it may load, and the tool scope it declares against the pool.
///
/// The persona/identity half of a master lives in [`MasterPreset`] (via
/// [`MasterPreset::for_role`]); this half answers the tier question — *what
/// may this master delegate downwards* — which the identity object has no
/// field for. Binding both halves together happens at session assembly.
#[derive(Debug, Clone)]
pub struct MasterPresetDelegation {
    /// Stable id (also the `[master] preset = "…"` config value).
    pub preset_id: &'static str,
    /// Runner preset names this master may load, in preference order.
    pub runner_presets: &'static [&'static str],
    /// The tool scope this master declares against the pool.
    pub tool_scope: ToolScope,
}

impl MasterPresetDelegation {
    /// Whether a master bound to this preset may load the runner preset
    /// named `name`.
    pub fn admits_runner(&self, name: &str) -> bool {
        self.runner_presets.contains(&name)
    }

    /// All shipping master delegations, developer first (the default).
    pub const ALL: &'static [MasterPresetDelegation] = &[MASTER_DEVELOPER, MASTER_CODE_ANALYST];

    /// The code-analyst's tool declaration: the full read/analyze surface
    /// plus the workspace-contained `execute_command` variant.
    ///
    /// `ToolScope::Only` holds a `BTreeSet` and cannot be spelled in a
    /// `const`, so the flat names live here; [`Self::selection`] builds the
    /// [`ToolScope`] lazily from them.
    pub const CODE_ANALYST_TOOLS: &'static [&'static str] = &[
        "read_text",
        "find_files",
        "list_dir",
        "read_image",
        "search_text",
        "run_command",
        "edit_text",
        "write_file",
        "read_url",
        "search_web",
        "write_todos",
        "update_todo",
        "ask_user",
    ];

    /// The concrete tool names this preset declares against the pool:
    /// `None` for [`ToolScope::All`] (the pool's whole catalog), `Some`
    /// for an explicit list. Introspected by the pool resolver and the UI.
    pub fn declared_tools(&self) -> Option<&'static [&'static str]> {
        match self.preset_id {
            "code_analyst" => Some(Self::CODE_ANALYST_TOOLS),
            _ => None,
        }
    }

    /// The [`ToolSelection`] this preset declares to the pool resolver.
    pub fn selection(&self) -> ToolSelection {
        let mut selection = match self.declared_tools() {
            None => ToolSelection::unrestricted(),
            Some(names) => ToolSelection::only(names.iter().copied()),
        };
        if self.preset_id == "code_analyst" {
            selection
                .variants
                .insert("run_command".to_string(), "workspace".to_string());
        }
        selection
    }
}

impl MasterPreset {
    /// Materialize a [`MasterPresetId`] onto a product's base [`AgentIdentity`].
    ///
    /// The base identity's `name` is preserved (the agent is still called
    /// "muta"); only the mission — and, for focused roles, a persona
    /// override — shifts to match the role. Capability scope and operation
    /// boundary narrow per the role's contract (see [`MasterPresetId`]).
    ///
    /// Runtime config and the attended flag are left at defaults; the live
    /// `[master]` config overlay and the current attended setting are not
    /// disturbed by a role switch.
    pub fn for_role(role: MasterPresetId, base: &AgentIdentity) -> Self {
        match role {
            MasterPresetId::Code => Self::with_identity("code", base.clone()),
            MasterPresetId::CodeAnalyst => {
                let identity = AgentIdentity::new(
                    base.name.clone(),
                    "an expert code analyst — conducts deep codebase exploration, syntax analysis, \
                     and sandboxed functional tests without host mutation",
                );
                let mut preset = Self::with_identity("code_analyst", identity);
                preset.agent_selection = MASTER_CODE_ANALYST.selection();
                preset
            }
            MasterPresetId::Architect => {
                let identity = AgentIdentity::new(
                    base.name.clone(),
                    "an expert software architect — evaluates design tradeoffs, \
                     proposes structure, and writes design rationale before changing code",
                );
                Self::with_identity("architect", identity)
            }
            MasterPresetId::Reviewer => {
                // Read-only inspection tools. `run_command` is excluded: a reviewer
                // reports findings, it does not execute arbitrary commands.
                let identity = AgentIdentity::new(
                    base.name.clone(),
                    "a meticulous code reviewer — reports findings and proposed \
                     diffs without applying changes",
                );
                Self::with_identity("reviewer", identity).with_selection(ToolSelection::only([
                    "read_text",
                    "find_files",
                    "list_dir",
                    "read_image",
                    "search_text",
                    "read_url",
                    "search_web",
                    "write_todos",
                    "update_todo",
                    "ask_user",
                ]))
            }
            MasterPresetId::Security => {
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
                        "find_files",
                        "list_dir",
                        "read_image",
                        "search_text",
                        "run_command",
                        "read_url",
                        "search_web",
                        "write_todos",
                        "update_todo",
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
        let p = MasterPreset::with_identity("code", AgentIdentity::new("n", "m"));
        assert_eq!(p.name, "code");
        assert!(!p.delegated);
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
        let p = MasterPreset::with_identity("ops", AgentIdentity::default())
            .with_delegated(true)
            .with_runtime_config(MasterRuntimeConfig {
                hard_stop_turns: 7,
                ..Default::default()
            });
        assert!(p.delegated);
        assert_eq!(p.config.hard_stop_turns, 7);
    }

    #[test]
    fn runtime_config_is_copy() {
        let c = MasterRuntimeConfig::default();
        let _copy = c; // Copy: no move
        let _again = c;
    }

    #[test]
    fn role_round_trips_through_parse() {
        for role in MasterPresetId::ALL {
            let parsed = MasterPresetId::parse(role.as_str());
            assert_eq!(parsed, Some(*role), "{} should parse back", role.as_str());
        }
        // Aliases.
        assert_eq!(MasterPresetId::parse("Coder"), Some(MasterPresetId::Code));
        assert_eq!(
            MasterPresetId::parse("REVIEW"),
            Some(MasterPresetId::Reviewer)
        );
        assert_eq!(
            MasterPresetId::parse("auditor"),
            Some(MasterPresetId::Security)
        );
        // Unknown.
        assert!(MasterPresetId::parse("wizard").is_none());
    }

    #[test]
    fn for_role_preserves_product_name() {
        let base = AgentIdentity::new("muta", "an expert AI coding assistant");
        for role in MasterPresetId::ALL {
            let profile = MasterPreset::for_role(*role, &base);
            // The product name survives a role switch; only the mission shifts.
            assert_eq!(profile.identity.name, "muta", "{:?} kept the name", role);
            assert!(!profile.identity.mission.is_empty());
            assert_eq!(profile.name, role.as_str());
        }
    }

    #[test]
    fn code_role_is_unrestricted_baseline() {
        let base = AgentIdentity::new("muta", "coding assistant");
        let code = MasterPreset::for_role(MasterPresetId::Code, &base);
        assert_eq!(code.agent_selection.scope, crate::ToolScope::All);
        assert!(code.operation_scope.paths.is_none());
        assert!(code.operation_scope.commands.is_none());
    }

    #[test]
    fn reviewer_role_is_read_only() {
        let base = AgentIdentity::new("muta", "coding assistant");
        let reviewer = MasterPreset::for_role(MasterPresetId::Reviewer, &base);
        // Scoped: write/edit/command execution are NOT admitted.
        let crate::ToolScope::Only(names) = &reviewer.agent_selection.scope else {
            panic!("reviewer must be scoped, not unrestricted");
        };
        assert!(!names.contains("write_file"));
        assert!(!names.contains("edit_text"));
        assert!(!names.contains("run_command"));
        assert!(names.contains("read_text"));
    }

    #[test]
    fn security_role_confines_commands() {
        let base = AgentIdentity::new("muta", "coding assistant");
        let security = MasterPreset::for_role(MasterPresetId::Security, &base);
        // Command execution is admitted for audits, but its scope is narrowed.
        let crate::ToolScope::Only(names) = &security.agent_selection.scope else {
            panic!("security must be scoped");
        };
        assert!(names.contains("run_command"));
        let commands = security.operation_scope.commands.as_ref().unwrap();
        assert!(commands.allows("git log"));
        assert!(commands.allows("cargo audit"));
        assert!(!commands.allows("rm -rf /"));
    }
}
