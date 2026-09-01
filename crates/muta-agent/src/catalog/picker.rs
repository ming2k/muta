//! The provider picker snapshot: the presentation-layer view of the catalog
//! (rows, per-model effort/thinking info, auth badges) rendered by the
//! TUI's Connections/Models modals.

use super::derive::derive_entries;
use super::{Stores, effective_default_connection_id};
use muta_contracts::catalog::{Channel, ProviderEntry, Transport};
use muta_contracts::{
    Effort, ProviderModelInfo, ProviderPickerRow, ProviderPickerSnapshot, ThinkingMode,
};
use muta_persistence::config::Config;
use muta_persistence::connection_usage::ConnectionUsage;

/// Whether a model id matches any pattern in `hidden_patterns` (case-insensitive glob or exact match).
pub fn model_is_hidden(model: &str, hidden_patterns: &[String]) -> bool {
    let model_lower = model.to_ascii_lowercase();
    hidden_patterns.iter().any(|pattern| {
        let pat = pattern.trim().to_ascii_lowercase();
        if pat.is_empty() {
            return false;
        }
        if pat.contains('*') || pat.contains('?') {
            glob::Pattern::new(&pat)
                .map(|p| p.matches(&model_lower))
                .unwrap_or(false)
        } else {
            pat == model_lower
        }
    })
}

pub(super) fn active_model_id_for_entry(
    config: &Config,
    entry: &ProviderEntry,
    usage: &ConnectionUsage,
) -> Option<String> {
    config
        .default_model
        .as_deref()
        .filter(|m| entry.offers_model(m))
        .map(|m| m.to_string())
        .or_else(|| {
            usage
                .last_model_for(&entry.id)
                .filter(|m| entry.offers_model(m))
                .filter(|m| !model_is_hidden(m, &config.hidden_models))
                .map(|m| m.to_string())
        })
        .or_else(|| {
            entry
                .channels
                .iter()
                .find(|c| !model_is_hidden(&c.model, &config.hidden_models))
                .map(|channel| channel.model.clone())
        })
        .or_else(|| entry.default_channel().map(|channel| channel.model.clone()))
}

pub fn build_picker_state(config: &Config, usage: &ConnectionUsage) -> ProviderPickerSnapshot {
    let stores = Stores::load();
    let entries = derive_entries(
        &stores.connections,
        &stores.cache,
        &stores.routes,
        &stores.creds,
    );
    let default_id = effective_default_connection_id(config, &stores);
    let rows = entries
        .iter()
        .map(|entry| {
            let (protocol, base_url) = entry
                .default_channel()
                .map(channel_protocol_and_base_url)
                .unwrap_or_default();
            let model = active_model_id_for_entry(config, entry, usage).unwrap_or_default();
            let visible_channels: Vec<_> = entry
                .channels
                .iter()
                .filter(|c| !model_is_hidden(&c.model, &config.hidden_models))
                .collect();
            let channels_to_show = if visible_channels.is_empty() {
                entry.channels.iter().collect::<Vec<_>>()
            } else {
                visible_channels
            };
            let model_info = channels_to_show
                .iter()
                .copied()
                .map(channel_model_info)
                .map(|mut info| {
                    // Favorite is model-level (ADR-0046): a starred
                    // daily-driver model carries its flag into the flat
                    // Models picker wherever it is served.
                    info.favorite = config.favorites.iter().any(|fav| fav == &info.model);
                    // Recency is model-level too (stage-2 usage telemetry):
                    // the flat Models picker's "recent" section is ordered by
                    // it. 0 (never activated) surfaces as `None`.
                    let recency = usage.model_recency(&info.model);
                    info.last_used_ms = (recency > 0).then_some(recency);
                    info
                })
                .collect();
            let connection = stores.connections.get(&entry.id);
            let preset_id = connection
                .and_then(|p| p.preset_id.clone())
                .unwrap_or_default();
            let client_identity = connection
                .map(|p| p.client_identity.clone())
                .unwrap_or_default();
            let auth = connection.map(|p| p.auth).unwrap_or_default();
            let recency = usage.recency_of(&entry.id);
            ProviderPickerRow {
                id: entry.id.clone(),
                name: entry.name.clone(),
                model,
                models: channels_to_show.iter().map(|c| c.model.clone()).collect(),
                model_info,
                builtin: entry.builtin,
                protocol,
                base_url,
                key_ready: entry.key_ready(),
                preset_id,
                client_identity,
                last_used_ms: if recency == 0 { None } else { Some(recency) },
                auth,
            }
        })
        .collect();
    ProviderPickerSnapshot { default_id, rows }
}

/// Prune model ids and connection entries from `config` (favorites, default_model)
/// and `usage` (recency, last_models) that are no longer served by any known connection.
pub fn prune_stale_models(config: &mut Config, usage: &mut ConnectionUsage) -> bool {
    let stores = Stores::load();
    let valid_connection_ids: std::collections::HashSet<String> = stores
        .connections
        .connections
        .iter()
        .map(|c| c.id.clone())
        .collect();

    let mut connection_models_map: std::collections::HashMap<
        String,
        std::collections::HashSet<String>,
    > = std::collections::HashMap::new();
    let mut all_valid_models: std::collections::HashSet<String> = std::collections::HashSet::new();

    for conn in &stores.connections.connections {
        let models = super::derive::route_models(conn, &stores.cache);
        for m in &models {
            all_valid_models.insert(m.clone());
        }
        connection_models_map.insert(conn.id.clone(), models.into_iter().collect());
    }

    let mut changed = false;

    // Prune favorites: only retain models currently offered by at least one connection.
    let prev_fav_len = config.favorites.len();
    config
        .favorites
        .retain(|fav| all_valid_models.contains(fav));
    if config.favorites.len() != prev_fav_len {
        changed = true;
        if let Err(error) = config.save_preserving_connection_selection() {
            tracing::warn!(?error, "could not persist pruned favorites");
        }
    }

    // Prune default_model if it is set to a model no longer offered by the default connection (or any connection).
    if let Some(ref dm) = config.default_model {
        let valid_for_default = connection_models_map
            .get(&config.default_connection)
            .map(|set| set.contains(dm))
            .unwrap_or(false);
        if !valid_for_default && !all_valid_models.contains(dm) {
            config.default_model = None;
            changed = true;
            if let Err(error) = config.save_preserving_connection_selection() {
                tracing::warn!(?error, "could not persist pruned default model");
            }
        }
    }

    // Prune connection usage telemetry.
    let usage_changed = usage.prune(
        |conn_id| valid_connection_ids.contains(conn_id),
        |model_id| all_valid_models.contains(model_id),
        |conn_id, model_id| {
            connection_models_map
                .get(conn_id)
                .is_some_and(|set| set.contains(model_id))
        },
    );
    if usage_changed {
        changed = true;
        if let Err(error) = usage.save_exact() {
            tracing::warn!(?error, "could not persist pruned connection usage");
        }
    }

    changed
}

/// Load on-disk config and usage, prune stale models, and persist if changed.
pub fn prune_stale_models_on_disk() -> bool {
    let mut config = Config::load();
    let mut usage = ConnectionUsage::load();
    prune_stale_models(&mut config, &mut usage)
}

pub(super) fn channel_protocol_and_base_url(channel: &Channel) -> (String, String) {
    match &channel.transport {
        Transport::OpenAi { base_url, .. } => (
            muta_contracts::WireProtocol::OpenAiChatCompletions
                .as_str()
                .to_string(),
            base_url.clone(),
        ),
        Transport::OpenAiResponses { base_url, .. } => (
            muta_contracts::WireProtocol::OpenAiResponses
                .as_str()
                .to_string(),
            base_url.clone(),
        ),
        Transport::Anthropic { base_url, .. } => (
            muta_contracts::WireProtocol::AnthropicMessages
                .as_str()
                .to_string(),
            base_url.clone(),
        ),
        Transport::Google { base_url, .. } => (
            muta_contracts::WireProtocol::GoogleGenerateContent
                .as_str()
                .to_string(),
            base_url.clone(),
        ),
    }
}

pub fn channel_model_info(channel: &Channel) -> ProviderModelInfo {
    match &channel.transport {
        Transport::Anthropic {
            effort, thinking, ..
        } => {
            // ADR-0046: reasoning is opt-in. A channel's effective thinking
            // state is off unless it has an explicit on override. The info
            // surfaces both knobs so the picker can show a model's effort only
            // when it is actually opted in to reasoning (thinking on).
            let thinking_on = matches!(thinking, Some(ThinkingMode::Adaptive));
            ProviderModelInfo {
                model: channel.model.clone(),
                protocol: muta_contracts::WireProtocol::AnthropicMessages
                    .as_str()
                    .to_string(),
                effort: Some((*effort).unwrap_or(Effort::High).as_str().to_string()),
                thinking: Some(thinking_on),
                favorite: false,
                last_used_ms: None,
            }
        }
        Transport::OpenAi { effort, .. } => {
            let model = muta_contracts::model::resolve(&channel.model);
            // Fallback when the channel has no explicit effort override is
            // `Effort::channel_default` — the SAME rule the provider factory
            // stamps onto the wire, so the picker can never promise a tier
            // the request does not send.
            let effective = Effort::channel_default(model.family, model.effort_levels)
                .map(|default| (*effort).unwrap_or(default).as_str().to_string());
            ProviderModelInfo {
                model: channel.model.clone(),
                protocol: muta_contracts::WireProtocol::OpenAiChatCompletions
                    .as_str()
                    .to_string(),
                effort: effective,
                thinking: None,
                favorite: false,
                last_used_ms: None,
            }
        }
        Transport::OpenAiResponses { effort, .. } => {
            let model = muta_contracts::model::resolve(&channel.model);
            // Same shared default rule as the chat-completions arm.
            let effective = Effort::channel_default(model.family, model.effort_levels)
                .map(|default| (*effort).unwrap_or(default).as_str().to_string());
            ProviderModelInfo {
                model: channel.model.clone(),
                protocol: muta_contracts::WireProtocol::OpenAiResponses
                    .as_str()
                    .to_string(),
                effort: effective,
                thinking: None,
                favorite: false,
                last_used_ms: None,
            }
        }
        Transport::Google { effort, .. } => {
            // Same contract as the OpenAI arms: a model that advertises an
            // effort ladder is configurable; one with an empty ladder (a
            // non-reasoning Gemini, or an id no baseline knows) stays inert.
            // The channel's explicit override wins; otherwise the shared
            // `Effort::channel_default` rule (`high` clamped to the ladder —
            // Gemini is never a `gpt` family) applies.
            let model = muta_contracts::model::resolve(&channel.model);
            let effective = Effort::channel_default(model.family, model.effort_levels)
                .map(|default| (*effort).unwrap_or(default).as_str().to_string());
            ProviderModelInfo {
                model: channel.model.clone(),
                protocol: muta_contracts::WireProtocol::GoogleGenerateContent
                    .as_str()
                    .to_string(),
                effort: effective,
                thinking: None,
                favorite: false,
                last_used_ms: None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_is_hidden_matches_exact_and_glob_case_insensitively() {
        let hidden = vec![
            "gemini-3.6-flash*".to_string(),
            "chat_*".to_string(),
            "deprecated-model".to_string(),
        ];

        assert!(model_is_hidden("gemini-3.6-flash-high", &hidden));
        assert!(model_is_hidden("GEMINI-3.6-FLASH-LOW", &hidden));
        assert!(model_is_hidden("gemini-3.6-flash", &hidden));
        assert!(model_is_hidden("chat_20706", &hidden));
        assert!(model_is_hidden("deprecated-model", &hidden));

        assert!(!model_is_hidden("gemini-3.7-flash", &hidden));
        assert!(!model_is_hidden("gemini-3.7-flash-tiered", &hidden));
        assert!(!model_is_hidden("gemini-pro-agent", &hidden));
        assert!(!model_is_hidden("claude-sonnet-4-6", &hidden));
    }
}
