//! The provider picker snapshot: the presentation-layer view of the catalog
//! (rows, per-model effort/thinking info, auth badges) rendered by the
//! TUI's Connections/Models modals.

use super::derive::derive_entries;
use super::{Stores, effective_default_provider_id};
use neenee_contracts::catalog::{Channel, ProviderEntry, Transport};
use neenee_contracts::{
    Effort, ProviderModelInfo, ProviderPickerRow, ProviderPickerSnapshot, ThinkingMode,
};
use neenee_persistence::config::Config;
use neenee_persistence::provider_usage::ProviderUsage;

pub(super) fn active_model_id_for_entry(
    config: &Config,
    entry: &ProviderEntry,
    usage: &ProviderUsage,
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
                .map(|m| m.to_string())
        })
        .or_else(|| entry.default_channel().map(|channel| channel.model.clone()))
}

pub fn build_picker_state(config: &Config, usage: &ProviderUsage) -> ProviderPickerSnapshot {
    let stores = Stores::load();
    let entries = derive_entries(
        &stores.instances,
        &stores.cache,
        &stores.routes,
        &stores.creds,
    );
    let default_id = effective_default_provider_id(config, &stores);
    let rows = entries
        .iter()
        .map(|entry| {
            let (protocol, base_url) = entry
                .default_channel()
                .map(channel_protocol_and_base_url)
                .unwrap_or_default();
            let model = active_model_id_for_entry(config, entry, usage).unwrap_or_default();
            let model_info = entry
                .channels
                .iter()
                .map(channel_model_info)
                .map(|mut info| {
                    // Favorite is model-level (ADR-0046): a starred
                    // daily-driver model carries its flag into the flat
                    // Models picker wherever it is served.
                    info.favorite = config.favorites.iter().any(|fav| fav == &info.model);
                    info
                })
                .collect();
            // The template that birthed this instance drives the
            // Connections list's provider-type label (distinct from the
            // user-given instance name).
            let template_id = stores
                .instances
                .get(&entry.id)
                .and_then(|p| p.template_id.clone())
                .unwrap_or_default();
            ProviderPickerRow {
                id: entry.id.clone(),
                name: entry.name.clone(),
                model,
                models: entry.channels.iter().map(|c| c.model.clone()).collect(),
                model_info,
                builtin: entry.builtin,
                protocol,
                base_url,
                key_ready: entry.key_ready(),
                template_id,
                last_used_ms: usage.last_used_ms(&entry.id),
                auth: stores
                    .instances
                    .get(&entry.id)
                    .map(|p| p.auth)
                    .unwrap_or_default(),
            }
        })
        .collect();
    ProviderPickerSnapshot { default_id, rows }
}

pub(super) fn channel_protocol_and_base_url(channel: &Channel) -> (String, String) {
    match &channel.transport {
        Transport::OpenAi { base_url, .. } => ("openai".to_string(), base_url.clone()),
        Transport::OpenAiResponses {
            base_url, copilot, ..
        } => {
            let protocol = if *copilot {
                "openai"
            } else {
                "openai-responses"
            };
            (protocol.to_string(), base_url.clone())
        }
        Transport::Anthropic { base_url, .. } => ("anthropic".to_string(), base_url.clone()),
        Transport::Google { base_url, .. } => ("google".to_string(), base_url.clone()),
    }
}

pub(super) fn channel_model_info(channel: &Channel) -> ProviderModelInfo {
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
                protocol: "anthropic".to_string(),
                effort: Some((*effort).unwrap_or(Effort::High).as_str().to_string()),
                thinking: Some(thinking_on),
                favorite: false,
            }
        }
        Transport::OpenAi { effort, .. } => {
            let model = neenee_contracts::model::resolve(&channel.model);
            let effective = if model.effort_levels.is_empty() {
                None
            } else {
                // Fallback when the channel has no explicit effort override:
                // GPT defaults to `medium` (its wire middle tier); every other
                // model defaults to `high` clamped to its ladder — a ladder
                // that tops out below `high` (e.g. EFFORT_COMMON relays)
                // resolves to its deepest tier, and a ladder that omits
                // `high`/`medium` (Kimi K3's `low`/`high`/`max`, or a single
                // fixed rung) snaps up to the nearest supported tier, since
                // the platform pins its own default. Never emits a tier the
                // model does not support.
                let default = if model.family == "gpt" {
                    Effort::Medium
                } else {
                    Effort::High.clamp_to(model.effort_levels)
                };
                Some((*effort).unwrap_or(default).as_str().to_string())
            };
            ProviderModelInfo {
                model: channel.model.clone(),
                protocol: "openai".to_string(),
                effort: effective,
                thinking: None,
                favorite: false,
            }
        }
        Transport::OpenAiResponses { effort, .. } => {
            let model = neenee_contracts::model::resolve(&channel.model);
            let effective = if model.effort_levels.is_empty() {
                None
            } else {
                // Same default rule as the chat-completions arm: GPT defaults
                // to `medium`; every other Responses-speaking model (DeepSeek
                // V4) defaults to `high` clamped to its ladder.
                let default = if model.family == "gpt" {
                    Effort::Medium
                } else {
                    Effort::High.clamp_to(model.effort_levels)
                };
                Some((*effort).unwrap_or(default).as_str().to_string())
            };
            ProviderModelInfo {
                model: channel.model.clone(),
                protocol: "openai".to_string(),
                effort: effective,
                thinking: None,
                favorite: false,
            }
        }
        Transport::Google { effort, .. } => {
            // Same contract as the OpenAI arms: a model that advertises an
            // effort ladder is configurable; one with an empty ladder (a
            // non-reasoning Gemini, or an id no baseline knows) stays inert.
            // The channel's explicit override wins; otherwise Gemini 3.x
            // (`thinkingLevel`) defaults to `high` clamped to its ladder and
            // Gemini 2.5 (`thinkingBudget`) resolves its own default bucket.
            let model = neenee_contracts::model::resolve(&channel.model);
            let effective = if model.effort_levels.is_empty() {
                None
            } else {
                let default = Effort::High.clamp_to(model.effort_levels);
                Some((*effort).unwrap_or(default).as_str().to_string())
            };
            ProviderModelInfo {
                model: channel.model.clone(),
                protocol: "google".to_string(),
                effort: effective,
                thinking: None,
                favorite: false,
            }
        }
    }
}
