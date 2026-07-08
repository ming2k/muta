//! System-prompt assembly and skill injection (ADR-0039).
//!
//! The system prompt is no longer one imperative method that pushes string
//! literals into a `Vec`. It is a [`PromptRegistry`] of declarative
//! [`PromptSection`]s — one per behavioral paragraph — registered on the
//! [`Agent`] at construction. [`Agent::ensure_system_prompt`] rebuilds the
//! context from live agent state each round and asks the registry to compose
//! the active system sections in rank order.
//!
//! The default system sections ([`IdentityPreamble`], [`ToneGuidance`],
//! [`PersistenceGuidance`], [`PursuitObjective`], [`DelegationGuidance`])
//! compose the system message in rank order: sections
//! that need a visual gap include a leading `\n` in their own `render`, so
//! joining on a single `\n` preserves a stable layout.
//!
//! [`Agent::inject_implicit_skills`] stays here for now (it is a user-channel
//! injection); ADR-0039 stage 4 will fold it into a user-channel section.

use crate::{
    Agent, InjectionKind, InjectionOrigin, Message, PromptChannel, PromptContext, PromptRegistry,
    PromptSection, Role,
};
use neenee_core::{REVIEW, SessionReview};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Default system-channel sections.
//
// Each is a zero-sized struct: the only state a section needs is the live
// turn state, which arrives via [`PromptContext`]. That makes each section
// individually unit-testable and individually re-orderable / disable-able.
// ---------------------------------------------------------------------------

/// Opening identity sentence (name/mission/persona), composed by the
/// embedding. Empty preamble (tests / identity-less agents) → inactive.
struct IdentityPreamble;

impl PromptSection for IdentityPreamble {
    fn id(&self) -> &'static str {
        "system.identity_preamble"
    }
    fn channel(&self) -> PromptChannel {
        PromptChannel::System
    }
    fn kind(&self) -> InjectionKind {
        InjectionKind::SystemPrompt
    }
    fn rank(&self) -> u32 {
        10
    }
    fn is_active(&self, ctx: &PromptContext) -> bool {
        !ctx.identity_preamble.is_empty()
    }
    fn render(&self, ctx: &PromptContext) -> Option<String> {
        Some(ctx.identity_preamble.clone())
    }
}

/// Mission-neutral tone / output guidance. Currently empty — always inactive.
/// Exists as a structural slot for future tone directives.
struct ToneGuidance;

impl PromptSection for ToneGuidance {
    fn id(&self) -> &'static str {
        "system.tone"
    }
    fn channel(&self) -> PromptChannel {
        PromptChannel::System
    }
    fn kind(&self) -> InjectionKind {
        InjectionKind::SystemPrompt
    }
    fn rank(&self) -> u32 {
        20
    }
    fn render(&self, _ctx: &PromptContext) -> Option<String> {
        None
    }
}

/// Model-specific guidance. Each model behaves differently, so the resolved
/// model's `Model::model_guidance` is the per-model hook for whatever
/// behavioral nudge it needs. Renders it verbatim when non-empty — the model
/// entry is the single source of truth. Empty for all known models today.
struct ModelGuidance;

impl PromptSection for ModelGuidance {
    fn id(&self) -> &'static str {
        "system.model_guidance"
    }
    fn channel(&self) -> PromptChannel {
        PromptChannel::System
    }
    fn kind(&self) -> InjectionKind {
        InjectionKind::SystemPrompt
    }
    fn rank(&self) -> u32 {
        25
    }
    fn is_active(&self, ctx: &PromptContext) -> bool {
        !ctx.model_guidance.is_empty()
    }
    fn render(&self, ctx: &PromptContext) -> Option<String> {
        if ctx.model_guidance.is_empty() {
            None
        } else {
            Some(format!("\n{}", ctx.model_guidance))
        }
    }
}

/// Provider/protocol-specific guidance. Concrete SDK providers expose narrow
/// facts about their wire projection; the prompt registry owns rendering them.
struct ProviderGuidance;

impl PromptSection for ProviderGuidance {
    fn id(&self) -> &'static str {
        "system.provider_guidance"
    }
    fn channel(&self) -> PromptChannel {
        PromptChannel::System
    }
    fn kind(&self) -> InjectionKind {
        InjectionKind::SystemPrompt
    }
    fn rank(&self) -> u32 {
        27
    }
    fn is_active(&self, ctx: &PromptContext) -> bool {
        !ctx.provider_guidance.is_empty()
    }
    fn render(&self, ctx: &PromptContext) -> Option<String> {
        if ctx.provider_guidance.is_empty() {
            None
        } else {
            Some(format!("\n{}", ctx.provider_guidance))
        }
    }
}

/// Task-completion ethos: see the work through to a real result in one turn
/// instead of stopping at analysis or a partial fix. Always active. Mirrors
/// codex's "Autonomy and Persistence" section, condensed.
struct PersistenceGuidance;

const PERSISTENCE: &str = "\nSee the task through to a real result in this turn. Don't stop at \
                           analysis or a partial fix — carry the work through implementation and \
                           verification. If a tool call fails or you hit a blocker, try to resolve \
                           it yourself before yielding; only hand back to the user when the work \
                           is actually done or you genuinely need their input.";

impl PromptSection for PersistenceGuidance {
    fn id(&self) -> &'static str {
        "system.persistence"
    }
    fn channel(&self) -> PromptChannel {
        PromptChannel::System
    }
    fn kind(&self) -> InjectionKind {
        InjectionKind::SystemPrompt
    }
    fn rank(&self) -> u32 {
        35
    }
    fn render(&self, _ctx: &PromptContext) -> Option<String> {
        Some(String::from(PERSISTENCE))
    }
}

/// The autonomous-operation posture. Active only when the agent is running
/// unattended this round. With the harness having reclaimed `ask_user` and
/// auto-approving every side-effecting tool, this tells the model the human is
/// unreachable — it must resolve ambiguity itself, pick a sensible default, and
/// never block waiting for an answer that will not come. Leading `\n`
/// separates it from the paragraphs above.
struct UnattendedGuidance;

const UNATTENDED: &str = "\nYou are running unattended: no human is reachable this turn. The \
                          question tool has been reclaimed and every tool permission auto-approves, \
                          so nothing you do will pause for confirmation. Decide and act on your own \
                          authority: when faced with ambiguity, pick the most reasonable default \
                          and proceed rather than asking — there is no one to answer. Surface any \
                          irreversible or high-stakes choice you made on your own in your final \
                          summary instead of stopping to ask.";

impl PromptSection for UnattendedGuidance {
    fn id(&self) -> &'static str {
        "system.unattended"
    }
    fn channel(&self) -> PromptChannel {
        PromptChannel::System
    }
    fn kind(&self) -> InjectionKind {
        InjectionKind::SystemPrompt
    }
    fn rank(&self) -> u32 {
        36
    }
    fn is_active(&self, ctx: &PromptContext) -> bool {
        ctx.unattended
    }
    fn render(&self, _ctx: &PromptContext) -> Option<String> {
        Some(String::from(UNATTENDED))
    }
}

/// The active pursuit objective, when one is armed. Leading `\n` separates
/// it from the guidance paragraphs above.
struct PursuitObjective;

impl PromptSection for PursuitObjective {
    fn id(&self) -> &'static str {
        "system.pursuit_objective"
    }
    fn channel(&self) -> PromptChannel {
        PromptChannel::System
    }
    fn kind(&self) -> InjectionKind {
        InjectionKind::SystemPrompt
    }
    fn rank(&self) -> u32 {
        40
    }
    fn is_active(&self, ctx: &PromptContext) -> bool {
        ctx.pursuit.is_some()
    }
    fn render(&self, ctx: &PromptContext) -> Option<String> {
        let pursuit = ctx.pursuit.as_ref()?;
        let state_label = if pursuit.is_complete {
            "complete"
        } else {
            "active"
        };
        Some(format!(
            "\nActive harness pursuit ({state_label}):\n{}",
            pursuit.objective
        ))
    }
}

/// Guidance for delegating read-only exploration. Active only when a
/// dispatch tool is admitted this turn, so identity-less / tool-less test
/// agents are unaffected. Generic tool-category policy: it names no specific
/// tool — the model matches it to whatever dispatch/exploration tools are
/// admitted. Leading `\n` separates it from the paragraphs above.
struct DelegationGuidance;

const DELEGATION: &str = "\nFor open-ended exploration or gathering broad context, delegate to a \
                          read-only sub-agent rather than running the searches yourself — it keeps \
                          your own context lean and lets several investigations run in parallel. \
                          For needle queries (a known path or a specific symbol) go direct: read \
                          or search the target yourself.";

impl PromptSection for DelegationGuidance {
    fn id(&self) -> &'static str {
        "system.delegation_guidance"
    }
    fn channel(&self) -> PromptChannel {
        PromptChannel::System
    }
    fn kind(&self) -> InjectionKind {
        InjectionKind::SystemPrompt
    }
    fn rank(&self) -> u32 {
        55
    }
    fn is_active(&self, ctx: &PromptContext) -> bool {
        ctx.tool_names
            .iter()
            .any(|name| name == "envoy" || name == "task")
    }
    fn render(&self, _ctx: &PromptContext) -> Option<String> {
        Some(String::from(DELEGATION))
    }
}

/// Generic tool-category guidance: prefer a dedicated mutation tool over
/// driving the same change through the shell. Active only when a file-
/// editing tool is admitted this turn (a mechanical guard on tool names —
/// the rendered text names no specific tool, so adding a new file-editing
/// tool only needs the guard updated, not the prose). Leading `\n`
/// separates it from the paragraphs above.
struct FileEditingGuidance;

const FILE_EDITING: &str = "\nWhen a dedicated tool exists for an operation, prefer it over \
                            driving the same operation through the shell. This applies in \
                            particular to creating or modifying files: use the file-editing \
                            tools, not shell redirection (sed, echo >, tee, and the like). The \
                            dedicated tools are atomic, diff-reviewable, and never leave a \
                            half-written file behind if a turn is interrupted; a shell pipeline \
                            is none of those.";

impl PromptSection for FileEditingGuidance {
    fn id(&self) -> &'static str {
        "system.file_editing_guidance"
    }
    fn channel(&self) -> PromptChannel {
        PromptChannel::System
    }
    fn kind(&self) -> InjectionKind {
        InjectionKind::SystemPrompt
    }
    fn rank(&self) -> u32 {
        56
    }
    fn is_active(&self, ctx: &PromptContext) -> bool {
        ctx.tool_names
            .iter()
            .any(|name| name == "write_file" || name == "edit_file")
    }
    fn render(&self, _ctx: &PromptContext) -> Option<String> {
        Some(String::from(FILE_EDITING))
    }
}

/// Build the registry with the default system-channel sections, in rank
/// order. Called once from [`Agent::new`]; an embedding may add more sections
/// (or reorder / disable these) afterwards via the registry handle.
///
/// Note: skills are deliberately *not* injected into the system prompt. The
/// model discovers them lazily via the `list_skills` tool and loads bodies on
/// demand via `use_skill`. Injecting a catalog up front bloats every turn for
/// a benefit the tools already cover.
pub(crate) fn default_prompt_registry() -> PromptRegistry {
    let mut registry = PromptRegistry::new();
    registry.register(IdentityPreamble);
    registry.register(ToneGuidance);
    registry.register(ModelGuidance);
    registry.register(ProviderGuidance);
    registry.register(PersistenceGuidance);
    registry.register(UnattendedGuidance);
    registry.register(PursuitObjective);
    registry.register(DelegationGuidance);
    registry.register(FileEditingGuidance);
    registry
}

// ---------------------------------------------------------------------------
// Session-review system-channel sections (ADR-0039 stage 6).
//
// The `/review` diagnostic spawns a read-only reviewer envoy that used to
// pre-seed its system message (`build_reviewer_system_prompt`) and then run
// the streaming turn loop. But `ensure_system_prompt` replaces any leading
// system message on round 1, so the seeded persona + dimensions + JSON
// contract were clobbered by the default registry's tone+todo and never
// reached the model — the feature limped along only because verdict parsing
// degrades gracefully. The fix mirrors ADR-0039 stage 3: give the reviewer a
// dedicated registry whose composition IS the review prompt, so the message
// rebuilt every round is correct by construction.
// ---------------------------------------------------------------------------

/// The [`REVIEW`] role framing.
struct ReviewPersona;

impl PromptSection for ReviewPersona {
    fn id(&self) -> &'static str {
        "review.persona"
    }
    fn channel(&self) -> PromptChannel {
        PromptChannel::System
    }
    fn kind(&self) -> InjectionKind {
        InjectionKind::SystemPrompt
    }
    fn rank(&self) -> u32 {
        10
    }
    fn render(&self, _ctx: &PromptContext) -> Option<String> {
        Some(String::from(REVIEW.system_prompt))
    }
}

/// The list of registered review dimensions to evaluate, pre-rendered from
/// the live `[SessionReview]` set. Carried as owned text because the dimension
/// list is bespoke per `/review` run and does not fit the shared
/// [`PromptContext`].
struct ReviewDimensions {
    body: String,
}

impl PromptSection for ReviewDimensions {
    fn id(&self) -> &'static str {
        "review.dimensions"
    }
    fn channel(&self) -> PromptChannel {
        PromptChannel::System
    }
    fn kind(&self) -> InjectionKind {
        InjectionKind::SystemPrompt
    }
    fn rank(&self) -> u32 {
        20
    }
    fn render(&self, _ctx: &PromptContext) -> Option<String> {
        Some(self.body.clone())
    }
}

/// The exact JSON verdict contract the runner parses. Pinned here so prompting
/// and parsing stay in sync.
struct ReviewJsonContract;

const REVIEW_JSON_CONTRACT: &str = "Return ONLY a JSON object (no markdown, no prose) of this exact shape:\n\
     {\"verdicts\":[{\"dimension\":\"<id>\",\"status\":\"healthy|watch|stuck\",\
     \"detail\":\"<one short sentence>\"}]}\n\
     Use status \"healthy\" when there is no concern, \"watch\" when progress is \
     slow or risky but not stuck, and \"stuck\" only when the agent is clearly \
     looping without converging. Include one entry per dimension.";

impl PromptSection for ReviewJsonContract {
    fn id(&self) -> &'static str {
        "review.json_contract"
    }
    fn channel(&self) -> PromptChannel {
        PromptChannel::System
    }
    fn kind(&self) -> InjectionKind {
        InjectionKind::SystemPrompt
    }
    fn rank(&self) -> u32 {
        30
    }
    fn render(&self, _ctx: &PromptContext) -> Option<String> {
        Some(String::from(REVIEW_JSON_CONTRACT))
    }
}

/// Render the registered dimensions as the bulleted list the reviewer sees
/// between the persona and the JSON contract.
fn render_review_dimensions(dimensions: &[Arc<dyn SessionReview>]) -> String {
    let mut body = String::from(
        "You are evaluating the health of another agent's turn. Assess each of \
         these dimensions:\n\n",
    );
    for dim in dimensions {
        body.push_str(&format!(
            "- `{}` — {}. {}\n",
            dim.id(),
            dim.label(),
            dim.instruction()
        ));
    }
    body
}

/// Build the reviewer envoy's prompt registry: persona + dimensions + JSON
/// contract. Installed on the reviewer via [`Agent::set_prompt_registry`] so
/// its head system message — rebuilt every round — is the review composition.
pub(crate) fn reviewer_prompt_registry(dimensions: &[Arc<dyn SessionReview>]) -> PromptRegistry {
    let mut registry = PromptRegistry::new();
    registry.register(ReviewPersona);
    registry.register(ReviewDimensions {
        body: render_review_dimensions(dimensions),
    });
    registry.register(ReviewJsonContract);
    registry
}

impl Agent {
    /// Derive the read-only prompt context from live agent state. Owned plain
    /// data (ADR-0039): rebuilt each round, no `&Agent` leaks into sections.
    pub(crate) fn build_prompt_context(&self, messages: &[Message]) -> PromptContext {
        let tool_names: Vec<String> = self
            .visible_tools()
            .iter()
            .map(|t| t.name().to_string())
            .collect();
        let last_visible_user_text = messages
            .iter()
            .filter(|m| m.role == Role::User && !m.hidden)
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let model_guidance = neenee_core::resolve_model(&self.provider.model()).model_guidance;
        let provider_guidance = self.provider.prompt_hints().system_guidance;
        PromptContext {
            identity_preamble: self.identity.preamble(),
            pursuit: self.get_pursuit(),
            tool_names,
            last_visible_user_text,
            model_guidance,
            provider_guidance,
            unattended: self.get_unattended(),
        }
    }

    /// Compose the system message from live state and place it at the head of
    /// the conversation, replacing an existing leading system message in place
    /// or inserting a new one.
    pub(crate) fn ensure_system_prompt(&self, messages: &mut Vec<Message>) {
        let ctx = self.build_prompt_context(messages);
        let system = self.prompt_registry.build_system_message(&ctx);
        match messages.first_mut() {
            Some(first) if first.role == Role::System => *first = system,
            _ => messages.insert(0, system),
        }
    }

    /// Single pre-request funnel for both turn loops: drop empty assistant
    /// tails, rebuild the head system message, then auto-load mentioned
    /// skills. Collapses the previously duplicated triple at the two
    /// round-boundary call sites (ADR-0039).
    pub(crate) fn prepare_turn_messages(&self, messages: &mut Vec<Message>) {
        crate::agent::remove_empty_assistant_messages(messages);
        // Project out non-driving command echoes so they never reach the
        // provider, while remaining durable + visible on resume/export. The
        // predicate is the single `is_command_echo` check; this funnel is the
        // one pre-wire chokepoint every backend passes through (ADR-0050).
        messages.retain(|m| !m.is_command_echo());
        self.ensure_system_prompt(messages);
        self.inject_implicit_skills(messages);
    }

    /// Auto-load skills whose names are mentioned in the latest user turn.
    /// Mentioned skills are injected as hidden user messages so the model
    /// behaves as if the skill content was explicitly loaded.
    pub(crate) fn inject_implicit_skills(&self, messages: &mut Vec<Message>) {
        let text = messages
            .iter()
            .filter(|m| m.role == Role::User && !m.hidden)
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if text.is_empty() {
            return;
        }

        let already_loaded: std::collections::HashSet<String> = messages
            .iter()
            .filter(|m| m.role == Role::User && m.hidden)
            .filter_map(|m| {
                let prefix = "[Skill '";
                let start = m.content.find(prefix)? + prefix.len();
                let end = m.content[start..].find("' loaded]")?;
                Some(m.content[start..start + end].to_string())
            })
            .collect();

        let mentioned: Vec<String> = {
            let registry = self.skills_registry.lock();
            registry
                .resolve_mentions(&text)
                .into_iter()
                .map(|s| s.name)
                .filter(|name| !already_loaded.contains(name))
                .collect()
        };

        for name in mentioned {
            // Body is loaded lazily (and cached) on first use of this skill.
            let Some(Ok(content)) = self.skills_registry.body_for(&name) else {
                continue;
            };
            messages.push(Message::injected(
                Role::User,
                format!("[Skill '{}' loaded]\n{}\n[/Skill]", name, content),
                InjectionOrigin::new(InjectionKind::ImplicitSkill).with_reason(name),
            ));
        }
    }
}
