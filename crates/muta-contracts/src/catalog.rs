//! Provider/channel catalog: the two-layer abstraction over LLM backends.
//!
//! A [`ProviderEntry`] is a configured provider preset (e.g. `zai-code`,
//! `kimi-code`) that owns one or more [`Channel`]s — delivery paths
//! distinguished by transport and endpoint. Each channel references a model by
//! its wire id; intrinsic model metadata (context window, capabilities) is
//! resolved from the [`crate::model`] registry, not duplicated per provider.
//!
//! This module owns the *types* and the provider *construction* path. It is
//! deliberately decoupled from any specific config struct: a [`Channel`] already
//! carries resolved credentials and the wire model id, so constructing a
//! provider from it (see `build_provider_for_channel` in `muta-providers`)
//! is a pure operation. Resolution (environment variable then config field)
//! lives in the loader, not here, so the same types serve both built-in
//! presets and future user-defined entries.
//!
//! See `docs/adr/0002-model-channel-abstraction.md` for the design.

use std::fmt;

/// Provider-specific behavior layered on the OpenAI Chat Completions protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OpenAiChatDialect {
    #[default]
    Standard,
    Copilot,
}

/// Provider-specific behavior layered on the OpenAI Responses protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OpenAiResponsesDialect {
    #[default]
    Standard,
    ChatGpt,
    Copilot,
}

/// Provider-specific behavior layered on the Anthropic Messages protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnthropicMessagesDialect {
    #[default]
    Standard,
    Copilot,
}

/// Provider-specific behavior layered on Google's generateContent protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GoogleGenerateContentDialect {
    #[default]
    GenerativeLanguage,
    Antigravity,
}

/// How a [`Channel`] speaks to its model. Determines which `Provider`
/// implementation is constructed for it (in `muta-providers`).
///
/// Variants carry only the endpoint shape intrinsic to the transport.
/// Per-call credentials and the wire model id live on the [`Channel`] itself,
/// so the same transport serves a built-in preset and a user-defined relay.
#[derive(Debug, Clone)]
pub enum Transport {
    /// OpenAI-compatible chat-completions endpoint at `base_url`. The
    /// `client_profile` defines the User-Agent and client identity headers sent.
    /// `effort`, when set, becomes the OpenAI `reasoning_effort` field.
    OpenAi {
        base_url: String,
        client_profile: crate::ClientProfile,
        effort: Option<crate::Effort>,
        dialect: OpenAiChatDialect,
    },
    /// Anthropic-compatible `/messages` endpoint at `base_url` (the full URL).
    /// Auth uses the `x-api-key` header plus `anthropic-version`.
    Anthropic {
        base_url: String,
        client_profile: crate::ClientProfile,
        effort: Option<crate::Effort>,
        thinking: Option<crate::ThinkingMode>,
        dialect: AnthropicMessagesDialect,
    },
    /// Google native API (`generativelanguage.googleapis.com` or Antigravity).
    Google {
        base_url: String,
        client_profile: crate::ClientProfile,
        effort: Option<crate::Effort>,
        dialect: GoogleGenerateContentDialect,
    },
    /// OpenAI **Responses** API (`/responses` endpoint), used by the ChatGPT
    /// subscription backend and GitHub Copilot Responses backend.
    OpenAiResponses {
        base_url: String,
        client_profile: crate::ClientProfile,
        effort: Option<crate::Effort>,
        dialect: OpenAiResponsesDialect,
    },
}

impl Transport {
    /// The wire-protocol name for this transport.
    pub fn protocol_label(&self) -> &'static str {
        match self {
            Transport::OpenAi { .. } => "openai",
            Transport::Anthropic { .. } => "anthropic",
            Transport::Google { .. } => "google",
            Transport::OpenAiResponses { .. } => "openai-responses",
        }
    }

    /// The base URL endpoint for this transport.
    pub fn base_url(&self) -> &str {
        match self {
            Transport::OpenAi { base_url, .. }
            | Transport::Anthropic { base_url, .. }
            | Transport::Google { base_url, .. }
            | Transport::OpenAiResponses { base_url, .. } => base_url,
        }
    }

    /// The resolved client profile for this transport.
    pub fn client_profile(&self) -> &crate::ClientProfile {
        match self {
            Transport::OpenAi { client_profile, .. }
            | Transport::Anthropic { client_profile, .. }
            | Transport::Google { client_profile, .. }
            | Transport::OpenAiResponses { client_profile, .. } => client_profile,
        }
    }

    /// The User-Agent header string for this transport.
    pub fn user_agent(&self) -> &str {
        self.client_profile().user_agent()
    }

    /// Whether this transport needs an API key at all.
    pub fn needs_api_key(&self) -> bool {
        match self {
            Transport::OpenAi { .. }
            | Transport::Anthropic { .. }
            | Transport::Google { .. }
            | Transport::OpenAiResponses { .. } => true,
        }
    }
}

/// One delivery path for a [`ProviderEntry`].
///
/// A channel pairs a [`Transport`] with a [`crate::CredentialSource`] and
/// the wire `model` id. Built-in presets materialize exactly one channel per
/// entry (id `"default"`); user-defined entries may declare several channels
/// per model (e.g. Gemini via Studio, Vertex, or a relay), with the entry's
/// `default_channel` selecting one. See ADR-0002.
#[derive(Clone)]
pub struct Channel {
    /// Stable identifier within the model (e.g. `"studio"`, `"vertex"`).
    /// Built-in presets use `"default"`.
    pub id: String,
    /// Display label shown in the picker (e.g. `"Google Studio"`).
    pub label: String,
    /// Endpoint shape and provider implementation selector.
    pub transport: Transport,
    /// The dynamic or static credential source for this channel.
    pub credentials: std::sync::Arc<dyn crate::CredentialSource>,
    /// Resolved wire model id sent to the provider.
    pub model: String,
    /// Provider-scoped live capability metadata. A trusted provider's remote
    /// catalogue owns the fields it explicitly supplies; the static model
    /// registry remains the fallback for omitted or offline data.
    pub remote: Option<crate::RemoteModelMetadata>,
    /// The user's explicit capability overrides for this route — the top
    /// layer of the capability resolution order (ADR-0149), applied after
    /// the remote overlay in [`Channel::capabilities`]. `None` means the
    /// user has no opinion and the two lower layers decide.
    pub user_overrides: Option<crate::model::CapabilityOverrides>,
    /// Prompt-cache controls and telemetry declared by this concrete route.
    /// This is intentionally independent of model-family capabilities: a
    /// relay speaking the same wire protocol may expose none of them.
    pub prompt_cache: crate::PromptCacheCapabilities,
    /// User-selected cache intent for this route.
    pub prompt_cache_preference: crate::PromptCachePreference,
}

impl fmt::Debug for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Channel")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("transport", &self.transport)
            .field("credentials", &self.credentials)
            .field("model", &self.model)
            .field("remote", &self.remote)
            .field("user_overrides", &self.user_overrides)
            .field("prompt_cache", &self.prompt_cache)
            .field("prompt_cache_preference", &self.prompt_cache_preference)
            .finish()
    }
}

impl Channel {
    /// Return the active credential source for this channel.
    pub fn credentials_source(&self) -> std::sync::Arc<dyn crate::CredentialSource> {
        self.credentials.clone()
    }

    /// Whether this channel has a usable API key or valid credential.
    pub fn key_ready(&self) -> bool {
        if !self.transport.needs_api_key() {
            return true;
        }
        self.credentials.is_ready()
    }

    /// Resolve effective capabilities for this delivery path. The provider's
    /// remote snapshot overlays only its explicit fields onto the static model
    /// baseline, preventing a global model id from overwriting account-specific
    /// routes or capabilities.
    pub fn capabilities(&self) -> crate::ModelCapabilities {
        let merged = crate::ModelCapabilities::for_channel(&self.model, self.remote.as_ref());
        match &self.user_overrides {
            Some(user) => merged.apply_overrides(user),
            None => merged,
        }
    }
}

/// A catalog entry: a provider preset with one or more channels. Each channel
/// references a model by wire id; model metadata (context window, capabilities)
/// is resolved from the [`crate::model`] registry.
#[derive(Debug, Clone)]
pub struct ProviderEntry {
    /// Canonical stable identifier — the provider/preset id
    /// (`"zai-code"`, `"kimi-code"`, ...).
    pub id: String,
    /// Display name (e.g. `"ZAI Code"`).
    pub name: String,
    /// Short human-readable description.
    pub description: String,
    /// Delivery paths for this provider. Phase 1: exactly one per entry.
    pub channels: Vec<Channel>,
    /// Index into `channels` of the preferred path.
    pub default_channel: usize,
    /// `true` for the built-in presets; `false` for user-defined entries.
    pub builtin: bool,
}

impl ProviderEntry {
    /// The preferred channel, or `None` if the entry has no channels.
    pub fn default_channel(&self) -> Option<&Channel> {
        self.channels.get(self.default_channel)
    }

    /// Whether the entry has a usable API key on its default channel. Built-in
    /// keyless entries (local server) always report ready.
    pub fn key_ready(&self) -> bool {
        self.default_channel()
            .map(Channel::key_ready)
            .unwrap_or(true)
    }

    /// The context window (in tokens) of the model on the default channel,
    /// resolved from the model registry. Returns `0` when the entry has no
    /// default channel or the model is not in the registry.
    pub fn context_window(&self) -> usize {
        self.default_channel()
            .map(|ch| ch.capabilities().context_window)
            .unwrap_or(0)
    }

    /// The channel carrying `model_id`, if any. A multi-model provider (e.g.
    /// `opencode-go`) exposes one channel per model — each with the transport
    /// matching that model's [`crate::model::WireProtocol`] — so selecting a
    /// model is selecting a channel. Returns `None` when the entry does not
    /// serve that model id.
    pub fn channel_for_model(&self, model_id: &str) -> Option<&Channel> {
        self.channels.iter().find(|ch| ch.model == model_id)
    }

    /// Whether this entry serves `model_id` on any of its channels.
    pub fn offers_model(&self, model_id: &str) -> bool {
        self.channel_for_model(model_id).is_some()
    }
}

/// Display metadata for a built-in provider preset. Returns `(name,
/// description)`. Model-level metadata (context window, capabilities) lives in
/// the [`crate::model`] registry and is resolved separately. Returns `None` for
/// ids with no built-in metadata; the loader falls back to the raw id as the
/// name in that case.
pub fn builtin_provider_metadata(id: &str) -> Option<(&'static str, &'static str)> {
    let (name, description) = match id {
        "kimi-code" => ("Kimi Code", "Moonshot AI coding model"),
        "openai" => ("OpenAI", "OpenAI API"),
        // Google hosts the Gemini family as one multi-model provider.
        "google" => ("Google", "Google"),
        // DeepSeek hosts V4 Flash + Pro as one multi-model provider.
        "deepseek" => ("DeepSeek", "DeepSeek V4 (Flash 0731 + Pro)"),
        "zai-code" => (
            "ZAI Code (CN)",
            "Zhipu BigModel / Z.AI coding plan (CN, GLM-5.3)",
        ),
        // OpenCode Go — opencode.ai's low-cost relay. One provider id hosts many
        // models (GLM/Kimi/DeepSeek/MiMo via OpenAI format, MiniMax/Qwen via
        // Anthropic /messages protocol); the per-model [`WireProtocol`] in the model
        // registry selects the transport. Both formats share one
        // `OPENCODE_API_KEY`.
        "opencode-go" => ("OpenCode Go", "opencode.ai relay (multi-model)"),
        // Anthropic — Claude family over the `/messages` API (configurable base
        // URL; defaults to the official endpoint).
        "anthropic" => ("Anthropic", "Claude models"),
        // xAI Grok — OpenAI-compatible chat completions; SuperGrok OAuth or API key.
        "xai" => ("xAI", "Grok models (SuperGrok / API key)"),
        _ => return None,
    };
    Some((name, description))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PromptCacheCapabilities, PromptCachePreference};

    #[test]
    fn catalog_lookup_is_exact_match() {
        let entries = [ProviderEntry {
            id: "deepseek-v4-flash".to_string(),
            name: "DeepSeek V4 Flash".to_string(),
            description: String::new(),
            channels: vec![Channel {
                id: "default".to_string(),
                label: "DeepSeek V4 Flash".to_string(),
                transport: Transport::OpenAi {
                    base_url: "https://api.deepseek.com/v1/chat/completions".to_string(),
                    client_profile: crate::ClientProfile::from("agent"),
                    effort: None,
                    dialect: Default::default(),
                },
                credentials: crate::static_credential("k"),
                model: "deepseek-v4-flash".to_string(),
                remote: None,
                user_overrides: None,
                prompt_cache_preference: PromptCachePreference::default(),
                prompt_cache: PromptCacheCapabilities::unsupported(),
            }],
            default_channel: 0,
            builtin: true,
        }];
        assert_eq!(
            entries
                .iter()
                .find(|e| e.id == "deepseek-v4-flash")
                .expect("exact id")
                .id,
            "deepseek-v4-flash"
        );
        // No alias mapping: stale ids do not resolve.
        assert!(entries.iter().find(|e| e.id == "deepseek").is_none());
        assert!(entries.iter().find(|e| e.id == "deepseek-flash").is_none());
        assert!(entries.iter().find(|e| e.id == "unknown").is_none());
    }

    #[test]
    fn key_ready_is_false_for_empty_cloud_key() {
        let channel = Channel {
            id: "default".to_string(),
            label: "OpenAI".to_string(),
            transport: Transport::OpenAi {
                base_url: "https://api.openai.com/v1/chat/completions".to_string(),
                client_profile: crate::ClientProfile::from("agent"),
                effort: None,
                dialect: Default::default(),
            },
            credentials: crate::static_credential("   "),
            model: "gpt-4o".to_string(),
            remote: None,
            user_overrides: None,
            prompt_cache_preference: PromptCachePreference::default(),
            prompt_cache: PromptCacheCapabilities::unsupported(),
        };
        assert!(!channel.key_ready());
    }

    #[test]
    fn anthropic_transport_needs_a_key() {
        // The Anthropic /messages transport is a cloud transport: it must
        // report needing a key, and an empty key must not be "ready".
        let needs_key = Transport::Anthropic {
            base_url: "https://opencode.ai/zen/go/v1/messages".to_string(),
            client_profile: crate::ClientProfile::from("agent"),
            effort: None,
            thinking: None,
            dialect: Default::default(),
        }
        .needs_api_key();
        assert!(needs_key, "Anthropic transport must require an API key");

        let channel = Channel {
            id: "default".to_string(),
            label: "OpenCode Go (Messages)".to_string(),
            transport: Transport::Anthropic {
                base_url: "https://opencode.ai/zen/go/v1/messages".to_string(),
                client_profile: crate::ClientProfile::from("agent"),
                effort: None,
                thinking: None,
                dialect: Default::default(),
            },
            credentials: crate::static_credential("  "),
            model: "minimax-m3".to_string(),
            remote: None,
            user_overrides: None,
            prompt_cache_preference: PromptCachePreference::default(),
            prompt_cache: PromptCacheCapabilities::unsupported(),
        };
        assert!(!channel.key_ready(), "empty key must not be ready");
    }

    #[test]
    fn multi_model_entry_resolves_channel_by_model_id() {
        // A provider like opencode-go hosts one channel per model, each with the
        // transport matching that model's wire format. Selecting a model is
        // selecting a channel.
        let entry = ProviderEntry {
            id: "opencode-go".to_string(),
            name: "OpenCode Go".to_string(),
            description: String::new(),
            channels: vec![
                Channel {
                    id: "glm-5.2".to_string(),
                    label: "GLM-5.2".to_string(),
                    transport: Transport::OpenAi {
                        base_url: "https://opencode.ai/zen/go/v1/chat/completions".to_string(),
                        client_profile: crate::ClientProfile::from("agent"),
                        effort: None,
                        dialect: Default::default(),
                    },
                    credentials: crate::static_credential("k"),
                    model: "glm-5.2".to_string(),
                    remote: None,
                    user_overrides: None,
                    prompt_cache_preference: PromptCachePreference::default(),
                    prompt_cache: PromptCacheCapabilities::unsupported(),
                },
                Channel {
                    id: "minimax-m3".to_string(),
                    label: "MiniMax M3".to_string(),
                    transport: Transport::Anthropic {
                        base_url: "https://opencode.ai/zen/go/v1/messages".to_string(),
                        client_profile: crate::ClientProfile::from("agent"),
                        effort: None,
                        thinking: None,
                        dialect: Default::default(),
                    },
                    credentials: crate::static_credential("k"),
                    model: "minimax-m3".to_string(),
                    remote: None,
                    user_overrides: None,
                    prompt_cache_preference: PromptCachePreference::default(),
                    prompt_cache: PromptCacheCapabilities::unsupported(),
                },
            ],
            default_channel: 0,
            builtin: true,
        };
        // OpenAI-format model resolves to the OpenAi channel.
        let glm = entry.channel_for_model("glm-5.2").expect("glm-5.2 channel");
        assert!(matches!(glm.transport, Transport::OpenAi { .. }));
        // Anthropic-format model resolves to the Anthropic channel.
        let mm = entry
            .channel_for_model("minimax-m3")
            .expect("minimax-m3 channel");
        assert!(matches!(mm.transport, Transport::Anthropic { .. }));
        // An unknown model id resolves to nothing.
        assert!(entry.channel_for_model("nope").is_none());
        assert!(!entry.offers_model("nope"));
        assert!(entry.offers_model("glm-5.2"));
    }

    #[test]
    fn builtin_provider_metadata_covers_every_preset() {
        for id in [
            "kimi-code",
            "openai",
            "google",
            "deepseek",
            "zai-code",
            "opencode-go",
            "anthropic",
        ] {
            let (name, _) = builtin_provider_metadata(id)
                .unwrap_or_else(|| panic!("missing metadata for {id}"));
            assert!(!name.is_empty());
        }
        assert!(builtin_provider_metadata("unknown").is_none());
    }

    #[test]
    fn context_window_resolves_from_model_registry() {
        // Uses the `fixture-alpha` baseline registered in `model::tests` (core's
        // own test binary links no provider crate, so real vendor ids fall back).
        let entry = ProviderEntry {
            id: "fixture-provider".to_string(),
            name: "Fixture".to_string(),
            description: String::new(),
            channels: vec![Channel {
                id: "default".to_string(),
                label: "Fixture".to_string(),
                transport: Transport::OpenAi {
                    base_url: "https://example.com/v1/chat/completions".to_string(),
                    client_profile: crate::ClientProfile::from("agent"),
                    effort: None,
                    dialect: Default::default(),
                },
                credentials: crate::static_credential("k"),
                model: "fixture-alpha".to_string(),
                remote: None,
                user_overrides: None,
                prompt_cache_preference: PromptCachePreference::default(),
                prompt_cache: PromptCacheCapabilities::unsupported(),
            }],
            default_channel: 0,
            builtin: true,
        };
        assert_eq!(entry.context_window(), 111_000);
    }
}
