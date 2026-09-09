//! Live model discovery and the fitted-model overlay.
//!
//! Discovery fetches each discovery-capable connection's `GET /models` list
//! live and folds it into the catalog's remote-catalog overlay (ADR-0203):
//! advertised capability fields are trusted per preset and recorded in the
//! per-connection discovery cache. Routes are *derived* from that cache at
//! catalog-build time — nothing here mutates config or the connection store.
//! Transport, status, and schema failures retain the last valid subset. A
//! structurally valid empty result is authoritative and clears the connection.

use super::Stores;
use super::derive::{resolve_credential, route_models};
use futures::stream::{self, StreamExt};
use muta_contracts::{RemoteCatalogEndpoint, RemoteCatalogSourceOverride, WireProtocol};
use muta_persistence::config::{DiscoveryCache, FittedModelInfo, ModelListCacheState};
use muta_persistence::connections::Connections;
use muta_providers::{
    DiscoveryProtocol, ModelDiscoveryOptions, ModelDiscoveryRequest, ModelDiscoveryUpdate,
    ModelProviderSpec, RemoteCatalogSource, model_provider_spec, route_for_model,
};
use sha2::{Digest, Sha256};

const MODEL_LIST_CACHE_TTL_MS: i64 = 5 * 60 * 1000;
const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const DISCOVERY_CONCURRENCY: usize = 8;

/// The concrete network source a discovery job speaks. Both variants are
/// network feeds normalized to the same [`ModelDiscoveryUpdate`]; the axis is
/// *who serves the data*, not the transport.
enum DiscoverySource {
    /// The provider's own catalog endpoint (first-party `GET /models`).
    FirstParty {
        protocol: DiscoveryProtocol,
        base_url: String,
        client_profile: muta_contracts::ClientProfile,
        cached_etag: Option<String>,
    },
    /// A third-party catalog entry (models.dev), keyed by provider id.
    ModelsDev { provider: String },
}

impl DiscoverySource {
    /// Fingerprint every request attribute that may select a different catalog
    /// representation. Validators and TTLs must never cross this boundary.
    fn identity(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(b"remote-catalog-request-v1\0");
        match self {
            Self::ModelsDev { provider } => {
                digest.update(b"models-dev\0");
                digest.update(provider.as_bytes());
            }
            Self::FirstParty {
                protocol,
                base_url,
                client_profile,
                ..
            } => {
                digest.update(b"first-party\0");
                digest.update(discovery_protocol_id(*protocol).as_bytes());
                digest.update(b"\0");
                if *protocol == DiscoveryProtocol::Codex {
                    digest.update(muta_contracts::client_identity::CODEX_VERSION.as_bytes());
                    digest.update(b"\0");
                }
                digest.update(base_url.as_bytes());
                digest.update(b"\0");
                digest.update(client_profile.user_agent().as_bytes());
                let mut headers = client_profile.headers();
                headers.sort_unstable();
                for (name, value) in headers {
                    digest.update(b"\0");
                    digest.update(name.as_bytes());
                    digest.update(b"\0");
                    digest.update(value.as_bytes());
                }
            }
        }
        format!("sha256:{:x}", digest.finalize())
    }

    fn discard_validator(&mut self) {
        if let Self::FirstParty { cached_etag, .. } = self {
            *cached_etag = None;
        }
    }
}

const fn discovery_protocol_id(protocol: DiscoveryProtocol) -> &'static str {
    match protocol {
        DiscoveryProtocol::OpenAi => "openai",
        DiscoveryProtocol::Anthropic => "anthropic",
        DiscoveryProtocol::Google => "google",
        DiscoveryProtocol::GoogleCloudCode => "google-cloud-code",
        DiscoveryProtocol::Codex => "codex",
    }
}

struct DiscoveryJob {
    connection: muta_persistence::connections::Connection,
    spec: &'static ModelProviderSpec,
    source: DiscoverySource,
    api_key: muta_contracts::SecretString,
}

struct DiscoveryFetch {
    connection: muta_persistence::connections::Connection,
    spec: &'static ModelProviderSpec,
    source_identity: String,
    update: Result<ModelDiscoveryUpdate, String>,
}

async fn fetch_models(job: DiscoveryJob) -> DiscoveryFetch {
    let source_identity = job.source.identity();
    match job.source {
        DiscoverySource::ModelsDev { provider } => {
            let update = fetch_models_dev(&provider).await;
            DiscoveryFetch {
                connection: job.connection,
                spec: job.spec,
                source_identity,
                update,
            }
        }
        DiscoverySource::FirstParty {
            protocol,
            base_url,
            client_profile,
            cached_etag,
        } => {
            let auth = if job.connection.auth.is_oauth() {
                let source = muta_providers::oauth::OAuthCredentialSource::new(
                    &job.connection.name,
                    job.connection.auth,
                );
                match muta_contracts::CredentialSource::resolve_auth(&source).await {
                    Ok(auth) => auth,
                    Err(error) => {
                        return DiscoveryFetch {
                            connection: job.connection,
                            spec: job.spec,
                            source_identity,
                            update: Err(error),
                        };
                    }
                }
            } else {
                muta_contracts::ResolvedAuth::new(job.api_key)
            };
            let extra_headers = client_profile.headers();
            let request = ModelDiscoveryRequest {
                protocol,
                base_url: &base_url,
                api_key: &auth.token,
                account_id: auth.account_id.as_deref(),
                user_agent: Some(client_profile.user_agent()),
                extra_headers: &extra_headers,
            };
            let options = ModelDiscoveryOptions {
                etag: cached_etag.as_deref(),
            };
            let update = muta_providers::discover_models(request, options)
                .await
                .map_err(|error| error.to_string());
            DiscoveryFetch {
                connection: job.connection,
                spec: job.spec,
                source_identity,
                update,
            }
        }
    }
}

/// Resolve a models.dev provider entry into a discovery update. This is the
/// shared fetch for both the dedicated `ModelsDev` source and the first-party
/// fallback path.
async fn fetch_models_dev(provider: &str) -> Result<ModelDiscoveryUpdate, String> {
    muta_providers::models_dev_models(provider)
        .await
        .map(|models| ModelDiscoveryUpdate::Modified { models, etag: None })
        .map_err(|error| error.to_string())
}

/// The result of a live model-discovery pass ([`discover_provider_models`]).
#[derive(Debug, Default)]
pub struct DiscoveryOutcome {
    /// Whether any connection changed its cached model list or fitted metadata.
    pub changed: bool,
    /// Per-connection fetch failures: `(connection_name, error_message)`.
    pub failures: Vec<(String, String)>,
}

/// Canonical ADR-0203 alias for [`DiscoveryOutcome`].
pub type RemoteCatalogOutcome = DiscoveryOutcome;

/// Synchronize the remote model catalog across all connections (ADR-0203 canonical entry point).
pub async fn sync_remote_catalog(force: bool) -> DiscoveryOutcome {
    discover_provider_models(force).await
}

/// Synchronize the remote model catalog for one exact connection (ADR-0203 canonical entry point).
pub async fn sync_connection_remote_catalog(
    connection_name: &str,
    force: bool,
) -> DiscoveryOutcome {
    discover_connection_models(connection_name, force).await
}

/// Fetch every discovery-capable connection's live model list and update the
/// discovery cache.
pub async fn discover_provider_models(force: bool) -> DiscoveryOutcome {
    discover_models_matching(None, force).await
}

/// Refresh one exact connection. Login and add flows use this path so an
/// unrelated slow provider cannot delay or contaminate their result.
pub async fn discover_connection_models(connection_name: &str, force: bool) -> DiscoveryOutcome {
    discover_models_matching(Some(connection_name), force).await
}

pub async fn refresh_connection_models_for_etag(
    connection_name: &str,
    advertised_etag: &str,
) -> DiscoveryOutcome {
    let cached = DiscoveryCache::load();
    let connections = Connections::load();
    let Some(connection) = connections.get(connection_name) else {
        return DiscoveryOutcome::default();
    };
    let Some(spec) = model_provider_spec(&connection.provider) else {
        return DiscoveryOutcome::default();
    };
    let Some(source) = discovery_source(connection, &cached, spec) else {
        return DiscoveryOutcome::default();
    };
    let expected_source_identity = source.identity();
    let Some(state) = cached.model_lists.get(connection_name) else {
        return discover_connection_models(connection_name, true).await;
    };
    if state.etag.as_deref() != Some(advertised_etag)
        || state.client_version != CLIENT_VERSION
        || state.source_identity != expected_source_identity
    {
        return discover_connection_models(connection_name, true).await;
    }

    let now_ms = chrono::Utc::now().timestamp_millis();
    if now_ms.saturating_sub(state.refreshed_at_ms) < MODEL_LIST_CACHE_TTL_MS / 2 {
        return DiscoveryOutcome::default();
    }
    let mut locked = match DiscoveryCache::lock().await {
        Ok(lock) => lock,
        Err(error) => {
            return DiscoveryOutcome {
                changed: false,
                failures: vec![(connection_name.to_string(), error)],
            };
        }
    };
    let Some(current) = locked.model_lists.get_mut(connection_name) else {
        drop(locked);
        return discover_connection_models(connection_name, true).await;
    };
    if current.etag.as_deref() != Some(advertised_etag)
        || current.client_version != CLIENT_VERSION
        || current.source_identity != expected_source_identity
    {
        drop(locked);
        return discover_connection_models(connection_name, true).await;
    }
    current.refreshed_at_ms = now_ms;
    match locked.save() {
        Ok(()) => DiscoveryOutcome::default(),
        Err(error) => DiscoveryOutcome {
            changed: false,
            failures: vec![(connection_name.to_string(), error.to_string())],
        },
    }
}

async fn discover_models_matching(target: Option<&str>, force: bool) -> DiscoveryOutcome {
    let stores = Stores::load();
    let mut failures: Vec<(String, String)> = Vec::new();
    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut jobs = Vec::new();

    for connection in &stores.connections.connections {
        if target.is_some_and(|target| target != connection.name) {
            continue;
        }
        let Some(spec) = model_provider_spec(&connection.provider) else {
            continue;
        };
        let Some(mut source) = discovery_source(connection, &stores.cache, spec) else {
            continue;
        };
        let source_identity = source.identity();
        let cache_identity_matches =
            stores
                .cache
                .model_lists
                .get(&connection.name)
                .is_some_and(|state| {
                    state.client_version == CLIENT_VERSION
                        && state.source_identity == source_identity
                });
        if !force
            && cache_identity_matches
            && stores
                .cache
                .model_lists
                .get(&connection.name)
                .is_some_and(|state| {
                    now_ms.saturating_sub(state.refreshed_at_ms) < MODEL_LIST_CACHE_TTL_MS
                })
            && stores
                .cache
                .connection_models
                .get(&connection.name)
                .is_some_and(|models| !models.is_empty())
        {
            continue;
        }
        if force || !cache_identity_matches {
            source.discard_validator();
        }
        jobs.push(DiscoveryJob {
            connection: connection.clone(),
            spec,
            source,
            api_key: resolve_credential(connection, &stores.creds),
        });
    }

    if jobs.is_empty() {
        return DiscoveryOutcome::default();
    }

    // Requests are independent and bounded by their own timeout. Fetch with a
    // fixed concurrency ceiling outside any lock, preserving connection order.
    let fetched = stream::iter(jobs.into_iter().map(fetch_models))
        .buffered(DISCOVERY_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;

    // Acquire lock to reconcile with fresh connections and update cache atomically.
    let mut locked_cache = match DiscoveryCache::lock().await {
        Ok(lock) => lock,
        Err(error) => {
            return DiscoveryOutcome {
                changed: false,
                failures: vec![(target.unwrap_or("model-discovery-cache").to_string(), error)],
            };
        }
    };

    let current_connections = Connections::load();
    let mut changed = false;
    let mut cache_dirty = false;

    for fetched in fetched {
        let connection = &fetched.connection;
        // Never merge or resurrect if connection was deleted during the network fetch!
        if current_connections.get(&connection.name).is_none() {
            continue;
        }
        let _spec = fetched.spec;
        match fetched.update {
            Ok(ModelDiscoveryUpdate::Modified { models, etag }) => {
                let mut connection_changed = false;
                let fitted: std::collections::BTreeMap<String, FittedModelInfo> = models
                    .iter()
                    .filter(|model| model.picker_enabled != Some(false))
                    .filter(|model| muta_contracts::model::model_by_id(&model.id).is_none())
                    .map(|model| (model.id.clone(), fitted_model_info(model)))
                    .collect();
                if locked_cache.fitted_models.get(&connection.name) != Some(&fitted) {
                    locked_cache
                        .fitted_models
                        .insert(connection.name.clone(), fitted);
                    connection_changed = true;
                }
                let supported: Vec<String> = models
                    .iter()
                    .filter(|model| model.picker_enabled != Some(false))
                    .map(|model| model.id.clone())
                    .collect();
                let remote_metadata: std::collections::BTreeMap<String, _> = models
                    .iter()
                    .filter(|model| model.picker_enabled != Some(false))
                    .map(|model| (model.id.clone(), model.remote_metadata()))
                    .collect();
                let prev_remote = locked_cache
                    .remote_metadata
                    .get(&connection.name)
                    .cloned()
                    .unwrap_or_default();
                if prev_remote != remote_metadata {
                    locked_cache
                        .remote_metadata
                        .insert(connection.name.clone(), remote_metadata);
                    connection_changed = true;
                }
                if locked_cache.connection_models.get(&connection.name) != Some(&supported) {
                    locked_cache
                        .connection_models
                        .insert(connection.name.clone(), supported);
                    connection_changed = true;
                }
                let state = ModelListCacheState {
                    etag,
                    client_version: CLIENT_VERSION.to_string(),
                    source_identity: fetched.source_identity.clone(),
                    refreshed_at_ms: now_ms,
                };
                if locked_cache.model_lists.get(&connection.name) != Some(&state) {
                    locked_cache
                        .model_lists
                        .insert(connection.name.clone(), state);
                    cache_dirty = true;
                }
                if connection_changed {
                    changed = true;
                    cache_dirty = true;
                    tracing::info!(
                        connection = %connection.name,
                        discovered_count = models.len(),
                        "live model discovery updated connection"
                    );
                }
            }
            Ok(ModelDiscoveryUpdate::NotModified { etag }) => {
                locked_cache.model_lists.insert(
                    connection.name.clone(),
                    ModelListCacheState {
                        etag,
                        client_version: CLIENT_VERSION.to_string(),
                        source_identity: fetched.source_identity.clone(),
                        refreshed_at_ms: now_ms,
                    },
                );
                cache_dirty = true;
                tracing::debug!(
                    connection = %connection.name,
                    "live model catalog revalidated without changes"
                );
            }
            Err(error) => {
                tracing::warn!(
                    connection = %connection.name,
                    error = %error,
                    "live model discovery failed; keeping previous models"
                );
                failures.push((connection.name.clone(), error.to_string()));
            }
        }
    }

    if cache_dirty && let Err(error) = locked_cache.save() {
        tracing::error!(?error, "could not persist model-discovery cache");
        failures.push(("model-discovery-cache".to_string(), error.to_string()));
        changed = false;
    }

    DiscoveryOutcome { changed, failures }
}

fn discovery_source(
    connection: &muta_persistence::connections::Connection,
    cache: &DiscoveryCache,
    spec: &'static ModelProviderSpec,
) -> Option<DiscoverySource> {
    match connection.catalog_source.as_ref() {
        Some(RemoteCatalogSourceOverride::ModelsDev { models_dev }) => {
            Some(DiscoverySource::ModelsDev {
                provider: models_dev.clone(),
            })
        }
        Some(RemoteCatalogSourceOverride::Endpoint { endpoint }) => {
            let protocol = match endpoint {
                RemoteCatalogEndpoint::OpenAiCompatible | RemoteCatalogEndpoint::Copilot => {
                    DiscoveryProtocol::OpenAi
                }
                RemoteCatalogEndpoint::Anthropic => DiscoveryProtocol::Anthropic,
                RemoteCatalogEndpoint::Google => DiscoveryProtocol::Google,
                RemoteCatalogEndpoint::GoogleCloudCode => DiscoveryProtocol::GoogleCloudCode,
                RemoteCatalogEndpoint::Codex => DiscoveryProtocol::Codex,
            };
            build_first_party_source(connection, cache, spec, protocol)
        }
        None => match spec.catalog_source {
            RemoteCatalogSource::ModelsDev { provider } => Some(DiscoverySource::ModelsDev {
                provider: provider.to_string(),
            }),
            RemoteCatalogSource::Endpoint(protocol) => {
                build_first_party_source(connection, cache, spec, protocol)
            }
            RemoteCatalogSource::None => None,
        },
    }
}

#[cfg(test)]
pub(super) fn source_identity_for_connection(
    connection: &muta_persistence::connections::Connection,
    cache: &DiscoveryCache,
) -> Option<String> {
    let spec = model_provider_spec(&connection.provider)?;
    discovery_source(connection, cache, spec).map(|source| source.identity())
}

/// Build the [`DiscoverySource::FirstParty`] variant for a connection,
/// Returns `None` only when the
/// connection's first route cannot be derived (unknown provider or an empty
/// model seed) — a provider's declared `live_catalog` scheme is authoritative,
/// so OAuth providers (ChatGPT Codex, Google Antigravity cloudcode) discover
/// their own first-party catalog exactly like keyed providers do.
#[allow(clippy::too_many_arguments)]
fn build_first_party_source(
    connection: &muta_persistence::connections::Connection,
    cache: &DiscoveryCache,
    spec: &'static ModelProviderSpec,
    protocol: DiscoveryProtocol,
) -> Option<DiscoverySource> {
    let first_model = route_models(connection, cache)
        .into_iter()
        .next()
        .unwrap_or_default();
    let (_wire, provider_base, provider_ua) = route_for_model(&connection.provider, &first_model)?;
    let base_url = connection
        .base_url
        .clone()
        .filter(|u| !u.trim().is_empty())
        .unwrap_or_else(|| provider_base.to_string());
    let client_profile = if let Some(user_agent) = connection.user_agent.as_deref() {
        muta_contracts::ClientProfile::from_user_agent(user_agent)
    } else if connection.client_identity != muta_contracts::ClientIdentity::Native {
        connection.client_identity.clone()
    } else if spec.default_client_profile != muta_contracts::ClientPreset::Native {
        muta_contracts::ClientProfile::from(spec.default_client_profile)
    } else if let Some(user_agent) = provider_ua {
        muta_contracts::ClientProfile::from_user_agent(user_agent)
    } else {
        muta_contracts::ClientProfile::Native
    };
    let cached_etag = cache
        .model_lists
        .get(&connection.name)
        .and_then(|state| state.etag.clone());
    Some(DiscoverySource::FirstParty {
        protocol,
        base_url,
        client_profile,
        cached_etag,
    })
}

/// Rebuild the fitted-model overlay (`muta_contracts::model`) from the
/// discovery cache.
pub fn sync_fitted_model_registry() {
    let cache = DiscoveryCache::load();
    let connections = Connections::load();
    let fitted: Vec<muta_contracts::model::FittedModel> = connections
        .connections
        .iter()
        .flat_map(|connection| {
            let spec = model_provider_spec(&connection.provider);
            let fitted_map = cache.fitted_models.get(&connection.name);
            fitted_map.map(|map| {
                let (format, family) = match spec {
                    Some(spec) => (spec.protocol, spec.id.to_string()),
                    None => (
                        WireProtocol::OpenAiChatCompletions,
                        connection.provider.clone(),
                    ),
                };
                map.iter()
                    .map(move |(id, info)| muta_contracts::model::FittedModel {
                        id: id.clone(),
                        family: family.clone(),
                        context_window: info.context_window,
                        reasoning: info.reasoning,
                        vision: info.vision,
                        protocol: format,
                        effort_levels: info
                            .efforts
                            .iter()
                            .filter_map(|level| match muta_contracts::Effort::parse(level) {
                                Some(e) => Some(e),
                                None => {
                                    tracing::warn!(
                                        level = level,
                                        model = %id,
                                        "effort tier outside the known vocabulary; \
                                         preserved on the channel but not the static \
                                         baseline"
                                    );
                                    None
                                }
                            })
                            .collect(),
                    })
            })
        })
        .flatten()
        .collect();
    muta_contracts::model::register_fitted_models(fitted);
}

fn fitted_model_info(model: &muta_providers::DiscoveredModel) -> FittedModelInfo {
    FittedModelInfo {
        context_window: model.context_window.unwrap_or(0),
        reasoning: model.reasoning.unwrap_or(false),
        vision: model.vision.unwrap_or(false),
        efforts: model.effort_levels.clone().unwrap_or_default(),
    }
}
