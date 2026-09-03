//! Provider-switch / favorite / default-model handlers.
//!
//! Each handler is one match arm of the agent background task's dispatch.
//! Provider instances live in the `providers.toml` state store, credentials in
//! `credentials.toml`, and per-route facts in the discovery cache — `config`
//! holds only *behavior* (`default_provider` / `default_model` / favorites),
//! which is what these handlers persist there. Routes are derived by the
//! catalog at activation time.

use muta_agent::Agent;
use muta_agent::catalog;
use muta_agent::orchestration::round_response;
use muta_contracts::{
    AgentNotice, AgentResponse, ClientIdentity, CommandRecord, CommandResult, Provider, RoundEvent,
    SecretString, WireProtocol,
};
use muta_persistence::config::{Config, Credentials, DiscoveryCache};
use muta_persistence::connection_usage::ConnectionUsage;
use muta_persistence::connections::{Connection, Connections};
use muta_persistence::route_settings::RouteSettingsStore;
use muta_persistence::session::{ProviderSelection, SessionStore};
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;

use crate::agent_setup::{reseed_prune_threshold, reseed_tool_variants};
use crate::session_view::provider_key_status;

/// `AgentRequest::SwitchProvider` — persist the chosen key/url/model/default,
/// rebuild the provider through the catalog so resolution stays shared with
/// startup, swap it into the shared holder, re-seed mid-turn relief, and push
/// the picker + key snapshots.
///
/// The switch writes the selection to the global `config.toml`
/// (`default_provider`/`default_model`) so the next launch — a fresh session
/// without a pin — lands on the switched provider, and additionally pins the
/// selection to this session's store so resuming *this* session restores its
/// own choice. Other live sessions keep their in-memory selection and live
/// provider; only fresh sessions follow the new global default.
/// Bundled handler environment: the plumbing arguments every provider
/// request handler threads through (config, agent, shared provider slot,
/// session store, response channel, connection usage). Handlers keep only
/// their request-specific parameters.
pub(crate) struct ProviderEnv<'a> {
    pub config: &'a mut Config,
    pub agent: &'a Agent,
    pub provider_for_task: &'a Arc<RwLock<Arc<dyn Provider>>>,
    pub session: &'a SessionStore,
    pub resp_tx: &'a mpsc::UnboundedSender<AgentResponse>,
    pub provider_usage: &'a mut ConnectionUsage,
}

pub(crate) struct AddProviderParams {
    pub name: String,
    pub protocol: WireProtocol,
    pub base_url: String,
    pub api_key: SecretString,
    pub user_agent: Option<String>,
    pub models: Vec<String>,
    pub auth: muta_contracts::ConnectionAuth,
    pub preset_id: Option<String>,
    pub client_identity: Option<ClientIdentity>,
}

pub(crate) struct PendingOAuthAuthorization {
    pub auth: muta_contracts::ConnectionAuth,
    pub tokens: muta_providers::oauth::TokenSet,
}

pub(crate) struct ActivateEnv<'a> {
    pub config: &'a Config,
    pub agent: &'a Agent,
    pub provider_for_task: &'a Arc<RwLock<Arc<dyn Provider>>>,
    pub session: Option<&'a SessionStore>,
    pub resp_tx: &'a mpsc::UnboundedSender<AgentResponse>,
    pub provider_usage: &'a mut ConnectionUsage,
}

impl<'a> From<ProviderEnv<'a>> for ActivateEnv<'a> {
    fn from(env: ProviderEnv<'a>) -> Self {
        Self {
            config: env.config,
            agent: env.agent,
            provider_for_task: env.provider_for_task,
            session: Some(env.session),
            resp_tx: env.resp_tx,
            provider_usage: env.provider_usage,
        }
    }
}

pub(crate) async fn switch(
    ProviderEnv {
        config,
        agent,
        provider_for_task,
        session,
        resp_tx,
        provider_usage,
    }: ProviderEnv<'_>,
    provider_type: String,
    model: String,
    api_key: Option<SecretString>,
    base_url: Option<String>,
) {
    let mut connections = Connections::load();
    // A key entered in the TUI is the connection's credential; an environment
    // variable (`api_key_env`) still wins at catalog resolution time.
    if let Some(key) = api_key
        && connections.get(&provider_type).is_some()
    {
        let mut creds = Credentials::load();
        creds.set_api_key(&provider_type, Some(key));
        if creds.save().is_err() {
            tracing::warn!("switch: could not persist credential");
        }
    }
    if let Some(url) = base_url
        && !url.trim().is_empty()
        && let Some(connection) = connections.get_mut(&provider_type)
    {
        connection.base_url = Some(url.trim().to_string());
        if connections.save().is_err() {
            tracing::warn!("switch: could not persist base-url override");
        }
    }

    // Set the selection on the effective config so `activate` resolves the
    // right channel; the save below persists it as the global default, and
    // the session pin (written further below) records this session's own
    // choice for exact restore on resume. The active model always lives in the
    // shared `default_model` — every instance is multi-model capable.
    config.default_connection = provider_type.clone();
    config.default_model = Some(model.clone());
    if let Err(error) = config.save() {
        tracing::warn!(?error, "could not persist provider selection");
    }
    // Pin the provider + model to this session so resume restores it exactly.
    // Best-effort: a failed pin does not block the live switch.
    if let Err(error) = session
        .set_provider_selection(Some(ProviderSelection {
            provider: provider_type.clone(),
            model: Some(model.clone()),
        }))
        .await
    {
        tracing::warn!(?error, "could not persist session provider selection");
    }
    // Pass the session through so `activate` can surface the acknowledgment
    // toast + record the ledger entry for this genuine user-initiated switch.
    activate(
        ActivateEnv {
            config,
            agent,
            provider_for_task,
            session: Some(session),
            resp_tx,
            provider_usage,
        },
        provider_type,
        model,
    )
    .await;
}

/// `AgentRequest::AddProvider` — create a connection (from a preset
/// or as a pure-custom declaration), persist it to the state store, set its
/// credential, then activate it. For OAuth presets the TUI runs
/// [`authorize`] first, then calls this with `auth` set.
pub(crate) async fn add(
    ProviderEnv {
        config,
        agent,
        provider_for_task,
        session,
        resp_tx,
        provider_usage,
    }: ProviderEnv<'_>,
    params: AddProviderParams,
    pending_authorization: Option<PendingOAuthAuthorization>,
) {
    let AddProviderParams {
        name,
        protocol,
        base_url,
        api_key,
        user_agent,
        models,
        auth,
        preset_id,
        client_identity,
    } = params;
    let mut connections = Connections::load();
    let id = connections.unique_id(&name);
    let trimmed_key = api_key.expose_secret().trim();
    // Pasted API key on an OAuth preset → ordinary ApiKey auth.
    let auth = match (auth, !trimmed_key.is_empty()) {
        (a, true) if a.is_oauth() => muta_contracts::ConnectionAuth::ApiKey,
        (other, _) => other,
    };
    let base_url = {
        let trimmed = base_url.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    };
    // Stamp the preset id so the catalog derives this connection's routes from
    // the preset. Only a known id is recorded; an unknown / blank value
    // keeps the connection pure-custom (its declared models are honored).
    let resolved_preset_id =
        preset_id.filter(|pid| muta_providers::provider_preset_spec(pid).is_some());
    // Sanitized declared model ids. Preset connections derive their model set
    // and leave `models` empty; a preset that must still seed its list
    // (custom-openai) declares it here.
    let declared_models: Vec<String> = models
        .iter()
        .map(|m| muta_contracts::sanitize_model_id(m))
        .filter(|m| !m.is_empty())
        .collect();
    let is_preset = resolved_preset_id.is_some();
    // A pure-custom provider must declare at least one model; a preset
    // connection with a preset that seeds none is a no-op.
    if !is_preset && declared_models.is_empty() {
        return;
    }
    let active_model = declared_models
        .first()
        .cloned()
        .or_else(|| {
            resolved_preset_id
                .as_deref()
                .and_then(muta_providers::provider_preset_spec)
                .and_then(|spec| spec.models.first())
                .map(|m| (*m).to_string())
        })
        .unwrap_or_default();

    let client_identity = client_identity.unwrap_or_else(|| {
        if auth == muta_contracts::ConnectionAuth::AntigravityOAuth {
            ClientIdentity::Antigravity
        } else {
            resolved_preset_id
                .as_deref()
                .and_then(muta_providers::provider_preset_spec)
                .and_then(|spec| spec.user_agent)
                .map(ClientIdentity::from_user_agent)
                .unwrap_or_default()
        }
    });

    // Pre-connection authorization is a single-use, session-local value. It
    // never enters the global store under a generic provider namespace.
    let pending_tokens = if auth.is_oauth() {
        match pending_authorization {
            Some(pending) if pending.auth == auth && pending.tokens.is_valid() => {
                Some(pending.tokens)
            }
            _ => {
                let _ = resp_tx.send(AgentResponse::ConnectStatus(
                    muta_contracts::ConnectStatus::Failed {
                        provider: id.clone(),
                        message: "OAuth authorization is missing or invalid; authorize this connection again"
                            .to_string(),
                    },
                ));
                return;
            }
        }
    } else {
        None
    };

    // Step 1: Write credentials to disk FIRST so a visible connection never exists without credentials.
    let mut stored_oauth = false;
    if let Some(tokens) = pending_tokens {
        let mut store = match muta_providers::oauth::AuthStore::lock().await {
            Ok(store) => store,
            Err(error) => {
                let _ = resp_tx.send(AgentResponse::ConnectStatus(
                    muta_contracts::ConnectStatus::Failed {
                        provider: id.clone(),
                        message: format!("could not lock OAuth credential store: {error}"),
                    },
                ));
                return;
            }
        };
        store.set(&id, tokens);
        if let Err(error) = store.save() {
            let _ = resp_tx.send(AgentResponse::ConnectStatus(
                muta_contracts::ConnectStatus::Failed {
                    provider: id.clone(),
                    message: format!("could not persist OAuth credentials: {error}"),
                },
            ));
            return;
        }
        stored_oauth = true;
    }

    let mut stored_api_key = false;
    if auth == muta_contracts::ConnectionAuth::ApiKey && !trimmed_key.is_empty() {
        let mut creds = Credentials::load();
        creds.set_api_key(&id, Some(SecretString::from(trimmed_key)));
        let save_err = creds.save().err().map(|e| e.to_string());
        if let Some(error_msg) = save_err {
            if stored_oauth && let Ok(mut store) = muta_providers::oauth::AuthStore::lock().await {
                store.remove(&id);
                let _ = store.save();
            }
            let _ = resp_tx.send(AgentResponse::ConnectStatus(
                muta_contracts::ConnectStatus::Failed {
                    provider: id.clone(),
                    message: format!("could not persist API key: {error_msg}"),
                },
            ));
            return;
        }
        stored_api_key = true;
    }

    // Step 2: Publish connection to Connections store.
    let connection = Connection {
        id: id.clone(),
        name: (!name.trim().is_empty()).then(|| name.trim().to_string()),
        preset_id: resolved_preset_id,
        auth,
        api_key_env: None,
        client_identity,
        protocol: if is_preset { None } else { Some(protocol) },
        base_url: if is_preset { None } else { base_url },
        user_agent: if is_preset { None } else { user_agent },
        models: if is_preset {
            Vec::new()
        } else {
            declared_models
        },
    };
    connections.connections.push(connection);
    let conn_save_err = connections.save().err().map(|e| e.to_string());
    if let Some(error_msg) = conn_save_err {
        tracing::error!(%error_msg, "add: could not persist connection; rolling back credentials");
        if stored_oauth && let Ok(mut store) = muta_providers::oauth::AuthStore::lock().await {
            store.remove(&id);
            let _ = store.save();
        }
        if stored_api_key {
            let mut creds = Credentials::load();
            creds.remove_api_key(&id);
            let _ = creds.save();
        }
        let _ = resp_tx.send(AgentResponse::ConnectStatus(
            muta_contracts::ConnectStatus::Failed {
                provider: id.clone(),
                message: format!("could not persist connection store: {error_msg}"),
            },
        ));
        return;
    }

    config.default_connection = id.clone();
    config.default_model = Some(active_model.clone());
    if let Err(error) = config.save() {
        tracing::warn!(?error, "add: could not persist selection");
    }
    // Pin the newly-added provider to this session — adding a provider is
    // also a live switch, so it is pinned like `/models`.
    if let Err(error) = session
        .set_provider_selection(Some(ProviderSelection {
            provider: id.clone(),
            model: Some(active_model.clone()),
        }))
        .await
    {
        tracing::warn!(?error, "could not persist session provider selection");
    }
    // For OAuth providers, run live model discovery right away so the picker
    // shows the account's real entitlements immediately rather than the seed
    // list. A failure keeps the seed; each failure is reported back as a
    // warning so the user knows the list may be incomplete.
    if auth.is_oauth() && auth != muta_contracts::ConnectionAuth::AntigravityOAuth {
        let outcome = catalog::discover_connection_models(&id, true).await;
        if outcome.changed {
            catalog::sync_fitted_model_registry();
            catalog::prune_stale_models_on_disk();
        }
        for (failed_provider, message) in &outcome.failures {
            let _ = resp_tx.send(AgentResponse::ConnectStatus(
                muta_contracts::ConnectStatus::DiscoveryWarning {
                    provider: failed_provider.clone(),
                    message: message.clone(),
                },
            ));
        }
    }
    activate(
        ActivateEnv {
            config,
            agent,
            provider_for_task,
            session: None,
            resp_tx,
            provider_usage,
        },
        id,
        active_model,
    )
    .await;
}

/// `AgentRequest::EditProvider` — update a connection's display name, endpoint
/// override, credential, and client identity in place.
pub(crate) async fn edit(
    ProviderEnv {
        config,
        agent,
        provider_for_task,
        resp_tx,
        provider_usage,
        ..
    }: ProviderEnv<'_>,
    id: String,
    name: String,
    protocol: WireProtocol,
    base_url: String,
    api_key: SecretString,
    client_identity: Option<ClientIdentity>,
) {
    let mut connections = Connections::load();
    let trimmed_url = base_url.trim();
    let trimmed_key = api_key.expose_secret().trim();
    let trimmed_name = name.trim();
    let Some(instance) = connections.get_mut(&id) else {
        return;
    };
    if !trimmed_name.is_empty() {
        instance.name = Some(trimmed_name.to_string());
    }
    if let Some(ci) = client_identity {
        instance.client_identity = ci;
    }
    // OAuth connections' endpoint and bearer are resolved by the auth flow;
    // Preset connections' endpoints are derived from the hardcoded preset spec.
    // Pure-custom connections (preset_id = None) adopt the edited base_url and transport.
    if !instance.auth.is_oauth() {
        if instance.preset_id.is_none() {
            if !trimmed_url.is_empty() {
                instance.base_url = Some(trimmed_url.to_string());
            }
            instance.protocol = Some(protocol);
        }
        // An empty key keeps whatever the instance already had.
        if !trimmed_key.is_empty() {
            let mut creds = Credentials::load();
            creds.set_api_key(&id, Some(SecretString::from(trimmed_key)));
            if creds.save().is_err() {
                tracing::warn!("edit: could not persist credential");
            }
        }
    }
    if connections.save().is_err() {
        tracing::warn!("edit: could not persist connection");
    }
    catalog::prune_stale_models(config, provider_usage);
    // Only rebuild the live provider when editing the active one (so a new
    // endpoint/key takes effect); editing an inactive provider just refreshes
    // the persisted state + the picker snapshot without switching.
    if config.default_connection == id {
        let model = catalog::resolved_model_name_with_usage(config, &id, provider_usage)
            .unwrap_or_default();
        activate(
            ActivateEnv {
                config,
                agent,
                provider_for_task,
                session: None,
                resp_tx,
                provider_usage,
            },
            id,
            model,
        )
        .await;
    } else {
        let _ = resp_tx.send(AgentResponse::ProviderKeys(provider_key_status(config)));
        let _ = resp_tx.send(AgentResponse::ProviderPicker(catalog::build_picker_state(
            config,
            provider_usage,
        )));
    }
}

/// `AgentRequest::RemoveProviderModel` — drop a declared model from a
/// pure-custom connection, persist, and push a fresh picker snapshot. The last
/// remaining model is kept (a connection must serve at least one model). If the
/// removed model was the active `default_model`, it is cleared so the connection
/// falls back to its first route.
pub async fn remove_model(
    config: &mut Config,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ConnectionUsage,
    provider_id: String,
    model: String,
) {
    let mut connections = Connections::load();
    if let Some(connection) = connections.get_mut(&provider_id)
        && connection.preset_id.is_none()
        && connection.models.len() > 1
        && let Some(pos) = connection.models.iter().position(|m| *m == model)
    {
        connection.models.remove(pos);
        if connections.save().is_err() {
            tracing::warn!("remove_model: could not persist connection");
        }
    }
    catalog::prune_stale_models(config, provider_usage);
    let _ = resp_tx.send(AgentResponse::ProviderPicker(catalog::build_picker_state(
        config,
        provider_usage,
    )));
}

/// `AgentRequest::EditProviderModel` — update the per-(connection, model)
/// reasoning overrides in the discovery cache. Connection metadata (name /
/// endpoint / credential) is untouched.
pub(crate) async fn edit_model(
    ProviderEnv {
        config,
        agent,
        provider_for_task,
        resp_tx,
        provider_usage,
        ..
    }: ProviderEnv<'_>,
    provider_id: String,
    model: String,
    effort: Option<String>,
    thinking: Option<bool>,
    overrides: Option<muta_contracts::CapabilityOverrides>,
) {
    let valid_effort = effort.and_then(|e| {
        let t = e.trim();
        (!t.is_empty())
            .then(|| t.to_ascii_lowercase())
            .filter(|s| muta_contracts::effort::Effort::parse(s).is_some())
    });

    // Resolve the route's transport to decide which knobs apply (Anthropic
    // honors thinking; OpenAI/Responses carry effort only; Google ignores both).
    let stores = catalog::Stores::load();
    let Some(connection) = stores.connections.get(&provider_id) else {
        return;
    };
    let transport = catalog::derive_channel(
        connection,
        &model,
        &stores.cache,
        &stores.routes,
        &stores.creds,
    )
    .transport;

    let mut routes = RouteSettingsStore::load();
    let entry = routes.settings_for_mut(&provider_id, &model);
    match transport {
        muta_contracts::catalog::Transport::Anthropic { .. } => {
            entry.effort = valid_effort;
            entry.thinking = thinking;
        }
        muta_contracts::catalog::Transport::OpenAi { .. }
        | muta_contracts::catalog::Transport::OpenAiResponses { .. } => {
            entry.effort = valid_effort;
            entry.thinking = None;
        }
        muta_contracts::catalog::Transport::Google { .. } => {}
    }
    // Capability overrides (ADR-0149 layer 1): `None` keeps the stored
    // record untouched; `Some(record)` replaces it wholesale (empty clears).
    if let Some(record) = overrides {
        entry.capability_overrides = (!record.is_empty()).then_some(record);
    }
    if entry.is_empty() {
        routes.remove(&provider_id, &model);
    }
    if routes.save().is_err() {
        tracing::warn!("edit_model: could not persist route settings");
    }

    let active_model =
        catalog::resolved_model_name_with_usage(config, &provider_id, provider_usage)
            .unwrap_or_default();
    if config.default_connection == provider_id && active_model == model {
        activate(
            ActivateEnv {
                config,
                agent,
                provider_for_task,
                session: None,
                resp_tx,
                provider_usage,
            },
            provider_id,
            model,
        )
        .await;
    } else {
        let _ = resp_tx.send(AgentResponse::ProviderPicker(catalog::build_picker_state(
            config,
            provider_usage,
        )));
    }
}

/// `AgentRequest::EditModelReasoning` — update the per-(connection, model)
/// reasoning overrides for the currently active connection. Serves the model
/// `e` editor for any model; the setting is scoped to the connection that
/// actually serves it (a model id can be served by more than one connection).
/// If the edited model is the active one, the live provider is re-activated
/// so the new settings take effect at once.
pub(crate) async fn edit_model_reasoning(
    ProviderEnv {
        config,
        agent,
        provider_for_task,
        resp_tx,
        provider_usage,
        ..
    }: ProviderEnv<'_>,
    model: String,
    effort: Option<String>,
    thinking: Option<bool>,
    overrides: Option<muta_contracts::CapabilityOverrides>,
) {
    let valid_effort = effort.and_then(|e| {
        let t = e.trim();
        (!t.is_empty())
            .then(|| t.to_ascii_lowercase())
            .filter(|s| muta_contracts::effort::Effort::parse(s).is_some())
    });

    let provider_id = config.default_connection.clone();
    let mut routes = RouteSettingsStore::load();
    let entry = routes.settings_for_mut(&provider_id, &model);
    entry.effort = valid_effort;
    entry.thinking = thinking;
    if let Some(record) = overrides {
        entry.capability_overrides = (!record.is_empty()).then_some(record);
    }
    if entry.is_empty() {
        routes.remove(&provider_id, &model);
    }
    if routes.save().is_err() {
        tracing::warn!("edit_model_reasoning: could not persist route settings");
    }

    // Re-activate if this model is the live one so the change applies now.
    let active_model =
        catalog::resolved_model_name_with_usage(config, &provider_id, provider_usage)
            .unwrap_or_default();
    if active_model == model {
        activate(
            ActivateEnv {
                config,
                agent,
                provider_for_task,
                session: None,
                resp_tx,
                provider_usage,
            },
            provider_id,
            model,
        )
        .await;
    } else {
        let _ = resp_tx.send(AgentResponse::ProviderPicker(catalog::build_picker_state(
            config,
            provider_usage,
        )));
    }
}

/// `AgentRequest::DeleteProvider` — remove a connection entirely: drop
/// it from the connection store, its credential, its discovery-cache records,
/// and its OAuth tokens, and prune its model ids from favorites. When the
/// deleted connection was the active one, fall back to the effective default and
/// re-activate so the live provider never points at a removed entry.
pub(crate) async fn delete(
    ProviderEnv {
        config,
        agent,
        provider_for_task,
        resp_tx,
        provider_usage,
        ..
    }: ProviderEnv<'_>,
    id: String,
) {
    let mut connections = Connections::load();
    let Some(deleted) = connections.remove(&id) else {
        return;
    };
    let _ = deleted;
    if connections.save().is_err() {
        tracing::warn!("delete: could not persist connection store");
    }
    // Clean up the credential and any OAuth tokens stored for this connection.
    let mut creds = Credentials::load();
    creds.remove_api_key(&id);
    if creds.save().is_err() {
        tracing::warn!("delete: could not persist credentials");
    }
    match muta_providers::oauth::AuthStore::lock().await {
        Ok(mut auth_store) => {
            if auth_store.remove(&id).is_some()
                && let Err(error) = auth_store.save()
            {
                tracing::error!(?error, connection_id = %id, "could not remove OAuth credential");
            }
        }
        Err(error) => {
            tracing::error!(?error, connection_id = %id, "could not open OAuth credential store");
        }
    }
    if let Err(error) = DiscoveryCache::modify(|cache| cache.remove_connection(&id)).await {
        tracing::warn!(?error, connection_id = %id, "could not persist discovery cache on delete");
    }
    // The deleted connection's route settings go with it (state, not cache).
    let mut routes = RouteSettingsStore::load();
    routes.retain_connection_except(&id);
    if routes.save().is_err() {
        tracing::warn!("delete: could not persist route settings");
    }
    catalog::prune_stale_models(config, provider_usage);

    let was_active = config.default_connection == id;
    if was_active {
        config.default_connection =
            catalog::effective_default_connection_id(config, &catalog::Stores::load());
        config.default_model = None;
    }
    if let Err(error) = config.save_preserving_connection_selection() {
        tracing::warn!(?error, "could not persist deleted connection");
    }

    if was_active {
        let fallback = config.default_connection.clone();
        let model = catalog::resolved_model_name_with_usage(config, &fallback, provider_usage)
            .unwrap_or_default();
        activate(
            ActivateEnv {
                config,
                agent,
                provider_for_task,
                session: None,
                resp_tx,
                provider_usage,
            },
            fallback,
            model,
        )
        .await;
    } else {
        // Deleting an inactive connection: refresh the picker + key snapshots
        // without switching the live provider.
        let _ = resp_tx.send(AgentResponse::ProviderKeys(provider_key_status(config)));
        let _ = resp_tx.send(AgentResponse::ProviderPicker(catalog::build_picker_state(
            config,
            provider_usage,
        )));
    }
}

/// Re-apply the active session's provider/model pin to the live provider
/// holder (C6). Called after a session swap (`/session open`, `/session
/// resume`, `/session new`) so the live provider tracks the now-active
/// session's own pin, or falls back to the global default when the new session
/// has no pin (`None`). Builds a transient `Config` clone with the overlay so
/// the caller's immutable `&Config` is not mutated; activation writes only the
/// live holder, telemetry, and TUI snapshots — never `config.toml`.
pub async fn reapply_session_selection(
    config: &Config,
    agent: &Agent,
    provider_for_task: &Arc<RwLock<Arc<dyn Provider>>>,
    session: &SessionStore,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ConnectionUsage,
) {
    // Overlay the session pin onto a throwaway clone so catalog resolution
    // picks the session's provider/model, not the global default.
    let mut effective = config.clone();
    let selection = session.provider_selection().await;
    let (provider_id, model_id): (String, Option<String>) = match &selection {
        Some(sel) => {
            effective.default_connection = sel.provider.clone();
            if let Some(model) = &sel.model {
                effective.default_model = Some(model.clone());
            }
            (sel.provider.clone(), sel.model.clone())
        }
        None => (
            catalog::default_connection_id(config).to_string(),
            config.default_model.clone(),
        ),
    };
    let model = model_id.filter(|m| !m.is_empty()).unwrap_or_else(|| {
        catalog::resolved_model_name_with_usage(&effective, &provider_id, provider_usage)
            .unwrap_or_default()
    });
    activate(
        ActivateEnv {
            config: &effective,
            agent,
            provider_for_task,
            session: None,
            resp_tx,
            provider_usage,
        },
        provider_id,
        model,
    )
    .await;
}

/// `AgentRequest::AuthorizeOAuth` — run an OAuth login before a connection
/// exists ("+ Add connection → xAI OAuth / ChatGPT OAuth"). `auth`
/// selects which provider's flow to run. The returned token set remains
/// session-local until `AddProvider` consumes it into the final connection id.
pub async fn authorize(
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    method: muta_contracts::LoginMethod,
    auth: muta_contracts::ConnectionAuth,
) -> Option<muta_providers::oauth::TokenSet> {
    let Some(cfg) = auth
        .oauth_provider_id()
        .and_then(muta_providers::oauth::config_by_provider_id)
    else {
        let _ = resp_tx.send(AgentResponse::ConnectStatus(
            muta_contracts::ConnectStatus::Failed {
                provider: "oauth".to_string(),
                message: "not an OAuth provider".to_string(),
            },
        ));
        return None;
    };
    let label = cfg.provider_id.to_string();
    run_oauth(resp_tx, &label, method, cfg).await
}

/// `AgentRequest::ConnectProvider` — re-auth an existing OAuth connection, then
/// activate it.
///
/// After a successful login, runs live model discovery so the connection's
/// model list reflects the account's real entitlements immediately (rather
/// than waiting for the next launch). Discovery failures are non-fatal: the
/// connection keeps its previous model subset.
pub async fn connect(
    config: &mut Config,
    agent: &Agent,
    provider_for_task: &Arc<RwLock<Arc<dyn Provider>>>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ConnectionUsage,
    provider_id: String,
    method: muta_contracts::LoginMethod,
) {
    if run_oauth_for_connect(resp_tx, provider_id.clone(), method).await {
        connect_post_oauth(
            config,
            agent,
            provider_for_task,
            resp_tx,
            provider_usage,
            provider_id,
        )
        .await;
    }
}

/// Run the OAuth portion of connect in a non-blocking way.
pub async fn run_oauth_for_connect(
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_id: String,
    method: muta_contracts::LoginMethod,
) -> bool {
    let connections = Connections::load();
    let auth_mode = connections
        .get(&provider_id)
        .map(|p| p.auth)
        .unwrap_or_default();
    let Some(cfg) = auth_mode
        .oauth_provider_id()
        .and_then(muta_providers::oauth::config_by_provider_id)
    else {
        let _ = resp_tx.send(AgentResponse::ConnectStatus(
            muta_contracts::ConnectStatus::Failed {
                provider: provider_id,
                message: "not an OAuth provider".to_string(),
            },
        ));
        return false;
    };
    let Some(mut tokens) = run_oauth(resp_tx, &provider_id, method, cfg).await else {
        return false;
    };
    let mut store = match muta_providers::oauth::AuthStore::lock().await {
        Ok(store) => store,
        Err(error) => {
            let _ = resp_tx.send(AgentResponse::ConnectStatus(
                muta_contracts::ConnectStatus::Failed {
                    provider: provider_id,
                    message: format!("could not lock OAuth credential store: {error}"),
                },
            ));
            return false;
        }
    };
    if tokens.refresh.is_empty()
        && let Some(previous) = store.get(&provider_id)
        && !previous.refresh.is_empty()
    {
        tokens.refresh = previous.refresh.clone();
    }
    store.set(&provider_id, tokens);
    if let Err(error) = store.save() {
        let _ = resp_tx.send(AgentResponse::ConnectStatus(
            muta_contracts::ConnectStatus::Failed {
                provider: provider_id,
                message: format!("could not persist OAuth credentials: {error}"),
            },
        ));
        return false;
    }
    let _ = resp_tx.send(AgentResponse::ConnectStatus(
        muta_contracts::ConnectStatus::Done {
            provider: provider_id.clone(),
        },
    ));
    true
}

/// Run the post-OAuth discovery and activation logic for connect.
pub async fn connect_post_oauth(
    config: &mut Config,
    agent: &Agent,
    provider_for_task: &Arc<RwLock<Arc<dyn Provider>>>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ConnectionUsage,
    provider_id: String,
) {
    // Live model discovery: fetch the provider's actual model list with the
    // fresh token so the picker shows the account's real entitlements right
    // away. A failure keeps the previous subset; each failure is reported back
    // as a warning so the user knows *why* the list did not refresh.
    let outcome = catalog::discover_connection_models(&provider_id, true).await;
    if outcome.changed {
        catalog::sync_fitted_model_registry();
    }
    catalog::prune_stale_models(config, provider_usage);
    for (failed_provider, message) in &outcome.failures {
        let _ = resp_tx.send(AgentResponse::ConnectStatus(
            muta_contracts::ConnectStatus::DiscoveryWarning {
                provider: failed_provider.clone(),
                message: message.clone(),
            },
        ));
    }
    let model = catalog::build_picker_state(config, provider_usage)
        .rows
        .into_iter()
        .find(|r| r.id == provider_id)
        .map(|r| r.model)
        .unwrap_or_default();
    activate(
        ActivateEnv {
            config,
            agent,
            provider_for_task,
            session: None,
            resp_tx,
            provider_usage,
        },
        provider_id,
        model,
    )
    .await;
}

/// Shared OAuth exchange for any provider: browser loopback (PKCE) or device
/// code. Persistence belongs to the caller because only it knows the final,
/// exact connection namespace.
async fn run_oauth(
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    label: &str,
    method: muta_contracts::LoginMethod,
    cfg: muta_providers::oauth::OAuthConfig,
) -> Option<muta_providers::oauth::TokenSet> {
    use muta_providers::oauth::OAuth;

    let oauth = OAuth::new(cfg.clone());

    let login = match oauth.begin_login(method).await {
        Ok(login) => login,
        Err(error) => {
            let _ = resp_tx.send(AgentResponse::ConnectStatus(
                muta_contracts::ConnectStatus::Failed {
                    provider: label.to_string(),
                    message: error.to_string(),
                },
            ));
            return None;
        }
    };
    let prompt = login.prompt();
    let _ = resp_tx.send(AgentResponse::ConnectStatus(
        muta_contracts::ConnectStatus::Pending {
            provider: label.to_string(),
            url: prompt.url.clone(),
            user_code: prompt.user_code.clone().unwrap_or_default(),
            message: prompt.message.clone(),
        },
    ));

    let tokens = match login.complete().await {
        Ok(t) => t,
        Err(error) => {
            let _ = resp_tx.send(AgentResponse::ConnectStatus(
                muta_contracts::ConnectStatus::Failed {
                    provider: label.to_string(),
                    message: error.to_string(),
                },
            ));
            return None;
        }
    };
    let now_ms = chrono::Utc::now().timestamp_millis();

    // Capture the ChatGPT account id from the id_token/access_token so the
    // Responses transport can send the `ChatGPT-Account-Id` header. xAI tokens
    // carry no such claim, so this is `None` for them.
    let mut account_id = if cfg.is_chatgpt() {
        tokens
            .id_token
            .as_ref()
            .map(SecretString::expose_secret)
            .or(Some(tokens.access_token.expose_secret()))
            .and_then(muta_providers::oauth::chatgpt_account_id)
    } else {
        None
    };

    let mut project_id = None;
    let mut user_email = None;

    if cfg.is_antigravity() {
        if let Ok(project) = muta_providers::oauth::resolve_antigravity_project(
            oauth.client(),
            tokens.access_token.expose_secret(),
        )
        .await
            && !project.is_empty()
        {
            project_id = Some(project.clone());
            if account_id.is_none() {
                account_id = Some(project);
            }
        }
        if let Ok(info) = muta_providers::oauth::fetch_google_userinfo(
            oauth.client(),
            tokens.access_token.expose_secret(),
        )
        .await
        {
            user_email = info.email;
        }
    }

    let expires_ms = muta_providers::oauth::access_token_expiry_ms(
        tokens.access_token.expose_secret(),
        tokens.expires_in,
        now_ms,
    );
    Some(muta_providers::oauth::TokenSet {
        access: tokens.access_token,
        refresh: tokens.refresh_token.unwrap_or_default(),
        expires_ms,
        account_id,
        id_token: tokens.id_token,
        token_type: tokens.token_type,
        scope: tokens.scope,
        project_id,
        user_email,
    })
}

pub(crate) async fn refresh_oauth_if_needed(_config: &Config, provider_id: &str) {
    let connections = Connections::load();
    let Some(instance) = connections.get(provider_id) else {
        return;
    };
    if !instance.auth.is_oauth() {
        return;
    }
    let source = muta_providers::oauth::OAuthCredentialSource::new(provider_id, instance.auth);
    if let Err(error) = muta_contracts::CredentialSource::resolve_auth(&source).await {
        tracing::warn!(error = %error, provider = %provider_id, "OAuth token resolution failed");
    }
}

/// Record a provider switch's acknowledgment in the durable command ledger.
async fn record_provider_ack(session: &SessionStore, provider: &str, model: &str, ack: String) {
    let record = CommandRecord::new("models", format!("{provider} {model}")).with_result(
        CommandResult::Ack {
            title: ack,
            detail: None,
        },
    );
    if let Err(error) = session.mutate_commands(|c| c.push(record)).await {
        tracing::warn!(?error, "could not persist provider-switch ack");
    }
}

/// Shared tail of [`switch`] and [`add`]: rebuild the active provider through the
/// catalog, swap it into the shared holder, re-seed mid-turn relief, and push
/// the key + picker snapshots.
async fn activate(
    ActivateEnv {
        config,
        agent,
        provider_for_task,
        session,
        resp_tx,
        provider_usage,
    }: ActivateEnv<'_>,
    provider_type: String,
    model: String,
) {
    refresh_oauth_if_needed(config, &provider_type).await;

    let session_id = agent.thread_id();
    let Some(new_p) = catalog::build_provider_for_model(
        config,
        &provider_type,
        Some(&model),
        session_id.as_deref(),
    )
    .or_else(|| catalog::build_provider_for(config, &provider_type)) else {
        tracing::warn!(
            provider_type = %provider_type,
            model = %model,
            "activate refused: catalog could not resolve a real provider/channel",
        );
        let _ = resp_tx.send(AgentResponse::Error(format!(
            "No connection configured for '{provider_type}'. \
             Add one with /connections before sending a message."
        )));
        // Re-push the picker so the UI reflects that nothing switched.
        let _ = resp_tx.send(AgentResponse::ProviderPicker(catalog::build_picker_state(
            config,
            provider_usage,
        )));
        return;
    };
    *provider_for_task
        .write()
        .unwrap_or_else(|error| error.into_inner()) = new_p;

    reseed_prune_threshold(agent, config);
    reseed_tool_variants(agent, config);

    let _ = resp_tx.send(AgentResponse::ProviderKeys(provider_key_status(config)));
    provider_usage.record(&provider_type);
    provider_usage.record_model(&provider_type, &model);
    if let Err(error) = provider_usage.save() {
        tracing::warn!(?error, "could not persist model usage telemetry");
    }
    let ack = format!("Connection switched to {provider_type} ({model})");
    let _ = resp_tx.send(AgentResponse::ProviderSwitched {
        provider: provider_type.clone(),
        model: model.clone(),
    });
    if let Some(session) = session {
        let session_id = session.id().await;
        let _ = resp_tx.send(round_response(
            &session_id,
            RoundEvent::Notice(AgentNotice::command_ack(ack.clone())),
        ));
        record_provider_ack(session, &provider_type, &model, ack).await;
    }
    let _ = resp_tx.send(AgentResponse::ProviderPicker(catalog::build_picker_state(
        config,
        provider_usage,
    )));
}

/// `AgentRequest::ToggleFavorite` — flip the model id in the favorites list.
pub async fn toggle_favorite(
    config: &mut Config,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &ConnectionUsage,
    id: String,
) {
    if let Some(pos) = config.favorites.iter().position(|fav| *fav == id) {
        config.favorites.remove(pos);
    } else {
        config.favorites.push(id.clone());
    }
    if let Err(error) = config.save_preserving_connection_selection() {
        tracing::warn!(?error, "could not persist favorites");
    }
    let _ = resp_tx.send(AgentResponse::ProviderPicker(catalog::build_picker_state(
        config,
        provider_usage,
    )));
}

/// `AgentRequest::SetDefaultModel` — make `id` the default AND activate it.
pub async fn set_default_model(
    config: &mut Config,
    agent: &Agent,
    provider_for_task: &Arc<RwLock<Arc<dyn Provider>>>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ConnectionUsage,
    id: String,
) {
    let stores = catalog::Stores::load();
    let entries = catalog::derive_entries(
        &stores.connections,
        &stores.cache,
        &stores.routes,
        &stores.creds,
    );
    let current_provider_id = config.default_connection.clone();
    let current_offers = entries
        .iter()
        .find(|e| e.id == current_provider_id)
        .is_some_and(|e| e.offers_model(&id));
    let provider_id = if current_offers {
        current_provider_id
    } else {
        let Some(first_match) = entries.iter().find(|e| e.offers_model(&id)) else {
            tracing::warn!(model = %id, "set_default_model: model is not served by any connection");
            return;
        };
        first_match.id.clone()
    };

    config.default_connection = provider_id.clone();
    config.default_model = Some(id.clone());
    if let Err(error) = config.save() {
        tracing::warn!(?error, "could not persist default model");
    }

    activate(
        ActivateEnv {
            config,
            agent,
            provider_for_task,
            session: None,
            resp_tx,
            provider_usage,
        },
        provider_id,
        id,
    )
    .await;
}

/// `AgentRequest::RefreshProviderModels` — run live model discovery for all
/// discovery-enabled connections from upstream.
pub async fn refresh_models(
    config: &mut Config,
    _agent: &Agent,
    _provider_for_task: &Arc<RwLock<Arc<dyn Provider>>>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ConnectionUsage,
    session: Option<&SessionStore>,
    user_initiated: bool,
) {
    let outcome = catalog::discover_provider_models(user_initiated).await;
    if outcome.changed {
        catalog::sync_fitted_model_registry();
    }
    catalog::prune_stale_models(config, provider_usage);

    for (failed_provider, message) in &outcome.failures {
        let _ = resp_tx.send(AgentResponse::ConnectStatus(
            muta_contracts::ConnectStatus::DiscoveryWarning {
                provider: failed_provider.clone(),
                message: message.clone(),
            },
        ));
    }

    if let Some(session) = session
        && user_initiated
    {
        let session_id = session.id().await;
        let ack = if !outcome.failures.is_empty() && !outcome.changed {
            "Model refresh failed to reach upstream".to_string()
        } else if outcome.changed {
            "Model list updated".to_string()
        } else {
            "Model list refreshed (up to date)".to_string()
        };
        let _ = resp_tx.send(round_response(
            &session_id,
            RoundEvent::Notice(AgentNotice::command_ack(ack)),
        ));
    }

    let _ = resp_tx.send(AgentResponse::ProviderKeys(provider_key_status(config)));
    let _ = resp_tx.send(AgentResponse::ProviderPicker(catalog::build_picker_state(
        config,
        provider_usage,
    )));
}

/// Mask an API key for safe display (e.g. `sk-12...abcd`).
fn mask_api_key(key: &str) -> Option<String> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() <= 8 {
        Some("********".to_string())
    } else {
        Some(format!(
            "{}...{}",
            &trimmed[..4],
            &trimmed[trimmed.len() - 4..]
        ))
    }
}

/// `AgentRequest::QueryConnectionDetail` — return connection details immediately and query live provider usage in background.
pub(crate) async fn query_connection_detail(
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    id: String,
) {
    let stores = catalog::Stores::load();
    let Some(connection) = stores.connections.get(&id) else {
        return;
    };

    let entry = catalog::derive_entry(connection, &stores.cache, &stores.routes, &stores.creds);
    let (protocol, base_url) = entry
        .default_channel()
        .map(|c| match &c.transport {
            muta_agent::Transport::OpenAi { base_url, .. } => {
                ("openai".to_string(), base_url.clone())
            }
            muta_agent::Transport::OpenAiResponses { base_url, .. } => {
                ("openai_responses".to_string(), base_url.clone())
            }
            muta_agent::Transport::Anthropic { base_url, .. } => {
                ("anthropic".to_string(), base_url.clone())
            }
            muta_agent::Transport::Google { base_url, .. } => {
                ("google".to_string(), base_url.clone())
            }
        })
        .unwrap_or_else(|| {
            let p = connection
                .protocol
                .unwrap_or(WireProtocol::OpenAiChatCompletions);
            (
                p.to_string(),
                connection.base_url.clone().unwrap_or_default(),
            )
        });

    let preset_label = connection
        .preset_id
        .as_deref()
        .and_then(muta_providers::provider_preset_spec)
        .map(|spec| spec.id.to_string());

    let raw_key = catalog::resolve_credential(connection, &stores.creds);
    let api_key_masked = mask_api_key(raw_key.expose_secret());

    let api_key_source = if connection.auth.is_oauth() {
        "OAuth".to_string()
    } else if let Some(env) = connection.api_key_env.as_deref() {
        if std::env::var(env).is_ok() {
            format!("Environment (${env})")
        } else {
            format!("Missing (${env} not set)")
        }
    } else if stores.creds.api_key(&connection.id).is_some() {
        "credentials.toml".to_string()
    } else {
        "Not configured".to_string()
    };

    let user_agent = entry
        .default_channel()
        .map(|c| c.transport.user_agent().to_string())
        .filter(|ua| !ua.is_empty())
        .unwrap_or_else(|| connection.client_identity.user_agent().to_string());

    let models = entry
        .channels
        .iter()
        .map(|c| c.model.clone())
        .collect::<Vec<_>>();
    let model_info = entry
        .channels
        .iter()
        .map(muta_agent::catalog::channel_model_info)
        .collect::<Vec<_>>();
    let default_channel = entry.default_channel();
    let active_model = default_channel.map(|c| c.model.clone());
    let active_channel_info = default_channel.map(muta_agent::catalog::channel_model_info);
    let active_model_effort = active_channel_info.as_ref().and_then(|info| {
        let show = match info.protocol.as_str() {
            "anthropic" => info.thinking == Some(true),
            _ => info.effort.is_some(),
        };
        if show { info.effort.clone() } else { None }
    });
    let active_model_thinking = active_channel_info.as_ref().and_then(|info| info.thinking);

    let auth_type = if connection.auth.is_oauth() {
        format!("OAuth ({:?})", connection.auth)
    } else if connection.api_key_env.is_some() {
        "API Key (Environment)".to_string()
    } else {
        "API Key".to_string()
    };

    let mut initial_detail = muta_contracts::ConnectionDetail {
        id: connection.id.clone(),
        name: connection.display_name().to_string(),
        preset_id: connection.preset_id.clone(),
        preset_label,
        protocol,
        base_url: base_url.clone(),
        auth_type,
        api_key_masked,
        api_key_source,
        client_identity: if connection.client_identity != ClientIdentity::Native {
            connection.client_identity.clone()
        } else if connection.auth == muta_contracts::ConnectionAuth::AntigravityOAuth
            || connection.preset_id.as_deref() == Some("antigravity-oauth")
        {
            ClientIdentity::Antigravity
        } else {
            connection.client_identity.clone()
        },
        user_agent,
        models,
        model_info,
        active_model,
        active_model_effort,
        active_model_thinking,
        usage: muta_contracts::ConnectionUsageState::Fetching,
    };

    // Phase 1: Send local detail snapshot immediately so UI renders instantly.
    let _ = resp_tx.send(AgentResponse::ConnectionDetail(initial_detail.clone()));

    // Phase 2: Async remote query in background task.
    let resp_tx_bg = resp_tx.clone();
    let conn_id = connection.id.clone();
    let conn_auth = connection.auth;
    let preset_id = connection.preset_id.clone();
    let raw_key_str = raw_key.expose_secret().to_string();
    tokio::spawn(async move {
        let (api_key, is_oauth) = if conn_auth.is_oauth() {
            let source = muta_providers::oauth::OAuthCredentialSource::new(&conn_id, conn_auth);
            match muta_contracts::CredentialSource::resolve_auth(&source).await {
                Ok(auth) => (auth.token.expose_secret().to_string(), true),
                Err(err) => {
                    initial_detail.usage = muta_contracts::ConnectionUsageState::Error(err);
                    let _ = resp_tx_bg.send(AgentResponse::ConnectionDetail(initial_detail));
                    return;
                }
            }
        } else {
            (raw_key_str, false)
        };

        let mut usage =
            muta_providers::fetch_provider_usage(preset_id.as_deref(), &base_url, &api_key).await;

        if is_oauth
            && let muta_contracts::ConnectionUsageState::Error(ref err) = usage
            && is_auth_error(err)
        {
            let source = muta_providers::oauth::OAuthCredentialSource::new(&conn_id, conn_auth);
            let rejected = SecretString::from(api_key.as_str());
            if let Ok(refreshed) =
                muta_contracts::CredentialSource::force_refresh_after_rejection(&source, &rejected)
                    .await
            {
                usage = muta_providers::fetch_provider_usage(
                    preset_id.as_deref(),
                    &base_url,
                    refreshed.token.expose_secret(),
                )
                .await;
            }
        }

        initial_detail.usage = usage;
        let _ = resp_tx_bg.send(AgentResponse::ConnectionDetail(initial_detail));
    });
}

fn is_auth_error(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("401")
        || lower.contains("unauthorized")
        || lower.contains("unauthenticated")
        || lower.contains("invalid_token")
        || lower.contains("token expired")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn record_provider_ack_appends_durable_ack_to_command_ledger() {
        let tmp = tempfile::tempdir().unwrap();
        let session = SessionStore::for_path(tmp.path().join("session.json"));
        record_provider_ack(
            &session,
            "111xianyu",
            "k3",
            "Connection switched to 111xianyu (k3)".to_string(),
        )
        .await;

        let commands = session.commands().await;
        assert_eq!(commands.len(), 1);
        let record = &commands[0];
        assert_eq!(record.name, "models");
        assert_eq!(record.args, "111xianyu k3");
        assert_eq!(record.status, muta_contracts::CommandStatus::Success);
        match &record.result {
            Some(muta_contracts::CommandResult::Ack { title, .. }) => {
                assert_eq!(title, "Connection switched to 111xianyu (k3)");
            }
            other => panic!("expected a durable Ack result, got {other:?}"),
        }
    }

    #[test]
    fn wire_protocol_parser_is_exact_and_rejects_unknown_labels() {
        assert_eq!(
            "anthropic-messages".parse::<WireProtocol>(),
            Ok(WireProtocol::AnthropicMessages)
        );
        assert_eq!(
            "openai-responses".parse::<WireProtocol>(),
            Ok(WireProtocol::OpenAiResponses)
        );
        assert!("openai".parse::<WireProtocol>().is_err());
        assert!("future".parse::<WireProtocol>().is_err());
    }

    #[test]
    fn connection_unique_id_slugifies_and_disambiguates() {
        let mut connections = Connections::default();
        connections.connections.push(Connection {
            id: "my-relay".to_string(),
            ..Default::default()
        });
        assert_eq!(connections.unique_id("My Relay"), "my-relay-2");
        assert_eq!(connections.unique_id("  Acme  AI  "), "acme-ai");
        assert_eq!(connections.unique_id("***"), "custom");
        assert_eq!(connections.unique_id(""), "custom");
    }

    #[tokio::test]
    async fn add_provider_oauth_auth_mismatch_never_writes_connection_or_token() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = muta_persistence::paths::Dirs {
            config_dir: dir.path().join("config"),
            data_dir: dir.path().join("data"),
            state_dir: dir.path().join("state"),
            cache_dir: dir.path().join("cache"),
            runtime_dir: None,
        };
        muta_persistence::paths::set_test_default(Some(dirs));

        let (resp_tx, mut resp_rx) = tokio::sync::mpsc::unbounded_channel();
        let session = SessionStore::for_path(dir.path().join("session.json"));
        let mut config = Config::default();
        let mut usage = ConnectionUsage::default();
        let agent = muta_agent::Agent::builder(
            Arc::new(muta_agent::NoProvider),
            Vec::new(),
            muta_agent::AgentIdentity::default(),
        )
        .build();
        let provider_for_task = Arc::new(std::sync::RwLock::new(agent.provider.clone()));

        let env = ProviderEnv {
            config: &mut config,
            agent: &agent,
            provider_for_task: &provider_for_task,
            session: &session,
            resp_tx: &resp_tx,
            provider_usage: &mut usage,
        };

        // Pending auth is for AntigravityOAuth, but params requests ChatGptOAuth
        let pending = PendingOAuthAuthorization {
            auth: muta_contracts::ConnectionAuth::AntigravityOAuth,
            tokens: muta_providers::oauth::TokenSet {
                access: "tok".into(),
                refresh: "ref".into(),
                expires_ms: 1000,
                account_id: None,
                id_token: None,
                token_type: None,
                scope: None,
                project_id: None,
                user_email: None,
            },
        };

        let params = AddProviderParams {
            name: "Mismatched Provider".to_string(),
            protocol: WireProtocol::OpenAiResponses,
            base_url: "https://api.openai.com".to_string(),
            api_key: "".into(),
            user_agent: None,
            models: vec!["gpt-5.6".to_string()],
            auth: muta_contracts::ConnectionAuth::ChatGptOAuth,
            preset_id: Some("chatgpt".to_string()),
            client_identity: None,
        };

        add(env, params, Some(pending)).await;

        // Verify response was ConnectStatus::Failed
        let resp = resp_rx.recv().await.unwrap();
        assert!(matches!(
            resp,
            AgentResponse::ConnectStatus(muta_contracts::ConnectStatus::Failed { .. })
        ));

        // Verify no connection was written
        let conns = Connections::load();
        assert!(conns.connections.is_empty());

        // Verify no token was written
        let store = muta_providers::oauth::AuthStore::load().unwrap();
        assert!(store.tokens.is_empty());

        muta_persistence::paths::set_test_default(None);
    }

    #[tokio::test]
    async fn query_connection_detail_sends_initial_and_background_detail() {
        let dir = tempfile::tempdir().unwrap();
        let dirs = muta_persistence::paths::Dirs {
            config_dir: dir.path().join("config"),
            data_dir: dir.path().join("data"),
            state_dir: dir.path().join("state"),
            cache_dir: dir.path().join("cache"),
            runtime_dir: None,
        };
        muta_persistence::paths::set_test_default(Some(dirs));

        let mut conns = Connections::default();
        conns.connections.push(Connection {
            id: "test-relay".to_string(),
            name: Some("Test Relay".to_string()),
            base_url: Some("https://example.com".to_string()),
            protocol: Some(WireProtocol::OpenAiChatCompletions),
            ..Default::default()
        });
        conns.save().unwrap();

        let (resp_tx, mut resp_rx) = tokio::sync::mpsc::unbounded_channel();
        query_connection_detail(&resp_tx, "test-relay".to_string()).await;

        // Phase 1: immediate detail with Fetching usage
        let initial = resp_rx.recv().await.expect("initial response");
        match initial {
            AgentResponse::ConnectionDetail(detail) => {
                assert_eq!(detail.id, "test-relay");
                assert_eq!(detail.usage, muta_contracts::ConnectionUsageState::Fetching);
            }
            other => panic!("expected ConnectionDetail, got {other:?}"),
        }

        // Phase 2: background resolution
        let final_resp = resp_rx.recv().await.expect("final response");
        match final_resp {
            AgentResponse::ConnectionDetail(detail) => {
                assert_eq!(detail.id, "test-relay");
                // Unsupported since base_url is example.com and no API key is set
                assert!(matches!(
                    detail.usage,
                    muta_contracts::ConnectionUsageState::Error(_)
                        | muta_contracts::ConnectionUsageState::Unsupported
                ));
            }
            other => panic!("expected final ConnectionDetail, got {other:?}"),
        }

        muta_persistence::paths::set_test_default(None);
    }
}
