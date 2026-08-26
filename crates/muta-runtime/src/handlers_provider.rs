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
    SecretString,
};
use muta_persistence::config::{Config, Credentials, DiscoveryCache, UserTransport};
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
#[allow(clippy::too_many_arguments)]
pub async fn switch(
    config: &mut Config,
    agent: &Agent,
    provider_for_task: &Arc<RwLock<Arc<dyn Provider>>>,
    session: &SessionStore,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ConnectionUsage,
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
        config,
        agent,
        provider_for_task,
        Some(session),
        resp_tx,
        provider_usage,
        provider_type,
        model,
    )
    .await;
}

/// `AgentRequest::AddProvider` — create a connection (from a preset
/// or as a pure-custom declaration), persist it to the state store, set its
/// credential, then activate it. For OAuth presets the TUI runs
/// [`authorize`] first, then calls this with `auth` set.
#[allow(clippy::too_many_arguments)]
pub async fn add(
    config: &mut Config,
    agent: &Agent,
    provider_for_task: &Arc<RwLock<Arc<dyn Provider>>>,
    session: &SessionStore,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ConnectionUsage,
    name: String,
    protocol: String,
    base_url: String,
    api_key: SecretString,
    user_agent: Option<String>,
    models: Vec<String>,
    auth: muta_contracts::ChannelAuth,
    template_id: Option<String>,
    client_identity: Option<ClientIdentity>,
) {
    let mut connections = Connections::load();
    let id = connections.unique_id(&name);
    let transport = transport_for_protocol(&protocol);
    let trimmed_key = api_key.expose_secret().trim();
    // Pasted API key on an OAuth preset → ordinary ApiKey auth.
    let auth = match (auth, !trimmed_key.is_empty()) {
        (a, true) if a.is_oauth() => muta_contracts::ChannelAuth::ApiKey,
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
        template_id.filter(|pid| muta_providers::provider_preset_spec(pid).is_some());
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

    let client_identity = client_identity.unwrap_or_default();

    let connection = Connection {
        id: id.clone(),
        name: (!name.trim().is_empty()).then(|| name.trim().to_string()),
        preset_id: resolved_preset_id,
        auth,
        api_key_env: None,
        client_identity,
        transport: if is_preset { None } else { Some(transport) },
        base_url: if is_preset { None } else { base_url },
        user_agent: if is_preset { None } else { user_agent },
        models: if is_preset {
            Vec::new()
        } else {
            declared_models
        },
    };
    connections.connections.push(connection);
    if connections.save().is_err() {
        tracing::warn!("add: could not persist connection");
    }
    // The connection's credential, if the user supplied one.
    if auth == muta_contracts::ChannelAuth::ApiKey && !trimmed_key.is_empty() {
        let mut creds = Credentials::load();
        creds.set_api_key(&id, Some(SecretString::from(trimmed_key)));
        if creds.save().is_err() {
            tracing::warn!("add: could not persist credential");
        }
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
    if auth.is_oauth() && auth != muta_contracts::ChannelAuth::AntigravityOAuth {
        let outcome = catalog::discover_provider_models().await;
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
        config,
        agent,
        provider_for_task,
        None,
        resp_tx,
        provider_usage,
        id,
        active_model,
    )
    .await;
}

/// `AgentRequest::EditProvider` — update a connection's display name, endpoint
/// override, credential, and client identity in place.
#[allow(clippy::too_many_arguments)]
pub async fn edit(
    config: &mut Config,
    agent: &Agent,
    provider_for_task: &Arc<RwLock<Arc<dyn Provider>>>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ConnectionUsage,
    id: String,
    name: String,
    protocol: String,
    base_url: String,
    api_key: SecretString,
    client_identity: Option<ClientIdentity>,
) {
    let mut connections = Connections::load();
    let transport = transport_for_protocol(&protocol);
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
            instance.transport = Some(transport);
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
            config,
            agent,
            provider_for_task,
            None,
            resp_tx,
            provider_usage,
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
#[allow(clippy::too_many_arguments)]
pub async fn edit_model(
    config: &mut Config,
    agent: &Agent,
    provider_for_task: &Arc<RwLock<Arc<dyn Provider>>>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ConnectionUsage,
    provider_id: String,
    model: String,
    effort: Option<String>,
    thinking: Option<bool>,
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
            config,
            agent,
            provider_for_task,
            None,
            resp_tx,
            provider_usage,
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
#[allow(clippy::too_many_arguments)]
pub async fn edit_model_reasoning(
    config: &mut Config,
    agent: &Agent,
    provider_for_task: &Arc<RwLock<Arc<dyn Provider>>>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ConnectionUsage,
    model: String,
    effort: Option<String>,
    thinking: Option<bool>,
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
    if routes.save().is_err() {
        tracing::warn!("edit_model_reasoning: could not persist route settings");
    }

    // Re-activate if this model is the live one so the change applies now.
    let active_model =
        catalog::resolved_model_name_with_usage(config, &provider_id, provider_usage)
            .unwrap_or_default();
    if active_model == model {
        activate(
            config,
            agent,
            provider_for_task,
            None,
            resp_tx,
            provider_usage,
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
#[allow(clippy::too_many_arguments)]
pub async fn delete(
    config: &mut Config,
    agent: &Agent,
    provider_for_task: &Arc<RwLock<Arc<dyn Provider>>>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ConnectionUsage,
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
    let mut auth_store = muta_providers::oauth::AuthStore::load();
    if auth_store.remove(&id).is_some() {
        let _ = auth_store.save();
    }
    let mut cache = DiscoveryCache::load();
    cache.remove_connection(&id);
    if cache.save().is_err() {
        tracing::warn!("delete: could not persist discovery cache");
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
            config,
            agent,
            provider_for_task,
            None,
            resp_tx,
            provider_usage,
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
        &effective,
        agent,
        provider_for_task,
        None,
        resp_tx,
        provider_usage,
        provider_id,
        model,
    )
    .await;
}

/// `AgentRequest::AuthorizeOAuth` — run an OAuth login before a connection
/// exists ("+ Add connection → xAI OAuth / ChatGPT OAuth"). `auth`
/// selects which provider's flow to run; tokens persist under that provider's
/// `auth.toml` key.
pub async fn authorize(
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    method: muta_contracts::LoginMethod,
    auth: muta_contracts::ChannelAuth,
) {
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
        return;
    };
    let label = cfg.provider_id.to_string();
    if run_oauth(resp_tx, &label, method, cfg).await {
        let _ = resp_tx.send(AgentResponse::ConnectStatus(
            muta_contracts::ConnectStatus::Done { provider: label },
        ));
    }
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
    let connections = Connections::load();
    let auth_mode = connections
        .get(&provider_id)
        .map(|p| p.auth)
        .unwrap_or_default();
    let Some(mut cfg) = auth_mode
        .oauth_provider_id()
        .and_then(muta_providers::oauth::config_by_provider_id)
    else {
        let _ = resp_tx.send(AgentResponse::ConnectStatus(
            muta_contracts::ConnectStatus::Failed {
                provider: provider_id,
                message: "not an OAuth provider".to_string(),
            },
        ));
        return;
    };
    cfg.provider_id = std::borrow::Cow::Owned(provider_id.clone());
    if !run_oauth(resp_tx, &provider_id, method, cfg).await {
        return;
    }
    let _ = resp_tx.send(AgentResponse::ConnectStatus(
        muta_contracts::ConnectStatus::Done {
            provider: provider_id.clone(),
        },
    ));
    // Live model discovery: fetch the provider's actual model list with the
    // fresh token so the picker shows the account's real entitlements right
    // away. A failure keeps the previous subset; each failure is reported back
    // as a warning so the user knows *why* the list did not refresh.
    let outcome = catalog::discover_provider_models().await;
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
        config,
        agent,
        provider_for_task,
        None,
        resp_tx,
        provider_usage,
        provider_id,
        model,
    )
    .await;
}

/// Shared OAuth body for any provider: browser loopback (PKCE) or device-code,
/// parameterized by the provider's [`muta_providers::oauth::OAuthConfig`]. Persists the resulting token
/// set (plus the ChatGPT account id, when present) under the provider's
/// `auth.toml` key.
async fn run_oauth(
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    label: &str,
    method: muta_contracts::LoginMethod,
    cfg: muta_providers::oauth::OAuthConfig,
) -> bool {
    use muta_providers::oauth::{AuthStore, OAuth};

    let oauth = OAuth::new(cfg.clone());
    let now_ms = chrono::Utc::now().timestamp_millis();

    let result = match method {
        muta_contracts::LoginMethod::Device => match cfg.device_flow {
            muta_providers::oauth::config::DeviceFlow::ChatGpt => {
                let device =
                    match muta_providers::oauth::request_chatgpt_device_code(oauth.client(), &cfg)
                        .await
                    {
                        Ok(d) => d,
                        Err(e) => {
                            let msg = e.to_string();
                            let _ = resp_tx.send(AgentResponse::ConnectStatus(
                                muta_contracts::ConnectStatus::Failed {
                                    provider: label.to_string(),
                                    message: msg,
                                },
                            ));
                            return false;
                        }
                    };
                let _ = resp_tx.send(AgentResponse::ConnectStatus(
                    muta_contracts::ConnectStatus::Pending {
                        provider: label.to_string(),
                        url: device.user_url(&cfg),
                        user_code: device.user_code.clone(),
                        message: "Open the URL on any device and enter the code to authorize."
                            .to_string(),
                    },
                ));
                let polled =
                    muta_providers::oauth::poll_chatgpt_device_code(oauth.client(), &cfg, &device)
                        .await;
                match polled {
                    Ok(token) => muta_providers::oauth::exchange_chatgpt_device_code(
                        oauth.client(),
                        &cfg,
                        &token,
                    )
                    .await
                    .map_err(|e| e.to_string()),
                    Err(e) => Err(e.to_string()),
                }
            }
            muta_providers::oauth::config::DeviceFlow::Rfc8628 => {
                let device =
                    match muta_providers::oauth::request_device_code(oauth.client(), &cfg).await {
                        Ok(d) => d,
                        Err(e) => {
                            let msg = e.to_string();
                            let _ = resp_tx.send(AgentResponse::ConnectStatus(
                                muta_contracts::ConnectStatus::Failed {
                                    provider: label.to_string(),
                                    message: msg,
                                },
                            ));
                            return false;
                        }
                    };
                let _ = resp_tx.send(AgentResponse::ConnectStatus(
                    muta_contracts::ConnectStatus::Pending {
                        provider: label.to_string(),
                        url: device.user_url().to_string(),
                        user_code: device.user_code.clone(),
                        message: "Open the URL on any device and enter the code to authorize."
                            .to_string(),
                    },
                ));
                muta_providers::oauth::poll_device_code(oauth.client(), &cfg, &device)
                    .await
                    .map_err(|e| e.to_string())
            }
            muta_providers::oauth::config::DeviceFlow::Disabled => {
                let _ = resp_tx.send(AgentResponse::ConnectStatus(
                        muta_contracts::ConnectStatus::Failed {
                            provider: label.to_string(),
                            message: "Device code flow is not supported for this provider. Please use browser login.".to_string(),
                        },
                    ));
                return false;
            }
        },
        muta_contracts::LoginMethod::Browser => {
            let login = match oauth.begin_browser_login().await {
                Ok(l) => l,
                Err(e) => {
                    let msg = e.to_string();
                    let _ = resp_tx.send(AgentResponse::ConnectStatus(
                        muta_contracts::ConnectStatus::Failed {
                            provider: label.to_string(),
                            message: msg,
                        },
                    ));
                    return false;
                }
            };
            let _ = resp_tx.send(AgentResponse::ConnectStatus(
                muta_contracts::ConnectStatus::Pending {
                    provider: label.to_string(),
                    url: login.url.clone(),
                    user_code: String::new(),
                    message: "Complete authorization in your browser (or open the link below)."
                        .to_string(),
                },
            ));
            login
                .complete(oauth.client())
                .await
                .map_err(|e| e.to_string())
        }
    };

    let tokens = match result {
        Ok(t) => t,
        Err(msg) => {
            let _ = resp_tx.send(AgentResponse::ConnectStatus(
                muta_contracts::ConnectStatus::Failed {
                    provider: label.to_string(),
                    message: msg,
                },
            ));
            return false;
        }
    };

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

    let set = muta_providers::oauth::TokenSet {
        access: tokens.access_token,
        refresh: tokens.refresh_token.unwrap_or_default(),
        expires_ms: now_ms + (tokens.expires_in.unwrap_or(3600) as i64) * 1000,
        account_id,
        id_token: tokens.id_token,
        token_type: tokens.token_type,
        scope: tokens.scope,
        project_id,
        user_email,
    };
    let mut store = AuthStore::load();
    store.set(&cfg.provider_id, set);
    if let Err(e) = store.save() {
        let _ = resp_tx.send(AgentResponse::ConnectStatus(
            muta_contracts::ConnectStatus::Failed {
                provider: label.to_string(),
                message: format!("could not save tokens: {e}"),
            },
        ));
        return false;
    }
    true
}

async fn refresh_oauth_if_needed(_config: &Config, provider_id: &str) {
    use muta_providers::oauth::{AuthStore, OAuth};

    let connections = Connections::load();
    let Some(instance) = connections.get(provider_id) else {
        return;
    };
    let auth = instance.auth;
    let preset_id = instance.preset_id.as_deref();

    let Some(mut cfg) = auth
        .oauth_provider_id()
        .and_then(muta_providers::oauth::config_by_provider_id)
    else {
        return;
    };
    cfg.provider_id = std::borrow::Cow::Owned(provider_id.to_string());

    let store = AuthStore::load();
    let Some(stored) = store
        .get_for_provider(provider_id, preset_id, auth)
        .cloned()
    else {
        return;
    };
    if stored.access.is_empty() || stored.refresh.is_empty() {
        return;
    }
    let oauth = OAuth::new(cfg.clone());
    match oauth.resolve_access_token(stored).await {
        Ok((_access, tokens)) => {
            let mut store = AuthStore::load();
            store.set(provider_id, tokens);
            let _ = store.save();
        }
        Err(e) => {
            tracing::warn!(error = %e, provider = %provider_id, "OAuth: token refresh failed; clearing store");
            let mut store = AuthStore::load();
            store.remove(provider_id);
            let _ = store.save();
        }
    }
}

/// Record a provider switch's acknowledgment in the durable command ledger.
async fn record_provider_ack(session: &SessionStore, provider: &str, model: &str, ack: String) {
    let record = CommandRecord::new("models", format!("{provider} {model}"))
        .with_result(CommandResult::Ack { title: ack });
    if let Err(error) = session.mutate_commands(|c| c.push(record)).await {
        tracing::warn!(?error, "could not persist provider-switch ack");
    }
}

/// Shared tail of [`switch`] and [`add`]: rebuild the active provider through the
/// catalog, swap it into the shared holder, re-seed mid-turn relief, and push
/// the key + picker snapshots.
#[allow(clippy::too_many_arguments)]
async fn activate(
    config: &Config,
    agent: &Agent,
    provider_for_task: &Arc<RwLock<Arc<dyn Provider>>>,
    session: Option<&SessionStore>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ConnectionUsage,
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
        config,
        agent,
        provider_for_task,
        None,
        resp_tx,
        provider_usage,
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
    let outcome = catalog::discover_provider_models().await;
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

/// Map a wire-protocol label from the TUI to the persisted transport enum.
fn transport_for_protocol(protocol: &str) -> UserTransport {
    match protocol {
        "anthropic" => UserTransport::Anthropic,
        "google" | "gemini" => UserTransport::Google,
        // The OpenAI Responses API over an API key (e.g. DeepSeek V4).
        "openai-responses" => UserTransport::OpenAiResponses,
        // Default (and explicit "openai"): the OpenAI-compatible chat surface.
        _ => UserTransport::OpenAi,
    }
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
            Some(muta_contracts::CommandResult::Ack { title }) => {
                assert_eq!(title, "Connection switched to 111xianyu (k3)");
            }
            other => panic!("expected a durable Ack result, got {other:?}"),
        }
    }

    #[test]
    fn transport_for_protocol_maps_known_labels() {
        assert_eq!(
            transport_for_protocol("anthropic"),
            UserTransport::Anthropic
        );
        assert_eq!(transport_for_protocol("google"), UserTransport::Google);
        assert_eq!(
            transport_for_protocol("openai-responses"),
            UserTransport::OpenAiResponses
        );
        assert_eq!(transport_for_protocol("openai"), UserTransport::OpenAi);
        assert_eq!(transport_for_protocol("future"), UserTransport::OpenAi);
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
}
