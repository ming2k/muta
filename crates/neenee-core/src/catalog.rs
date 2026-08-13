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
//! provider from it (see `build_provider_for_channel` in `neenee-providers`)
//! is a pure operation. Resolution (environment variable then config field)
//! lives in the loader, not here, so the same types serve both built-in
//! presets and future user-defined entries.
//!
//! See `docs/adr/0002-model-channel-abstraction.md` for the design.

/// How a [`Channel`] speaks to its model. Determines which `Provider`
/// implementation is constructed for it (in `neenee-providers`).
///
/// Variants carry only the endpoint shape intrinsic to the transport.
/// Per-call credentials and the wire model id live on the [`Channel`] itself,
/// so the same transport serves a built-in preset and a user-defined relay.
#[derive(Debug, Clone)]
pub enum Transport {
    /// OpenAI-compatible chat-completions endpoint at `base_url`. The
    /// `user_agent` is sent verbatim on every request. `effort`, when set,
    /// becomes the OpenAI `reasoning_effort` field for models that expose that
    /// throttle. `copilot` flips on GitHub Copilot's required per-request
    /// headers for a Copilot OAuth channel that speaks chat-completions (the
    /// GPT-4o family and Copilot Free accounts, which have no Responses access).
    OpenAi {
        base_url: String,
        user_agent: String,
        effort: Option<crate::Effort>,
        copilot: bool,
    },
    /// Anthropic-compatible `/messages` endpoint at `base_url` (the full URL).
    /// Auth uses the `x-api-key` header plus `anthropic-version`. Models served
    /// in this format (e.g. MiniMax/Qwen behind opencode-go's `/v1/messages`)
    /// speak the Anthropic Messages wire protocol, not OpenAI chat-completions.
    ///
    /// Two orthogonal reasoning knobs ride on the transport, both optional and
    /// both typed (they live in this crate — `Effort` and
    /// [`crate::ThinkingMode`] — so there is no string↔enum shuffle at the
    /// factory layer):
    ///
    /// - `effort` — reasoning **depth** (the throttle). Clamped to the
    ///   resolved model's supported levels at request-build time.
    /// - `thinking` — reasoning **on/off** (the switch). `None` is the opt-in
    ///   default: the model does NOT reason (ADR-0046). `Some(Adaptive)` opts
    ///   the model in to extended thinking; `Some(Off)` is an explicit off.
    ///
    /// The two are independent on the wire: a request may carry `effort`
    /// without enabling thinking, or thinking at any depth. They are therefore
    /// modeled and surfaced as separate controls — never coupled.
    Anthropic {
        base_url: String,
        user_agent: String,
        effort: Option<crate::Effort>,
        thinking: Option<crate::ThinkingMode>,
        /// GitHub Copilot's `/v1/messages` adapter uses a bearer and the
        /// Copilot client headers rather than Anthropic's `x-api-key` auth.
        copilot: bool,
    },
    /// Google native API (`generativelanguage.googleapis.com`). The model
    /// id and API key are read from the owning [`Channel`]. `base_url` is the
    /// versioned base (default `https://generativelanguage.googleapis.com/v1beta`);
    /// the provider appends `/models/{model}:generateContent` (or the `:stream`
    /// variant), so a 中转站/relay supplies its host with the `/v1beta` prefix.
    ///
    /// `effort`, when set, is the reasoning-depth override — the same
    /// provider-independent [`crate::Effort`] the other transports carry, translated
    /// onto Google's `thinkingConfig` at request-build time: Gemini 3.x maps it
    /// to `thinkingLevel` (`minimal`/`low`/`medium`/`high`); Gemini 2.5 maps it
    /// to a `thinkingBudget` token bucket. So effort reaches Google the same
    /// way it reaches every other provider — through the single abstraction.
    Google {
        base_url: String,
        user_agent: String,
        effort: Option<crate::Effort>,
    },
    /// OpenAI **Responses** API (`/responses` endpoint), used by the ChatGPT
    /// subscription backend (`chatgpt.com/backend-api/codex/responses`) and by
    /// the GitHub Copilot subscription backend
    /// (`api.githubcopilot.com/responses`). Unlike
    /// [`OpenAi`](Self::OpenAi) (chat completions), the Responses
    /// API takes `instructions` + an `input` items array and streams
    /// `response.*` events. `account_id` is sent as the `ChatGPT-Account-Id`
    /// header (resolved from the OAuth `chatgpt_account_id` claim); `None` is
    /// valid for single-account users. `copilot` flips the per-request header
    /// set to Copilot's required headers (`x-initiator`, `Openai-Intent`,
    /// `X-GitHub-Api-Version`, and `Copilot-Vision-Request` when vision is
    /// used) and drops the ChatGPT account-id header.
    OpenAiResponses {
        base_url: String,
        user_agent: String,
        effort: Option<crate::Effort>,
        account_id: Option<String>,
        copilot: bool,
    },
}

impl Transport {
    /// Whether this transport needs an API key at all. Every remaining transport
    /// is a cloud transport, so all require a key. (A keyless OpenAI-compatible
    /// relay still constructs fine — an empty `api_key` simply suppresses the
    /// `Authorization` header in the provider.)
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
/// A channel pairs a [`Transport`] with resolved credentials (`api_key`) and
/// the wire `model` id. Built-in presets materialize exactly one channel per
/// entry (id `"default"`); user-defined entries may declare several channels
/// per model (e.g. Gemini via Studio, Vertex, or a relay), with the entry's
/// `default_channel` selecting one. See ADR-0002.
#[derive(Debug, Clone)]
pub struct Channel {
    /// Stable identifier within the model (e.g. `"studio"`, `"vertex"`).
    /// Built-in presets use `"default"`.
    pub id: String,
    /// Display label shown in the picker (e.g. `"Google Studio"`).
    pub label: String,
    /// Endpoint shape and provider implementation selector.
    pub transport: Transport,
    /// Resolved API key (env var first, then config field). Empty for keyless
    /// channels; never absent so construction never branches on `Option`.
    /// Redacted from `Debug` output — read it only via
    /// [`SecretString::expose_secret`](crate::SecretString::expose_secret) at
    /// the provider-construction boundary.
    pub api_key: crate::SecretString,
    /// Resolved wire model id sent to the provider.
    pub model: String,
    /// Provider-scoped live capability metadata. A trusted provider's remote
    /// catalogue owns the fields it explicitly supplies; the static model
    /// registry remains the fallback for omitted or offline data.
    pub remote: Option<crate::RemoteModelMetadata>,
}

impl Channel {
    /// Whether this channel has a usable API key. Keyless transports
    /// (the in-memory mock) always report ready; the rest require a non-empty
    /// key.
    pub fn key_ready(&self) -> bool {
        if !self.transport.needs_api_key() {
            return true;
        }
        !self.api_key.expose_secret().trim().is_empty()
    }

    /// Resolve effective capabilities for this delivery path. The provider's
    /// remote snapshot overlays only its explicit fields onto the static model
    /// baseline, preventing a global model id from overwriting account-specific
    /// routes or capabilities.
    pub fn capabilities(&self) -> crate::ModelCapabilities {
        crate::ModelCapabilities::for_channel(&self.model, self.remote.as_ref())
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
    /// matching that model's [`crate::model::WireFormat`] — so selecting a
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
        "zai-code" => ("ZAI Code", "Z.AI coding plan (GLM-5.2)"),
        // OpenCode Go — opencode.ai's low-cost relay. One provider id hosts many
        // models (GLM/Kimi/DeepSeek/MiMo via OpenAI format, MiniMax/Qwen via
        // Anthropic /messages format); the per-model [`WireFormat`] in the model
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
                    user_agent: "agent".to_string(),
                    effort: None,
                    copilot: false,
                },
                api_key: "k".into(),
                model: "deepseek-v4-flash".to_string(),
                remote: None,
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
                user_agent: "agent".to_string(),
                effort: None,
                copilot: false,
            },
            api_key: "   ".into(),
            model: "gpt-4o".to_string(),
            remote: None,
        };
        assert!(!channel.key_ready());
    }

    #[test]
    fn anthropic_transport_needs_a_key() {
        // The Anthropic /messages transport is a cloud transport: it must
        // report needing a key, and an empty key must not be "ready".
        let needs_key = Transport::Anthropic {
            base_url: "https://opencode.ai/zen/go/v1/messages".to_string(),
            user_agent: "agent".to_string(),
            effort: None,
            thinking: None,
            copilot: false,
        }
        .needs_api_key();
        assert!(needs_key, "Anthropic transport must require an API key");

        let channel = Channel {
            id: "default".to_string(),
            label: "OpenCode Go (Messages)".to_string(),
            transport: Transport::Anthropic {
                base_url: "https://opencode.ai/zen/go/v1/messages".to_string(),
                user_agent: "agent".to_string(),
                effort: None,
                thinking: None,
                copilot: false,
            },
            api_key: "  ".into(),
            model: "minimax-m3".to_string(),
            remote: None,
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
                        user_agent: "agent".to_string(),
                        effort: None,
                        copilot: false,
                    },
                    api_key: "k".into(),
                    model: "glm-5.2".to_string(),
                    remote: None,
                },
                Channel {
                    id: "minimax-m3".to_string(),
                    label: "MiniMax M3".to_string(),
                    transport: Transport::Anthropic {
                        base_url: "https://opencode.ai/zen/go/v1/messages".to_string(),
                        user_agent: "agent".to_string(),
                        effort: None,
                        thinking: None,
                        copilot: false,
                    },
                    api_key: "k".into(),
                    model: "minimax-m3".to_string(),
                    remote: None,
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
                    user_agent: "agent".to_string(),
                    effort: None,
                    copilot: false,
                },
                api_key: "k".into(),
                model: "fixture-alpha".to_string(),
                remote: None,
            }],
            default_channel: 0,
            builtin: true,
        };
        assert_eq!(entry.context_window(), 111_000);
    }
}
