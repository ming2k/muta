//! Shared connection configuration and typed request/response carriers.
//!
//! Every concrete provider carries the same five connection fields —
//! `api_key`, `model`, `base_url`, `user_agent`, `id` — duplicated verbatim
//! across [`crate::protocol::openai::OpenAiChatCompletionsProvider`],
//! [`crate::protocol::anthropic::AnthropicMessagesProvider`], and
//! [`crate::protocol::google::GoogleProvider`]. [`Endpoint`] factors that out so each
//! provider struct keeps only the fields *unique* to its wire format.
//!
//! This is the analogue of vercel/ai's per-provider client configuration: the
//! shared transport concerns (where to send, how to authenticate, how to label
//! attribution) live in one place, while each API's request *shape* lives in
//! its own module.

use std::sync::Mutex;

use neenee_contracts::TokenUsage;

/// Default user agent this project sends to providers.
pub const NEENEE_USER_AGENT: &str = concat!("neenee/", env!("CARGO_PKG_VERSION"));

/// OpenCode version currently impersonated by neenee.
pub const OPENCODE_VERSION: &str = "1.18.18";

/// User-Agent header value sent when impersonating OpenCode.
pub const OPENCODE_USER_AGENT: &str = "opencode/1.18.18";

/// User-Agent header value sent when impersonating Claude Code.
pub const CLAUDE_CODE_USER_AGENT: &str = "claude-code/0.2.29";

/// Client-identity headers used when impersonating Claude Code.
pub const CLAUDE_CODE_CLIENT_HEADERS: &[(&str, &str)] = &[
    ("x-app", "claude-code"),
    ("anthropic-version", "2023-06-01"),
];

/// User-Agent header value sent when impersonating OpenAI Codex.
pub const CODEX_USER_AGENT: &str = "codex/1.0.0";

/// Client-identity headers used when impersonating OpenAI Codex.
pub const CODEX_CLIENT_HEADERS: &[(&str, &str)] = &[
    ("Openai-Intent", "conversation-edits"),
    ("x-initiator", "user"),
];

/// User-Agent header value sent when impersonating Cline.
pub const CLINE_USER_AGENT: &str = "Cline/3.5.0";

/// Client-identity headers used when impersonating Cline.
pub const CLINE_CLIENT_HEADERS: &[(&str, &str)] =
    &[("X-Title", "Cline"), ("HTTP-Referer", "https://cline.bot")];

/// User-Agent header value sent when impersonating Cursor.
pub const CURSOR_USER_AGENT: &str = "Cursor/0.45.0";

/// Client-identity headers used when impersonating Cursor.
pub const CURSOR_CLIENT_HEADERS: &[(&str, &str)] = &[
    ("X-Title", "Cursor"),
    ("x-cursor-client-version", "0.45.0"),
    ("x-ghost-mode", "true"),
];

/// User-Agent header value sent when impersonating Kilo Code.
pub const KILO_CODE_USER_AGENT: &str = "Kilo-Code/5.3.0";

/// Client-identity headers used when impersonating Kilo Code.
pub const KILO_CODE_CLIENT_HEADERS: &[(&str, &str)] = &[
    ("X-Title", "Kilo Code"),
    ("X-Kilocode-Version", "5.3.0"),
    ("HTTP-Referer", "https://kilocode.ai"),
];

/// User-Agent header value sent when impersonating Roo Code.
pub const ROO_CODE_USER_AGENT: &str = "Roo-Code/3.8.0";

/// Client-identity headers used when impersonating Roo Code.
pub const ROO_CODE_CLIENT_HEADERS: &[(&str, &str)] = &[
    ("X-Title", "Roo Code"),
    ("HTTP-Referer", "https://github.com/RooVetGit/Roo-Cline"),
];

/// User-Agent header value sent when impersonating Windsurf.
pub const WINDSURF_USER_AGENT: &str = "Windsurf/1.0.0";

/// Client-identity headers used when impersonating Windsurf.
pub const WINDSURF_CLIENT_HEADERS: &[(&str, &str)] = &[
    ("X-Title", "Windsurf"),
    ("HTTP-Referer", "https://codeium.com/windsurf"),
];

/// User-Agent header value sent when impersonating Aider.
pub const AIDER_USER_AGENT: &str = "aider/0.74.0";

/// Client-identity headers used when impersonating Aider.
pub const AIDER_CLIENT_HEADERS: &[(&str, &str)] =
    &[("X-Title", "Aider"), ("HTTP-Referer", "https://aider.chat")];

/// User-Agent header value sent when impersonating Zhipu ZCode.
pub const ZCODE_USER_AGENT: &str = "ZCode/3.5.3";

/// Client-identity headers used when impersonating Zhipu / Z.AI's native ZCode desktop client.
pub const ZCODE_CLIENT_HEADERS: &[(&str, &str)] = &[
    ("X-Title", "Z Code"),
    ("X-ZCode-Agent", "glm"),
    ("X-ZCode-App-Version", "3.5.3"),
    ("HTTP-Referer", "https://zcode.z.ai"),
];

/// Client-identity headers GitHub's Copilot backend (`api.githubcopilot.com`)
/// uses to resolve the caller against the account's actual Copilot plan.
/// `Copilot-Integration-Id` is the load-bearing one — without a recognized
/// integration id the backend cannot tell which entitlement set applies and
/// both the chat surface and `GET /models` fall back to (or reject outside)
/// the always-available GPT-4o family, regardless of the account's real plan.
/// The two `Editor-*` headers are sent alongside it by every real Copilot
/// Chat request and are kept in sync with it here so all three travel
/// together. Distinct from the per-turn headers in
/// `openai::request::headers` / `responses::request::headers`
/// (`x-initiator`, `Openai-Intent`, `X-GitHub-Api-Version`), which describe
/// the request rather than the client, and from the discovery-only
/// `Copilot-Vision-Request` header, which depends on request content.
pub const COPILOT_CLIENT_HEADERS: &[(&str, &str)] = &[
    ("Copilot-Integration-Id", "vscode-chat"),
    ("Editor-Version", "vscode/1.107.0"),
    ("Editor-Plugin-Version", "copilot-chat/0.35.0"),
];

/// First-class client identity presets and templates used for provider recognition and impersonation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ClientIdentity {
    /// Default native identity (`User-Agent: neenee/<version>`).
    #[default]
    Native,
    /// Impersonate OpenCode (`User-Agent: opencode/1.18.18`).
    OpenCode,
    /// Impersonate Claude Code (`User-Agent: claude-code/0.2.29`, `x-app: claude-code`).
    ClaudeCode,
    /// Impersonate OpenAI Codex (`User-Agent: codex/1.0.0`, `Openai-Intent: conversation-edits`).
    Codex,
    /// Impersonate Cline (`User-Agent: Cline/3.5.0`, `X-Title: Cline`).
    Cline,
    /// Impersonate Cursor (`User-Agent: Cursor/0.45.0`, `x-cursor-client-version: 0.45.0`).
    Cursor,
    /// Impersonate Kilo Code (`User-Agent: Kilo-Code/1.0.0`, `X-Title: Kilo Code`).
    KiloCode,
    /// Impersonate Roo Code (`User-Agent: Roo-Code/3.8.0`, `X-Title: Roo Code`).
    RooCode,
    /// Impersonate Windsurf (`User-Agent: Windsurf/1.0.0`, `X-Title: Windsurf`).
    Windsurf,
    /// Impersonate Aider (`User-Agent: aider/0.74.0`).
    Aider,
    /// Impersonate Zhipu / Z.AI's native ZCode client.
    ZCode,
    /// Impersonate GitHub Copilot / VS Code Chat.
    Copilot,
    /// Custom client identity with specific User-Agent and headers.
    Custom {
        user_agent: String,
        extra_headers: Vec<(String, String)>,
    },
}

impl ClientIdentity {
    /// All standard built-in client identity presets.
    pub const PRESETS: &[ClientIdentity] = &[
        Self::Native,
        Self::OpenCode,
        Self::ClaudeCode,
        Self::Codex,
        Self::Cline,
        Self::Cursor,
        Self::KiloCode,
        Self::RooCode,
        Self::Windsurf,
        Self::Aider,
        Self::ZCode,
        Self::Copilot,
    ];

    /// Return the canonical id string for this preset identity.
    pub fn id(&self) -> &str {
        match self {
            Self::Native => "native",
            Self::OpenCode => "opencode",
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Cline => "cline",
            Self::Cursor => "cursor",
            Self::KiloCode => "kilo-code",
            Self::RooCode => "roo-code",
            Self::Windsurf => "windsurf",
            Self::Aider => "aider",
            Self::ZCode => "zcode",
            Self::Copilot => "copilot",
            Self::Custom { .. } => "custom",
        }
    }

    /// Return human-friendly display label.
    pub fn label(&self) -> &str {
        match self {
            Self::Native => "neenee (Native)",
            Self::OpenCode => "OpenCode",
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "OpenAI Codex",
            Self::Cline => "Cline",
            Self::Cursor => "Cursor",
            Self::KiloCode => "Kilo Code",
            Self::RooCode => "Roo Code",
            Self::Windsurf => "Windsurf",
            Self::Aider => "Aider",
            Self::ZCode => "ZCode (Z.AI)",
            Self::Copilot => "GitHub Copilot",
            Self::Custom { .. } => "Custom",
        }
    }

    /// Return the User-Agent header value for this client identity.
    pub fn user_agent(&self) -> &str {
        match self {
            Self::Native => NEENEE_USER_AGENT,
            Self::OpenCode => OPENCODE_USER_AGENT,
            Self::ClaudeCode => CLAUDE_CODE_USER_AGENT,
            Self::Codex => CODEX_USER_AGENT,
            Self::Cline => CLINE_USER_AGENT,
            Self::Cursor => CURSOR_USER_AGENT,
            Self::KiloCode => KILO_CODE_USER_AGENT,
            Self::RooCode => ROO_CODE_USER_AGENT,
            Self::Windsurf => WINDSURF_USER_AGENT,
            Self::Aider => AIDER_USER_AGENT,
            Self::ZCode => ZCODE_USER_AGENT,
            Self::Copilot => NEENEE_USER_AGENT,
            Self::Custom { user_agent, .. } => user_agent.as_str(),
        }
    }

    /// Return the client-identity headers for this client identity.
    pub fn headers(&self) -> Vec<(&str, &str)> {
        match self {
            Self::Native | Self::OpenCode => Vec::new(),
            Self::ClaudeCode => CLAUDE_CODE_CLIENT_HEADERS.to_vec(),
            Self::Codex => CODEX_CLIENT_HEADERS.to_vec(),
            Self::Cline => CLINE_CLIENT_HEADERS.to_vec(),
            Self::Cursor => CURSOR_CLIENT_HEADERS.to_vec(),
            Self::KiloCode => KILO_CODE_CLIENT_HEADERS.to_vec(),
            Self::RooCode => ROO_CODE_CLIENT_HEADERS.to_vec(),
            Self::Windsurf => WINDSURF_CLIENT_HEADERS.to_vec(),
            Self::Aider => AIDER_CLIENT_HEADERS.to_vec(),
            Self::ZCode => ZCODE_CLIENT_HEADERS.to_vec(),
            Self::Copilot => COPILOT_CLIENT_HEADERS.to_vec(),
            Self::Custom { extra_headers, .. } => extra_headers
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect(),
        }
    }

    /// Parse a preset from an id or common alias.
    pub fn from_id(id: &str) -> Option<Self> {
        match id.trim().to_ascii_lowercase().as_str() {
            "native" | "neenee" | "default" => Some(Self::Native),
            "opencode" => Some(Self::OpenCode),
            "claude" | "claude-code" | "claudecode" => Some(Self::ClaudeCode),
            "codex" | "openai-codex" => Some(Self::Codex),
            "cline" => Some(Self::Cline),
            "cursor" => Some(Self::Cursor),
            "kilo" | "kilo-code" | "kilocode" => Some(Self::KiloCode),
            "roo" | "roo-code" | "roocode" => Some(Self::RooCode),
            "windsurf" => Some(Self::Windsurf),
            "aider" => Some(Self::Aider),
            "zcode" | "z-code" | "zai" => Some(Self::ZCode),
            "copilot" | "github-copilot" | "vscode" => Some(Self::Copilot),
            _ => None,
        }
    }

    /// Resolve a client identity preset from a User-Agent string.
    pub fn from_user_agent(ua: &str) -> Self {
        let trimmed = ua.trim();
        if trimmed.is_empty() || trimmed.starts_with("neenee/") {
            Self::Native
        } else if trimmed.starts_with("opencode") {
            Self::OpenCode
        } else if trimmed.starts_with("claude-code") || trimmed.starts_with("claude-cli") {
            Self::ClaudeCode
        } else if trimmed.starts_with("codex") || trimmed.starts_with("OpenAI-Codex") {
            Self::Codex
        } else if trimmed.starts_with("Cline") || trimmed.starts_with("cline") {
            Self::Cline
        } else if trimmed.starts_with("Cursor") || trimmed.starts_with("cursor") {
            Self::Cursor
        } else if trimmed.starts_with("Kilo-Code") || trimmed.starts_with("kilo") {
            Self::KiloCode
        } else if trimmed.starts_with("Roo-Code") || trimmed.starts_with("roo") {
            Self::RooCode
        } else if trimmed.starts_with("Windsurf") || trimmed.starts_with("windsurf") {
            Self::Windsurf
        } else if trimmed.starts_with("aider") {
            Self::Aider
        } else if trimmed.starts_with("ZCode") || trimmed.starts_with("zcode") {
            Self::ZCode
        } else if trimmed.contains("Copilot") || trimmed.contains("copilot") {
            Self::Copilot
        } else {
            Self::Custom {
                user_agent: trimmed.to_string(),
                extra_headers: Vec::new(),
            }
        }
    }
}

/// The five connection fields every provider shares.
///
/// A provider-specific struct embeds this as `pub endpoint: Endpoint` and adds
/// only its wire-format-unique fields (e.g. Anthropic's `max_tokens` /
/// `thinking`). `id` is the stable provider/solution id surfaced via
/// [`neenee_contracts::Provider::provider_id`] so assistant responses can be
/// attributed to the logical channel even after a mid-session switch.
#[derive(Clone)]
pub struct Endpoint {
    /// API key. An *empty* key means "keyless": OpenAI-compatible relays omit
    /// the `Authorization` header rather than send an empty bearer token;
    /// Google still appends `?key=` (a relay that ignores it tolerates the
    /// empty value). Each provider's auth layer decides.
    pub api_key: String,
    /// Model id sent on the wire (`model` field of the request body).
    pub model: String,
    /// Full endpoint URL. For OpenAI/Anthropic this is the chat-completions /
    /// `/messages` path; for Google it is the versioned base
    /// (`.../v1beta`) to which the per-call model path is appended.
    pub base_url: String,
    /// `User-Agent` header value.
    pub user_agent: String,
    /// Stable attribution id (`provider_id()`).
    pub id: String,
}

impl Endpoint {
    /// The three-tier constructor used by every provider's `new` /
    /// `with_base_url` / `with_base_url_and_user_agent` ladder.
    pub fn new(api_key: String, model: String, base_url: impl Into<String>, id: &str) -> Self {
        Self {
            api_key,
            model,
            base_url: base_url.into(),
            user_agent: NEENEE_USER_AGENT.to_string(),
            id: id.to_string(),
        }
    }

    /// Stamp an attribution id after construction (the catalog does this with
    /// the config entry id).
    pub fn with_id(mut self, id: String) -> Self {
        self.id = id;
        self
    }

    /// Stamp the attribution id in place (non-consuming variant for the
    /// registry, which builds the provider via a constructor and then sets the
    /// id from the channel entry id).
    pub fn set_id(&mut self, id: String) {
        self.id = id;
    }

    // ── accessors ────────────────────────────────────────────────────────
    //
    // Provided once here so each provider forwards through its embedded
    // `endpoint` field instead of restating them. Naming note: these are
    // intentionally distinct from the `Provider` trait methods (`model`,
    // `provider_id`) that every concrete provider also implements, so there is
    // no name collision — the trait methods return owned `String`s and serve
    // the `dyn Provider` interface, while these borrow the underlying field.

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn model_id(&self) -> &str {
        &self.model
    }

    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }

    /// Return the resolved [`ClientIdentity`] for this endpoint based on its `user_agent`.
    pub fn client_identity(&self) -> ClientIdentity {
        ClientIdentity::from_user_agent(&self.user_agent)
    }

    /// Set the [`ClientIdentity`] for this endpoint, updating its `user_agent`.
    pub fn with_client_identity(mut self, identity: &ClientIdentity) -> Self {
        self.user_agent = identity.user_agent().to_string();
        self
    }

    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}

/// Response-side mutable state shared by all three providers: the most recent
/// token-usage snapshot drained via [`neenee_contracts::Provider::take_last_usage`].
///
/// Factored out so a provider struct embeds `pub turn: TurnState` instead of
/// restating the `Mutex` field. Recovering from a poisoned mutex (a prior
/// panic must not take down the next request) is handled uniformly here.
pub struct TurnState {
    /// Stash for the most recent `usage` object, drained once by
    /// `take_last_usage`.
    last_usage: Mutex<Option<TokenUsage>>,
}

impl TurnState {
    pub fn new() -> Self {
        Self {
            last_usage: Mutex::new(None),
        }
    }

    /// Stash the usage from the most recent turn.
    pub fn stash_usage(&self, usage: TokenUsage) {
        *self.last_usage.lock().unwrap_or_else(|e| e.into_inner()) = Some(usage);
    }

    /// Drain and return the most recent usage snapshot, if any.
    pub fn take_usage(&self) -> Option<TokenUsage> {
        self.last_usage
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }
}

impl Default for TurnState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_identity_presets_have_matching_user_agents_and_headers() {
        for preset in ClientIdentity::PRESETS {
            assert!(!preset.id().is_empty());
            assert!(!preset.label().is_empty());
            assert!(!preset.user_agent().is_empty());

            // Each preset round-trips through from_id
            let parsed = ClientIdentity::from_id(preset.id()).expect("parses from canonical id");
            assert_eq!(&parsed, preset);

            // from_user_agent detects standard presets
            if *preset != ClientIdentity::Copilot {
                let detected = ClientIdentity::from_user_agent(preset.user_agent());
                assert_eq!(
                    &detected,
                    preset,
                    "detected from UA: {}",
                    preset.user_agent()
                );
            }
        }
    }

    #[test]
    fn client_identity_headers_attached_for_impersonated_clients() {
        let zcode = ClientIdentity::ZCode;
        let zcode_headers = zcode.headers();
        assert!(
            zcode_headers
                .iter()
                .any(|(k, v)| *k == "X-Title" && *v == "Z Code")
        );
        assert!(
            zcode_headers
                .iter()
                .any(|(k, v)| *k == "X-ZCode-Agent" && *v == "glm")
        );

        let claude = ClientIdentity::ClaudeCode;
        assert!(
            claude
                .headers()
                .iter()
                .any(|(k, v)| *k == "x-app" && *v == "claude-code")
        );

        let cline = ClientIdentity::Cline;
        assert!(
            cline
                .headers()
                .iter()
                .any(|(k, v)| *k == "X-Title" && *v == "Cline")
        );

        let cursor = ClientIdentity::Cursor;
        assert!(
            cursor
                .headers()
                .iter()
                .any(|(k, v)| *k == "X-Title" && *v == "Cursor")
        );
    }
}
