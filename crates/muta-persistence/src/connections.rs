//! Connections — the persisted "who I connect to" records.
//!
//! A connection is the **security master** of provider connectivity: it
//! declares which preset (if any) it is created from, how it authenticates,
//! its client identity (impersonation / User-Agent), and — for pure-custom
//! connections with no preset — the transport/endpoint and model ids it serves.
//! It owns exactly one credential, keyed by connection id in `credentials.toml`
//! (or resolved from an `api_key_env` env var).
//!
//! Connections deliberately carry **no channels and no model-list state**:
//! the routes (per-model transport/endpoint/effort) are *derived* at runtime
//! from the connection's preset plus the discovery cache, so two connections of
//! the same preset never duplicate or drift a channel set, and the app never
//! persists production data it can re-derive.
//!
//! Stored in `$XDG_STATE_HOME/muta/connections.toml` — a program-managed
//! state file, separate from the user-edited `config.toml`.

use muta_contracts::{ClientIdentity, ConnectionAuth, WireProtocol};
use serde::{Deserialize, Serialize};

use crate::fsutil;
use crate::paths;

pub use muta_contracts::model::DeclaredModel;

/// One connection: a credentialed, configured use of a preset (or a pure-custom relay).
/// The connection's id is the join key for its credential (`credentials.toml [connections.<id>]`),
/// its OAuth token set (`auth.toml [tokens.<id>]`), its discovery cache, and `config.toml`'s
/// `default_connection`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Connection {
    /// Stable, unique connection id. Referenced by `config.toml`'s
    /// `default_connection` and by every per-connection store.
    pub id: String,
    /// Display name shown in the picker. Falls back to the id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The preset this connection is created from. `None` marks a pure-custom
    /// connection whose transport/endpoint/models are declared directly below.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_id: Option<String>,
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
    // Pure-custom declaration (only for `preset_id = None`)
    /// Wire transport for a custom connection's routes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<WireProtocol>,
    /// Endpoint for a custom connection's routes. `None` falls back to the
    /// transport's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// `User-Agent` header override for a custom connection's routes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    /// The model ids a custom connection serves, in picker order. Preset
    /// connections never set this — their model set is derived from the preset
    /// (and live discovery).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    /// User-declared extra models for a preset connection (ADR-0198): hidden
    /// or unstable upstream ids unioned into the derived set after discovery,
    /// scoped to this connection only. Never set on pure-custom connections
    /// (their `models` list is already the full declaration).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_models: Vec<DeclaredModel>,
}

impl Default for Connection {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: None,
            preset_id: None,
            auth: ConnectionAuth::ApiKey,
            api_key_env: None,
            client_identity: ClientIdentity::Native,
            protocol: None,
            base_url: None,
            user_agent: None,
            models: Vec::new(),
            extra_models: Vec::new(),
        }
    }
}

impl Connection {
    /// Whether this connection was created from a preset (its routes are
    /// derived) rather than being a pure-custom declaration.
    pub fn is_preset(&self) -> bool {
        self.preset_id.is_some()
    }

    /// The display name: the user-given `name`, else the id.
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }

    /// The declared model ids of a pure-custom connection. Preset connections
    /// return an empty slice (their set is derived).
    pub fn declared_models(&self) -> &[String] {
        &self.models
    }

    /// The declared extra model for `model_id`, if this connection declares one.
    pub fn extra_model(&self, model_id: &str) -> Option<&DeclaredModel> {
        self.extra_models.iter().find(|m| m.id == model_id)
    }

    /// The declared extra model ids, in declaration order.
    pub fn extra_model_ids(&self) -> Vec<String> {
        self.extra_models.iter().map(|m| m.id.clone()).collect()
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

impl Connections {
    fn path() -> std::path::PathBuf {
        paths::get().connections_file()
    }

    /// Read the connections store, returning an empty value when missing or unparseable.
    pub fn load() -> Self {
        let path = Self::path();
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match toml::from_str(&content) {
            Ok(connections) => connections,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "could not parse connections.toml; ignoring it",
                );
                Self::default()
            }
        }
    }

    /// Persist atomically. Errors propagate to the caller.
    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        let bytes = toml::to_string_pretty(self)?.into_bytes();
        fsutil::atomic_write_bytes(&Self::path(), &bytes)?;
        Ok(())
    }

    /// Look up a connection by id.
    pub fn get(&self, id: &str) -> Option<&Connection> {
        self.connections.iter().find(|p| p.id == id)
    }

    /// Look up a connection by id, mutably.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut Connection> {
        self.connections.iter_mut().find(|p| p.id == id)
    }

    /// Whether a connection with `id` already exists.
    pub fn contains(&self, id: &str) -> bool {
        self.get(id).is_some()
    }

    /// Remove a connection by id. Returns the removed connection.
    pub fn remove(&mut self, id: &str) -> Option<Connection> {
        let index = self.connections.iter().position(|p| p.id == id)?;
        Some(self.connections.remove(index))
    }

    /// Derive a stable, unique connection id from a user-given name: lowercase
    /// ASCII-slugged, suffixed with `-N` when the slug collides with an
    /// existing connection. Symbol-only / empty names fall back to `custom`.
    pub fn unique_id(&self, name: &str) -> String {
        let base = slug(name);
        if self.contains(&base) {
            let mut n = 2;
            loop {
                let candidate = format!("{base}-{n}");
                if !self.contains(&candidate) {
                    return candidate;
                }
                n += 1;
            }
        }
        base
    }

    /// The connection ids in declaration order.
    pub fn ids(&self) -> Vec<String> {
        self.connections.iter().map(|p| p.id.clone()).collect()
    }

    /// The effective default connection: `default_connection` when it names a
    /// live connection, else the first connection, else `None`.
    pub fn effective_default(&self, default_connection: &str) -> Option<&Connection> {
        self.get(default_connection)
            .or_else(|| self.connections.first())
    }
}

/// Slug a user-given connection name into an id: lowercase, non-alphanumeric
/// runs collapse to a single `-`, trimmed. `"***"` / `""` → `custom`.
pub fn slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_dash = true; // trim leading separators
    for ch in name.chars().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() {
            out.push(ch);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "custom".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muta_contracts::reasoning::ReasoningSupport;

    fn deepseek_with_extras() -> Connection {
        Connection {
            id: "deepseek-personal".into(),
            preset_id: Some("deepseek".into()),
            extra_models: vec![DeclaredModel {
                id: "deepseek-v4-pro-preview-0912".into(),
                context_window: Some(1_000_000),
                max_output_tokens: Some(8_192),
                thinking: Some(ReasoningSupport::ReasoningContent),
                vision: Some(false),
                tool_call: Some(true),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn extra_models_roundtrip_through_toml() {
        let mut conn = deepseek_with_extras();
        conn.extra_models.push(DeclaredModel {
            id: "bare-id".into(),
            ..Default::default()
        });
        let store = Connections {
            connections: vec![conn],
        };
        let text = toml::to_string_pretty(&store).unwrap();
        // Optional fields left unset must not serialize for the bare entry.
        assert!(text.contains("[[connections.extra_models]]"));
        assert!(!text.contains("bare-id ="), "sanity: id present as value");
        let bare_section = text
            .split("[[connections.extra_models]]")
            .nth(2)
            .expect("bare entry section");
        assert!(
            !bare_section.contains("context_window")
                && !bare_section.contains("vision")
                && !bare_section.contains("thinking"),
            "unset fields skip serialization"
        );
        let parsed: Connections = toml::from_str(&text).unwrap();
        assert_eq!(parsed, store);
    }

    #[test]
    fn legacy_connection_without_extra_models_loads() {
        let legacy = r#"
[[connections]]
id = "ds"
preset_id = "deepseek"
"#;
        let parsed: Connections = toml::from_str(legacy).unwrap();
        assert!(parsed.connections[0].extra_models.is_empty());
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
