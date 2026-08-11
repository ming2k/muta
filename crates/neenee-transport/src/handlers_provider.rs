//! Provider-switch / favorite / default-model handlers, extracted verbatim
//! from the agent background task's `match req { … }` dispatch.
//!
//! Each handler is one match arm, lifted unchanged. Parameters are named to
//! match the original loop locals (`config`, `agent`, `provider_for_task`,
//! `resp_tx`, `provider_usage`) so the body reads exactly as it did inline.

use neenee_agent::Agent;
use neenee_agent::catalog;
use neenee_agent::orchestration::round_response;
use neenee_core::{
    AgentNotice, AgentResponse, CommandRecord, CommandResult, Provider, RoundEvent, SecretString,
};
use neenee_persistence::{
    config::Config,
    provider_usage::ProviderUsage,
    session::{ProviderSelection, SessionStore},
};
use std::sync::{Arc, RwLock};
use tokio::sync::mpsc;

use crate::agent_setup::{reseed_prune_threshold, reseed_tool_variants};
use crate::session_view::provider_key_status;

/// Whether `id` is a multi-model provider — a built-in that hosts several models
/// behind one key, or a user-defined provider with more than one channel. For
/// these the active model lives in `config.default_model` rather than a
/// per-provider model slot.
fn is_multi_model_provider(config: &Config, id: &str) -> bool {
    if matches!(
        id,
        "openai" | "opencode-go" | "anthropic" | "google" | "deepseek"
    ) {
        return true;
    }
    config
        .providers
        .iter()
        .any(|p| p.id == id && p.channels.len() > 1)
}

/// Persist a TUI-entered API key for `provider_type`. The legacy per-builtin
/// fields are still written (startup migration folds them into instances
/// created later), but the catalog builds providers exclusively from
/// `config.providers` instances — so when an instance already exists the key
/// must also land on every non-OAuth channel, otherwise the live provider
/// keeps the old key and the new one is dropped at the next startup. OAuth
/// channels are skipped: their bearer is owned by the auth flow.
fn apply_switch_api_key(config: &mut Config, provider_type: &str, key: &str) {
    match provider_type {
        "openai" => config.openai_api_key = Some(key.into()),
        "google" => config.google_api_key = Some(key.into()),
        "kimi-code" => config.moonshot_api_key = Some(key.into()),
        "deepseek" => config.deepseek_api_key = Some(key.into()),
        "zai-code" => config.zai_api_key = Some(key.into()),
        "opencode-go" => config.opencode_go_api_key = Some(key.into()),
        "anthropic" => config.anthropic_api_key = Some(key.into()),
        _ => {}
    }
    if let Some(provider) = config.providers.iter_mut().find(|p| p.id == provider_type) {
        for channel in &mut provider.channels {
            if !channel.auth.is_oauth() {
                channel.api_key = Some(key.into());
            }
        }
    }
}

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
    provider_usage: &mut ProviderUsage,
    provider_type: String,
    model: String,
    api_key: Option<SecretString>,
    base_url: Option<String>,
) {
    // A key entered in the TUI is persisted and wins over
    // config; environment variables still take precedence.
    if let Some(key) = api_key {
        apply_switch_api_key(config, &provider_type, key.expose_secret());
    }
    if let Some(url) = base_url
        && provider_type.as_str() == "anthropic"
    {
        config.anthropic_base_url = Some(url);
    }
    // ADR-0046: reasoning (effort/thinking) is no longer set on provider
    // switch — it is opted in per model via `[model_reasoning]`
    // (`EditModelReasoning`) / a channel's reasoning fields
    // (`EditProviderModel`). Switching just selects the provider + model.
    //
    // Set the selection on the effective config so `activate` resolves the
    // right channel; the save below persists it as the global default, and
    // the session pin (written further below) records this session's own
    // choice for exact restore on resume.
    config.default_provider = provider_type.clone();
    // Multi-model providers (opencode-go, anthropic, google, deepseek, and any
    // user-defined provider with several channels) carry the active model in the
    // shared `default_model` field — every channel shares one API key and each
    // model's transport is derived from its catalog channel. Single-model
    // built-ins keep their per-provider model slot as before.
    let pinned_model: Option<String> = if is_multi_model_provider(config, &provider_type) {
        config.default_model = Some(model.clone());
        Some(model.clone())
    } else {
        config.default_model = None;
        match provider_type.as_str() {
            "kimi-code" => config.moonshot_model = Some(model.clone()),
            "zai-code" => config.zai_model = Some(model.clone()),
            _ => {}
        }
        // Keep the per-session pin's model explicit so reopen lands on the
        // exact model even for single-model providers.
        Some(model.clone())
    };
    // Persist the switch as the global default so the next launch (a fresh
    // session without a pin) lands on this provider/model.
    if let Err(error) = config.save() {
        tracing::warn!(?error, "could not persist provider selection");
    }
    // Pin the provider + model to this session so resume restores it exactly.
    // Best-effort: a failed pin does not block the live switch.
    if let Err(error) = session
        .set_provider_selection(Some(ProviderSelection {
            provider: provider_type.clone(),
            model: pinned_model,
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

/// `AgentRequest::AddProvider` — persist a user-defined ("custom") provider to
/// `config.providers`, then activate it. For SuperGrok OAuth the TUI runs
/// [`authorize`] first, then calls this with `auth = XaiOAuth`.
#[allow(clippy::too_many_arguments)]
pub async fn add(
    config: &mut Config,
    agent: &Agent,
    provider_for_task: &Arc<RwLock<Arc<dyn Provider>>>,
    session: &SessionStore,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ProviderUsage,
    name: String,
    protocol: String,
    base_url: String,
    api_key: SecretString,
    user_agent: Option<String>,
    models: Vec<String>,
    auth: neenee_core::ChannelAuth,
    template_id: Option<String>,
) {
    use neenee_persistence::config::{UserChannelConfig, UserProviderConfig, UserTransport};

    let id = unique_provider_id(config, &name);
    let transport = match protocol.as_str() {
        "anthropic" => UserTransport::Anthropic,
        "google" | "gemini" => UserTransport::Google,
        // Default (and explicit "openai"): the OpenAI-compatible chat surface.
        _ => UserTransport::OpenAi,
    };
    let trimmed_key = api_key.expose_secret().trim();
    let api_key = (!trimmed_key.is_empty()).then(|| SecretString::from(trimmed_key));
    // Pasted API key on an OAuth template → ordinary ApiKey auth.
    let auth = match (auth, api_key.is_some()) {
        (a, true) if a.is_oauth() => neenee_core::ChannelAuth::ApiKey,
        (other, _) => other,
    };
    let base_url = {
        let trimmed = base_url.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    };
    // ADR-0046: reasoning is opt-in per model. New channels are created with no
    // effort/thinking — the user opts a model in from the stage-2 model `e`
    // editor (`EditProviderModel`). One channel per seeded model — a template
    // that seeds the whole Claude family lands every model in the picker's
    // stage-2 list, all sharing the provider's transport/endpoint/key. Empty/
    // whitespace model ids are dropped.
    let channels: Vec<UserChannelConfig> = models
        .iter()
        .map(|m| m.trim())
        .filter(|m| !m.is_empty())
        .map(|model| UserChannelConfig {
            label: model.to_string(),
            transport,
            api_key_env: None,
            api_key: api_key.clone(),
            model: Some(model.to_string()),
            base_url: base_url.clone(),
            user_agent: user_agent.clone(),
            effort: None,
            thinking: None,
            auth,
            remote: None,
        })
        .collect();
    // A provider must serve at least one model; a template with no usable model
    // id is a no-op rather than a broken zero-channel entry.
    if channels.is_empty() {
        return;
    }
    let active_model = channels[0].model.clone().unwrap_or_default();
    // Stamp the template id so the catalog re-seeds this instance from the
    // template's current models at startup. Only a known id is recorded; an
    // unknown / blank value keeps the instance pure-custom (no tracking).
    let resolved_template_id =
        template_id.filter(|tid| neenee_providers::provider_template_spec(tid).is_some());
    // A template-sourced instance adopts the template's default model source:
    // Api (live `GET /models` discovery with snapshot fallback) where the
    // template supports it, Fixed otherwise. A pure-custom instance ignores
    // model_source, so the Fixed default is harmless.
    let model_source = resolved_template_id
        .as_deref()
        .and_then(neenee_providers::provider_template_spec)
        .map(catalog::default_model_source_for_spec)
        .unwrap_or_default();
    let entry = UserProviderConfig {
        id: id.clone(),
        name: (!name.trim().is_empty()).then(|| name.trim().to_string()),
        channels,
        default_channel: 0,
        template_id: resolved_template_id,
        model_source,
        // A fresh instance has no fitted metadata yet; live discovery fills
        // it for fitting-enabled templates on the next refresh.
        fitted_models: Default::default(),
    };
    config.providers.push(entry);
    config.default_provider = id.clone();
    // Record the first seeded model as the active model so the picker and status
    // surfaces land on it.
    config.default_model = Some(active_model.clone());
    // Adding a provider is also a live switch: persist the selection as the
    // global default (like `/models`), then pin it to this session so resume
    // restores it exactly.
    let _ = config.save();
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
    if auth.is_oauth() {
        let outcome = catalog::discover_provider_models(config).await;
        if outcome.changed {
            catalog::sync_fitted_model_registry(config);
            if let Err(error) = config.save_preserving_provider_selection() {
                tracing::warn!(?error, "live discovery after add: could not persist");
            }
        }
        for (failed_provider, message) in &outcome.failures {
            let _ = resp_tx.send(AgentResponse::ConnectStatus(
                neenee_core::ConnectStatus::DiscoveryWarning {
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

/// `AgentRequest::EditProvider` — update a user-defined provider's metadata in
/// place: display name, and every API-key channel's transport/base-URL/key.
/// Each channel's model id is preserved, so a multi-model custom provider keeps
/// all its models. OAuth channels (ChatGPT/Codex, xAI) are skipped: their
/// base-URL/transport/token are owned by the auth flow and must not be
/// overwritten, so only the name (and an API-key channel's fields) change. An
/// empty `api_key` leaves the existing key untouched. Persists, then
/// re-activates so the live provider picks up the new endpoint/key.
#[allow(clippy::too_many_arguments)]
pub async fn edit(
    config: &mut Config,
    agent: &Agent,
    provider_for_task: &Arc<RwLock<Arc<dyn Provider>>>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ProviderUsage,
    id: String,
    name: String,
    protocol: String,
    base_url: String,
    api_key: SecretString,
) {
    use neenee_persistence::config::UserTransport;

    let transport = match protocol.as_str() {
        "anthropic" => UserTransport::Anthropic,
        "google" | "gemini" => UserTransport::Google,
        _ => UserTransport::OpenAi,
    };
    let trimmed_url = base_url.trim();
    let trimmed_key = api_key.expose_secret().trim();
    let trimmed_name = name.trim();
    let Some(provider) = config.providers.iter_mut().find(|p| p.id == id) else {
        return;
    };
    if !trimmed_name.is_empty() {
        provider.name = Some(trimmed_name.to_string());
    }
    for channel in &mut provider.channels {
        // An OAuth channel's endpoint and bearer are resolved by the auth flow
        // (xAI `https://api.x.ai/...`, ChatGPT
        // `https://chatgpt.com/backend-api/codex/...`); editing the provider
        // must never clobber them. The editor hides Base URL/Token for OAuth,
        // but the server guards too so a malformed/empty payload can't wipe
        // them.
        if channel.auth.is_oauth() {
            continue;
        }
        channel.transport = transport;
        channel.base_url = (!trimmed_url.is_empty()).then(|| trimmed_url.to_string());
        // An empty key keeps whatever the channel already had.
        if !trimmed_key.is_empty() {
            channel.api_key = Some(SecretString::from(trimmed_key));
        }
        // ADR-0046: reasoning (effort/thinking) is no longer edited here — it
        // is per-model, via `EditProviderModel`. Editing provider metadata
        // leaves each channel's reasoning knobs untouched.
    }
    let _ = config.save_preserving_provider_selection();
    // Only rebuild the live provider when editing the active one (so a new
    // endpoint/key takes effect); editing an inactive provider just refreshes
    // the persisted config + the picker snapshot without switching.
    if config.default_provider == id {
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

/// `AgentRequest::RemoveProviderModel` — drop a model (channel) from a
/// user-defined provider, persist, and push a fresh picker snapshot. The last
/// remaining channel is kept (a provider must serve at least one model). If the
/// removed model was the active `default_model`, it is cleared so the provider
/// falls back to its default channel.
pub async fn remove_model(
    config: &mut Config,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &ProviderUsage,
    provider_id: String,
    model: String,
) {
    if let Some(provider) = config.providers.iter_mut().find(|p| p.id == provider_id)
        && provider.channels.len() > 1
        && let Some(pos) = provider
            .channels
            .iter()
            .position(|c| c.model.as_deref() == Some(model.as_str()))
    {
        provider.channels.remove(pos);
        if provider.default_channel >= provider.channels.len() {
            provider.default_channel = 0;
        }
    }
    if config.default_model.as_deref() == Some(model.as_str()) {
        config.default_model = None;
    }
    // Favorite is model-level (ADR-0046): a removed model's star is pruned so
    // the picker never references a model that is no longer served.
    config.favorites.retain(|fav| *fav != model);
    if let Err(error) = config.save_preserving_provider_selection() {
        tracing::warn!(?error, "could not persist removed provider model");
    }
    let _ = resp_tx.send(AgentResponse::ProviderPicker(catalog::build_picker_state(
        config,
        provider_usage,
    )));
}

/// `AgentRequest::EditProviderModel` — update settings for one channel of a
/// user-defined provider. Provider metadata (name/base URL/key) is untouched.
#[allow(clippy::too_many_arguments)]
pub async fn edit_model(
    config: &mut Config,
    agent: &Agent,
    provider_for_task: &Arc<RwLock<Arc<dyn Provider>>>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ProviderUsage,
    provider_id: String,
    model: String,
    effort: Option<String>,
    thinking: Option<bool>,
) {
    let valid_effort = effort.and_then(|e| {
        let t = e.trim();
        (!t.is_empty())
            .then(|| t.to_ascii_lowercase())
            .filter(|s| neenee_core::effort::Effort::parse(s).is_some())
    });

    let Some(provider) = config.providers.iter_mut().find(|p| p.id == provider_id) else {
        return;
    };
    let Some(channel) = provider
        .channels
        .iter_mut()
        .find(|c| c.model.as_deref() == Some(model.as_str()))
    else {
        return;
    };

    match channel.transport {
        neenee_persistence::config::UserTransport::Anthropic => {
            channel.effort = valid_effort;
            channel.thinking = thinking;
        }
        neenee_persistence::config::UserTransport::OpenAi => {
            channel.effort = valid_effort;
            channel.thinking = None;
        }
        neenee_persistence::config::UserTransport::Google => {}
    }

    if let Err(error) = config.save_preserving_provider_selection() {
        tracing::warn!(?error, "could not persist provider model settings");
    }

    let active_model =
        catalog::resolved_model_name_with_usage(config, &provider_id, provider_usage)
            .unwrap_or_default();
    if config.default_provider == provider_id && active_model == model {
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

/// `AgentRequest::EditModelReasoning` — update the per-model reasoning
/// settings (Anthropic effort/thinking) persisted in the
/// `[model_reasoning."<model-id>"]` table. This serves the **built-in**
/// `anthropic` provider (and any built-in Anthropic-format model), which has
/// no user-editable channels: its per-model knobs live in this shared table
/// keyed by model id (ADR-0045). If the edited model is the active one, the
/// live provider is re-activated so the new settings take effect at once.
#[allow(clippy::too_many_arguments)]
pub async fn edit_model_reasoning(
    config: &mut Config,
    agent: &Agent,
    provider_for_task: &Arc<RwLock<Arc<dyn Provider>>>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ProviderUsage,
    model: String,
    effort: Option<String>,
    thinking: Option<bool>,
) {
    let valid_effort = effort.and_then(|e| {
        let t = e.trim();
        (!t.is_empty())
            .then(|| t.to_ascii_lowercase())
            .filter(|s| neenee_core::effort::Effort::parse(s).is_some())
    });

    let settings = config.model_reasoning.for_model_mut(&model);
    settings.effort = valid_effort;
    settings.thinking = thinking;

    if let Err(error) = config.save_preserving_provider_selection() {
        tracing::warn!(?error, "could not persist per-model reasoning settings");
    }

    // Re-activate if this model is the live one so the change applies now.
    let provider_id = &config.default_provider;
    let active_model = catalog::resolved_model_name_with_usage(config, provider_id, provider_usage)
        .unwrap_or_default();
    if active_model == model {
        activate(
            config,
            agent,
            provider_for_task,
            None,
            resp_tx,
            provider_usage,
            provider_id.clone(),
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

/// `AgentRequest::DeleteProvider` — remove a user-defined provider entry
/// entirely. Drops it from `config.providers` (a no-op for built-ins or an
/// unknown id), prunes it from `favorites`, and persists. When the deleted
/// provider was the active one (`config.default_provider`), it falls back to
/// the default built-in provider (`"kimi-code"`) and re-activates so the live
/// provider never points at a removed entry. Otherwise (deleting an inactive
/// provider) it only refreshes the picker snapshot.
#[allow(clippy::too_many_arguments)]
pub async fn delete(
    config: &mut Config,
    agent: &Agent,
    provider_for_task: &Arc<RwLock<Arc<dyn Provider>>>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ProviderUsage,
    id: String,
) {
    // Drop the user-defined entry. `retain` is a no-op when the id is unknown,
    // and built-in ids are never present in `config.providers`, so this is
    // safely a built-in guard. Capture the deleted provider's model ids first —
    // favorite is model-level (ADR-0046), so those are the favorites to prune.
    let deleted_models: Vec<String> = config
        .providers
        .iter()
        .find(|p| p.id == id)
        .map(|p| p.channels.iter().filter_map(|c| c.model.clone()).collect())
        .unwrap_or_default();
    let before = config.providers.len();
    config.providers.retain(|p| p.id != id);
    // Nothing to do — the id was not a user-defined provider.
    if config.providers.len() == before {
        return;
    }
    // Prune the deleted provider's model ids from favorites (model-level) so
    // the picker never references a model that is no longer served.
    if !deleted_models.is_empty() {
        config
            .favorites
            .retain(|fav| !deleted_models.iter().any(|m| m == fav));
    }

    let was_active = config.default_provider == id;
    if was_active {
        // Fall back to the catalog's default built-in provider (kimi-code),
        // clear any model pointer that belonged to the deleted provider, then
        // activate so the live provider is rebuilt from a valid entry.
        config.default_provider = catalog::default_provider_id(&Config::default()).to_string();
        config.default_model = None;
    }
    if let Err(error) = config.save_preserving_provider_selection() {
        tracing::warn!(?error, "could not persist deleted provider");
    }

    if was_active {
        let fallback = config.default_provider.clone();
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
        // Deleting an inactive provider: refresh the picker + key snapshots
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
    provider_usage: &mut ProviderUsage,
) {
    // Overlay the session pin onto a throwaway clone so catalog resolution
    // picks the session's provider/model, not the global default.
    let mut effective = config.clone();
    let selection = session.provider_selection().await;
    let (provider_id, model_id): (String, Option<String>) = match &selection {
        Some(sel) => {
            effective.default_provider = sel.provider.clone();
            if let Some(model) = &sel.model {
                effective.default_model = Some(model.clone());
            }
            (sel.provider.clone(), sel.model.clone())
        }
        None => (
            catalog::default_provider_id(config).to_string(),
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

/// Derive a stable provider id from a user-supplied display name: lowercase,
/// non-alphanumeric runs collapsed to single hyphens, trimmed. Falls back to
/// `"custom"` for an empty/symbol-only name so the id is always non-empty.
fn custom_provider_id(name: &str) -> String {
    let mut id = String::new();
    let mut prev_hyphen = false;
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            id.push(ch.to_ascii_lowercase());
            prev_hyphen = false;
        } else if !prev_hyphen && !id.is_empty() {
            id.push('-');
            prev_hyphen = true;
        }
    }
    let id = id.trim_end_matches('-').to_string();
    if id.is_empty() {
        "custom".to_string()
    } else {
        id
    }
}

fn unique_provider_id(config: &Config, name: &str) -> String {
    let base = custom_provider_id(name);
    if !config.providers.iter().any(|p| p.id == base) {
        return base;
    }
    for n in 2usize.. {
        let candidate = format!("{base}-{n}");
        if !config.providers.iter().any(|p| p.id == candidate) {
            return candidate;
        }
    }
    unreachable!("unbounded suffix search must eventually find an id")
}

/// `AgentRequest::AuthorizeOAuth` — run an OAuth login before a provider
/// instance exists ("+ Add provider → xAI OAuth / ChatGPT OAuth"). `auth`
/// selects which provider's flow to run; tokens persist under that provider's
/// `auth.toml` key (`xai` / `chatgpt`).
pub async fn authorize(
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    method: neenee_core::LoginMethod,
    auth: neenee_core::ChannelAuth,
) {
    let Some(cfg) = auth
        .oauth_provider_id()
        .and_then(neenee_providers::oauth::config_by_provider_id)
        .copied()
    else {
        let _ = resp_tx.send(AgentResponse::ConnectStatus(
            neenee_core::ConnectStatus::Failed {
                provider: "oauth".to_string(),
                message: "not an OAuth provider".to_string(),
            },
        ));
        return;
    };
    let label = cfg.provider_id.to_string();
    if run_oauth(resp_tx, &label, method, cfg).await {
        let _ = resp_tx.send(AgentResponse::ConnectStatus(
            neenee_core::ConnectStatus::Done { provider: label },
        ));
    }
}

/// `AgentRequest::ConnectProvider` — re-auth an existing OAuth provider, then
/// activate it.
///
/// After a successful login, runs live model discovery so the provider's
/// model list reflects the account's real entitlements immediately (rather
/// than waiting for the next launch). Discovery failures are non-fatal: the
/// provider keeps its previous model subset.
pub async fn connect(
    config: &mut Config,
    agent: &Agent,
    provider_for_task: &Arc<RwLock<Arc<dyn Provider>>>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ProviderUsage,
    provider_id: String,
    method: neenee_core::LoginMethod,
) {
    let auth_mode = catalog::build_picker_state(config, provider_usage)
        .rows
        .into_iter()
        .find(|r| r.id == provider_id)
        .map(|r| r.auth)
        .unwrap_or_default();
    let Some(cfg) = auth_mode
        .oauth_provider_id()
        .and_then(neenee_providers::oauth::config_by_provider_id)
        .copied()
    else {
        let _ = resp_tx.send(AgentResponse::ConnectStatus(
            neenee_core::ConnectStatus::Failed {
                provider: provider_id,
                message: "not an OAuth provider".to_string(),
            },
        ));
        return;
    };
    if !run_oauth(resp_tx, &provider_id, method, cfg).await {
        return;
    }
    let _ = resp_tx.send(AgentResponse::ConnectStatus(
        neenee_core::ConnectStatus::Done {
            provider: provider_id.clone(),
        },
    ));
    // Live model discovery: fetch the provider's actual model list with the
    // fresh token so the picker shows the account's real entitlements right
    // away. A failure keeps the previous subset; each failure is reported back
    // as a warning so the user knows *why* the list did not refresh.
    let outcome = catalog::discover_provider_models(config).await;
    if outcome.changed {
        catalog::sync_fitted_model_registry(config);
        if let Err(error) = config.save_preserving_provider_selection() {
            tracing::warn!(?error, "live discovery after login: could not persist");
        }
    }
    for (failed_provider, message) in &outcome.failures {
        let _ = resp_tx.send(AgentResponse::ConnectStatus(
            neenee_core::ConnectStatus::DiscoveryWarning {
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
        .unwrap_or_else(|| "gpt-5.6-sol".to_string());
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
/// parameterized by the provider's [`OAuthConfig`]. Persists the resulting token
/// set (plus the ChatGPT account id, when present) under the provider's
/// `auth.toml` key.
async fn run_oauth(
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    label: &str,
    method: neenee_core::LoginMethod,
    cfg: neenee_providers::oauth::OAuthConfig,
) -> bool {
    use neenee_providers::oauth::{AuthStore, OAuth};

    let oauth = OAuth::new(cfg);
    let now_ms = chrono::Utc::now().timestamp_millis();

    let result =
        match method {
            neenee_core::LoginMethod::Device => match cfg.device_flow {
                neenee_providers::oauth::config::DeviceFlow::ChatGpt => {
                    let device = match neenee_providers::oauth::request_chatgpt_device_code(
                        oauth.client(),
                        &cfg,
                    )
                    .await
                    {
                        Ok(d) => d,
                        Err(e) => {
                            let msg = e.to_string();
                            let _ = resp_tx.send(AgentResponse::ConnectStatus(
                                neenee_core::ConnectStatus::Failed {
                                    provider: label.to_string(),
                                    message: msg,
                                },
                            ));
                            return false;
                        }
                    };
                    let _ = resp_tx.send(AgentResponse::ConnectStatus(
                        neenee_core::ConnectStatus::Pending {
                            provider: label.to_string(),
                            url: device.user_url(&cfg),
                            user_code: device.user_code.clone(),
                            message: "Open the URL on any device and enter the code to authorize."
                                .to_string(),
                        },
                    ));
                    let polled = neenee_providers::oauth::poll_chatgpt_device_code(
                        oauth.client(),
                        &cfg,
                        &device,
                    )
                    .await;
                    match polled {
                        Ok(token) => neenee_providers::oauth::exchange_chatgpt_device_code(
                            oauth.client(),
                            &cfg,
                            &token,
                        )
                        .await
                        .map_err(|e| e.to_string()),
                        Err(e) => Err(e.to_string()),
                    }
                }
                neenee_providers::oauth::config::DeviceFlow::Rfc8628 => {
                    let device =
                        match neenee_providers::oauth::request_device_code(oauth.client(), &cfg)
                            .await
                        {
                            Ok(d) => d,
                            Err(e) => {
                                let msg = e.to_string();
                                let _ = resp_tx.send(AgentResponse::ConnectStatus(
                                    neenee_core::ConnectStatus::Failed {
                                        provider: label.to_string(),
                                        message: msg,
                                    },
                                ));
                                return false;
                            }
                        };
                    let _ = resp_tx.send(AgentResponse::ConnectStatus(
                        neenee_core::ConnectStatus::Pending {
                            provider: label.to_string(),
                            url: device.user_url().to_string(),
                            user_code: device.user_code.clone(),
                            message: "Open the URL on any device and enter the code to authorize."
                                .to_string(),
                        },
                    ));
                    neenee_providers::oauth::poll_device_code(oauth.client(), &cfg, &device)
                        .await
                        .map_err(|e| e.to_string())
                }
            },
            neenee_core::LoginMethod::Browser => {
                let login = match oauth.begin_browser_login().await {
                    Ok(l) => l,
                    Err(e) => {
                        let msg = e.to_string();
                        let _ = resp_tx.send(AgentResponse::ConnectStatus(
                            neenee_core::ConnectStatus::Failed {
                                provider: label.to_string(),
                                message: msg,
                            },
                        ));
                        return false;
                    }
                };
                let _ = resp_tx.send(AgentResponse::ConnectStatus(
                    neenee_core::ConnectStatus::Pending {
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
                neenee_core::ConnectStatus::Failed {
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
    let account_id = tokens
        .id_token
        .as_ref()
        .map(SecretString::expose_secret)
        .or(Some(tokens.access_token.expose_secret()))
        .and_then(neenee_providers::oauth::chatgpt_account_id);

    let set = neenee_providers::oauth::TokenSet {
        access: tokens.access_token,
        refresh: tokens.refresh_token.unwrap_or_default(),
        expires_ms: now_ms + (tokens.expires_in.unwrap_or(3600) as i64) * 1000,
        account_id,
    };
    let mut store = AuthStore::load();
    store.set(cfg.provider_id, set);
    if let Err(e) = store.save() {
        let _ = resp_tx.send(AgentResponse::ConnectStatus(
            neenee_core::ConnectStatus::Failed {
                provider: label.to_string(),
                message: format!("could not save tokens: {e}"),
            },
        ));
        return false;
    }
    true
}

async fn refresh_oauth_if_needed(config: &Config, provider_id: &str) {
    use neenee_providers::oauth::{AuthStore, OAuth};

    // Resolve the channel's OAuth config (xAI or ChatGPT) from its auth mode.
    let auth = config
        .providers
        .iter()
        .find(|p| p.id == provider_id)
        .and_then(|p| p.channels.first())
        .map(|ch| ch.auth);
    let Some(cfg) = auth
        .and_then(|a| a.oauth_provider_id())
        .and_then(neenee_providers::oauth::config_by_provider_id)
        .copied()
    else {
        return;
    };
    let Some(stored) = AuthStore::load().get(cfg.provider_id).cloned() else {
        return;
    };
    if stored.access.is_empty() || stored.refresh.is_empty() {
        return;
    }
    let oauth = OAuth::new(cfg);
    match oauth.resolve_access_token(stored).await {
        Ok((_access, tokens)) => {
            let mut store = AuthStore::load();
            store.set(cfg.provider_id, tokens);
            let _ = store.save();
        }
        Err(e) => {
            tracing::warn!(error = %e, provider = %cfg.provider_id, "OAuth: token refresh failed; clearing store");
            let mut store = AuthStore::load();
            store.remove(cfg.provider_id);
            let _ = store.save();
        }
    }
}

/// Record a provider switch's acknowledgment in the durable command ledger —
/// the ADR-0091 twin of the ADR-0088 toast. The live confirmation stays a
/// transient toast (emitted by `activate`); the ledger keeps a durable `Ack`
/// so resume/export/audit can show the switch happened, without polluting the
/// message stream. Recorded under the `"models"` command word (the picker the
/// user actually invoked to switch); best-effort, a failed persist logs but
/// does not abort the switch.
async fn record_provider_ack(session: &SessionStore, provider: &str, model: &str, ack: String) {
    let record = CommandRecord::new("models", format!("{provider} {model}"))
        .with_result(CommandResult::Ack { title: ack });
    if let Err(error) = session.mutate_commands(|c| c.push(record)).await {
        tracing::warn!(?error, "could not persist provider-switch ack");
    }
}

/// Shared tail of [`switch`] and [`add`]: rebuild the active provider through the
/// catalog (so api-key / endpoint / user-agent resolution matches startup), swap
/// it into the shared holder, re-seed mid-turn relief, and push the key + picker
/// snapshots. `config` must already be persisted with the chosen pointers.
///
/// `session` is `Some` only for a genuine user-initiated switch ([`switch`]);
/// those call sites additionally surface a toast acknowledgment and record the
/// switch in the durable command ledger (ADR-0088/0091). The many *rebuild*
/// callers (edit/delete/reasoning/reapply, …) pass `None` — re-activating the
/// same provider is not a user-visible "switch", so it stays silent and
/// unrecorded, exactly as before.
#[allow(clippy::too_many_arguments)]
async fn activate(
    config: &Config,
    agent: &Agent,
    provider_for_task: &Arc<RwLock<Arc<dyn Provider>>>,
    session: Option<&SessionStore>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ProviderUsage,
    provider_type: String,
    model: String,
) {
    refresh_oauth_if_needed(config, &provider_type).await;

    // The live session id flows into prompt-cache control (ADR-0067): when the
    // selected model's family is Moonshot / Kimi, it becomes the provider's
    // `prompt_cache_key` so the server-side cache namespaces per session. The
    // agent already carries the thread id (set at session start), so we resolve
    // it here instead of threading a new parameter through every dispatch arm.
    let session_id = agent.thread_id();
    // For multi-model providers the explicit model selects the channel (and thus
    // the per-model transport); build_provider_for_model reads `default_model` as
    // a fallback. Returns `None` when the provider id is unknown or has no
    // resolvable channel — refuse the switch with a user-facing error instead
    // of silently installing a non-functional placeholder.
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
            "No provider configured for '{provider_type}'. \
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

    // The new model may have a different context window; re-seed
    // the mid-turn prune threshold so relief tracks it.
    reseed_prune_threshold(agent, config);
    // Tool-description overrides are keyed by model id, so they must
    // re-track the live model too.
    reseed_tool_variants(agent, config);

    let _ = resp_tx.send(AgentResponse::ProviderKeys(provider_key_status(config)));
    // Record the switch as an activation so the picker's recency
    // ordering tracks it. Both the provider and the exact model are bumped:
    // provider recency drives stage-1 order, model recency drives stage-2
    // order, and pinning the model under this provider makes a re-open land
    // on it. Best-effort: telemetry is rebuildable.
    provider_usage.record(&provider_type);
    provider_usage.record_model(&provider_type, &model);
    if let Err(error) = provider_usage.save() {
        tracing::warn!(?error, "could not persist model usage telemetry");
    }
    let ack = format!("Provider switched to {provider_type} ({model})");
    let _ = resp_tx.send(AgentResponse::ProviderSwitched {
        provider: provider_type.clone(),
        model: model.clone(),
    });
    // A user-initiated switch is a command acknowledgment, not model output
    // (ADR-0088): surface it as a transient toast, never appended to the
    // transcript. Emitting it wrapped in `RoundEvent::Notice` (rather than as
    // a top-level `AgentResponse::Notice`) routes the toast over the session's
    // broadcast tap so every attached client sees it, and matches the TUI's
    // toast drain. The ledger keeps the durable `Ack` for audit (ADR-0091).
    // `ProviderSwitched` above already refreshed the hint bar, which is the
    // long-lived "still in effect" indicator after the toast fades.
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

/// `AgentRequest::ToggleFavorite` — flip the model id in the favorites list,
/// persist, and push a fresh picker snapshot so the ★ flips at once. Favorite
/// is model-level (ADR-0046), so `id` is a model wire id; the flag surfaces on
/// every flat Models row that serves that model.
pub async fn toggle_favorite(
    config: &mut Config,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &ProviderUsage,
    id: String,
) {
    if let Some(pos) = config.favorites.iter().position(|fav| *fav == id) {
        config.favorites.remove(pos);
    } else {
        config.favorites.push(id.clone());
    }
    if let Err(error) = config.save_preserving_provider_selection() {
        tracing::warn!(?error, "could not persist favorites");
    }
    let _ = resp_tx.send(AgentResponse::ProviderPicker(catalog::build_picker_state(
        config,
        provider_usage,
    )));
}

/// `AgentRequest::SetDefaultModel` — make `id` the default AND activate it,
/// reusing the catalog so resolution rules stay shared. No new key/model
/// comes from the TUI — the provider's existing resolved config is used as-is.
pub async fn set_default_model(
    config: &mut Config,
    agent: &Agent,
    provider_for_task: &Arc<RwLock<Arc<dyn Provider>>>,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
    provider_usage: &mut ProviderUsage,
    id: String,
) {
    config.default_provider = id.clone();
    if let Err(error) = config.save() {
        tracing::warn!(?error, "could not persist default model");
    }
    // Same refusal contract as `activate`: when the catalog cannot resolve a
    // real provider/channel, surface an error and leave the live holder alone
    // rather than silently falling back to a placeholder.
    let Some(new_p) = catalog::build_provider_for_model(
        config,
        &id,
        config.default_model.as_deref(),
        agent.thread_id().as_deref(),
    ) else {
        tracing::warn!(
            provider_id = %id,
            "set_default_model refused: catalog could not resolve a real provider/channel",
        );
        let _ = resp_tx.send(AgentResponse::Error(format!(
            "No provider configured for '{id}'. \
             Add one with /connections before sending a message."
        )));
        let _ = resp_tx.send(AgentResponse::ProviderPicker(catalog::build_picker_state(
            config,
            provider_usage,
        )));
        return;
    };
    *provider_for_task
        .write()
        .unwrap_or_else(|error| error.into_inner()) = new_p;
    // Re-seed mid-turn relief for the newly activated model's
    // context window.
    reseed_prune_threshold(agent, config);
    // Tool-description overrides track the live model id.
    reseed_tool_variants(agent, config);
    // `resolved_model_name_with_usage` returns `None` only when the entry has
    // no resolvable model — but `build_provider_for_model` above already
    // succeeded, so the entry has at least its default-channel model. Fall
    // back to the empty string defensively; the wire model is what the holder
    // actually carries.
    let model_name =
        catalog::resolved_model_name_with_usage(config, &id, provider_usage).unwrap_or_default();
    provider_usage.record(&id);
    if !model_name.is_empty() {
        provider_usage.record_model(&id, &model_name);
    }
    if let Err(error) = provider_usage.save() {
        tracing::warn!(?error, "could not persist model usage telemetry");
    }
    let _ = resp_tx.send(AgentResponse::ProviderSwitched {
        provider: id.clone(),
        model: model_name.clone(),
    });
    let _ = resp_tx.send(AgentResponse::ProviderKeys(provider_key_status(config)));
    let _ = resp_tx.send(AgentResponse::ProviderPicker(catalog::build_picker_state(
        config,
        provider_usage,
    )));
}

#[cfg(test)]
mod tests {
    use super::*;
    use neenee_persistence::config::{UserChannelConfig, UserProviderConfig};

    #[tokio::test]
    async fn record_provider_ack_appends_durable_ack_to_command_ledger() {
        // ADR-0091: a genuine provider switch keeps a durable `Ack` in the
        // command ledger (the toast is its ephemeral live surface). The record
        // rides under the `models` command word with the selection as args.
        let tmp = tempfile::tempdir().unwrap();
        let session = SessionStore::load_for_project(tmp.path().to_path_buf());
        record_provider_ack(
            &session,
            "111xianyu",
            "k3",
            "Provider switched to 111xianyu (k3)".to_string(),
        )
        .await;

        let commands = session.commands().await;
        assert_eq!(commands.len(), 1);
        let record = &commands[0];
        assert_eq!(record.name, "models");
        assert_eq!(record.args, "111xianyu k3");
        assert_eq!(record.status, neenee_core::CommandStatus::Success);
        match &record.result {
            Some(neenee_core::CommandResult::Ack { title }) => {
                assert_eq!(title, "Provider switched to 111xianyu (k3)");
            }
            other => panic!("expected a durable Ack result, got {other:?}"),
        }
    }

    #[test]
    fn custom_provider_id_slugifies_names() {
        assert_eq!(custom_provider_id("My Relay"), "my-relay");
        assert_eq!(custom_provider_id("  Acme  AI  "), "acme-ai");
        assert_eq!(custom_provider_id("relay.example.com"), "relay-example-com");
        assert_eq!(custom_provider_id("OpenAI!!!"), "openai");
        // Symbol-only / empty names fall back to a usable id.
        assert_eq!(custom_provider_id("***"), "custom");
        assert_eq!(custom_provider_id(""), "custom");
    }

    #[test]
    fn unique_provider_id_suffixes_colliding_instance_names() {
        let mut config = Config::default();
        config.providers.push(UserProviderConfig {
            id: "openai".to_string(),
            ..Default::default()
        });
        assert_eq!(unique_provider_id(&config, "OpenAI"), "openai-2");
        config.providers.push(UserProviderConfig {
            id: "openai-2".to_string(),
            ..Default::default()
        });
        assert_eq!(unique_provider_id(&config, "OpenAI"), "openai-3");
    }

    #[test]
    fn multi_model_provider_covers_builtins_and_multichannel_user_entries() {
        let mut config = Config::default();
        for id in ["openai", "opencode-go", "anthropic", "google", "deepseek"] {
            assert!(is_multi_model_provider(&config, id), "{id} is multi-model");
        }
        // Single-model built-ins are not multi-model.
        assert!(!is_multi_model_provider(&config, "kimi-code"));
        assert!(!is_multi_model_provider(&config, "zai-code"));
        // A user provider counts as multi-model only with >1 channel.
        config.providers.push(UserProviderConfig {
            id: "my-relay".to_string(),
            channels: vec![Default::default(), Default::default()],
            ..Default::default()
        });
        assert!(is_multi_model_provider(&config, "my-relay"));
    }

    #[test]
    fn switch_key_lands_on_instance_channels_and_legacy_field() {
        let mut config = Config::default();
        config.providers.push(UserProviderConfig {
            id: "kimi-code".to_string(),
            channels: vec![
                UserChannelConfig {
                    label: "k3".to_string(),
                    api_key: Some("sk-old".into()),
                    ..Default::default()
                },
                UserChannelConfig {
                    label: "kimi-for-coding".to_string(),
                    api_key: None,
                    ..Default::default()
                },
            ],
            ..Default::default()
        });

        apply_switch_api_key(&mut config, "kimi-code", "sk-new");

        // The catalog builds providers from instance channels, so every
        // channel must carry the new key — otherwise the live provider keeps
        // the old key and the new one is dropped at the next startup.
        let provider = &config.providers[0];
        assert!(
            provider
                .channels
                .iter()
                .all(|c| c.api_key.as_ref().map(SecretString::expose_secret) == Some("sk-new"))
        );
        // The legacy field stays in sync for the credentials fold.
        assert_eq!(
            config
                .moonshot_api_key
                .as_ref()
                .map(SecretString::expose_secret),
            Some("sk-new")
        );
    }

    #[test]
    fn switch_key_skips_oauth_channels() {
        let mut config = Config::default();
        config.providers.push(UserProviderConfig {
            id: "chatgpt-relay".to_string(),
            channels: vec![
                UserChannelConfig {
                    label: "oauth-model".to_string(),
                    auth: neenee_core::ChannelAuth::ChatGptOAuth,
                    api_key: None,
                    ..Default::default()
                },
                UserChannelConfig {
                    label: "key-model".to_string(),
                    api_key: Some("sk-old".into()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });

        apply_switch_api_key(&mut config, "chatgpt-relay", "sk-new");

        let provider = &config.providers[0];
        assert!(
            provider.channels[0].api_key.is_none(),
            "an OAuth channel's bearer is owned by the auth flow"
        );
        assert_eq!(
            provider.channels[1]
                .api_key
                .as_ref()
                .map(SecretString::expose_secret),
            Some("sk-new")
        );
    }
}
