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

use neenee_core::TokenUsage;

/// Default user agent this project sends to providers.
pub const NEENEE_USER_AGENT: &str = concat!("neenee/", env!("CARGO_PKG_VERSION"));

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

/// The five connection fields every provider shares.
///
/// A provider-specific struct embeds this as `pub endpoint: Endpoint` and adds
/// only its wire-format-unique fields (e.g. Anthropic's `max_tokens` /
/// `thinking`). `id` is the stable provider/solution id surfaced via
/// [`neenee_core::Provider::provider_id`] so assistant responses can be
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

    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}

/// Response-side mutable state shared by all three providers: the most recent
/// token-usage snapshot drained via [`neenee_core::Provider::take_last_usage`].
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
