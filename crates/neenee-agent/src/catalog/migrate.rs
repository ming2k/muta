//! One-shot config migrations: legacy single-field built-ins into named
//! provider instances, the DeepSeek chat-completions → Responses
//! re-routing, and the legacy per-provider instance shape. Each migration
//! reports whether it changed anything so the caller persists only on a
//! real change.

use super::discovery::default_model_source_for_spec;
use neenee_contracts::{SecretString, WireFormat};
use neenee_persistence::config::{Config, UserChannelConfig, UserProviderConfig, UserTransport};
use neenee_providers::{
    ANTHROPIC_BUILTIN_MODELS, DEEPSEEK_BUILTIN_MODELS, GOOGLE_BUILTIN_MODELS, KIMI_CODE_MODELS,
    OPENAI_BUILTIN_MODELS, OPENCODE_GO_SERVED_MODELS, OPENCODE_USER_AGENT, ZAI_CODE_MODELS,
    ZCODE_USER_AGENT, provider_template_spec,
};

pub fn migrate_legacy_provider_instances(config: &mut Config) -> bool {
    let mut changed = false;
    let legacy_default = config.default_provider.clone();
    let legacy_model = config.default_model.clone();
    let openai_key = config.openai_api_key.take();
    let google_key = config.google_api_key.take();
    let google_base_url = config
        .google_base_url
        .as_deref()
        .unwrap_or("https://generativelanguage.googleapis.com/v1beta")
        .to_string();
    let kimi_key = config.moonshot_api_key.take();
    let deepseek_key = config.deepseek_api_key.take();
    let zai_key = config.zai_api_key.take();
    let anthropic_key = config.anthropic_api_key.take();
    let anthropic_base_url = config
        .anthropic_base_url
        .as_deref()
        .unwrap_or("https://api.anthropic.com/v1/messages")
        .to_string();
    let legacy_key_present = [
        openai_key.as_ref(),
        google_key.as_ref(),
        kimi_key.as_ref(),
        deepseek_key.as_ref(),
        zai_key.as_ref(),
        anthropic_key.as_ref(),
        config.opencode_go_api_key.as_ref(),
    ]
    .into_iter()
    .flatten()
    .any(|key| !key.expose_secret().trim().is_empty());

    changed |= migrate_legacy_instance(
        config,
        "openai",
        "OpenAI",
        UserTransport::OpenAi,
        "https://api.openai.com/v1/chat/completions",
        None,
        OPENAI_BUILTIN_MODELS,
        openai_key,
        legacy_model.as_deref(),
        Some("openai"),
    );
    changed |= migrate_legacy_instance(
        config,
        "google",
        "Google",
        UserTransport::Google,
        &google_base_url,
        None,
        GOOGLE_BUILTIN_MODELS,
        google_key,
        legacy_model.as_deref(),
        Some("google"),
    );
    changed |= migrate_legacy_instance(
        config,
        "kimi-code",
        "Kimi Code",
        UserTransport::OpenAi,
        "https://api.kimi.com/coding/v1/chat/completions",
        Some(OPENCODE_USER_AGENT),
        KIMI_CODE_MODELS,
        kimi_key,
        legacy_model.as_deref(),
        Some("kimi-code"),
    );
    changed |= migrate_legacy_instance(
        config,
        "deepseek",
        "DeepSeek",
        UserTransport::OpenAiResponses,
        DEEPSEEK_RESPONSES_URL,
        None,
        DEEPSEEK_BUILTIN_MODELS,
        deepseek_key,
        legacy_model.as_deref(),
        Some("deepseek"),
    );
    changed |= migrate_legacy_instance(
        config,
        "zai-code",
        "ZAI Code (CN)",
        UserTransport::OpenAi,
        "https://open.bigmodel.cn/api/coding/paas/v4/chat/completions",
        Some(ZCODE_USER_AGENT),
        ZAI_CODE_MODELS,
        zai_key,
        legacy_model.as_deref(),
        Some("zai-code"),
    );
    changed |= migrate_legacy_instance(
        config,
        "anthropic",
        "Anthropic",
        UserTransport::Anthropic,
        &anthropic_base_url,
        None,
        ANTHROPIC_BUILTIN_MODELS,
        anthropic_key,
        legacy_model.as_deref(),
        Some("anthropic"),
    );

    if let Some(key) = config
        .opencode_go_api_key
        .take()
        .filter(|k| !k.expose_secret().trim().is_empty())
        && !config.providers.iter().any(|p| p.id == "opencode-go")
    {
        let channels = opencode_go_seed_channels(key);
        if !channels.is_empty() {
            let default_channel = legacy_model
                .as_deref()
                .and_then(|model| {
                    channels
                        .iter()
                        .position(|channel| channel.model.as_deref() == Some(model))
                })
                .unwrap_or(0);
            config.providers.push(UserProviderConfig {
                id: "opencode-go".to_string(),
                name: Some("OpenCode Go".to_string()),
                channels,
                default_channel,
                // The OpenCode Go seed mirrors the relay's served catalogue
                // (OPENCODE_GO_SERVED_MODELS), so it is NOT a 1:1 mirror of
                // the `opencode-go` template spec (which uses a fixed list).
                // Leave it untracked so a template edit does not clobber the
                // curated served set.
                template_id: None,
                // A pure-custom (untracked) instance; model_source is ignored,
                // so the Fixed default is harmless.
                model_source: Default::default(),
                fitted_models: Default::default(),
            });
            changed = true;
        }
    }

    // Strip the legacy per-provider fields the migration above consumes.
    // `default_model` is NOT legacy: the switch handler persists it as the
    // global model pointer and the runtime honors it as the active model when
    // the default provider serves it — taking it would erase the persisted
    // selection on every startup.
    if config.openai_model.take().is_some()
        | config.moonshot_model.take().is_some()
        | config.zai_model.take().is_some()
        | config.google_base_url.take().is_some()
        | config.anthropic_base_url.take().is_some()
        | config.anthropic_effort.take().is_some()
        | config.anthropic_thinking.take().is_some()
    {
        changed = true;
    }
    if legacy_key_present {
        changed = true;
    }

    if !config
        .providers
        .iter()
        .any(|provider| provider.id == config.default_provider)
    {
        config.default_provider = if config.providers.iter().any(|p| p.id == legacy_default) {
            legacy_default
        } else {
            config
                .providers
                .first()
                .map(|p| p.id.clone())
                .unwrap_or_default()
        };
        changed = true;
    }

    changed
}

/// The official DeepSeek endpoints: the Responses URL the template seeds
/// today, and the legacy chat-completions URL existing instances may still
/// carry from before the Responses migration.
pub const DEEPSEEK_RESPONSES_URL: &str = "https://api.deepseek.com/v1/responses";

const DEEPSEEK_CHAT_COMPLETIONS_URL: &str = "https://api.deepseek.com/v1/chat/completions";

pub fn migrate_deepseek_channels_to_responses(config: &mut Config) -> bool {
    let mut changed = false;
    for provider in &mut config.providers {
        let is_deepseek_template = provider.template_id.as_deref() == Some("deepseek");
        for channel in &mut provider.channels {
            if channel.auth.is_oauth() {
                continue;
            }
            let on_official_endpoint = match channel.base_url.as_deref() {
                Some(url) => {
                    let url = url.trim().trim_end_matches('/');
                    url == DEEPSEEK_CHAT_COMPLETIONS_URL
                        || url == "https://api.deepseek.com/chat/completions"
                }
                // An unset URL on a deepseek-template channel resolves to the
                // official endpoint at build time — migrate it too.
                None => is_deepseek_template,
            };
            if !on_official_endpoint {
                continue;
            }
            if channel.transport != UserTransport::OpenAiResponses {
                channel.transport = UserTransport::OpenAiResponses;
                changed = true;
            }
            if channel.base_url.as_deref() != Some(DEEPSEEK_RESPONSES_URL) {
                channel.base_url = Some(DEEPSEEK_RESPONSES_URL.to_string());
                changed = true;
            }
        }
    }
    changed
}

pub(super) fn matching_template(
    provider: &UserProviderConfig,
) -> Option<&'static neenee_providers::ProviderTemplateSpec> {
    let current = provider.channel_models();
    neenee_providers::PROVIDER_TEMPLATE_SPECS
        .iter()
        .find(|spec| spec.models == current.as_slice())
}

pub(super) fn transport_for_protocol(protocol: &str) -> UserTransport {
    match protocol {
        "anthropic" => UserTransport::Anthropic,
        "google" | "gemini" => UserTransport::Google,
        "openai-responses" => UserTransport::OpenAiResponses,
        _ => UserTransport::OpenAi,
    }
}

pub(super) fn migrate_legacy_instance(
    config: &mut Config,
    id: &str,
    name: &str,
    transport: UserTransport,
    base_url: &str,
    user_agent: Option<&str>,
    models: &[&str],
    api_key: Option<SecretString>,
    active_model: Option<&str>,
    template_id: Option<&str>,
) -> bool {
    let Some(api_key) = api_key.filter(|k| !k.expose_secret().trim().is_empty()) else {
        return false;
    };
    if config.providers.iter().any(|p| p.id == id) {
        return false;
    }
    let channels: Vec<UserChannelConfig> = models
        .iter()
        .map(|model| UserChannelConfig {
            label: (*model).to_string(),
            transport,
            api_key_env: None,
            api_key: Some(api_key.clone()),
            model: Some((*model).to_string()),
            base_url: Some(base_url.to_string()),
            user_agent: user_agent.map(str::to_string),
            effort: None,
            thinking: None,
            auth: Default::default(),
            remote: None,
        })
        .collect();
    let default_channel = active_model
        .and_then(|model| {
            channels
                .iter()
                .position(|channel| channel.model.as_deref() == Some(model))
        })
        .unwrap_or(0);
    // Stamp the template id so the migrated instance starts tracking the
    // template's model list (a legacy key migration seeds exactly the template
    // models, so the instance is a mirror of it). Only a known id is recorded.
    let template_id = template_id
        .filter(|tid| provider_template_spec(tid).is_some())
        .map(str::to_string);
    // A migrated template-sourced instance adopts the template's default model
    // source (Api where the template supports discovery, Fixed otherwise).
    let model_source = template_id
        .as_deref()
        .and_then(provider_template_spec)
        .map(default_model_source_for_spec)
        .unwrap_or_default();
    config.providers.push(UserProviderConfig {
        id: id.to_string(),
        name: Some(name.to_string()),
        channels,
        default_channel,
        template_id,
        model_source,
        fitted_models: Default::default(),
    });
    true
}

pub(super) fn opencode_go_seed_channels(api_key: SecretString) -> Vec<UserChannelConfig> {
    let Some(spec) = provider_template_spec("opencode-go") else {
        return Vec::new();
    };
    let mut models: Vec<_> = spec
        .baselines
        .iter()
        .filter(|m| OPENCODE_GO_SERVED_MODELS.contains(&m.id))
        .collect();
    models.sort_by(|a, b| a.id.cmp(b.id));
    models
        .into_iter()
        .map(|model| {
            let (transport, base_url) = match model.format {
                WireFormat::AnthropicCompat => (
                    UserTransport::Anthropic,
                    "https://opencode.ai/zen/go/v1/messages",
                ),
                WireFormat::Google => (UserTransport::Google, "https://opencode.ai/zen/go/v1beta"),
                WireFormat::OpenAi => (
                    UserTransport::OpenAi,
                    "https://opencode.ai/zen/go/v1/chat/completions",
                ),
            };
            UserChannelConfig {
                label: model.id.to_string(),
                transport,
                api_key_env: None,
                api_key: Some(api_key.clone()),
                model: Some(model.id.to_string()),
                base_url: Some(base_url.to_string()),
                auth: Default::default(),
                user_agent: None,
                effort: None,
                thinking: None,
                remote: None,
            }
        })
        .collect()
}
