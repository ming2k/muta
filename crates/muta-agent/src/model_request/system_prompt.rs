//! Agent-owned declarative system-prompt composition (ADR-0056).
//!
//! A system prompt has a lifecycle unlike every other model-context message:
//! many policy sections compose into one head `Role::System` message, and that
//! singleton is rebuilt before every provider request. Harness-authored user
//! context is event-driven and carries a bespoke payload, so it stays outside
//! this registry and is constructed by `conversation_context` lifecycle owners.
//!
//! Registration is static and explicit. The policy lives with the agent that
//! owns request preparation; only provider-supplied prompt hints cross the
//! lower-layer contract boundary.

use muta_contracts::{InjectionKind, InjectionOrigin, Message, Role};

/// Read-only view of the live turn state a section may draw on to render.
///
/// Owned plain data (no `&Agent`) keeps a section's `render` signature free of
/// lifetime parameters. The context is rebuilt each round; the cost of cloning
/// a few small strings is negligible next to a model request. New fields are
/// added only when a real section needs them, so the surface stays minimal.
#[derive(Debug, Clone, Default)]
pub struct SystemPromptContext {
    /// The composed identity preamble sentence (name/mission/persona), empty
    /// for tests / when no identity is set.
    pub identity_preamble: String,
    /// Names of the tools admitted this turn (e.g. `["ask_user", ...]`).
    pub tool_names: Vec<String>,
    /// Model-specific guidance from the resolved model. Empty for all
    /// known models today; non-empty when a model entry carries a
    /// `Model::model_guidance`. Rendered verbatim by `ModelGuidance`.
    pub model_guidance: &'static str,
    /// Provider/protocol-specific prompt guidance from the active provider.
    /// This is intentionally factual and narrow: the SDK/provider may describe
    /// how its wire protocol projects tools, thinking, or replay metadata, but
    /// the agent still owns identity, workflow, and behavior policy.
    pub provider_guidance: &'static str,
    /// Whether the agent is running on autopilot this round — without human
    /// intervention. When true the harness has reclaimed `ask_user` and auto-
    /// approves every side-effecting tool, so the prompt tells the model no
    /// human is reachable and it must decide and act on its own authority.
    pub autopilot: bool,
    /// Available skills formatted as XML metadata for progressive disclosure.
    pub available_skills: String,
    /// Canonicalized additional workspace roots admitted alongside the
    /// primary (ADR-0142). Empty for the default single-root session;
    /// `WorkspaceRootsGuidance` renders the cross-project admission notice
    /// only when it is non-empty.
    pub additional_workspace_roots: Vec<String>,
}

impl SystemPromptContext {
    /// An all-empty context for registry-mechanics tests and for turns that
    /// genuinely carry no identity / tools.
    pub fn empty() -> Self {
        Self::default()
    }
}

/// A self-contained, declaratively registered system-prompt fragment.
///
/// Sections are typically unit-ish structs whose `render` draws only from the
/// shared [`SystemPromptContext`], making each independently testable.
///
/// The default [`SystemPromptSection::is_active`] is `true`; a section overrides it
/// to encode the single branch it owns — the *decision to appear* — kept with
/// the *text it would emit*.
pub trait SystemPromptSection: Send + Sync {
    /// Stable id, used for registration, override, disable, and debugging.
    /// Convention: `system.<area>[.<name>]`, e.g. `"system.tone"`.
    fn id(&self) -> &'static str;
    /// Default ordering within the system message. Lower sorts earlier. Stable so
    /// the output never depends on registration call order alone.
    fn rank(&self) -> u32;
    /// Whether this section applies in the current context. Default `true`.
    fn is_active(&self, _ctx: &SystemPromptContext) -> bool {
        true
    }
    /// Render the section body. `None` means "active but produces no text this
    /// turn"; the registry skips a `None` without leaving a blank gap.
    fn render(&self, ctx: &SystemPromptContext) -> Option<String>;
}

/// A registered section plus its runtime overrides.
struct Entry {
    section: Box<dyn SystemPromptSection + Send + Sync>,
    rank_override: Option<u32>,
    disabled: bool,
}

impl Entry {
    fn effective_rank(&self) -> u32 {
        self.rank_override.unwrap_or_else(|| self.section.rank())
    }
}

/// System-prompt policy assembled before an agent starts running.
///
/// Holds one [`SystemPromptSection`] per policy fragment, keyed by stable id.
/// Active fragments are assembled into one head message by
/// [`build_message`](Self::build_message).
#[derive(Default)]
pub struct SystemPromptRegistry {
    entries: Vec<Entry>,
}

/// Configuration error returned while composing a [`SystemPromptRegistry`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SystemPromptRegistryError {
    /// A section with the same stable id is already registered.
    DuplicateId(&'static str),
    /// An override refers to an id that is not registered.
    UnknownId(String),
}

impl std::fmt::Display for SystemPromptRegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateId(id) => write!(f, "duplicate SystemPromptSection id: {id}"),
            Self::UnknownId(id) => write!(f, "unknown SystemPromptSection id: {id}"),
        }
    }
}

impl std::error::Error for SystemPromptRegistryError {}

impl SystemPromptRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a section. Panics on a duplicate id — use
    /// [`try_register`](Self::try_register) when the section comes from an
    /// embedding or other fallible configuration surface.
    pub fn register<S: SystemPromptSection + 'static>(&mut self, section: S) {
        if let Err(error) = self.try_register(section) {
            panic!("{error}");
        }
    }

    /// Register a section without panicking on an id collision.
    pub fn try_register<S: SystemPromptSection + 'static>(
        &mut self,
        section: S,
    ) -> Result<(), SystemPromptRegistryError> {
        let id = section.id();
        if self.entries.iter().any(|e| e.section.id() == id) {
            return Err(SystemPromptRegistryError::DuplicateId(id));
        }
        self.entries.push(Entry {
            section: Box::new(section),
            rank_override: None,
            disabled: false,
        });
        Ok(())
    }

    /// Override a section's ordering by id, without editing its source. This
    /// is the lever for "flexible reordering": default order comes from
    /// [`SystemPromptSection::rank`], runtime overrides come from here.
    pub fn set_rank(&mut self, id: &str, rank: u32) -> Result<(), SystemPromptRegistryError> {
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.section.id() == id)
            .ok_or_else(|| SystemPromptRegistryError::UnknownId(id.to_owned()))?;
        entry.rank_override = Some(rank);
        Ok(())
    }

    /// Disable a section by id (it is skipped as if inactive). The opposite of
    /// `set_rank` — used to turn a section off without removing its
    /// registration.
    pub fn disable(&mut self, id: &str) -> Result<(), SystemPromptRegistryError> {
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.section.id() == id)
            .ok_or_else(|| SystemPromptRegistryError::UnknownId(id.to_owned()))?;
        entry.disabled = true;
        Ok(())
    }

    /// Assemble every active section into one head `Role::System` message:
    /// filter by active state, sort by effective rank (stable, so equal ranks
    /// preserve registration order), join with a newline, and stamp
    /// [`InjectionKind::SystemPrompt`].
    ///
    /// Sections that need a visual separator include a leading `\n` in their
    /// own `render`, so joining on a single `\n` reproduces the legacy
    /// `parts.join("\n")` layout exactly.
    pub fn build_message(&self, ctx: &SystemPromptContext) -> Message {
        let mut active: Vec<(u32, String)> = self
            .entries
            .iter()
            .filter(|e| !e.disabled)
            .filter(|e| e.section.is_active(ctx))
            .filter_map(|e| e.section.render(ctx).map(|r| (e.effective_rank(), r)))
            .collect();
        active.sort_by_key(|(rank, _)| *rank);
        let body: String = active
            .into_iter()
            .map(|(_, r)| r)
            .collect::<Vec<_>>()
            .join("\n");
        Message::new(Role::System, body)
            .with_origin(InjectionOrigin::new(InjectionKind::SystemPrompt))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A configurable section for registry-mechanics tests.
    struct S {
        id: &'static str,
        rank: u32,
        active: bool,
        text: Option<&'static str>,
    }

    impl SystemPromptSection for S {
        fn id(&self) -> &'static str {
            self.id
        }
        fn rank(&self) -> u32 {
            self.rank
        }
        fn is_active(&self, _ctx: &SystemPromptContext) -> bool {
            self.active
        }
        fn render(&self, _ctx: &SystemPromptContext) -> Option<String> {
            self.text.map(String::from)
        }
    }

    fn sys(id: &'static str, rank: u32, text: &'static str) -> S {
        S {
            id,
            rank,
            active: true,
            text: Some(text),
        }
    }

    #[test]
    fn system_message_orders_by_rank_and_joins_with_newline() {
        let mut reg = SystemPromptRegistry::new();
        // Registered out of rank order; output must follow rank.
        reg.register(sys("system.tone", 20, "Tone body."));
        reg.register(sys("system.identity", 10, "Identity body."));
        reg.register(sys("system.todo", 30, "Todo body."));

        let msg = reg.build_message(&SystemPromptContext::empty());
        assert_eq!(msg.role, Role::System);
        assert_eq!(msg.content, "Identity body.\nTone body.\nTodo body.");
    }

    #[test]
    fn equal_ranks_preserve_registration_order() {
        let mut reg = SystemPromptRegistry::new();
        reg.register(sys("system.a", 10, "A"));
        reg.register(sys("system.b", 10, "B"));
        reg.register(sys("system.c", 10, "C"));
        let msg = reg.build_message(&SystemPromptContext::empty());
        assert_eq!(msg.content, "A\nB\nC");
    }

    #[test]
    fn inactive_and_empty_renders_are_skipped_without_gaps() {
        let mut reg = SystemPromptRegistry::new();
        reg.register(sys("system.a", 10, "A"));
        reg.register(S {
            id: "system.inactive",
            rank: 20,
            active: false,
            text: Some("should not appear"),
        });
        reg.register(S {
            id: "system.empty",
            rank: 30,
            active: true,
            text: None,
        });
        reg.register(sys("system.d", 40, "D"));

        let msg = reg.build_message(&SystemPromptContext::empty());
        // No blank line where the skipped sections would have been.
        assert_eq!(msg.content, "A\nD");
    }

    #[test]
    fn system_message_origin_is_system_prompt() {
        let mut reg = SystemPromptRegistry::new();
        reg.register(sys("system.tone", 10, "Tone"));
        let msg = reg.build_message(&SystemPromptContext::empty());
        assert_eq!(
            msg.origin.as_ref().map(|o| o.kind),
            Some(InjectionKind::SystemPrompt)
        );
    }

    #[test]
    fn set_rank_reorders_output() {
        let mut reg = SystemPromptRegistry::new();
        reg.register(sys("system.a", 10, "A"));
        reg.register(sys("system.b", 20, "B"));
        reg.set_rank("system.b", 5).unwrap();

        let msg = reg.build_message(&SystemPromptContext::empty());
        assert_eq!(msg.content, "B\nA", "override rank wins over default");
    }

    #[test]
    fn disable_removes_section_from_output() {
        let mut reg = SystemPromptRegistry::new();
        reg.register(sys("system.a", 10, "A"));
        reg.register(sys("system.b", 20, "B"));
        reg.disable("system.b").unwrap();

        let msg = reg.build_message(&SystemPromptContext::empty());
        assert_eq!(msg.content, "A");
    }

    #[test]
    fn fallible_configuration_reports_invalid_ids() {
        let mut reg = SystemPromptRegistry::new();
        reg.try_register(sys("system.tone", 10, "Tone")).unwrap();
        assert_eq!(
            reg.try_register(sys("system.tone", 20, "Tone again")),
            Err(SystemPromptRegistryError::DuplicateId("system.tone"))
        );
        assert_eq!(
            reg.disable("missing"),
            Err(SystemPromptRegistryError::UnknownId("missing".to_owned()))
        );
        assert_eq!(
            reg.set_rank("missing", 1),
            Err(SystemPromptRegistryError::UnknownId("missing".to_owned()))
        );
    }

    #[test]
    #[should_panic(expected = "duplicate SystemPromptSection id: system.tone")]
    fn register_panics_on_duplicate_id() {
        let mut reg = SystemPromptRegistry::new();
        reg.register(sys("system.tone", 10, "Tone"));
        reg.register(sys("system.tone", 20, "Tone again"));
    }
}
