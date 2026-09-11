//! Subagent profiles: declarative tool-permission roles for autonomous
//! subagents spawned by the `task` tool (and wrappers like
//! `verify_plan_execution`).
//!
//! ## Why this exists
//!
//! Before ADR-0011 the subagent's toolset was a hardcoded filter inside
//! the dispatch tool (`access() == Read` plus a name exclusion for itself).
//! That had two problems:
//!
//! 1. **It was name-driven, not semantic.** `ask_user` is `Read`, so it
//!    passed the filter and reached the subagent. But a subagent is
//!    autonomous and non-interactive — its `UserQuestionRequest` events are
//!    dropped by the subagent tool's event forwarder, so the request deadlocks
//!    until the parent turn is cancelled. The user could see the call but
//!    could not answer it.
//! 2. **The policy was buried in orchestration code.** Adding a second
//!    subagent role (or tightening the existing one) meant editing the
//!    dispatch tool rather than declaring intent.
//!
//! The fix is a profile primitive that expresses the tool policy in terms of
//! [`Tool`] capability axes — [`Tool::scope_target`], [`Tool::requires_user`],
//! [`Tool::spawns_subagent`] — so admission is data-driven and generalizes to
//! future tools without touching the dispatch path.
//!
//! ## The capability axes
//!
//! - [`Tool::scope_target`] — what the call touches (`Read` vs `Write` path). Existing.
//! - [`Tool::requires_user`] — may block on a live human (e.g. `ask_user`).
//! - [`Tool::spawns_subagent`] — dispatches a nested agent (e.g. `task`).
//!
//! Recursion is unconditionally forbidden in any subagent: a tool that
//! `spawns_subagent` is never admitted, regardless of profile. User
//! interaction is a per-profile knob ([`ToolPolicy::allow_user_interaction`])
//! so a future interactive role could opt in once the plumbing surfaces the
//! request; the built-in [`SUBAGENT_EXPLORE`] profile leaves it off.

use std::sync::Arc;

use crate::model::Model;
use crate::{Tool, ToolScope, ToolSelection, ToolSet};

/// Ceiling on what a subagent may do. There is no capability ladder — a tool is
/// admitted purely by name. [`Tool::spawns_subagent`] and
/// [`Tool::affects_control_flow`] tools are always excluded (recursion and
/// program teardown are absolute, not per-profile toggles). See ADR-0011/0028.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolPolicy {
    /// Which tools a subagent under this policy may use, by name. `None` admits
    /// the full parent toolset (the main agent's shape); `Some(set)` admits only
    /// tools whose `name()` is in the set. This is the sole admission axis —
    /// there is no capability ladder, so adding a new side-effecting tool to the
    /// parent does *not* silently widen a subagent unless its name is listed.
    pub allowed_tools: Option<&'static [&'static str]>,
    /// Whether tools that block on a human ([`Tool::requires_user`]) may run.
    pub allow_user_interaction: bool,
}

impl ToolPolicy {
    /// Returns `true` if a tool may be handed to a subagent under this policy.
    /// Combines the **name scope** ([`allowed_tools`](Self::allowed_tools)) with
    /// the **runtime hard rules** ([`admits_runtime`](Self::admits_runtime)).
    pub fn admits(&self, tool: &dyn Tool) -> bool {
        self.admits_runtime(tool) && self.scope().admits(tool.name())
    }

    /// The subagent hard rules that are independent of the name whitelist:
    /// recursion ([`Tool::spawns_subagent`]) and program teardown
    /// ([`Tool::affects_control_flow`]) are absolute, and human-blocking tools
    /// ([`Tool::requires_user`]) are gated by
    /// [`allow_user_interaction`](Self::allow_user_interaction). These are not
    /// expressible as a capability *name* scope, so the pool resolver (which
    /// handles name scope + the model-capability filter) cannot apply them — the
    /// subagent resolution applies this as a post-filter. See
    /// [`SubagentPreset::resolve_tools`].
    pub fn admits_runtime(&self, tool: &dyn Tool) -> bool {
        // Recursion is unconditionally forbidden in child sub-agents.
        if tool.spawns_subagent() || tool.spawns_subagent() {
            return false;
        }
        // Control-flow tools (e.g. the abort/exit escape hatch) are
        // unconditionally forbidden in subagents — a spawned agent must never
        // be able to tear down the whole program.
        if tool.affects_control_flow() {
            return false;
        }
        // Tools that block on a human are gated by the profile.
        if tool.requires_user() && !self.allow_user_interaction {
            return false;
        }
        true
    }

    /// This policy's capability **name scope** for the pool resolver: `None`
    /// [`allowed_tools`](Self::allowed_tools) → [`ToolScope::All`]; `Some(set)`
    /// → [`ToolScope::Only`] the listed names. The runtime hard rules
    /// ([`admits_runtime`](Self::admits_runtime)) are layered on separately.
    pub fn scope(&self) -> ToolScope {
        match self.allowed_tools {
            None => ToolScope::All,
            Some(names) => ToolScope::only(names.iter().copied()),
        }
    }
}

/// A declarative subagent role: a name, the system-prompt fragment that
/// frames the role, and the [`ToolPolicy`] that scopes what it may touch.
///
/// Profiles live in `muta-contracts` (domain vocabulary) so dispatch tools in
/// `muta-agent` resolve them without re-implementing admission logic. The
/// built-in [`SUBAGENT_EXPLORE`] profile is what `task` binds to today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubagentPreset {
    pub name: &'static str,
    pub system_prompt: &'static str,
    pub tool_policy: ToolPolicy,
    /// The profile's variant pins (the agent-side **override** axis): a list of
    /// `(capability, variant_id)` this role forces, regardless of the model's
    /// own preference. Empty for every built-in role — they accept whatever
    /// variant the model resolves. A non-empty pin wins over the model's choice
    /// for that capability (agent-over-model), but can still be overridden
    /// *down* by the model's hard capability limit if the pinned variant is
    /// unusable. See [`ToolSet::resolve_for`].
    pub variant_pins: &'static [(&'static str, &'static str)],
    /// Whether a subagent spawned under this profile runs in unattended
    /// execution mode: auto-approves all permissions without human intervention.
    pub unattended: bool,
    /// Whether a subagent spawned under this profile may have the **model**
    /// supply stdin bytes for an `execute_command` call it emits (the opt-in automatic-
    /// flow path). Default `false` for every built-in profile: autonomous
    /// subagents run non-interactively (the L1 hard floor + L2 idle watchdog
    /// keep them from hanging); a profile aimed at automated CI/batch flows
    /// where no human is reachable can set this `true` so the model can feed
    /// a command's stdin directly. Without it, stdin is structurally
    /// unreachable from the model's arguments even inside a subagent.
    pub allow_model_stdin: bool,
}

impl SubagentPreset {
    /// This profile's [`ToolSelection`] — the agent-identity selector it hands
    /// the pool: the capability **name scope** from its [`ToolPolicy`], plus its
    /// own variant pins (the **override** axis, agent side). Built-in profiles
    /// pin nothing, so they accept the model's variant for every capability;
    /// a profile that pins a variant takes precedence over the model's choice
    /// (agent-over-model) — see [`ToolSet::resolve_for`].
    pub fn selection(&self) -> ToolSelection {
        ToolSelection {
            scope: self.tool_policy.scope(),
            variants: self
                .variant_pins
                .iter()
                .map(|(cap, var)| (cap.to_string(), var.to_string()))
                .collect(),
        }
    }

    /// Resolve the pool down to the toolset a spawned subagent on `model` actually
    /// gets. This is the subagent's whole admission story in one call:
    ///
    /// 1. [`ToolSet::resolve_for`] composes this profile's [`selection`](Self::selection)
    ///    with the model's selection (`model_sel`) — scope by intersection,
    ///    variants by agent-over-model precedence, the model's capability limits
    ///    applied hard.
    /// 2. The subagent **runtime hard rules** ([`ToolPolicy::admits_runtime`]) are
    ///    applied as a post-filter: recursion, control-flow, and (unless the
    ///    profile opts in) human-blocking tools are stripped regardless of name.
    ///
    /// The result is the variant-resolved, model-legal, role-scoped tool list to
    /// hand the child agent.
    pub fn resolve_tools(
        &self,
        toolset: &ToolSet,
        model: &Model,
        model_sel: &ToolSelection,
    ) -> Vec<Arc<dyn Tool>> {
        toolset
            .resolve_for(model, &self.selection(), model_sel)
            .into_iter()
            .filter(|tool| self.tool_policy.admits_runtime(tool.as_ref()))
            .collect()
    }
}

/// Tools a read-only subagent (SUBAGENT_EXPLORE / REVIEW / SUBAGENT_TITLE) may use: pure
/// inspection with no side effects. Listed by name so adding a new
/// side-effecting tool to the parent never silently widens these profiles.
const READ_ONLY_TOOLS: &[&str] = &[
    "read_text",
    "read_image",
    "find_files",
    "list_dir",
    "search_text",
    "code_query",
    "read_url",
    "search_web",
];

/// The built-in read-only research role used by `task`.
///
/// Read-only, non-interactive, non-recursive. This is the profile the `task`
/// tool binds to; declaring additional profiles (and exposing a role selector
/// on `task`) is a future extension that needs no changes here.
pub const SUBAGENT_EXPLORE: SubagentPreset = SubagentPreset {
    name: "explore",
    system_prompt: "\
You are a delegated research subagent. Your single job is to answer the assigned \
task accurately and concisely. Explore the workspace or the web as needed, \
then write a clear, complete final answer with the key findings (file paths, \
signatures, relevant snippets, conclusions). The toolset handed to you is the \
full set you are permitted to use — work within it, do not request others. \
You are non-interactive: never ask the user any \
question — if information is missing, make a reasonable assumption, note it \
explicitly in your answer, or report that you could not find it. Run at most a \
handful of turns, then answer.",
    tool_policy: ToolPolicy {
        allowed_tools: Some(READ_ONLY_TOOLS),
        allow_user_interaction: false,
    },
    variant_pins: &[],
    unattended: true,
    allow_model_stdin: false,
};

/// The session-titling role (ADR-0022). Read-only and non-interactive, its
/// task is pure text-in/text-out — it admits no tool loop at all. The
/// digest generator makes a single `provider.chat()`
/// through the cognitive pipeline and normalizes the title via
/// `clean_title`. Declared as a profile (not an ad-hoc call) so the
/// capability-axis vocabulary stays the single source of truth for what a
/// bounded subagent may do, per ADR-0011.
pub const SUBAGENT_TITLE: SubagentPreset = SubagentPreset {
    name: "title",
    system_prompt: "\
You are a session-titling subagent. You are shown an excerpt of a conversation \
and asked for a short title that captures what the session is about. Reply with \
only the title — 3 to 7 words, plain text, no quotes, no markdown, no trailing \
punctuation, no preamble. Name the concrete subject of the work (a feature, \
file, bug, or task) rather than a generic word like \"chat\" or \"help\". Write \
the title in the same language as the conversation.",
    tool_policy: ToolPolicy {
        allowed_tools: Some(READ_ONLY_TOOLS),
        allow_user_interaction: false,
    },
    variant_pins: &[],
    unattended: true,
    allow_model_stdin: false,
};

/// Tools a coding subagent may use: the generic read-only inspection tools
/// (shared with [`SUBAGENT_EXPLORE`]) plus the workspace-mutating tools —
/// `run_command` for
/// running builds/tests/git, `edit_text` and `write_file` for code, and the
/// `todo*` pair so a long delegation can track its own progress. Listed by
/// name so adding a new side-effecting tool to the parent never silently
/// widens this profile — the only tools a SUBAGENT_CODE subagent can touch are the ones
/// enumerated here. Recursion (`spawn_agent`) and control-flow escapes are excluded
/// absolutely by [`ToolPolicy::admits_runtime`], independent of this list.
const CODING_TOOLS: &[&str] = &[
    // Generic read-only inspection (shared with SUBAGENT_EXPLORE).
    "read_text",
    "read_image",
    "find_files",
    "list_dir",
    "search_text",
    "read_url",
    "search_web",
    // Workspace mutation — the code-editing surface.
    "run_command",
    "edit_text",
    "write_file",
    // Self-contained task tracking (the subagent's own todo list, not the
    // parent's).
    "todo",
];

/// The coding subagent role. Unlike [`SUBAGENT_EXPLORE`] (read-only, autonomous), this is
/// a **write-capable** sub-agent: it can edit files and run commands to
/// implement a delegated task end-to-end, then hand back a technically
/// complete summary. It is the analogue of kimi-code's `coder` subagent.
///
/// Like every built-in subagent, the role is **autonomous** (`delegated: true`):
/// the principal's act of delegating a task via the `delegate_code` tool *is* the
/// authorization — the child runs its writes and commands on its own authority,
/// without routing each one back through the permission broker. The broker
/// (the TUI permission sheet, `/permissions`, the `Always` allowlist) is the
/// principal's gate, not the subagent's: it gates the top-level call that spawns
/// the subagent, and the principal stays accountable for the result via the
/// subagent's final handoff. See ADR-0087.
/// - `allow_user_interaction: true` admits `ask_user` (and any future
///   approval-gated tool), so an ambiguous requirement can be surfaced rather
///   than guessed; that path still uses the full-duplex channel
///   ([ADR-0029](../../adr/0029-full-duplex-subagent-communication.md)).
/// - `delegated: true` keeps the broker off for the child's writes/commands,
///   matching every other built-in profile.
///
/// ADR-0086 originally shipped this profile with an attended posture (every
/// write/command user-approved); ADR-0087 reverses that to keep the
/// delegation-as-authorization contract uniform across subagents.
///
/// This is the built-in profile with side effects that a dispatch tool can bind to
/// for delegated *implementation* work. The read-only research contract of
/// [`SUBAGENT_EXPLORE`] is untouched.
pub const SUBAGENT_CODE: SubagentPreset = SubagentPreset {
    name: "code",
    system_prompt: "\
You are a delegated implementation subagent. You are handed a well-scoped software-engineering \
task: implement the change end to end. Read the relevant code first, then edit \
files and run commands (builds, tests, git) to land the change and verify it. \
Prefer the narrowest change that satisfies the task, and run commands only \
when they advance the work. The toolset handed \
to you is the full set you are permitted to use — work within it, do not \
request others. All your `user` messages come from the parent agent, which \
cannot see your working context — it sees only your final message. Treat the \
parent as your caller: do not address the end user directly. Your final \
answer is the entire handoff, so make it technically complete — what you \
changed and why, the path of every file you touched, how you verified the \
change (tests or commands run, with results), and anything left undone. A \
final message of only a sentence or two is too brief. Run at most a handful \
of turns, then answer.",
    tool_policy: ToolPolicy {
        allowed_tools: Some(CODING_TOOLS),
        allow_user_interaction: true,
    },
    variant_pins: &[],
    unattended: true,
    allow_model_stdin: false,
};

/// The MCP specialist subagent role for running external and dynamic MCP tools
/// in an isolated sandbox (rationale: ADR-0138, archived — superseded by
/// [ADR-0144](../../../docs/adr/0144-three-tier-agent-hierarchy-and-tool-pool.md)).
///
/// Scope note: `allowed_tools: None` admits the **full** parent toolset — every
/// built-in write/execute tool included — not only the dynamic MCP tools the
/// role's description names. Narrowing that grant to the dynamic set is an open
/// question; this note records the current behaviour rather than endorsing it.
pub const SUBAGENT_MCP_SPECIALIST: SubagentPreset = SubagentPreset {
    name: "mcp_specialist",
    system_prompt: "\
You are a specialized integration subagent. Your mission is to execute tasks \
using external and specialized MCP tools (such as database queries, GitHub operations, \
or third-party API integrations) in an isolated sandbox. Focus on calling the necessary tools, \
analyzing the raw outputs, and returning a concise, high-signal summary of the results \
to the principal agent. Never output giant raw payloads if a clear summary answers the question.",
    tool_policy: ToolPolicy {
        allowed_tools: None, // Admits full dynamic/MCP toolset
        allow_user_interaction: false,
    },
    variant_pins: &[],
    unattended: true,
    allow_model_stdin: false,
};

/// The skill discovery and domain expertise subagent role.
///
/// Specialized in dynamically locating, inspecting, and synthesizing guidelines,
/// rules, workflows, and procedures from available project and user skills (`SKILL.md`)
/// on demand without cluttering the parent agent's persistent system prompt.
pub const SUBAGENT_SKILL: SubagentPreset = SubagentPreset {
    name: "skill",
    system_prompt: "\
You are a skill-discovery subagent. Your role is to locate, \
inspect, and synthesize specialized instructions and procedures from available \
skills (project-local `.muta/skills/`, user-global `~/.local/share/muta/skills/`, \
`.agents/skills/`, `.claude/skills/`, etc.) to guide the delegated task. Read the \
relevant `SKILL.md` documents and associated reference files, extract the concrete \
rules, tool sequences, edge cases, and best practices, and return an actionable, \
well-structured domain briefing to the calling agent. Do not modify files or ask \
questions of the user; focus strictly on skill discovery and instruction synthesis.",
    tool_policy: ToolPolicy {
        allowed_tools: Some(READ_ONLY_TOOLS),
        allow_user_interaction: false,
    },
    variant_pins: &[],
    unattended: true,
    allow_model_stdin: false,
};

impl SubagentPreset {
    pub const EXPLORE: Self = SUBAGENT_EXPLORE;
    pub const CODE: Self = SUBAGENT_CODE;
    pub const TITLE: Self = SUBAGENT_TITLE;
    pub const MCP_SPECIALIST: Self = SUBAGENT_MCP_SPECIALIST;
    pub const SKILL: Self = SUBAGENT_SKILL;
}

/// The pool of subagent presets available for master delegation.
#[derive(Debug, Clone, Copy, Default)]
pub struct SubagentPresetPool;

impl SubagentPresetPool {
    /// Static catalog of all built-in subagent presets.
    pub const ALL: &'static [&'static SubagentPreset] = &[
        &SUBAGENT_EXPLORE,
        &SUBAGENT_TITLE,
        &SUBAGENT_CODE,
        &SUBAGENT_MCP_SPECIALIST,
        &SUBAGENT_SKILL,
    ];

    /// Find a subagent preset by name.
    pub fn find(name: &str) -> Option<&'static SubagentPreset> {
        let normalized = match name {
            "mcp" => "mcp_specialist",
            other => other,
        };
        Self::ALL.iter().copied().find(|p| p.name == normalized)
    }

    /// List all available preset names in the pool.
    pub fn names() -> Vec<&'static str> {
        Self::ALL.iter().map(|p| p.name).collect()
    }

    /// Filter subagent presets admitted by an agent delegation policy.
    pub fn admitted_for_delegation(
        delegation: &crate::AgentPersonaDelegation,
    ) -> Vec<&'static SubagentPreset> {
        Self::ALL
            .iter()
            .copied()
            .filter(|p| delegation.admits_subagent(p.name))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    /// A configurable test tool used to exercise every admission branch. The
    /// admission axis is now *name* (no capability ladder), so each Stub is
    /// parameterized by the tool name it claims.
    struct Stub {
        name: &'static str,
        requires_user: bool,
        spawns_subagent: bool,
        affects_control_flow: bool,
    }

    #[async_trait]
    impl Tool for Stub {
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "stub"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        fn requires_user(&self) -> bool {
            self.requires_user
        }
        fn spawns_subagent(&self) -> bool {
            self.spawns_subagent
        }
        fn affects_control_flow(&self) -> bool {
            self.affects_control_flow
        }
        async fn call(&self, _arguments: &str) -> Result<String, String> {
            Ok("stub".to_string())
        }
    }

    /// Build a plain tool named `name`. The flags default to "harmless" — the
    /// admission axis is the name itself.
    fn make(name: &'static str) -> Stub {
        Stub {
            name,
            requires_user: false,
            spawns_subagent: false,
            affects_control_flow: false,
        }
    }

    fn with_user(mut t: Stub) -> Stub {
        t.requires_user = true;
        t
    }

    fn with_spawn(mut t: Stub) -> Stub {
        t.spawns_subagent = true;
        t
    }

    /// A control-flow tool shape. Used to prove profiles exclude control
    /// tools by the control-flow flag, regardless of name.
    fn make_control() -> Stub {
        Stub {
            name: "control-stub",
            requires_user: false,
            spawns_subagent: false,
            affects_control_flow: true,
        }
    }

    #[test]
    fn explore_admits_a_whitelisted_read_tool() {
        assert!(SUBAGENT_EXPLORE.tool_policy.admits(&make("read_text")));
        assert!(SUBAGENT_EXPLORE.tool_policy.admits(&make("search_text")));
    }

    #[test]
    fn explore_rejects_a_non_whitelisted_tool() {
        // write_file is not in READ_ONLY_TOOLS — a research explorer must not
        // mutate files.
        assert!(!SUBAGENT_EXPLORE.tool_policy.admits(&make("write_file")));
        // Command execution is also not whitelisted.
        assert!(
            !SUBAGENT_EXPLORE
                .tool_policy
                .admits(&make("execute_command"))
        );
    }

    #[test]
    fn explore_rejects_a_whitelisted_tool_that_requires_user() {
        // ask_user is not whitelisted, but even a whitelisted name is rejected
        // when requires_user is set and the profile disallows interaction.
        assert!(
            !SUBAGENT_EXPLORE
                .tool_policy
                .admits(&with_user(make("read_text")))
        );
    }

    #[test]
    fn explore_rejects_dispatch_tool_even_if_named_like_a_read() {
        // Recursion is absolute: even a whitelisted name is excluded when it
        // spawns a subagent.
        assert!(
            !SUBAGENT_EXPLORE
                .tool_policy
                .admits(&with_spawn(make("read_text")))
        );
    }

    #[test]
    fn explore_rejects_control_flow_tool() {
        assert!(!SUBAGENT_EXPLORE.tool_policy.admits(&make_control()));
    }

    #[test]
    fn recursion_is_rejected_even_by_a_permissive_policy() {
        let permissive = ToolPolicy {
            allowed_tools: None,
            allow_user_interaction: true,
        };
        assert!(!permissive.admits(&with_spawn(make("read_text"))));
        assert!(permissive.admits(&make("execute_command")));
    }

    #[test]
    fn control_flow_is_rejected_even_by_a_permissive_policy() {
        let permissive = ToolPolicy {
            allowed_tools: None,
            allow_user_interaction: true,
        };
        assert!(!permissive.admits(&make_control()));
    }

    /// A test model (vision-capable; the Stub tools require no vision, so the
    /// model-capability filter is a no-op here — this test isolates the scope +
    /// runtime-rule composition).
    fn test_model() -> Model {
        Model {
            id: "test",
            family: "test",
            context_window: 100_000,
            thinking: crate::reasoning::ReasoningSupport::None,
            tool_call: true,
            vision: true,
            protocol: crate::WireProtocol::ChatCompletions,
            model_guidance: "",
            effort_levels: &[],
        }
    }

    #[test]
    fn resolve_tools_applies_scope_and_runtime_rules() {
        // `search_text` is whitelisted; command execution is dropped
        // by scope. `read_text` is whitelisted *but spawns a subagent* → dropped
        // by the runtime recursion rule despite passing the name scope.
        let toolset = ToolSet::from_tools(vec![
            Arc::new(make("search_text")) as Arc<dyn Tool>,
            Arc::new(make("execute_command")) as Arc<dyn Tool>,
            Arc::new(with_spawn(make("read_text"))) as Arc<dyn Tool>,
        ]);
        let selected =
            SUBAGENT_EXPLORE.resolve_tools(&toolset, &test_model(), &ToolSelection::unrestricted());
        let names: Vec<&str> = selected.iter().map(|t| t.name()).collect();
        assert_eq!(names, vec!["search_text"]);
    }

    #[test]
    fn none_allowed_tools_admits_everything_named() {
        let open = ToolPolicy {
            allowed_tools: None,
            allow_user_interaction: false,
        };
        assert!(open.admits(&make("read_text")));
        assert!(open.admits(&make("execute_command")));
        assert!(open.admits(&make("write_file")));
    }

    /// SUBAGENT_EXPLORE (the research role) excludes unlisted tools (e.g. trading,
    /// write/execute tools). Only explicit READ_ONLY_TOOLS are admitted.
    #[test]
    fn explore_profile_excludes_unlisted_tools() {
        assert!(!SUBAGENT_EXPLORE.tool_policy.admits(&make("market_data")));
        assert!(!SUBAGENT_EXPLORE.tool_policy.admits(&make("backtest")));
        assert!(!SUBAGENT_EXPLORE.tool_policy.admits(&make("place_order")));
        assert!(!SUBAGENT_EXPLORE.tool_policy.admits(&make("cancel_order")));
        assert!(!SUBAGENT_EXPLORE.tool_policy.admits(&make("list_positions")));
    }

    /// SUBAGENT_CODE is the write-capable coding role. It admits the full edit surface
    /// (`execute_command`, `edit_text`, `write_file`) and the shared read-only
    /// tools, but — like
    /// every subagent — it still excludes recursion and control-flow escapes
    /// absolutely, and unlisted tools stay out.
    #[test]
    fn code_profile_admits_edit_surface_but_not_recursion_or_unlisted() {
        use crate::SUBAGENT_CODE;
        // Write/execute surface: admitted.
        assert!(SUBAGENT_CODE.tool_policy.admits(&make("run_command")));
        assert!(SUBAGENT_CODE.tool_policy.admits(&make("edit_text")));
        assert!(SUBAGENT_CODE.tool_policy.admits(&make("write_file")));
        assert!(SUBAGENT_CODE.tool_policy.admits(&make("todo")));
        // Shared read-only inspection: admitted.
        assert!(SUBAGENT_CODE.tool_policy.admits(&make("read_text")));
        assert!(SUBAGENT_CODE.tool_policy.admits(&make("search_text")));
        // A non-whitelisted tool is excluded (name scope is real — adding a
        // new tool to the parent never silently widens SUBAGENT_CODE).
        assert!(!SUBAGENT_CODE.tool_policy.admits(&make("some_new_tool")));
        assert!(!SUBAGENT_CODE.tool_policy.admits(&make("market_data")));
        assert!(!SUBAGENT_CODE.tool_policy.admits(&make("place_order")));
        // Recursion and control-flow remain absolute.
        assert!(
            !SUBAGENT_CODE
                .tool_policy
                .admits(&with_spawn(make("run_command")))
        );
        assert!(!SUBAGENT_CODE.tool_policy.admits(&make_control()));
    }

    /// ADR-0087: the principal's act of delegating via `delegate_code` *is* the
    /// authorization, so SUBAGENT_CODE runs autonomous like every other built-in
    /// profile — the permission broker is the principal's gate, not the
    /// subagent's. Pins the value so ADR-0086's attended posture cannot
    /// silently come back.
    // The assertion is constant by design: it pins the compiled-in `SUBAGENT_CODE`
    // profile value (see the doc comment above), not a computed property.
    #[allow(clippy::assertions_on_constants)]
    #[test]
    fn code_profile_runs_unattended() {
        use crate::SUBAGENT_CODE;
        assert!(SUBAGENT_CODE.unattended);
    }

    #[test]
    fn mcp_specialist_profile_admits_dynamic_tools_and_excludes_recursion() {
        use crate::SUBAGENT_MCP_SPECIALIST;
        // Pins the compiled-in profile value (running unattended); constant by design.
        #[allow(clippy::assertions_on_constants)]
        let unattended = SUBAGENT_MCP_SPECIALIST.unattended;
        assert!(unattended);
        // Dynamic / external tools admitted
        assert!(
            SUBAGENT_MCP_SPECIALIST
                .tool_policy
                .admits(&make("mcp__postgres__query"))
        );
        assert!(
            SUBAGENT_MCP_SPECIALIST
                .tool_policy
                .admits(&make("read_text"))
        );
        // Recursion and control-flow strictly forbidden
        assert!(
            !SUBAGENT_MCP_SPECIALIST
                .tool_policy
                .admits(&with_spawn(make("read_text")))
        );
        assert!(!SUBAGENT_MCP_SPECIALIST.tool_policy.admits(&make_control()));
    }

    #[test]
    fn subagent_preset_pool_catalog_and_filtering() {
        assert_eq!(SubagentPresetPool::ALL.len(), 5);
        assert_eq!(
            SubagentPresetPool::find("explore").map(|p| p.name),
            Some("explore")
        );
        assert_eq!(
            SubagentPresetPool::find("code").map(|p| p.name),
            Some("code")
        );
        assert_eq!(
            SubagentPresetPool::find("skill").map(|p| p.name),
            Some("skill")
        );
        assert_eq!(SubagentPresetPool::find("nonexistent"), None);

        let dev_delegation = crate::AGENT_DEVELOPER;
        let dev_subagents = SubagentPresetPool::admitted_for_delegation(&dev_delegation);
        assert_eq!(dev_subagents.len(), 5);

        let analyst_delegation = crate::AGENT_CODE_ANALYST;
        let analyst_subagents = SubagentPresetPool::admitted_for_delegation(&analyst_delegation);
        assert_eq!(analyst_subagents.len(), 3);
        let names: Vec<&str> = analyst_subagents.iter().map(|p| p.name).collect();
        assert_eq!(names, vec!["explore", "title", "skill"]);
    }
}
