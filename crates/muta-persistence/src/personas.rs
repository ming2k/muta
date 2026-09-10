//! Persistent personas (`personas.toml`, ADR-0220): user-authored named
//! principals. A persona is user-edited config, never program state; its
//! sessions live in the shared store independent of this definition's
//! lifecycle (ADR-0226).

use muta_contracts::{AgentIdentity, AgentPersonaId};
use serde::{Deserialize, Serialize, de::Deserializer};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// How a persona binds a filesystem workspace (ADR-0220 §1). Parsed from a
/// bare TOML string: `"none"`, `"inherit"`, or an absolute path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum PersonaWorkspace {
    None,
    Inherit,
    Fixed(PathBuf),
}

impl PersonaWorkspace {
    pub fn parse(value: &str) -> Self {
        match value.trim() {
            "none" => PersonaWorkspace::None,
            "inherit" => PersonaWorkspace::Inherit,
            other => PersonaWorkspace::Fixed(PathBuf::from(other)),
        }
    }

    pub fn requires_binding(&self) -> bool {
        !matches!(self, PersonaWorkspace::None)
    }
}

impl<'de> Deserialize<'de> for PersonaWorkspace {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Ok(PersonaWorkspace::parse(&raw))
    }
}

/// One declared persona. See [`PersonasConfig`] for the file shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Persona {
    pub name: String,
    pub mission: Option<String>,
    pub persona: Option<String>,
    pub preset: String,
    pub workspace: Option<PersonaWorkspace>,
    /// Optional default conversation space for a workspace-free persona
    /// (ADR-0226). The space is the user's label, not the persona's identity.
    pub space: Option<String>,
    pub unattended: bool,
    pub connection: Option<String>,
    pub model: Option<String>,
}

impl Default for Persona {
    fn default() -> Self {
        Self {
            name: String::new(),
            mission: None,
            persona: None,
            preset: default_preset(),
            workspace: None,
            space: None,
            unattended: false,
            connection: None,
            model: None,
        }
    }
}

fn default_preset() -> String {
    AgentPersonaId::Conversational.as_str().to_string()
}

impl Persona {
    /// The preset this persona binds, defaulting to `conversational` when absent
    /// or unrecognized.
    pub fn preset_id(&self) -> AgentPersonaId {
        AgentPersonaId::parse(&self.preset).unwrap_or(AgentPersonaId::Conversational)
    }

    /// The effective workspace policy: the explicit declaration when present,
    /// otherwise derived from the preset (a workspace-free preset binds nothing;
    /// a workspace preset inherits the launch directory).
    pub fn resolved_workspace(&self) -> PersonaWorkspace {
        self.workspace.clone().unwrap_or_else(|| {
            if self.preset_id() == AgentPersonaId::Conversational {
                PersonaWorkspace::None
            } else {
                PersonaWorkspace::Inherit
            }
        })
    }

    /// The persona's default conversation space, when declared (ADR-0226).
    /// `None` means the Personal space. This is a convenience default, not an
    /// identity: the space is user-owned and persona-independent.
    pub fn resolved_space(&self) -> Option<String> {
        self.space.clone()
    }

    /// The identity bound at agent construction (ADR-0053).
    pub fn identity(&self) -> AgentIdentity {
        if let Some(persona) = self.persona.as_ref().filter(|p| !p.trim().is_empty()) {
            AgentIdentity::from_persona(persona.clone())
        } else if let Some(mission) = self.mission.as_ref().filter(|m| !m.trim().is_empty()) {
            AgentIdentity::new(self.name.clone(), mission.clone())
        } else {
            AgentIdentity::new(self.name.clone(), "")
        }
    }

    /// Validate the persona against its bound preset (ADR-0220 §4).
    pub fn validate(&mut self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            self.name = self.preset.clone();
        }
        if AgentPersonaId::parse(&self.preset).is_none() {
            return Err(format!(
                "unknown preset '{}'; expected one of {}",
                self.preset,
                AgentPersonaId::ALL
                    .iter()
                    .map(|preset| preset.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        Ok(())
    }
}

/// The parsed `personas.toml` registry, keyed by stable persona id.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersonasConfig {
    #[serde(default)]
    pub personas: BTreeMap<String, Persona>,
}

impl PersonasConfig {
    /// Load `personas.toml` from the resolved config directory (ADR-0220 §2).
    /// Absent file = no personas. A parse or validation error is logged and
    /// degrades to an empty registry rather than failing startup.
    pub fn load() -> Self {
        let path = crate::paths::get().personas_file();
        match std::fs::read_to_string(&path) {
            Ok(raw) => match Self::from_toml_str(&raw) {
                Ok(config) => config,
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "personas.toml invalid; ignored");
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }

    /// Parse and validate a `personas.toml` body.
    pub fn from_toml_str(raw: &str) -> Result<Self, String> {
        let mut config: Self =
            toml::from_str(raw).map_err(|error| format!("personas.toml parse error: {error}"))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&mut self) -> Result<(), String> {
        for (id, persona) in self.personas.iter_mut() {
            if !is_valid_id(id) {
                return Err(format!(
                    "invalid persona id '{id}': use lowercase letters, digits, '-' or '_'"
                ));
            }
            persona
                .validate()
                .map_err(|error| format!("persona '{id}': {error}"))?;
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&Persona> {
        self.personas.get(id)
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.personas.keys().map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.personas.is_empty()
    }
}

fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
[personas.english-practice]
name = "English Practice"
mission = "a patient English conversation partner who corrects gently"
preset = "conversational"
workspace = "none"
"#;

    #[test]
    fn parses_and_round_trips_a_persona() {
        let config = PersonasConfig::from_toml_str(SAMPLE).unwrap();
        assert_eq!(config.ids().collect::<Vec<_>>(), vec!["english-practice"]);
        let persona = config.get("english-practice").unwrap();
        assert_eq!(persona.preset_id(), AgentPersonaId::Conversational);
        assert_eq!(persona.resolved_workspace(), PersonaWorkspace::None);
        assert_eq!(persona.resolved_space(), None);
        assert_eq!(
            persona.identity().preamble(),
            "You are English Practice, a patient English conversation partner who corrects gently."
        );
    }

    #[test]
    fn empty_file_is_an_empty_registry() {
        let config = PersonasConfig::from_toml_str("").unwrap();
        assert!(config.is_empty());
    }

    #[test]
    fn workspace_defaults_from_the_preset() {
        let config = PersonasConfig::from_toml_str(
            r#"
[personas.coder]
name = "Coder"
preset = "code"
"#,
        )
        .unwrap();
        let persona = config.get("coder").unwrap();
        assert_eq!(persona.resolved_workspace(), PersonaWorkspace::Inherit);
    }

    #[test]
    fn unknown_preset_is_rejected() {
        let error = PersonasConfig::from_toml_str(
            r#"
[personas.wizard]
name = "Wizard"
preset = "wizard"
"#,
        )
        .unwrap_err();
        assert!(error.contains("unknown preset"), "{error}");
    }

    #[test]
    fn workspace_policy_is_not_constrained_by_role() {
        // ADR-0223: the workspace requirement is derived from the execution
        // environment, not declared by the persona/preset. A coding persona may
        // resolve to a workspace-free binding.
        let config = PersonasConfig::from_toml_str(
            r#"
[personas.broken]
name = "Broken"
preset = "code"
workspace = "none"
"#,
        )
        .unwrap();
        assert_eq!(
            config.get("broken").unwrap().resolved_workspace(),
            PersonaWorkspace::None
        );
    }

    #[test]
    fn invalid_persona_id_is_rejected() {
        let error = PersonasConfig::from_toml_str(
            r#"
[personas."Bad Id"]
name = "Bad"
"#,
        )
        .unwrap_err();
        assert!(error.contains("invalid persona id"), "{error}");
    }
}
