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

pub use neenee_contracts::client_identity::*;

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

        let agy = ClientIdentity::Antigravity;
        assert!(
            agy.headers()
                .iter()
                .any(|(k, v)| *k == "x-goog-api-client" && *v == "gl-go/1.23.2 gdcl/0.1")
        );
    }
}
