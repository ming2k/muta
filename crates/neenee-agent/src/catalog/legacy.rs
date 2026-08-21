//! One-shot migration from the pre-refactor persistence layout to the new one.
//!
//! Before the channels refactor, provider instances (with embedded per-model
//! channels) lived in `config.toml` `[[providers]]`, credentials split between
//! `[builtins.<id>]` / `[user.<id>]` in `credentials.toml`, per-model reasoning
//! in `[model_reasoning]`, and fitted/remote metadata on the instance itself.
//! This module reads that legacy layout *raw* (independent of the new `Config`
//! struct, which no longer has those fields) and produces the new stores:
//!
//! - provider instances → `providers.toml` (see `neenee_persistence::instances`)
//! - keys → `credentials.toml` `[providers.<id>]`
//! - per-(instance, model) reasoning + remote metadata → `models_discovery.json`
//!
//! It lives in the catalog layer (not `neenee-persistence`) because it needs
//! the template registry to separate template-derived facts from user
//! overrides — `neenee-providers` depends on `neenee-persistence`, so the
//! reverse dependency is impossible. It is a *one-way converter*, not a
//! maintained dual-path: it runs once, idempotently.

use std::collections::BTreeMap;

use neenee_contracts::{ChannelAuth, RemoteModelMetadata, SecretString};
use neenee_persistence::config::{Credentials, DiscoveryCache, UserTransport};
use neenee_persistence::instances::{Instances, ProviderInstance};
use neenee_persistence::route_settings::RouteSettingsStore;
use neenee_providers::provider_template_spec;
use serde::Deserialize;

/// Run the one-shot migration if (a) no instance store exists yet and (b)
/// legacy provider data is present. Returns `true` when it migrated something.
/// Idempotent: after a successful write the instance store exists, so a second
/// call is a no-op.
pub fn migrate_legacy_state() -> bool {
    let instances_path = neenee_persistence::paths::get().providers_file();
    if instances_path.exists()
        && !neenee_persistence::instances::Instances::load()
            .providers
            .is_empty()
    {
        return false;
    }
    let Some(legacy_config) = LegacyConfig::read().filter(|c| !c.providers.is_empty()) else {
        return false;
    };
    let legacy_creds = LegacyCredentials::read().unwrap_or_default();

    let mut instances = Instances::default();
    let mut creds = Credentials::default();
    let mut cache = DiscoveryCache::load();
    let mut routes = RouteSettingsStore::load();

    for legacy in &legacy_config.providers {
        let Some(first) = legacy.channels.first() else {
            continue;
        };
        let template_id = legacy
            .template_id
            .as_deref()
            .filter(|tid| provider_template_spec(tid).is_some());
        let is_template = template_id.is_some();

        instances.providers.push(ProviderInstance {
            id: legacy.id.clone(),
            name: legacy.name.clone(),
            template_id: template_id.map(str::to_string),
            auth: first.auth,
            api_key_env: first.api_key_env.clone(),
            // A template instance's transport/endpoint are derived from the
            // template; only persist a base_url / user_agent / transport that
            // differs from the template default (a deliberate user override).
            transport: if is_template {
                None
            } else {
                Some(first.transport)
            },
            base_url: override_base_url(legacy, first, template_id),
            user_agent: override_user_agent(first, template_id),
            models: if is_template {
                Vec::new()
            } else {
                legacy
                    .channels
                    .iter()
                    .filter_map(|c| c.model.clone())
                    .collect()
            },
        });

        // Credential: first non-empty channel key, else the legacy top-level
        // field for the matching built-in id, else the legacy credentials file.
        if let Some(key) = first.api_key.clone() {
            creds.set_api_key(&legacy.id, Some(key));
        } else if let Some(key) = legacy_config.builtin_api_key(&legacy.id) {
            creds.set_api_key(&legacy.id, Some(key.clone()));
        } else if let Some(key) = legacy_creds.api_key(&legacy.id) {
            creds.set_api_key(&legacy.id, Some(key.clone()));
        }

        // Per-route reasoning + remote metadata.
        for channel in &legacy.channels {
            let Some(model) = channel.model.as_deref() else {
                continue;
            };
            if channel.effort.is_some() || channel.thinking.is_some() {
                let entry = routes.settings_for_mut(&legacy.id, model);
                entry.effort = channel.effort.clone();
                entry.thinking = channel.thinking;
            }
            if let Some(remote) = &channel.remote {
                cache
                    .remote_metadata
                    .entry(legacy.id.clone())
                    .or_default()
                    .insert(model.to_string(), remote.clone());
            }
        }
    }

    // Legacy per-model reasoning keyed by model id (built-in Anthropic models)
    // applies to every instance that serves the model.
    for (model, settings) in &legacy_config.model_reasoning {
        if settings.effort.is_none() && settings.thinking.is_none() {
            continue;
        }
        for instance in &instances.providers {
            let serves = if instance.is_template() {
                instance
                    .template_id
                    .as_deref()
                    .and_then(provider_template_spec)
                    .is_some_and(|spec| spec.models.contains(&model.as_str()))
            } else {
                instance.models.contains(model)
            };
            if serves {
                let entry = routes.settings_for_mut(&instance.id, model);
                entry.effort = settings.effort.clone();
                entry.thinking = settings.thinking;
            }
        }
    }

    // Built-in base URLs (google / anthropic) that the user overrode become
    // instance-level overrides on the matching instance.
    if let Some(url) = &legacy_config.google_base_url
        && let Some(instance) = instances.providers.iter_mut().find(|p| p.id == "google")
    {
        instance.base_url = Some(url.clone());
    }
    if let Some(url) = &legacy_config.anthropic_base_url
        && let Some(instance) = instances.providers.iter_mut().find(|p| p.id == "anthropic")
    {
        instance.base_url = Some(url.clone());
    }

    if instances.providers.is_empty() {
        return false;
    }
    if instances.save().is_err()
        || creds.save().is_err()
        || cache.save().is_err()
        || routes.save().is_err()
    {
        return false;
    }
    tracing::info!(
        instances = instances.providers.len(),
        "migrated legacy provider instances to the state store"
    );
    true
}

/// The instance-level `base_url` override: `Some` when the first channel's
/// endpoint differs from the template default (a deliberate user override),
/// `None` when it matches the derivation. Custom instances always carry their
/// declared endpoint.
fn override_base_url(
    legacy: &LegacyProvider,
    first: &LegacyChannel,
    template_id: Option<&str>,
) -> Option<String> {
    let url = first.base_url.clone().filter(|u| !u.trim().is_empty())?;
    if let Some(tid) = template_id {
        let template_default =
            neenee_providers::route_for_model(tid, legacy.models.first()?).map(|(_, base, _)| base);
        if template_default == Some(url.as_str()) {
            return None;
        }
    }
    Some(url)
}

/// The instance-level `user_agent` override, mirroring [`override_base_url`].
fn override_user_agent(first: &LegacyChannel, template_id: Option<&str>) -> Option<String> {
    let ua = first.user_agent.clone().filter(|u| !u.trim().is_empty())?;
    if let Some(tid) = template_id
        && let Some(spec) = provider_template_spec(tid)
        && spec.user_agent == Some(ua.as_str())
    {
        return None;
    }
    Some(ua)
}

// ── Legacy schema (raw reads, independent of the new `Config`) ─────────────

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case", default)]
struct LegacyConfig {
    providers: Vec<LegacyProvider>,
    openai_api_key: Option<SecretString>,
    google_api_key: Option<SecretString>,
    moonshot_api_key: Option<SecretString>,
    deepseek_api_key: Option<SecretString>,
    zai_api_key: Option<SecretString>,
    opencode_go_api_key: Option<SecretString>,
    anthropic_api_key: Option<SecretString>,
    google_base_url: Option<String>,
    anthropic_base_url: Option<String>,
    model_reasoning: BTreeMap<String, LegacyReasoning>,
}

impl LegacyConfig {
    fn read() -> Option<Self> {
        let path = neenee_persistence::paths::get().config_file();
        let content = std::fs::read_to_string(&path).ok()?;
        toml::from_str(&content).ok()
    }

    /// The legacy top-level key field for a built-in instance id, if any.
    fn builtin_api_key(&self, id: &str) -> Option<&SecretString> {
        match id {
            "openai" => self.openai_api_key.as_ref(),
            "google" => self.google_api_key.as_ref(),
            "kimi-code" => self.moonshot_api_key.as_ref(),
            "deepseek" => self.deepseek_api_key.as_ref(),
            "zai-code" => self.zai_api_key.as_ref(),
            "opencode-go" => self.opencode_go_api_key.as_ref(),
            "anthropic" => self.anthropic_api_key.as_ref(),
            _ => None,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case", default)]
struct LegacyProvider {
    id: String,
    name: Option<String>,
    template_id: Option<String>,
    models: Vec<String>,
    channels: Vec<LegacyChannel>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case", default)]
struct LegacyChannel {
    label: String,
    transport: UserTransport,
    api_key_env: Option<String>,
    api_key: Option<SecretString>,
    model: Option<String>,
    base_url: Option<String>,
    user_agent: Option<String>,
    auth: ChannelAuth,
    effort: Option<String>,
    thinking: Option<bool>,
    remote: Option<RemoteModelMetadata>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case", default)]
struct LegacyReasoning {
    effort: Option<String>,
    thinking: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case", default)]
struct LegacyCredentials {
    builtins: BTreeMap<String, SecretString>,
    user: BTreeMap<String, LegacyUserCredential>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case", default)]
struct LegacyUserCredential {
    api_key: SecretString,
}

impl LegacyCredentials {
    fn read() -> Option<Self> {
        let path = neenee_persistence::paths::get().credentials_file();
        let content = std::fs::read_to_string(&path).ok()?;
        toml::from_str(&content).ok()
    }

    fn api_key(&self, instance_id: &str) -> Option<&SecretString> {
        self.user
            .get(instance_id)
            .map(|c| &c.api_key)
            .or_else(|| self.builtins.get(instance_id))
    }
}
