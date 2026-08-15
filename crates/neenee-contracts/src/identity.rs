//! Agent identity: who an `Agent` (re-exported by the agent
//! crate) is and what it is for. Pure domain vocabulary — three strings and a
//! formatter — with no agent-layer dependencies, so it lives in core alongside
//! the role profiles ([`crate::PrincipalProfile`], [`crate::EnvoyProfile`]).
//!
//! Kept identity-agnostic: nothing here hardcodes "neenee" or "coding". The
//! embedding (a CLI, a server) supplies the fields, so the same engine can be
//! repurposed as a different persona or for a different mission (research, ops,
//! writing) by passing different values.

/// Who an agent is and what it is for. Identity-agnostic: it does not hardcode
/// "neenee" or "coding". The embedding (the CLI, a future frontend) supplies
/// the fields so the same engine can be repurposed as a different persona or
/// for a different mission (research, ops, writing) by passing different
/// values. Everything else in the system prompt (tone, todo/ask_user guidance)
/// is mission-neutral and stays in the agent crate.
///
/// The three fields compose the opening line:
/// - [`AgentIdentity::name`] — what the agent is called ("neenee" for this
///   project; swap to repurpose the engine under a different product).
/// - [`AgentIdentity::mission`] — what the agent is for ("an expert AI coding
///   assistant…" for this CLI; swap for research/ops/etc.).
/// - [`AgentIdentity::persona`] — optional full-text override of the opening.
///   When set, [`AgentIdentity::preamble`] returns it verbatim and ignores
///   `name`/`mission`. Envoys use this to inject their role's full system
///   prompt as the identity.
///
/// [`AgentIdentity::default`] yields empty fields (no preamble — the system
/// prompt opens straight at the tone line); tests use it.
#[derive(Debug, Clone, Default)]
pub struct AgentIdentity {
    /// What this agent is called, e.g. `"neenee"`. Empty means "unnamed".
    pub name: String,
    /// What this agent is for, e.g. `"an expert AI coding assistant with tool
    /// access"`. Empty means "no mission framing".
    pub mission: String,
    /// Optional full-text identity override. When non-empty, `preamble`
    /// returns this verbatim (used by envoys whose identity *is* their
    /// role's full system prompt). None/empty → compose from name + mission.
    pub persona: Option<String>,
}

impl AgentIdentity {
    /// Build a structured identity from a name and a mission.
    pub fn new(name: impl Into<String>, mission: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            mission: mission.into(),
            persona: None,
        }
    }

    /// Build an identity whose preamble is a full persona string, ignoring
    /// name/mission composition. Used by envoys: their identity is the
    /// role's complete system prompt.
    pub fn from_persona(persona: impl Into<String>) -> Self {
        Self {
            name: String::new(),
            mission: String::new(),
            persona: Some(persona.into()),
        }
    }

    /// Render the opening system-prompt sentence. A `persona` override returns
    /// it verbatim; otherwise `"You are {name}, {mission}."` when both are set,
    /// `"You are {name}."` / `"You are {mission}."` when one is set, and the
    /// empty string when neither is (tests / identity-less agents).
    pub fn preamble(&self) -> String {
        if let Some(persona) = &self.persona
            && !persona.is_empty()
        {
            return persona.clone();
        }
        match (self.name.is_empty(), self.mission.is_empty()) {
            (true, true) => String::new(),
            (false, true) => format!("You are {}.", self.name),
            (true, false) => format!("You are {}.", self.mission),
            (false, false) => format!("You are {}, {}.", self.name, self.mission),
        }
    }
}
