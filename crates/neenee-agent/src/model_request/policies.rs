//! System-prompt sections and policy registries (ADR-0056).
//!
//! The system prompt is no longer one imperative method that pushes string
//! literals into a `Vec`. It is a [`SystemPromptRegistry`] of declarative
//! [`SystemPromptSection`]s — one per behavioral paragraph — registered on the
//! [`Agent`](crate::Agent) at construction. The `model_request` assembler
//! rebuilds the singleton system message from live agent state before every
//! provider request.
//!
//! The default system sections ([`IdentityPreamble`], [`ToneGuidance`],
//! [`PersistenceGuidance`], [`PursuitObjective`], [`DelegationGuidance`])
//! compose the system message in rank order: sections
//! that need a visual gap include a leading `\n` in their own `render`, so
//! joining on a single `\n` preserves a stable layout.
use crate::{SystemPromptContext, SystemPromptRegistry, SystemPromptSection};
use neenee_core::{REVIEW, SessionReview};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Default system-prompt sections.
//
// Each is a zero-sized struct: the only state a section needs is the live
// turn state, which arrives via [`SystemPromptContext`]. That makes each section
// individually unit-testable and individually re-orderable / disable-able.
// ---------------------------------------------------------------------------

/// Opening identity sentence (name/mission/persona), composed by the
/// embedding. Empty preamble (tests / identity-less agents) → inactive.
struct IdentityPreamble;

impl SystemPromptSection for IdentityPreamble {
    fn id(&self) -> &'static str {
        "system.identity_preamble"
    }
    fn rank(&self) -> u32 {
        10
    }
    fn is_active(&self, ctx: &SystemPromptContext) -> bool {
        !ctx.identity_preamble.is_empty()
    }
    fn render(&self, ctx: &SystemPromptContext) -> Option<String> {
        Some(ctx.identity_preamble.clone())
    }
}

/// Mission-neutral tone / output guidance. Currently empty — always inactive.
/// Exists as a structural slot for future tone directives.
struct ToneGuidance;

impl SystemPromptSection for ToneGuidance {
    fn id(&self) -> &'static str {
        "system.tone"
    }
    fn rank(&self) -> u32 {
        20
    }
    fn render(&self, _ctx: &SystemPromptContext) -> Option<String> {
        None
    }
}

/// Model-specific guidance. Each model behaves differently, so the resolved
/// model's `Model::model_guidance` is the per-model hook for whatever
/// behavioral nudge it needs. Renders it verbatim when non-empty — the model
/// entry is the single source of truth. Empty for all known models today.
struct ModelGuidance;

impl SystemPromptSection for ModelGuidance {
    fn id(&self) -> &'static str {
        "system.model_guidance"
    }
    fn rank(&self) -> u32 {
        25
    }
    fn is_active(&self, ctx: &SystemPromptContext) -> bool {
        !ctx.model_guidance.is_empty()
    }
    fn render(&self, ctx: &SystemPromptContext) -> Option<String> {
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

impl SystemPromptSection for ProviderGuidance {
    fn id(&self) -> &'static str {
        "system.provider_guidance"
    }
    fn rank(&self) -> u32 {
        27
    }
    fn is_active(&self, ctx: &SystemPromptContext) -> bool {
        !ctx.provider_guidance.is_empty()
    }
    fn render(&self, ctx: &SystemPromptContext) -> Option<String> {
        if ctx.provider_guidance.is_empty() {
            None
        } else {
            Some(format!("\n{}", ctx.provider_guidance))
        }
    }
}

/// Task-completion ethos: see the work through to a real result in one round
/// instead of stopping at analysis or a partial fix. Always active. Mirrors
/// codex's "Autonomy and Persistence" section, condensed.
struct PersistenceGuidance;

const PERSISTENCE: &str = "\nSee the task through to a real result in this round. Don't stop at \
                           analysis or a partial fix — carry the work through implementation and \
                           verification. If a tool call fails or you hit a blocker, try to resolve \
                           it yourself before yielding; only hand back to the user when the work \
                           is actually done or you genuinely need their input.";

impl SystemPromptSection for PersistenceGuidance {
    fn id(&self) -> &'static str {
        "system.persistence"
    }
    fn rank(&self) -> u32 {
        35
    }
    fn render(&self, _ctx: &SystemPromptContext) -> Option<String> {
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

const UNATTENDED: &str = "\nYou are running unattended: no human is reachable this round. The \
                          question tool has been reclaimed and every tool permission auto-approves, \
                          so nothing you do will pause for confirmation. Decide and act on your own \
                          authority: when faced with ambiguity, pick the most reasonable default \
                          and proceed rather than asking — there is no one to answer. Surface any \
                          irreversible or high-stakes choice you made on your own in your final \
                          summary instead of stopping to ask.";

impl SystemPromptSection for UnattendedGuidance {
    fn id(&self) -> &'static str {
        "system.unattended"
    }
    fn rank(&self) -> u32 {
        36
    }
    fn is_active(&self, ctx: &SystemPromptContext) -> bool {
        ctx.unattended
    }
    fn render(&self, _ctx: &SystemPromptContext) -> Option<String> {
        Some(String::from(UNATTENDED))
    }
}

/// The active pursuit objective, when one is armed. Leading `\n` separates
/// it from the guidance paragraphs above.
struct PursuitObjective;

impl SystemPromptSection for PursuitObjective {
    fn id(&self) -> &'static str {
        "system.pursuit_objective"
    }
    fn rank(&self) -> u32 {
        40
    }
    fn is_active(&self, ctx: &SystemPromptContext) -> bool {
        ctx.pursuit.is_some()
    }
    fn render(&self, ctx: &SystemPromptContext) -> Option<String> {
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

impl SystemPromptSection for DelegationGuidance {
    fn id(&self) -> &'static str {
        "system.delegation_guidance"
    }
    fn rank(&self) -> u32 {
        55
    }
    fn is_active(&self, ctx: &SystemPromptContext) -> bool {
        ctx.tool_names
            .iter()
            .any(|name| name == "envoy" || name == "task")
    }
    fn render(&self, _ctx: &SystemPromptContext) -> Option<String> {
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

impl SystemPromptSection for FileEditingGuidance {
    fn id(&self) -> &'static str {
        "system.file_editing_guidance"
    }
    fn rank(&self) -> u32 {
        56
    }
    fn is_active(&self, ctx: &SystemPromptContext) -> bool {
        ctx.tool_names
            .iter()
            .any(|name| name == "write_file" || name == "edit_file")
    }
    fn render(&self, _ctx: &SystemPromptContext) -> Option<String> {
        Some(String::from(FILE_EDITING))
    }
}

/// Build the registry with the default system-prompt sections, in rank
/// order. Called by [`Agent::builder`]; an embedding may add, reorder, or
/// disable sections on [`crate::AgentBuilder`] before freezing the agent.
///
/// Note: skills are deliberately *not* injected into the system prompt. The
/// model discovers them lazily via the `list_skills` tool and loads bodies on
/// demand via `use_skill`. Injecting a catalog up front bloats every turn for
/// a benefit the tools already cover.
pub(crate) fn default_system_prompt_registry() -> SystemPromptRegistry {
    let mut registry = SystemPromptRegistry::new();
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
// Session-review system-prompt sections (ADR-0039 stage 6).
//
// The `/review` diagnostic spawns a read-only reviewer envoy that used to
// pre-seed its system message (`build_reviewer_system_prompt`) and then run
// the streaming turn loop. Request assembly projects any pre-seeded system
// message out, so the seeded persona + dimensions + JSON contract were
// replaced by the default registry's tone+todo and never reached the model —
// the feature limped along only because verdict parsing degrades gracefully.
// Give the reviewer a dedicated registry whose composition IS the review
// prompt, so the request-scoped message is correct by construction.
// ---------------------------------------------------------------------------

/// The [`REVIEW`] role framing.
struct ReviewPersona;

impl SystemPromptSection for ReviewPersona {
    fn id(&self) -> &'static str {
        "review.persona"
    }
    fn rank(&self) -> u32 {
        10
    }
    fn render(&self, _ctx: &SystemPromptContext) -> Option<String> {
        Some(String::from(REVIEW.system_prompt))
    }
}

/// The list of registered review dimensions to evaluate, pre-rendered from
/// the live `[SessionReview]` set. Carried as owned text because the dimension
/// list is bespoke per `/review` run and does not fit the shared
/// [`SystemPromptContext`].
struct ReviewDimensions {
    body: String,
}

impl SystemPromptSection for ReviewDimensions {
    fn id(&self) -> &'static str {
        "review.dimensions"
    }
    fn rank(&self) -> u32 {
        20
    }
    fn render(&self, _ctx: &SystemPromptContext) -> Option<String> {
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

impl SystemPromptSection for ReviewJsonContract {
    fn id(&self) -> &'static str {
        "review.json_contract"
    }
    fn rank(&self) -> u32 {
        30
    }
    fn render(&self, _ctx: &SystemPromptContext) -> Option<String> {
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
/// contract. Installed on the reviewer via
/// [`crate::AgentBuilder::with_system_prompt_registry`] so its head system message —
/// rebuilt every round — is the review composition.
pub(crate) fn reviewer_system_prompt_registry(
    dimensions: &[Arc<dyn SessionReview>],
) -> SystemPromptRegistry {
    let mut registry = SystemPromptRegistry::new();
    registry.register(ReviewPersona);
    registry.register(ReviewDimensions {
        body: render_review_dimensions(dimensions),
    });
    registry.register(ReviewJsonContract);
    registry
}
