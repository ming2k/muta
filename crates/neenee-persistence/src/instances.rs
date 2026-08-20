//! Provider instances — the persisted "who I connect to" records.
//!
//! An instance is the **security principal** of provider connectivity: it
//! declares which template (if any) it is created from, how it authenticates,
//! and — for pure-custom instances with no template — the transport/endpoint
//! and model ids it serves. It owns exactly one credential, keyed by instance
//! id in `credentials.toml` (or resolved from an `api_key_env` env var).
//!
//! Instances deliberately carry **no channels and no model-list state**:
//! the routes (per-model transport/endpoint/effort) are *derived* at runtime
//! from the instance's template plus the discovery cache, so two instances of
//! the same template never duplicate or drift a channel set, and the app never
//! persists production data it can re-derive. See
//! `neenee_agent::catalog::derive` for the derivation.
//!
//! Stored in `$XDG_STATE_HOME/neenee/providers.toml` — a program-managed
//! state file, not the user-edited `config.toml`, which keeps only *behavior*
//! (`default_provider` / `default_model` / permissions / …).

use neenee_contracts::ChannelAuth;
use serde::{Deserialize, Serialize};

use crate::config::UserTransport;
use crate::fsutil;
use crate::paths;

/// One provider instance: a credentialed, configured use of a template (or a
/// pure-custom relay). The instance's id is the join key for its credential
/// (`credentials.toml [providers.<id>]`), its OAuth token set
/// (`auth.toml [tokens.<id>]`), its discovery cache, and `config.toml`'s
/// `default_provider`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProviderInstance {
    /// Stable, unique instance id. Referenced by `config.toml`'s
    /// `default_provider` and by every per-instance store.
    pub id: String,
    /// Display name shown in the picker. Falls back to the id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The template this instance is created from. `None` marks a pure-custom
    /// instance whose transport/endpoint/models are declared directly below.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_id: Option<String>,
    /// How this instance authenticates. [`ChannelAuth::ApiKey`] (the default)
    /// resolves the bearer from the instance credential; the OAuth variants
    /// resolve from `auth.toml`.
    #[serde(default)]
    pub auth: ChannelAuth,
    /// Optional environment variable name holding this instance's credential.
    /// A 12-factor override: when set (and non-empty), it wins over
    /// `credentials.toml`. Declared once per instance, never per route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    // ── Pure-custom declaration (only for `template_id = None`) ─────────────
    /// Wire transport for a custom instance's routes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<UserTransport>,
    /// Endpoint for a custom instance's routes. `None` falls back to the
    /// transport's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// `User-Agent` header for a custom instance's routes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    /// The model ids a custom instance serves, in picker order. Template
    /// instances never set this — their model set is derived from the template
    /// (and live discovery).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
}

impl Default for ProviderInstance {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: None,
            template_id: None,
            auth: ChannelAuth::ApiKey,
            api_key_env: None,
            transport: None,
            base_url: None,
            user_agent: None,
            models: Vec::new(),
        }
    }
}

impl ProviderInstance {
    /// Whether this instance was created from a template (its routes are
    /// derived) rather than being a pure-custom declaration.
    pub fn is_template(&self) -> bool {
        self.template_id.is_some()
    }

    /// The display name: the user-given `name`, else the id.
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }

    /// The declared model ids of a pure-custom instance. Template instances
    /// return an empty slice (their set is derived).
    pub fn declared_models(&self) -> &[String] {
        &self.models
    }
}

/// The persisted set of provider instances (`providers.toml`). Program-managed
/// state, separate from the user-edited `config.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Instances {
    #[serde(default)]
    pub providers: Vec<ProviderInstance>,
}

impl Instances {
    fn path() -> std::path::PathBuf {
        paths::get().providers_file()
    }

    /// Read the instance store, returning an empty (not erroring) value when
    /// the file is missing or unparseable. A missing file is a normal
    /// first-run condition; a corrupt one must never block startup.
    pub fn load() -> Self {
        let path = Self::path();
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match toml::from_str(&content) {
            Ok(instances) => instances,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "could not parse the provider instance store; ignoring it",
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

    /// Look up an instance by id.
    pub fn get(&self, id: &str) -> Option<&ProviderInstance> {
        self.providers.iter().find(|p| p.id == id)
    }

    /// Look up an instance by id, mutably.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut ProviderInstance> {
        self.providers.iter_mut().find(|p| p.id == id)
    }

    /// Whether an instance with `id` already exists.
    pub fn contains(&self, id: &str) -> bool {
        self.get(id).is_some()
    }

    /// Remove an instance by id. Returns the removed instance.
    pub fn remove(&mut self, id: &str) -> Option<ProviderInstance> {
        let index = self.providers.iter().position(|p| p.id == id)?;
        Some(self.providers.remove(index))
    }

    /// Derive a stable, unique instance id from a user-given name: lowercase
    /// ASCII-slugged, suffixed with `-N` when the slug collides with an
    /// existing instance. Symbol-only / empty names fall back to `custom`.
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

    /// The instance ids in declaration order.
    pub fn ids(&self) -> Vec<String> {
        self.providers.iter().map(|p| p.id.clone()).collect()
    }

    /// The effective default instance: `default_provider` when it names a
    /// live instance, else the first instance, else `None`. This is the single
    /// place `config.toml`'s `default_provider` is reconciled against reality.
    pub fn effective_default(&self, default_provider: &str) -> Option<&ProviderInstance> {
        self.get(default_provider)
            .or_else(|| self.providers.first())
    }
}

/// Slug a user-given instance name into an id: lowercase, non-alphanumeric
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
