//! Principal profiles: declarative top-level agent roles. The principal-side
//! mirror of [`crate::subagent::SubagentPreset`] (ADR-0053).
//!
//! ## Why this exists
//!
//! Before ADR-0053 the principal role was *imperative*: `muta`'s
//! `main.rs` hand-assembled the identity, left the capability scope at the
//! constructor default, left the write/command boundary unrestricted, and
//! seeded the runtime knobs from a single `[master]` config table. There was
//! no first-class object that *named* a principal role. Meanwhile the subagent
//! side has been declarative since ADR-0011: a role is a preset const that the
//! spawn tool binds.
//!
//! That asymmetry meant adding a principal instance (a new binary, a new
//! persona) duplicated assembly logic instead of binding a profile.
//!
//! `AgentPersona` closes the gap: a principal role is a value the embedding
//! binds via `Agent::apply_preset` (re-exported
//! through the agent crate), exactly as the spawn tool binds a
//! [`crate::SubagentPreset`]. Both live in core as vocabulary so the engine stays
//! role-agnostic and ADR-0042's role taxonomy is declared in one place.

use crate::{AgentIdentity, ToolScope, ToolSelection};

/// User-tunable principal *runtime* behaviour — the declarative form of the
/// values `muta`'s `main.rs` used to seed imperatively from the
/// `[master]` config table. Mirrors the subset of [`crate::SubagentPreset`]
/// that concerns execution knobs (hard stop, doom-loop guard, model-stdin)
/// rather than the tool set, which lives directly on [`AgentPersona::tools`].
///
/// Defaults match [`crate::DoomGuardConfig`] / the constructor's built-in values
/// so a profile with [`AgentRuntimeConfig::default`] is a no-op over the
/// agent constructor's defaults.
///
/// `Copy` because every field is (`DoomGuardConfig` is `Copy`, the two scalars
/// trivially so) — a profile can be read and re-seeded cheaply.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AgentRuntimeConfig {
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
/// principal-side mirror of [`crate::SubagentPreset`] (ADR-0053).
///
/// A profile is a value the embedding binds after constructing the agent via
/// `Agent::apply_preset` (re-exported through the
/// agent crate). The built-in coding principal lives in the application layer
/// (`muta`'s `identity` module, `agent_code()` — ADR-0054); a future
/// quant/research/ops principal is another value.
///
/// Unlike [`crate::SubagentPreset`] (a `Copy` `const` of `&'static` slices), this
/// owns `String`s / `Vec`s because [`AgentIdentity`] and tool selections do.
/// That is fine — a principal is constructed once at startup, not per-spawn.
///
/// ## Identity is supplied at construction, not applied
///
/// [`AgentIdentity`] feeds the system-prompt preamble and is immutable past the
/// `Agent` constructor, so it is passed to `Agent::new` / `from_toolset`, not
/// set by `Agent::apply_preset`. A role whose
/// identity should differ per instance (side conversations, group chat) composes
/// the profile with [`Self::with_identity`] before construction.
/// A declarative agent persona: an identity, its admitted tools, execution
/// knobs, and equipped atomic extensions (ADR-0223/0224/0225).
#[derive(Debug, Clone)]
pub struct AgentPersona {
    /// The role's name, e.g. `"developer"`.
    pub name: &'static str,
    /// Who this principal is and what it is for (name + mission + optional persona).
    pub identity: AgentIdentity,
    /// The tools this persona admits from the pool (ADR-0041); the capability
    /// itself (ADR-0223).
    pub tools: ToolSelection,
    /// Runtime execution knobs (hard stop, doom guard, model stdin).
    pub config: AgentRuntimeConfig,
    /// Whether this principal runs in unattended execution mode.
    pub unattended: bool,
    /// Atomic extensions equipped by this persona (ADR-0224).
    pub extensions: Vec<std::sync::Arc<dyn crate::Extension>>,
}

impl AgentPersona {
    /// Build a role from an identity with full default scope and attended behaviour.
    pub fn with_identity(name: &'static str, identity: AgentIdentity) -> Self {
        Self {
            name,
            identity,
            tools: ToolSelection::unrestricted(),
            config: AgentRuntimeConfig::default(),
            unattended: false,
            extensions: Vec::new(),
        }
    }

    /// Attach an atomic extension to this persona (ADR-0224).
    pub fn with_extension(mut self, extension: std::sync::Arc<dyn crate::Extension>) -> Self {
        self.extensions.push(extension);
        self
    }

    /// Attach atomic extensions to this persona (ADR-0224).
    pub fn with_extensions(
        mut self,
        extensions: impl IntoIterator<Item = std::sync::Arc<dyn crate::Extension>>,
    ) -> Self {
        for extension in extensions {
            self.extensions.push(extension);
        }
        self
    }

    /// Role for standard developer master (native tools, full delegation, ADR-0211).
    pub fn role_developer() -> Self {
        Self::developer()
    }

    /// Role for code analyst master (sandbox execution, contained delegation, ADR-0211).
    pub fn role_code_analyst() -> Self {
        Self::code_analyst()
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
        .with_tools(AGENT_CODE_ANALYST.selection())
    }

    /// Narrow the capability scope (the scope axis of ADR-0041). Builder-style.
    pub fn with_tools(mut self, selection: ToolSelection) -> Self {
        self.tools = selection;
        self
    }

    /// Attach the runtime knobs. Builder-style.
    pub fn with_runtime_config(mut self, config: AgentRuntimeConfig) -> Self {
        self.config = config;
        self
    }

    /// Run attended (`false`, the default) or in unattended
    /// execution mode (`true`).
    pub fn with_unattended(mut self, unattended: bool) -> Self {
        self.unattended = unattended;
        self
    }
}

/// A named principal *role* a user can switch the live agent into at runtime
/// via `@role:{role}` or `/role <role>` (legacy `@master:` / `/master` are
/// accepted as aliases). Each focused role installs an imperative role
/// directive (never a "You are …" self-description) and narrows the capability
/// scope / operation boundary to match that role's contract.
///
/// The roles live in `muta-contracts` (shared vocabulary) rather than the
/// application layer so both the CLI and the server offer the same set without
/// duplicating definitions. They are *presets* over a base [`AgentIdentity`]:
/// [`AgentPersonaId::Code`] keeps the base untouched (the shipped coding CLI
/// ships an empty one, so the baseline prompt carries no identity line at
/// all), while a focused role replaces the identity with its directive. The
/// directive is the one framing line worth its tokens — it tells a switched
/// agent how to behave when the baseline policy would not.
///
/// Use [`AgentPersona::from_preset`] to materialize a role onto a base
/// identity.
/// Roles an interactive master agent may switch into (ADR-0183 / ADR-0211).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentPersonaId {
    /// The default coding principal — full capabilities, unrestricted writes,
    /// the embedding's own identity (empty for the shipped CLI). Identical in
    /// effect to the profile the embedding binds at startup, so switching back
    /// to `code` after another role restores the baseline and clears any role
    /// directive.
    Code,
    /// Code analyst: deep codebase analysis and sandbox testing without host execution.
    CodeAnalyst,
    /// Architect: design and review focus. Full read access, write tools
    /// retained but the directive steers toward analysis, tradeoff evaluation,
    /// and written design rationale before any change.
    Architect,
    /// Reviewer: read-only code review. Read/search/inspect tools only — no
    /// `write_file`, `edit_text`, or `execute_command`. The directive tells it
    /// to report findings and proposed diffs without applying them.
    Reviewer,
    /// Security auditor: read-only, command-confined. Read/search tools plus a
    /// narrow command allowlist (`git`, `rg`, `cargo audit`-class inspection).
    /// The directive focuses it on vulnerability and supply-chain review.
    Security,
    /// Conversational principal: a workspace-free identity (practice partner,
    /// tutor, companion). No filesystem or command tools; web + ask_user only
    /// (ADR-0220 §3).
    Conversational,
}

impl AgentPersonaId {
    /// Every role in its canonical display order, for pickers and `/help`.
    pub const ALL: &[AgentPersonaId] = &[
        AgentPersonaId::Code,
        AgentPersonaId::CodeAnalyst,
        AgentPersonaId::Architect,
        AgentPersonaId::Reviewer,
        AgentPersonaId::Security,
        AgentPersonaId::Conversational,
    ];

    /// The stable string name used in `@role:{name}` / `/role <name>`. Legacy
    /// `@master:{name}` / `/master <name>` are accepted as aliases.
    pub fn as_str(self) -> &'static str {
        match self {
            AgentPersonaId::Code => "code",
            AgentPersonaId::CodeAnalyst => "code_analyst",
            AgentPersonaId::Architect => "architect",
            AgentPersonaId::Reviewer => "reviewer",
            AgentPersonaId::Security => "security",
            AgentPersonaId::Conversational => "conversational",
        }
    }

    /// Parse a role name (case-insensitive). Returns `None` for an unknown
    /// name so the caller can surface a clear "unknown role" error listing
    /// [`AgentPersonaId::ALL`].
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "code" | "coder" | "developer" | "dev" | "default" => Some(AgentPersonaId::Code),
            "code_analyst" | "analyst" | "analysis" => Some(AgentPersonaId::CodeAnalyst),
            "architect" | "architecture" => Some(AgentPersonaId::Architect),
            "reviewer" | "review" => Some(AgentPersonaId::Reviewer),
            "security" | "audit" | "auditor" => Some(AgentPersonaId::Security),
            "conversational" | "chat" | "companion" | "tutor" => {
                Some(AgentPersonaId::Conversational)
            }
            _ => None,
        }
    }

    /// A short human description of what this role does, for confirmations.
    pub fn description(self) -> &'static str {
        match self {
            AgentPersonaId::Code => "the default developer master (full native capabilities)",
            AgentPersonaId::CodeAnalyst => {
                "code analyst (read-only analysis & sandboxed execution)"
            }
            AgentPersonaId::Architect => "architecture & design focus (analysis-first)",
            AgentPersonaId::Reviewer => "read-only code review",
            AgentPersonaId::Security => "read-only security audit (command-confined)",
            AgentPersonaId::Conversational => {
                "workspace-free conversation (no filesystem or command tools)"
            }
        }
    }
}

/// The developer master preset: native toolchain authority (ADR-0144 §3).
///
/// This is the *default* master — identical in capability to the historical
/// `Code` principal: unrestricted scope, host command execution, the full subagent
/// catalog. What it adds over `from_preset(Code, …)` is the explicit subagent
/// delegation set, which the master-facing subagent dispatch consults to decide
/// which [`crate::SubagentPreset`]s may be loaded.
pub const AGENT_DEVELOPER: AgentPersonaDelegation = AgentPersonaDelegation {
    preset_id: "developer",
    // Subagent presets a developer master may load: the full catalog, since the
    // developer owns host execution and may spawn write-capable subagents.
    subagent_presets: &[
        crate::subagent::SUBAGENT_EXPLORE.name,
        crate::subagent::SUBAGENT_TITLE.name,
        crate::subagent::SUBAGENT_CODE.name,
        crate::subagent::SUBAGENT_MCP_SPECIALIST.name,
        crate::subagent::SUBAGENT_SKILL.name,
    ],
    // Declared tools: everything (the host `execute_command` variant included).
    tool_scope: ToolScope::All,
};

/// The code-analyst master preset: no host command execution (ADR-0144 §3).
///
/// The analyst pins `execute_command` to its workspace-contained variant:
/// suitable for `cargo metadata`-class probes and basic functional tests, never host
/// writes. Its subagent delegation is restricted to read-only presets — an
/// analyst must not be able to spawn a write-capable subagent and thereby
/// regain the write authority its own preset denies.
pub const AGENT_CODE_ANALYST: AgentPersonaDelegation = AgentPersonaDelegation {
    preset_id: "code_analyst",
    subagent_presets: &[
        crate::subagent::SUBAGENT_EXPLORE.name,
        crate::subagent::SUBAGENT_TITLE.name,
        crate::subagent::SUBAGENT_SKILL.name,
    ],
    tool_scope: ToolScope::All,
};

/// The delegation face of an agent role (ADR-0144 §3 / ADR-0211): which subagents
/// it may load, and the tool scope it declares against the pool.
#[derive(Debug, Clone)]
pub struct AgentPersonaDelegation {
    /// Stable id (also the `[master] role = "…"` config value).
    pub preset_id: &'static str,
    /// Subagent names this master may load, in preference order.
    pub subagent_presets: &'static [&'static str],
    /// The tool scope this master declares against the pool.
    pub tool_scope: ToolScope,
}

impl AgentPersonaDelegation {
    /// Whether a master bound to this preset may load the subagent preset
    /// named `name`.
    pub fn admits_subagent(&self, name: &str) -> bool {
        self.subagent_presets.contains(&name)
    }

    /// All shipping master delegations, developer first (the default).
    pub const ALL: &'static [AgentPersonaDelegation] = &[AGENT_DEVELOPER, AGENT_CODE_ANALYST];

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
        "get_outline",
        "run_command",
        "edit_text",
        "write_file",
        "read_url",
        "search_web",
        "todo",
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

/// A focused role's imperative directive, rendered verbatim as the opening
/// system-prompt line (`from_persona` bypasses `name`/`mission` composition).
/// A directive is instruction, not self-description: it opens with what to do,
/// never with "You are …".
fn role_directive(text: &str) -> AgentIdentity {
    AgentIdentity::from_persona(text)
}

impl AgentPersona {
    /// Materialize a [`AgentPersonaId`] onto a product's base [`AgentIdentity`].
    ///
    /// [`AgentPersonaId::Code`] clones the base identity unchanged (empty for
    /// the shipped coding CLI, so the baseline prompt carries no identity line
    /// at all). A focused role replaces it with an imperative **role
    /// directive** — `from_persona`, so the text lands verbatim as the
    /// preamble — because that role's behaviour is not expressible through
    /// capability scope alone. Capability scope and operation boundary narrow
    /// per the role's contract (see [`AgentPersonaId`]).
    ///
    /// Runtime config and the attended flag are left at defaults; the live
    /// `[master]` config overlay and the current attended setting are not
    /// disturbed by a role switch.
    pub fn from_preset(role: AgentPersonaId, base: &AgentIdentity) -> Self {
        match role {
            AgentPersonaId::Code => Self::with_identity("code", base.clone()),
            AgentPersonaId::CodeAnalyst => {
                let identity = role_directive(
                    "Role: code analyst. Explore the codebase deeply, analyse syntax and \
                     structure, and run only sandboxed functional tests — never mutate the host.",
                );
                let mut preset = Self::with_identity("code_analyst", identity);
                preset.tools = AGENT_CODE_ANALYST.selection();
                preset
            }
            AgentPersonaId::Architect => {
                let identity = role_directive(
                    "Role: software architect. Evaluate design tradeoffs, propose structure, \
                     and write design rationale before changing code.",
                );
                Self::with_identity("architect", identity)
            }
            AgentPersonaId::Reviewer => {
                // Read-only inspection tools. `run_command` is excluded: a reviewer
                // reports findings, it does not execute arbitrary commands.
                let identity = role_directive(
                    "Role: code reviewer. Report findings and proposed diffs; never apply changes.",
                );
                Self::with_identity("reviewer", identity).with_tools(ToolSelection::only([
                    "read_text",
                    "find_files",
                    "list_dir",
                    "read_image",
                    "search_text",
                    "read_url",
                    "search_web",
                    "todo",
                    "ask_user",
                ]))
            }
            AgentPersonaId::Security => {
                // Read-only, plus a confined command allowlist for audit-style
                // inspection (version control, search, dependency audit).
                let identity = role_directive(
                    "Role: security auditor. Review for vulnerabilities and supply-chain risk; \
                     do not modify the project.",
                );
                Self::with_identity("security", identity).with_tools(ToolSelection::only([
                    "read_text",
                    "find_files",
                    "list_dir",
                    "read_image",
                    "search_text",
                    "run_command",
                    "read_url",
                    "search_web",
                    "todo",
                    "ask_user",
                ]))
            }
            AgentPersonaId::Conversational => {
                let identity = role_directive(
                    "Role: conversational partner. Hold a focused conversation, keep turns \
                     short, and stay on the user's topic. Do not modify files or run commands.",
                );
                Self::with_identity("conversational", identity).with_tools(ToolSelection::only([
                    "read_url",
                    "search_web",
                    "ask_user",
                ]))
            }
        }
    }
}

/// Alias for [`AgentPersonaDelegation`].
pub type DelegationPolicy = AgentPersonaDelegation;

impl AgentPersona {
    /// Preset developer policy (associated constant).
    pub const DEVELOPER: DelegationPolicy = AGENT_DEVELOPER;
    /// Preset code-analyst policy (associated constant).
    pub const CODE_ANALYST: DelegationPolicy = AGENT_CODE_ANALYST;
}

/// Canonical alias for [`AGENT_DEVELOPER`].
pub const PRESET_DEVELOPER: DelegationPolicy = AGENT_DEVELOPER;
/// Canonical alias for [`AGENT_CODE_ANALYST`].
pub const PRESET_CODE_ANALYST: DelegationPolicy = AGENT_CODE_ANALYST;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentIdentity;

    #[test]
    fn with_identity_is_unrestricted_and_attended() {
        let p = AgentPersona::with_identity("code", AgentIdentity::new("n", "m"));
        assert_eq!(p.name, "code");
        assert!(!p.unattended);
        // unrestricted selection ⇒ All scope, empty variant pins
        assert_eq!(p.tools.scope, crate::ToolScope::All);
        assert!(p.tools.variants.is_empty());
        // default runtime config
        assert_eq!(p.config.hard_stop_turns, 0);
        assert!(!p.config.allow_model_stdin);
        assert!(!p.config.skip_interactive_input);
        assert_eq!(p.config.nudge, crate::DoomGuardConfig::default());
    }

    #[test]
    fn builders_override_defaults() {
        let p = AgentPersona::with_identity("ops", AgentIdentity::default())
            .with_unattended(true)
            .with_runtime_config(AgentRuntimeConfig {
                hard_stop_turns: 7,
                ..Default::default()
            });
        assert!(p.unattended);
        assert_eq!(p.config.hard_stop_turns, 7);
    }

    #[test]
    fn runtime_config_is_copy() {
        let c = AgentRuntimeConfig::default();
        let _copy = c; // Copy: no move
        let _again = c;
    }

    #[test]
    fn role_round_trips_through_parse() {
        for role in AgentPersonaId::ALL {
            let parsed = AgentPersonaId::parse(role.as_str());
            assert_eq!(parsed, Some(*role), "{} should parse back", role.as_str());
        }
        // Aliases.
        assert_eq!(AgentPersonaId::parse("Coder"), Some(AgentPersonaId::Code));
        assert_eq!(
            AgentPersonaId::parse("REVIEW"),
            Some(AgentPersonaId::Reviewer)
        );
        assert_eq!(
            AgentPersonaId::parse("auditor"),
            Some(AgentPersonaId::Security)
        );
        // Unknown.
        assert!(AgentPersonaId::parse("wizard").is_none());
    }

    #[test]
    fn code_role_preserves_base_identity_and_focused_roles_install_a_directive() {
        // `code` is the baseline: it clones the embedding's identity untouched,
        // so switching back after another role restores it.
        let base = AgentIdentity::from_mission("an expert AI coding assistant");
        let code = AgentPersona::from_preset(AgentPersonaId::Code, &base);
        assert_eq!(code.identity.preamble(), base.preamble());
        assert_eq!(code.name, "code");

        // Focused roles replace it with an imperative directive — instruction,
        // never a "You are …" self-description.
        for role in [
            AgentPersonaId::CodeAnalyst,
            AgentPersonaId::Architect,
            AgentPersonaId::Reviewer,
            AgentPersonaId::Security,
        ] {
            let profile = AgentPersona::from_preset(role, &base);
            assert_eq!(profile.name, role.as_str());
            let directive = profile.identity.preamble();
            assert!(
                directive.starts_with("Role: "),
                "{role:?} directive: {directive}"
            );
            assert!(
                !directive.starts_with("You are"),
                "{role:?} must not self-describe: {directive}"
            );
            assert!(directive.ends_with('.'), "{role:?} directive: {directive}");
        }
    }

    #[test]
    fn code_role_is_unrestricted_baseline() {
        let base = AgentIdentity::from_mission("coding assistant");
        let code = AgentPersona::from_preset(AgentPersonaId::Code, &base);
        assert_eq!(code.tools.scope, crate::ToolScope::All);
    }

    #[test]
    fn reviewer_role_is_read_only() {
        let base = AgentIdentity::from_mission("coding assistant");
        let reviewer = AgentPersona::from_preset(AgentPersonaId::Reviewer, &base);
        // Scoped: write/edit/command execution are NOT admitted.
        let crate::ToolScope::Only(names) = &reviewer.tools.scope else {
            panic!("reviewer must be scoped, not unrestricted");
        };
        assert!(!names.contains("write_file"));
        assert!(!names.contains("edit_text"));
        assert!(!names.contains("run_command"));
        assert!(names.contains("read_text"));
    }

    #[test]
    fn security_role_admits_audit_tools() {
        // ADR-0223: the persona-level command allowlist is retired; the tool
        // surface is the capability. Security admits read/inspect tools plus
        // `run_command`.
        let base = AgentIdentity::from_mission("coding assistant");
        let security = AgentPersona::from_preset(AgentPersonaId::Security, &base);
        let crate::ToolScope::Only(names) = &security.tools.scope else {
            panic!("security must be scoped");
        };
        assert!(names.contains("run_command"));
        assert!(names.contains("read_text"));
        assert!(!names.contains("write_file"));
        assert!(!names.contains("edit_text"));
    }
}
