//! One-shot migration from legacy persistence layouts to connections.toml.

use std::collections::BTreeMap;

use neenee_contracts::{ChannelAuth, ClientIdentity, RemoteModelMetadata, SecretString};
use neenee_persistence::config::{Credentials, DiscoveryCache, UserTransport};
use neenee_persistence::connections::{Connection, Connections};
use neenee_persistence::route_settings::RouteSettingsStore;
use neenee_providers::provider_preset_spec;
use serde::Deserialize;

/// Run the one-shot migration if (a) no connections store exists yet and (b)
/// legacy provider data is present. Returns `true` when it migrated something.
pub fn migrate_legacy_state() -> bool {
    let connections_path = neenee_persistence::paths::get().connections_file();
    if connections_path.exists() && !Connections::load().connections.is_empty() {
        return false;
    }

    // Check if providers.toml exists from previous schema
    let old_providers_path = neenee_persistence::paths::get()
        .state_dir
        .join("providers.toml");
    if old_providers_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&old_providers_path) {
            #[derive(Deserialize)]
            struct OldProvidersFile {
                #[serde(default)]
                providers: Vec<Connection>,
            }
            if let Ok(old_file) = toml::from_str::<OldProvidersFile>(&content) {
                if !old_file.providers.is_empty() {
                    let connections = Connections {
                        connections: old_file.providers,
                    };
                    if connections.save().is_ok() {
                        tracing::info!("migrated providers.toml to connections.toml");
                        return true;
                    }
                }
            }
        }
    }

    let Some(legacy_config) = LegacyConfig::read().filter(|c| !c.providers.is_empty()) else {
        return false;
    };
    let legacy_creds = LegacyCredentials::read().unwrap_or_default();

    let mut connections = Connections::default();
    let mut creds = Credentials::default();
    let mut cache = DiscoveryCache::load();
    let mut routes = RouteSettingsStore::load();

    for legacy in &legacy_config.providers {
        let Some(first) = legacy.channels.first() else {
            continue;
        };
        let preset_id = legacy
            .template_id
            .as_deref()
            .filter(|tid| provider_preset_spec(tid).is_some());
        let is_preset = preset_id.is_some();

        connections.connections.push(Connection {
            id: legacy.id.clone(),
            name: legacy.name.clone(),
            preset_id: preset_id.map(str::to_string),
            auth: first.auth,
            api_key_env: first.api_key_env.clone(),
            client_identity: ClientIdentity::Native,
            transport: if is_preset {
                None
            } else {
                Some(first.transport)
            },
            base_url: override_base_url(legacy, first, preset_id),
            user_agent: override_user_agent(first, preset_id),
            models: if is_preset {
                Vec::new()
            } else {
                legacy
                    .channels
                    .iter()
                    .filter_map(|c| c.model.clone())
                    .collect()
            },
        });

        if let Some(key) = first.api_key.clone() {
            creds.set_api_key(&legacy.id, Some(key));
        } else if let Some(key) = legacy_config.builtin_api_key(&legacy.id) {
            creds.set_api_key(&legacy.id, Some(key.clone()));
        } else if let Some(key) = legacy_creds.api_key(&legacy.id) {
            creds.set_api_key(&legacy.id, Some(key.clone()));
        }

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

    for (model, settings) in &legacy_config.model_reasoning {
        if settings.effort.is_none() && settings.thinking.is_none() {
            continue;
        }
        for connection in &connections.connections {
            let serves = if connection.is_preset() {
                connection
                    .preset_id
                    .as_deref()
                    .and_then(provider_preset_spec)
                    .is_some_and(|spec| spec.models.contains(&model.as_str()))
            } else {
                connection.models.contains(model)
            };
            if serves {
                let entry = routes.settings_for_mut(&connection.id, model);
                entry.effort = settings.effort.clone();
                entry.thinking = settings.thinking;
            }
        }
    }

    if let Some(url) = &legacy_config.google_base_url
        && let Some(connection) = connections
            .connections
            .iter_mut()
            .find(|p| p.id == "google")
    {
        connection.base_url = Some(url.clone());
    }
    if let Some(url) = &legacy_config.anthropic_base_url
        && let Some(connection) = connections
            .connections
            .iter_mut()
            .find(|p| p.id == "anthropic")
    {
        connection.base_url = Some(url.clone());
    }

    if connections.connections.is_empty() {
        return false;
    }
    if connections.save().is_err()
        || creds.save().is_err()
        || cache.save().is_err()
        || routes.save().is_err()
    {
        return false;
    }
    tracing::info!(
        connections = connections.connections.len(),
        "migrated legacy connections to the state store"
    );
    true
}

fn override_base_url(
    legacy: &LegacyProvider,
    first: &LegacyChannel,
    preset_id: Option<&str>,
) -> Option<String> {
    let url = first.base_url.clone().filter(|u| !u.trim().is_empty())?;
    if let Some(pid) = preset_id {
        let preset_default =
            neenee_providers::route_for_model(pid, legacy.models.first()?).map(|(_, base, _)| base);
        if preset_default == Some(url.as_str()) {
            return None;
        }
    }
    Some(url)
}

fn override_user_agent(first: &LegacyChannel, preset_id: Option<&str>) -> Option<String> {
    let ua = first.user_agent.clone().filter(|u| !u.trim().is_empty())?;
    if let Some(pid) = preset_id
        && let Some(spec) = provider_preset_spec(pid)
        && spec.user_agent == Some(ua.as_str())
    {
        return None;
    }
    Some(ua)
}

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
