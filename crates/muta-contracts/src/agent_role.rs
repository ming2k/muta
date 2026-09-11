//! Agent role definitions: declarative principal and subagent profiles.
//!
//! Every agent is an instance of [`Agent<R: AgentRole>`]:
//! - [`MainAgent`]: interactive top-level agent staffed with a [`MainAgentRole`].
//! - [`SubAgent`]: autonomous delegated agent staffed with a [`SubAgentRole`].

use crate::{AgentIdentity, ToolScope, ToolSelection};

/// User-tunable agent runtime behaviour.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AgentRuntimeConfig {
    /// Opt-in hard-stop budget: abort a round after this many ReAct turns.
    pub hard_stop_turns: usize,
    /// Doom-loop guard config. Default disabled.
    pub nudge: crate::DoomGuardConfig,
    /// Whether the model may supply stdin bytes for an `execute_command` call. Default false.
    pub allow_model_stdin: bool,
    /// Whether an interactive `execute_command` call skips the inline input panel. Default false.
    pub skip_interactive_input: bool,
}

/// A declarative agent role profile: an identity, its admitted tools, execution
/// knobs, and equipped atomic extensions.
#[derive(Debug, Clone)]
pub struct AgentRoleProfile {
    /// The role's name, e.g. `"developer"`.
    pub name: &'static str,
    /// Who this principal is and what it is for.
    pub identity: AgentIdentity,
    /// The tools this role admits from the pool.
    pub tools: ToolSelection,
    /// Runtime execution knobs (hard stop, doom guard, model stdin).
    pub config: AgentRuntimeConfig,
    /// Whether this principal runs in unattended execution mode.
    pub unattended: bool,
    /// Atomic extensions equipped by this role.
    pub extensions: Vec<std::sync::Arc<dyn crate::Extension>>,
}

impl AgentRoleProfile {
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

    /// Attach an atomic extension to this role.
    pub fn with_extension(mut self, extension: std::sync::Arc<dyn crate::Extension>) -> Self {
        self.extensions.push(extension);
        self
    }

    /// Attach atomic extensions to this role.
    pub fn with_extensions(
        mut self,
        extensions: impl IntoIterator<Item = std::sync::Arc<dyn crate::Extension>>,
    ) -> Self {
        for extension in extensions {
            self.extensions.push(extension);
        }
        self
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

    /// Role for standard developer master.
    pub fn role_developer() -> Self {
        Self::developer()
    }

    /// Preset for philosophist role (workspace-free philosophical inquiry).
    pub fn philosophist() -> Self {
        let identity = role_directive(
            "Role: philosophist. Engage in deep philosophical inquiry, examine principles, \
             question assumptions, and explore ideas with clarity and nuance. Do not modify files or run commands.",
        );
        Self::with_identity("philosophist", identity).with_tools(ToolSelection::only([
            "read_url",
            "search_web",
            "ask_user",
        ]))
    }

    /// Narrow the capability scope. Builder-style.
    pub fn with_tools(mut self, selection: ToolSelection) -> Self {
        self.tools = selection;
        self
    }

    /// Attach the runtime knobs. Builder-style.
    pub fn with_runtime_config(mut self, config: AgentRuntimeConfig) -> Self {
        self.config = config;
        self
    }

    /// Run attended (`false`, the default) or in unattended execution mode (`true`).
    pub fn with_unattended(mut self, unattended: bool) -> Self {
        self.unattended = unattended;
        self
    }

    /// Materialize a [`MainAgentRole`] onto a product's base [`AgentIdentity`].
    pub fn from_role(role: MainAgentRole, base: &AgentIdentity) -> Self {
        match role {
            MainAgentRole::Developer => {
                let id = if base.preamble().is_empty() {
                    AgentIdentity::new(
                        "developer",
                        "an expert AI software engineer with native tool access",
                    )
                } else {
                    base.clone()
                };
                Self::with_identity("developer", id)
            }
            MainAgentRole::Philosophist => Self::philosophist(),
        }
    }

    /// Preset developer policy (associated constant).
    pub const DEVELOPER: AgentRoleDelegation = AGENT_ROLE_DEVELOPER;
    /// Preset philosophist policy (associated constant).
    pub const PHILOSOPHIST: AgentRoleDelegation = AGENT_ROLE_PHILOSOPHIST;
}

/// The common contract for any agent role (Main or Sub).
pub trait AgentRole: Send + Sync + 'static {
    /// Canonical name of this role.
    fn role_name(&self) -> &str;
    /// Whether this role admits direct interactive user input (e.g. `ask_user`).
    fn admits_user_interaction(&self) -> bool;
    /// The workspace root bound to this role, if any.
    fn workspace_root(&self) -> Option<&std::path::Path>;
    /// Tool selection admitted for this role.
    fn tool_selection(&self) -> ToolSelection;
}

/// Unified generic Agent structure: the single parent struct for both MainAgent and SubAgent.
#[derive(Debug, Clone)]
pub struct Agent<R: AgentRole> {
    /// Declarative profile and tools.
    pub profile: AgentRoleProfile,
    /// The specialized role staffing this agent.
    pub role: R,
}

impl<R: AgentRole> Agent<R> {
    /// Create a new agent from its profile and specialized role.
    pub fn new(profile: AgentRoleProfile, role: R) -> Self {
        Self { profile, role }
    }

    /// Access the agent's role.
    pub fn role(&self) -> &R {
        &self.role
    }

    /// Access the agent's role name.
    pub fn role_name(&self) -> &str {
        self.role.role_name()
    }
}

/// A top-level interactive agent staffed with a [`MainAgentRole`].
pub type MainAgent = Agent<MainAgentRole>;

/// An autonomous delegated subagent staffed with a [`SubAgentRole`].
pub type SubAgent = Agent<SubAgentRole>;

impl MainAgent {
    /// Switch this main agent's role.
    pub fn switch_role(&mut self, new_role: MainAgentRole) {
        self.role = new_role;
        self.profile.tools = self.role.tool_selection();
    }

    /// Spawn a delegated SubAgent under this MainAgent.
    pub fn spawn_subagent(&self, sub_role: SubAgentRole) -> SubAgent {
        let sub_profile = AgentRoleProfile::with_identity(
            sub_role.as_str(),
            AgentIdentity::from_mission(format!("Autonomous {} subagent", sub_role.as_str())),
        )
        .with_tools(sub_role.tool_selection());

        SubAgent::new(sub_profile, sub_role)
    }
}

/// Roles an interactive MainAgent may switch into.
/// Only two built-in roles exist: `developer` (workspace-bound) and `philosophist` (workspace-free).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainAgentRole {
    /// The default developer role (full native capabilities with workspace).
    Developer,
    /// Philosophist: philosophical inquiry & reflection (workspace-free).
    Philosophist,
}

impl MainAgentRole {
    /// Every main role in its canonical display order.
    pub const ALL: &[MainAgentRole] = &[
        MainAgentRole::Developer,
        MainAgentRole::Philosophist,
    ];

    /// The stable string name used in `/role <name>`.
    pub fn as_str(self) -> &'static str {
        match self {
            MainAgentRole::Developer => "developer",
            MainAgentRole::Philosophist => "philosophist",
        }
    }

    /// Parse a role name (case-insensitive). Returns `None` for an unknown name.
    pub fn parse(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "developer" | "dev" | "code" | "coder" | "default" => Some(MainAgentRole::Developer),
            "philosophist" | "philosopher" | "philosophy" | "conversational" | "chat"
            | "companion" | "tutor" => Some(MainAgentRole::Philosophist),
            _ => None,
        }
    }

    /// A short human description of what this role does, for confirmations.
    pub fn description(self) -> &'static str {
        match self {
            MainAgentRole::Developer => "the default developer role (full native capabilities with workspace)",
            MainAgentRole::Philosophist => {
                "philosophical inquiry & reflection (workspace-free)"
            }
        }
    }
}

impl AgentRole for MainAgentRole {
    fn role_name(&self) -> &str {
        self.as_str()
    }

    fn admits_user_interaction(&self) -> bool {
        true
    }

    fn workspace_root(&self) -> Option<&std::path::Path> {
        None
    }

    fn tool_selection(&self) -> ToolSelection {
        match self {
            MainAgentRole::Developer => ToolSelection::unrestricted(),
            MainAgentRole::Philosophist => ToolSelection::only([
                "read_url",
                "search_web",
                "ask_user",
            ]),
        }
    }
}

/// Delegated functional roles for autonomous SubAgents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubAgentRole {
    Explore,
    Code,
    Mcp,
    Skill,
}

impl SubAgentRole {
    pub const ALL: &[SubAgentRole] = &[
        SubAgentRole::Explore,
        SubAgentRole::Code,
        SubAgentRole::Mcp,
        SubAgentRole::Skill,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            SubAgentRole::Explore => "explore",
            SubAgentRole::Code => "code",
            SubAgentRole::Mcp => "mcp",
            SubAgentRole::Skill => "skill",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "explore" => Some(SubAgentRole::Explore),
            "code" => Some(SubAgentRole::Code),
            "mcp" => Some(SubAgentRole::Mcp),
            "skill" => Some(SubAgentRole::Skill),
            _ => None,
        }
    }
}

impl AgentRole for SubAgentRole {
    fn role_name(&self) -> &str {
        self.as_str()
    }

    fn admits_user_interaction(&self) -> bool {
        false
    }

    fn workspace_root(&self) -> Option<&std::path::Path> {
        None
    }

    fn tool_selection(&self) -> ToolSelection {
        match self {
            SubAgentRole::Explore => ToolSelection::only([
                "read_text",
                "find_files",
                "list_dir",
                "read_image",
                "search_text",
                "code_query",
                "read_url",
                "search_web",
            ]),
            SubAgentRole::Code => ToolSelection::only([
                "read_text",
                "find_files",
                "list_dir",
                "read_image",
                "search_text",
                "code_query",
                "edit_text",
                "write_file",
                "run_command",
                "read_url",
                "search_web",
            ]),
            SubAgentRole::Mcp => ToolSelection::unrestricted(),
            SubAgentRole::Skill => ToolSelection::only([
                "read_text",
                "find_files",
                "list_dir",
                "search_text",
            ]),
        }
    }
}

/// The developer master preset: native toolchain authority.
pub const AGENT_ROLE_DEVELOPER: AgentRoleDelegation = AgentRoleDelegation {
    preset_id: "developer",
    subagent_presets: &[
        crate::subagent::SUBAGENT_EXPLORE.name,
        crate::subagent::SUBAGENT_TITLE.name,
        crate::subagent::SUBAGENT_CODE.name,
        crate::subagent::SUBAGENT_MCP_SPECIALIST.name,
        crate::subagent::SUBAGENT_SKILL.name,
    ],
    tool_scope: ToolScope::All,
};

/// The delegation face of an agent role: which subagents it may load, and the tool scope it declares.
#[derive(Debug, Clone)]
pub struct AgentRoleDelegation {
    /// Stable id.
    pub preset_id: &'static str,
    /// Subagent names this master may load, in preference order.
    pub subagent_presets: &'static [&'static str],
    /// The tool scope this master declares against the pool.
    pub tool_scope: ToolScope,
}

pub type DelegationPolicy = AgentRoleDelegation;

impl AgentRoleDelegation {
    /// Whether a master bound to this preset may load the subagent preset
    pub fn admits_subagent(&self, name: &str) -> bool {
        self.subagent_presets.contains(&name)
    }

    /// All shipping master delegations, developer first.
    pub const ALL: &'static [AgentRoleDelegation] =
        &[AGENT_ROLE_DEVELOPER, AGENT_ROLE_PHILOSOPHIST];

    pub fn declared_tools(&self) -> Option<&'static [&'static str]> {
        match self.preset_id {
            "philosophist" => Some(&["read_url", "search_web", "ask_user"]),
            _ => None,
        }
    }

    pub fn selection(&self) -> ToolSelection {
        match self.declared_tools() {
            None => ToolSelection::unrestricted(),
            Some(names) => ToolSelection::only(names.iter().copied()),
        }
    }
}

/// The philosophist master preset: workspace-free philosophical exploration.
pub const AGENT_ROLE_PHILOSOPHIST: AgentRoleDelegation = AgentRoleDelegation {
    preset_id: "philosophist",
    subagent_presets: &[crate::subagent::SUBAGENT_EXPLORE.name],
    tool_scope: ToolScope::All,
};

fn role_directive(text: &str) -> AgentIdentity {
    AgentIdentity::from_persona(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AgentIdentity;

    #[test]
    fn with_identity_is_unrestricted_and_attended() {
        let p = AgentRoleProfile::with_identity("developer", AgentIdentity::new("n", "m"));
        assert_eq!(p.name, "developer");
        assert!(!p.unattended);
        assert_eq!(p.tools.scope, crate::ToolScope::All);
        assert!(p.tools.variants.is_empty());
        assert_eq!(p.config.hard_stop_turns, 0);
        assert!(!p.config.allow_model_stdin);
        assert!(!p.config.skip_interactive_input);
        assert_eq!(p.config.nudge, crate::DoomGuardConfig::default());
    }

    #[test]
    fn builders_override_defaults() {
        let p = AgentRoleProfile::with_identity("ops", AgentIdentity::default())
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
        let _copy = c;
        let _again = c;
    }

    #[test]
    fn role_round_trips_through_parse() {
        for role in MainAgentRole::ALL {
            let parsed = MainAgentRole::parse(role.as_str());
            assert_eq!(parsed, Some(*role), "{} should parse back", role.as_str());
        }
        assert_eq!(MainAgentRole::parse("Coder"), Some(MainAgentRole::Developer));
        assert_eq!(MainAgentRole::parse("dev"), Some(MainAgentRole::Developer));
        assert_eq!(
            MainAgentRole::parse("philosopher"),
            Some(MainAgentRole::Philosophist)
        );
        assert_eq!(
            MainAgentRole::parse("conversational"),
            Some(MainAgentRole::Philosophist)
        );
        assert!(MainAgentRole::parse("wizard").is_none());
    }

    #[test]
    fn developer_role_preserves_base_identity_and_philosophist_installs_directive() {
        let base = AgentIdentity::from_mission("an expert AI coding assistant");
        let dev = AgentRoleProfile::from_role(MainAgentRole::Developer, &base);
        assert_eq!(dev.identity.preamble(), base.preamble());
        assert_eq!(dev.name, "developer");

        let phil = AgentRoleProfile::from_role(MainAgentRole::Philosophist, &base);
        assert_eq!(phil.name, "philosophist");
        let directive = phil.identity.preamble();
        assert!(directive.starts_with("Role: "), "directive: {directive}");
        assert!(!directive.starts_with("You are"));
        assert!(directive.ends_with('.'));
    }

    #[test]
    fn developer_role_is_unrestricted_baseline() {
        let base = AgentIdentity::from_mission("coding assistant");
        let dev = AgentRoleProfile::from_role(MainAgentRole::Developer, &base);
        assert_eq!(dev.tools.scope, crate::ToolScope::All);
    }

    #[test]
    fn philosophist_role_is_workspace_free() {
        let base = AgentIdentity::from_mission("coding assistant");
        let phil = AgentRoleProfile::from_role(MainAgentRole::Philosophist, &base);
        let crate::ToolScope::Only(names) = &phil.tools.scope else {
            panic!("philosophist must be scoped, not unrestricted");
        };
        assert!(!names.contains("write_file"));
        assert!(!names.contains("edit_text"));
        assert!(!names.contains("run_command"));
        assert!(!names.contains("read_text"));
        assert!(names.contains("read_url"));
        assert!(names.contains("search_web"));
        assert!(names.contains("ask_user"));
    }

    #[test]
    fn main_and_sub_agent_derived_from_same_parent() {
        let dev = AgentRoleProfile::developer();
        let mut main_agent: MainAgent = Agent::new(dev, MainAgentRole::Developer);
        assert_eq!(main_agent.role_name(), "developer");
        assert!(main_agent.role().admits_user_interaction());

        let sub_agent: SubAgent = main_agent.spawn_subagent(SubAgentRole::Explore);
        assert_eq!(sub_agent.role_name(), "explore");
        assert!(!sub_agent.role().admits_user_interaction());

        main_agent.switch_role(MainAgentRole::Philosophist);
        assert_eq!(main_agent.role_name(), "philosophist");
    }
}
