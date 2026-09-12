//! Persistent agent roles (`roles.toml`, legacy `personas.toml`): user-authored
//! custom roles. A role is user-edited config, never program state; its
//! sessions live in the shared store independent of this definition's
//! lifecycle (ADR-0226).

use muta_contracts::{AgentIdentity, MainAgentRole};
use serde::{Deserialize, Serialize, de::Deserializer};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// How a role binds a filesystem workspace. Parsed from a
/// bare TOML string: `"none"`, `"inherit"`, or an absolute path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum RoleWorkspace {
    None,
    Inherit,
    Fixed(PathBuf),
}

impl RoleWorkspace {
    pub fn parse(value: &str) -> Self {
        match value.trim() {
            "none" => RoleWorkspace::None,
            "inherit" => RoleWorkspace::Inherit,
            other => RoleWorkspace::Fixed(PathBuf::from(other)),
        }
    }

    pub fn requires_binding(&self) -> bool {
        !matches!(self, RoleWorkspace::None)
    }
}

impl<'de> Deserialize<'de> for RoleWorkspace {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Ok(RoleWorkspace::parse(&raw))
    }
}

/// One declared custom role. See [`RolesConfig`] for the file shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomRole {
    pub name: String,
    pub mission: Option<String>,
    #[serde(alias = "persona")]
    pub directive: Option<String>,
    pub preset: String,
    pub workspace: Option<RoleWorkspace>,
    pub unattended: bool,
    pub connection: Option<String>,
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admit_mcp: Option<Vec<String>>,
}

impl Default for CustomRole {
    fn default() -> Self {
        Self {
            name: String::new(),
            mission: None,
            directive: None,
            preset: default_preset(),
            workspace: None,
            unattended: false,
            connection: None,
            model: None,
            admit_mcp: None,
        }
    }
}

fn default_preset() -> String {
    MainAgentRole::Philosophist.as_str().to_string()
}

impl CustomRole {
    /// The preset this role binds, defaulting to `philosophist` when absent
    /// or unrecognized.
    pub fn preset_id(&self) -> MainAgentRole {
        MainAgentRole::parse(&self.preset).unwrap_or(MainAgentRole::Philosophist)
    }

    /// The effective workspace policy: the explicit declaration when present,
    /// otherwise derived from the preset (a workspace-free preset binds nothing;
    /// a workspace preset inherits the launch directory).
    pub fn resolved_workspace(&self) -> RoleWorkspace {
        self.workspace.clone().unwrap_or_else(|| {
            if self.preset_id() == MainAgentRole::Philosophist {
                RoleWorkspace::None
            } else {
                RoleWorkspace::Inherit
            }
        })
    }

    /// The identity bound at agent construction.
    pub fn identity(&self) -> AgentIdentity {
        if let Some(directive) = self.directive.as_ref().filter(|p| !p.trim().is_empty()) {
            AgentIdentity::from_directive(directive.clone())
        } else if let Some(mission) = self.mission.as_ref().filter(|m| !m.trim().is_empty()) {
            AgentIdentity::new(self.name.clone(), mission.clone())
        } else {
            AgentIdentity::new(self.name.clone(), "")
        }
    }

    /// Validate the role against its bound preset.
    pub fn validate(&mut self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            self.name = self.preset.clone();
        }
        if MainAgentRole::parse(&self.preset).is_none() {
            return Err(format!(
                "unknown preset '{}'; expected one of {}",
                self.preset,
                MainAgentRole::ALL
                    .iter()
                    .map(|preset| preset.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        Ok(())
    }

    /// Whether this role admits tools from the given MCP server name (ADR-0242).
    /// An absent pattern or `["*"]` admits all configured servers.
    pub fn admits_mcp_server(&self, server: &str) -> bool {
        let Some(patterns) = &self.admit_mcp else {
            return true;
        };
        patterns.iter().any(|pat| {
            if pat == "*" {
                true
            } else if let Some(prefix) = pat.strip_suffix('*') {
                server.starts_with(prefix)
            } else {
                pat == server
            }
        })
    }
}

/// The parsed `roles.toml` (or legacy `personas.toml`) registry, keyed by stable role id.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RolesConfig {
    #[serde(default, alias = "personas")]
    pub roles: BTreeMap<String, CustomRole>,
}

impl RolesConfig {
    /// Load `roles.toml` from the resolved config directory, falling back to legacy `personas.toml`.
    /// Absent file = no roles. A parse or validation error is logged and
    /// degrades to an empty registry rather than failing startup.
    pub fn load() -> Self {
        let roles_path = crate::paths::get().roles_file();
        let path = if roles_path.exists() {
            roles_path
        } else {
            crate::paths::get().personas_file()
        };
        match std::fs::read_to_string(&path) {
            Ok(raw) => match Self::from_toml_str(&raw) {
                Ok(config) => config,
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "roles.toml invalid; ignored");
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }

    /// Load with optional workspace override (ADR-0243).
    /// If `workspace_root` contains `.muta/config.toml` with `[roles.<name>]`,
    /// those definitions override or extend the global roles.
    pub fn load_for_workspace(workspace_root: Option<&std::path::Path>) -> Self {
        let mut config = Self::load();
        if let Some(root) = workspace_root {
            let project_cfg_path = root.join(".muta").join("config.toml");
            if project_cfg_path.exists() {
                if let Ok(raw) = std::fs::read_to_string(&project_cfg_path) {
                    if let Ok(override_cfg) = Self::from_toml_str(&raw) {
                        for (id, role) in override_cfg.roles {
                            config.roles.insert(id, role);
                        }
                    }
                }
            }
        }
        config
    }

    /// Parse and validate a `roles.toml` / `personas.toml` body.
    pub fn from_toml_str(raw: &str) -> Result<Self, String> {
        let mut config: Self =
            toml::from_str(raw).map_err(|error| format!("roles.toml parse error: {error}"))?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&mut self) -> Result<(), String> {
        for (id, role) in self.roles.iter_mut() {
            if !is_valid_id(id) {
                return Err(format!(
                    "invalid role id '{id}': use lowercase letters, digits, '-' or '_'"
                ));
            }
            role.validate()
                .map_err(|error| format!("role '{id}': {error}"))?;
        }
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&CustomRole> {
        self.roles.get(id)
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.roles.keys().map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.roles.is_empty()
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
[roles.english-practice]
name = "English Practice"
mission = "a patient English conversation partner who corrects gently in context"
preset = "philosophist"
workspace = "none"
unattended = false
admit_mcp = ["obsidian", "phil*"]

[roles.coder]
name = "Coder"
preset = "developer"

[roles.wizard]
name = "Wizard"
preset = "developer"
workspace = "none"
"#;

    const SAMPLE_LEGACY: &str = r#"
[personas.english-practice]
name = "English Practice"
mission = "a patient English conversation partner who corrects gently in context"
preset = "philosophist"
workspace = "none"
unattended = false
"#;

    #[test]
    fn parses_and_round_trips_a_role() {
        let config = RolesConfig::from_toml_str(SAMPLE).unwrap();
        let role = config.get("english-practice").unwrap();
        assert_eq!(role.preset_id(), MainAgentRole::Philosophist);
        assert_eq!(role.resolved_workspace(), RoleWorkspace::None);
        assert_eq!(
            role.identity().preamble(),
            "You are English Practice, a patient English conversation partner who corrects gently in context."
        );
        assert!(!role.unattended);
        assert!(role.admits_mcp_server("obsidian"));
        assert!(role.admits_mcp_server("philpapers"));
        assert!(!role.admits_mcp_server("postgres"));
    }

    #[test]
    fn parses_legacy_personas_format() {
        let config = RolesConfig::from_toml_str(SAMPLE_LEGACY).unwrap();
        let role = config.get("english-practice").unwrap();
        assert_eq!(role.preset_id(), MainAgentRole::Philosophist);
    }

    #[test]
    fn coder_inherits_workspace_by_default() {
        let config = RolesConfig::from_toml_str(SAMPLE).unwrap();
        let role = config.get("coder").unwrap();
        assert_eq!(role.resolved_workspace(), RoleWorkspace::Inherit);
    }

    #[test]
    fn invalid_role_id_is_rejected() {
        let raw = r#"
[roles."Bad Id"]
name = "Bad"
preset = "developer"
"#;
        let error = RolesConfig::from_toml_str(raw).unwrap_err();
        assert!(error.contains("invalid role id"), "{error}");
    }

    #[test]
    fn workspace_role_override_replaces_global_definition() {
        let temp = tempfile::tempdir().unwrap();
        let muta_dir = temp.path().join(".muta");
        std::fs::create_dir_all(&muta_dir).unwrap();
        let project_cfg = r#"
[roles.coder]
name = "Workspace Coder"
preset = "developer"
admit_mcp = ["internal_pg"]
"#;
        std::fs::write(muta_dir.join("config.toml"), project_cfg).unwrap();

        let cfg = RolesConfig::load_for_workspace(Some(temp.path()));
        let coder = cfg.get("coder").unwrap();
        assert_eq!(coder.name, "Workspace Coder");
        assert_eq!(coder.admit_mcp.as_deref(), Some(&["internal_pg".to_string()][..]));
    }
}
