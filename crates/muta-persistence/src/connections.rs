//! Connections — the persisted "who I connect to" records.
//!
//! A connection is a **named pipe** to a model provider (ADR-0201): it points
//! at exactly one [`ModelProvider`] by id, owns one credential, declares the
//! client identity (impersonation / User-Agent) it speaks with, and may narrow
//! or override the provider's model universe. It never defines models itself.
//!
//! The connection's **name is its identity** — the primary key for its
//! credential (`credentials.toml [connections.<name>]`), its OAuth token set
//! (`auth.toml [tokens.<name>]`), its discovery cache, and `config.toml`'s
//! `default_connection`. Names are unique and compared case-insensitively; a
//! duplicate is rejected with a suggested alternative rather than silently
//! disambiguated.
//!
//! Connections deliberately carry **no channels and no model-list state**: the
//! routes (per-model transport/endpoint/effort) are *derived* at runtime from
//! the connection's provider plus the discovery cache, so two connections to
//! the same provider never duplicate or drift a channel set, and the app never
//! persists production data it can re-derive.
//!
//! Stored in `$XDG_STATE_HOME/muta/connections.toml` — a program-managed
//! state file, separate from the user-edited `config.toml`.

use muta_contracts::model_providers::canonical_provider_id;
use muta_contracts::{ClientIdentity, ConnectionAuth, WireProtocol};
use serde::{Deserialize, Serialize};

use crate::fsutil;
use crate::paths;

pub use muta_contracts::model::{
    ConnectionFilterPolicy, DeclaredModel, ModelCapabilityPatch, ModelScopeConfig,
    NamedFilterPolicy,
};

/// The provider id used by a connection that brings its own endpoint. It is an
/// ordinary model provider whose model universe is open (ADR-0201 §6).
pub const CUSTOM_PROVIDER: &str = "custom";

/// One connection: a credentialed pipe to a model provider.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    /// Connection name — the connection's identity and primary key (ADR-0201).
    /// Unique (case-insensitive) across the store; referenced by
    /// `config.toml`'s `default_connection` and by every per-connection store.
    pub name: String,
    /// The model provider this connection points at. Must resolve to a
    /// registered provider; an unknown value is rejected at load.
    pub provider: String,
    /// How this connection authenticates. [`ConnectionAuth::ApiKey`] (the default)
    /// resolves the bearer from the connection credential; the OAuth variants
    /// resolve from `auth.toml`.
    #[serde(default)]
    pub auth: ConnectionAuth,
    /// Optional environment variable name holding this connection's credential.
    /// A 12-factor override: when set (and non-empty), it wins over
    /// `credentials.toml`. Declared once per connection, never per route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// Client profile specifying User-Agent and client identity headers (Native/muta, OpenCode, ZCode, Claude Code, etc.).
    /// Defaults to [`muta_contracts::ClientProfile::Native`].
    #[serde(default, alias = "client_profile")]
    pub client_identity: ClientIdentity,
    /// Wire transport override. `None` uses the provider's default protocol.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<WireProtocol>,
    /// Endpoint override. `None` uses the provider's default endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// `User-Agent` header override. `None` uses the provider's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    /// Model scope configuration (ADR-0199, ADR-0201): this connection's
    /// include / exclude / override delta over the provider's universe. A
    /// curated provider admits only models it knows; `provider = "custom"`
    /// admits anything.
    #[serde(default, skip_serializing_if = "ModelScopeConfig::is_empty")]
    pub models: ModelScopeConfig,
}

impl Default for Connection {
    fn default() -> Self {
        Self {
            name: String::new(),
            provider: CUSTOM_PROVIDER.to_string(),
            auth: ConnectionAuth::ApiKey,
            api_key_env: None,
            client_identity: ClientIdentity::Native,
            protocol: None,
            base_url: None,
            user_agent: None,
            models: ModelScopeConfig::default(),
        }
    }
}

impl Connection {
    /// The declared model ids of this connection, in declaration order.
    pub fn declared_models(&self) -> Vec<String> {
        self.models.included_ids()
    }

    /// Look up an explicitly declared/included model on this connection.
    pub fn extra_model(&self, model_id: &str) -> Option<&DeclaredModel> {
        self.models.find_included(model_id)
    }

    /// The declared included model ids, in declaration order.
    pub fn extra_model_ids(&self) -> Vec<String> {
        self.models.included_ids()
    }
}

/// The persisted set of connections (`connections.toml`). Program-managed
/// state, separate from the user-edited `config.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Connections {
    #[serde(default)]
    pub connections: Vec<Connection>,
}

/// Pre-ADR-0201 wire shape, accepted only when reading an existing file. The
/// loader rewrites it into [`Connection`] and never serializes these fields
/// back — the migration is one-shot and leaves no alias in the schema.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConnection {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    preset_id: Option<String>,
    #[serde(default)]
    auth: ConnectionAuth,
    #[serde(default)]
    api_key_env: Option<String>,
    #[serde(default, alias = "client_profile")]
    client_identity: ClientIdentity,
    #[serde(default)]
    protocol: Option<WireProtocol>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    user_agent: Option<String>,
    #[serde(default, deserialize_with = "deserialize_connection_models")]
    models: ModelScopeConfig,
    #[serde(default)]
    extra_models: Vec<DeclaredModel>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConnections {
    #[serde(default)]
    connections: Vec<RawConnection>,
}

impl RawConnection {
    /// Migrate to the current shape, resolving the provider id and folding the
    /// legacy `extra_models` list into `models.include`.
    fn migrate(self) -> Result<Connection, String> {
        let name = self
            .name
            .or(self.id)
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .ok_or_else(|| "connection has neither `name` nor a legacy `id`".to_string())?;

        let declared = self.provider.or(self.preset_id);
        let provider = match declared {
            // A pure-custom connection (`preset_id` absent) becomes a `custom`
            // provider connection — one kind, not two (ADR-0201 §6).
            None => CUSTOM_PROVIDER.to_string(),
            Some(raw) => canonical_provider_id(raw.trim())
                .ok_or_else(|| format!("unknown model provider '{raw}'"))?,
        };

        let mut models = self.models;
        for extra in self.extra_models {
            if !models.include.iter().any(|m| m.id == extra.id) {
                models.include.push(extra);
            }
        }

        Ok(Connection {
            name,
            provider,
            auth: self.auth,
            api_key_env: self.api_key_env,
            client_identity: self.client_identity,
            protocol: self.protocol,
            base_url: self.base_url,
            user_agent: self.user_agent,
            models,
        })
    }
}

fn deserialize_connection_models<'de, D>(deserializer: D) -> Result<ModelScopeConfig, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum RawModels {
        Scope(ModelScopeConfig),
        List(Vec<String>),
    }

    match Option::<RawModels>::deserialize(deserializer)? {
        Some(RawModels::Scope(scope)) => Ok(scope),
        Some(RawModels::List(list)) => Ok(ModelScopeConfig {
            filter: None,
            include: list
                .into_iter()
                .map(|id| DeclaredModel {
                    id,
                    ..Default::default()
                })
                .collect(),
            exclude: Vec::new(),
            overrides: std::collections::BTreeMap::new(),
        }),
        None => Ok(ModelScopeConfig::default()),
    }
}

impl Connections {
    fn path() -> std::path::PathBuf {
        paths::get().connections_file()
    }

    /// Read the connections store, returning an empty value when missing or
    /// unparseable. Entries that fail migration — an unknown provider id, or no
    /// name at all — are rejected with an error log rather than reinterpreted
    /// as a different connection kind (ADR-0201 INV-2).
    pub fn load() -> Self {
        let path = Self::path();
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let raw: RawConnections = match toml::from_str(&content) {
            Ok(raw) => raw,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "could not parse connections.toml; ignoring it",
                );
                return Self::default();
            }
        };
        let mut connections: Vec<Connection> = Vec::with_capacity(raw.connections.len());
        for raw in raw.connections {
            match raw.migrate() {
                Ok(conn) => {
                    if connections
                        .iter()
                        .any(|existing| existing.name.eq_ignore_ascii_case(&conn.name))
                    {
                        tracing::error!(
                            name = %conn.name,
                            "duplicate connection name in connections.toml; dropping the later entry",
                        );
                        continue;
                    }
                    connections.push(conn);
                }
                Err(reason) => {
                    tracing::error!(
                        path = %path.display(),
                        reason = %reason,
                        "rejecting invalid connection entry",
                    );
                }
            }
        }
        Self { connections }
    }

    /// Persist atomically. Errors propagate to the caller.
    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        let bytes = toml::to_string_pretty(self)?.into_bytes();
        fsutil::atomic_write_bytes(&Self::path(), &bytes)?;
        Ok(())
    }

    /// Look up a connection by name (case-insensitive).
    pub fn get(&self, name: &str) -> Option<&Connection> {
        self.connections
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(name))
    }

    /// Look up a connection by name (case-insensitive), mutably.
    pub fn get_mut(&mut self, name: &str) -> Option<&mut Connection> {
        self.connections
            .iter_mut()
            .find(|c| c.name.eq_ignore_ascii_case(name))
    }

    /// Whether a connection named `name` already exists.
    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Remove a connection by name. Returns the removed connection.
    pub fn remove(&mut self, name: &str) -> Option<Connection> {
        let index = self
            .connections
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(name))?;
        Some(self.connections.remove(index))
    }

    /// The connection names in declaration order.
    pub fn names(&self) -> Vec<String> {
        self.connections.iter().map(|c| c.name.clone()).collect()
    }

    /// Validate a proposed new connection name. Returns the rejection reason
    /// plus a suggested alternative when the name is empty or already taken.
    /// Names are compared case-insensitively (ADR-0201 INV-3).
    pub fn check_new_name(&self, name: &str) -> Result<String, String> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err("a connection name is required".to_string());
        }
        if self.contains(trimmed) {
            let suggestion = self.suggest_name(trimmed);
            return Err(format!(
                "a connection named '{trimmed}' already exists; try '{suggestion}'"
            ));
        }
        Ok(trimmed.to_string())
    }

    /// A free name derived from `name`: the name with a numeric suffix, chosen
    /// to avoid every existing connection. Never used to silently rename a
    /// connection — only to fill the rejection message (ADR-0201 INV-3).
    pub fn suggest_name(&self, name: &str) -> String {
        let base = name.trim();
        let base = if base.is_empty() { "connection" } else { base };
        let mut n = 2;
        loop {
            let candidate = format!("{base}-{n}");
            if !self.contains(&candidate) {
                return candidate;
            }
            n += 1;
        }
    }

    /// The effective default connection: `default_connection` when it names a
    /// live connection, else the first connection, else `None`.
    pub fn effective_default(&self, default_connection: &str) -> Option<&Connection> {
        self.get(default_connection)
            .or_else(|| self.connections.first())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muta_contracts::reasoning::ReasoningSupport;

    fn deepseek_with_extras() -> Connection {
        Connection {
            name: "deepseek-personal".into(),
            provider: "deepseek".into(),
            models: ModelScopeConfig {
                filter: None,
                include: vec![DeclaredModel {
                    id: "deepseek-v4-pro-preview-0912".into(),
                    context_window: Some(1_000_000),
                    max_output_tokens: Some(8_192),
                    thinking: Some(ReasoningSupport::ReasoningContent),
                    vision: Some(false),
                    tool_call: Some(true),
                }],
                exclude: Vec::new(),
                overrides: std::collections::BTreeMap::new(),
            },
            ..Default::default()
        }
    }

    #[test]
    fn models_scope_roundtrip_through_toml() {
        let mut conn = deepseek_with_extras();
        conn.models.include.push(DeclaredModel {
            id: "bare-id".into(),
            ..Default::default()
        });
        conn.models.exclude.push("deprecated-model".into());
        let store = Connections {
            connections: vec![conn],
        };
        let text = toml::to_string_pretty(&store).unwrap();
        assert!(text.contains("[[connections.models.include]]"));
        assert!(text.contains("deprecated-model"));
        let parsed: Connections = toml::from_str(&text).unwrap();
        assert_eq!(parsed, store);
    }

    #[test]
    fn legacy_id_and_preset_id_migrate_on_load() {
        let raw: RawConnections = toml::from_str(
            r#"
[[connections]]
id = "ds"
preset_id = "deepseek"

[[connections.extra_models]]
id = "legacy-preview"
context_window = 500000
"#,
        )
        .unwrap();
        let conn = raw
            .connections
            .into_iter()
            .next()
            .unwrap()
            .migrate()
            .unwrap();
        assert_eq!(conn.name, "ds");
        assert_eq!(conn.provider, "deepseek");
        assert_eq!(conn.models.include.len(), 1);
        assert_eq!(conn.models.include[0].id, "legacy-preview");
        assert_eq!(conn.models.include[0].context_window, Some(500000));
    }

    #[test]
    fn legacy_pure_custom_connection_becomes_the_custom_provider() {
        let raw: RawConnections = toml::from_str(
            r#"
[[connections]]
id = "custom-ollama"
models = ["llama3:latest", "mistral:latest"]
"#,
        )
        .unwrap();
        let conn = raw
            .connections
            .into_iter()
            .next()
            .unwrap()
            .migrate()
            .unwrap();
        assert_eq!(conn.name, "custom-ollama");
        assert_eq!(conn.provider, CUSTOM_PROVIDER);
        assert_eq!(conn.models.include.len(), 2);
        assert_eq!(conn.models.include[0].id, "llama3:latest");
    }

    #[test]
    fn legacy_provider_ids_are_canonicalized() {
        for (legacy, canonical) in [
            ("chatgpt-oauth", "openai-subscription"),
            ("antigravity-oauth", "google-antigravity"),
            ("copilot-oauth", "github-copilot"),
            ("xai-oauth", "xai"),
            ("zai-code", "glm-cn"),
            ("custom-openai", "custom"),
        ] {
            let raw: RawConnections = toml::from_str(&format!(
                "[[connections]]\nname = \"c\"\npreset_id = \"{legacy}\"\n"
            ))
            .unwrap();
            let conn = raw
                .connections
                .into_iter()
                .next()
                .unwrap()
                .migrate()
                .unwrap();
            assert_eq!(conn.provider, canonical, "{legacy} → {canonical}");
        }
    }

    #[test]
    fn unknown_provider_is_rejected_not_reinterpreted() {
        let raw: RawConnections =
            toml::from_str("[[connections]]\nname = \"c\"\nprovider = \"nope\"\n").unwrap();
        let err = raw
            .connections
            .into_iter()
            .next()
            .unwrap()
            .migrate()
            .unwrap_err();
        assert!(err.contains("unknown model provider 'nope'"), "{err}");
    }

    #[test]
    fn names_are_unique_case_insensitively() {
        let store = Connections {
            connections: vec![deepseek_with_extras()],
        };
        assert!(store.contains("DeepSeek-Personal"));
        assert!(store.check_new_name("DeepSeek-Personal").is_err());
        let err = store.check_new_name("deepseek-personal").unwrap_err();
        assert!(err.contains("deepseek-personal-2"), "{err}");
        assert!(store.check_new_name("  ").is_err());
        assert_eq!(store.check_new_name(" other ").unwrap(), "other");
    }

    #[test]
    fn extra_model_lookup_is_exact_and_empty_by_default() {
        let conn = deepseek_with_extras();
        assert_eq!(
            conn.extra_model("deepseek-v4-pro-preview-0912")
                .unwrap()
                .context_window,
            Some(1_000_000)
        );
        // Exact id match — case variants are distinct ids.
        assert!(conn.extra_model("Deepseek-v4-pro-preview-0912").is_none());
        assert!(conn.extra_model("deepseek-v4-flash").is_none());
        assert!(Connection::default().extra_model("anything").is_none());
    }

    #[test]
    fn sanitized_declared_model_rejects_blank_ids() {
        assert!(DeclaredModel::sanitized("  ").is_none());
        let declared = DeclaredModel::sanitized("  deepseek-x  ").unwrap();
        assert_eq!(declared.id, "deepseek-x");
        assert!(declared.context_window.is_none());
    }
}
