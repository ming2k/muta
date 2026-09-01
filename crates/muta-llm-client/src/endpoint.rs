//! Shared connection configuration and typed request/response carriers.
//!
//! Every concrete provider carries the same connection fields —
//! `credentials`, `model`, `base_url`, `client_profile`, `id` — duplicated verbatim
//! across [`crate::protocol::openai::OpenAiChatCompletionsProvider`],
//! [`crate::protocol::anthropic::AnthropicMessagesProvider`], and
//! [`crate::protocol::google::GoogleProvider`]. [`Endpoint`] factors that out so each
//! provider struct keeps only the fields *unique* to its wire format.
//!
//! This is the analogue of vercel/ai's per-provider client configuration: the
//! shared transport concerns (where to send, how to authenticate, how to label
//! attribution) live in one place, while each API's request *shape* lives in
//! its own module.

use std::sync::Arc;

use muta_contracts::{CredentialSource, ResolvedAuth, SecretString, static_credential};

pub use muta_contracts::client_identity::*;

/// The connection fields every provider shares.
///
/// A provider-specific struct embeds this as `pub endpoint: Endpoint` and adds
/// only its wire-format-unique fields (e.g. Anthropic's `max_tokens` /
/// `thinking`). `id` is the stable provider/solution id surfaced via
/// [`muta_contracts::Provider::provider_id`] so assistant responses can be
/// attributed to the logical channel even after a mid-session switch.
#[derive(Clone)]
pub struct Endpoint {
    /// Dynamic or static credential source.
    pub credentials: Arc<dyn CredentialSource>,
    /// Model id sent on the wire (`model` field of the request body).
    pub model: String,
    /// Full endpoint URL. For OpenAI/Anthropic this is the chat-completions /
    /// `/messages` path; for Google it is the versioned base
    /// (`.../v1beta`) to which the per-call model path is appended.
    pub base_url: String,
    /// Client profile specifying the User-Agent and client identity headers.
    pub client_profile: ClientProfile,
    /// Stable attribution id (`provider_id()`).
    pub id: String,
}

impl Endpoint {
    /// Construct an endpoint with a dynamic [`CredentialSource`].
    pub fn new(
        credentials: Arc<dyn CredentialSource>,
        model: impl Into<String>,
        base_url: impl Into<String>,
        id: impl Into<String>,
    ) -> Self {
        Self {
            credentials,
            model: model.into(),
            base_url: base_url.into(),
            client_profile: ClientProfile::Native,
            id: id.into(),
        }
    }

    /// Construct an endpoint with a static API key string.
    pub fn from_static_key(
        api_key: impl Into<SecretString>,
        model: impl Into<String>,
        base_url: impl Into<String>,
        id: impl Into<String>,
    ) -> Self {
        Self::new(static_credential(api_key), model, base_url, id)
    }

    /// Construct an endpoint with dynamic credentials (alias for [`Self::new`]).
    pub fn with_credentials(
        credentials: Arc<dyn CredentialSource>,
        model: impl Into<String>,
        base_url: impl Into<String>,
        id: impl Into<String>,
    ) -> Self {
        Self::new(credentials, model, base_url, id)
    }

    /// Attach a dynamic credential source to this endpoint.
    pub fn with_credentials_source(mut self, credentials: Arc<dyn CredentialSource>) -> Self {
        self.credentials = credentials;
        self
    }

    /// Resolve the live authentication credentials for an outbound request.
    pub async fn resolve_auth(&self) -> Result<ResolvedAuth, String> {
        self.credentials.resolve_auth().await
    }

    /// Refresh in reaction to a rejection of the token used by this request.
    pub async fn force_refresh_auth_after(
        &self,
        rejected_access: &muta_contracts::SecretString,
    ) -> Result<ResolvedAuth, String> {
        self.credentials
            .force_refresh_after_rejection(rejected_access)
            .await
    }

    /// Whether this endpoint uses dynamic OAuth credentials.
    pub fn is_oauth(&self) -> bool {
        self.credentials.is_oauth()
    }

    /// Stamp the user-agent header value, updating the client profile.
    pub fn with_user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.client_profile = ClientProfile::from_user_agent(&user_agent.into());
        self
    }

    /// Stamp a client profile directly onto this endpoint.
    pub fn with_client_profile(mut self, profile: impl Into<ClientProfile>) -> Self {
        self.client_profile = profile.into();
        self
    }

    /// Stamp a client identity directly onto this endpoint.
    pub fn with_client_identity(mut self, identity: &ClientIdentity) -> Self {
        self.client_profile = identity.clone();
        self
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
        self.client_profile.user_agent()
    }

    /// Return the resolved [`ClientProfile`] for this endpoint.
    pub fn client_profile(&self) -> &ClientProfile {
        &self.client_profile
    }

    /// Return the resolved [`ClientIdentity`] for this endpoint.
    pub fn client_identity(&self) -> &ClientIdentity {
        &self.client_profile
    }

    /// Return the client-identity headers to attach to every request.
    pub fn headers(&self) -> Vec<(&str, &str)> {
        self.client_profile.headers()
    }

    pub fn id(&self) -> &str {
        &self.id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_profile_presets_have_matching_user_agents_and_headers() {
        for preset in ClientProfile::PRESETS {
            assert!(!preset.id().is_empty());
            assert!(!preset.label().is_empty());
            assert!(!preset.user_agent().is_empty());

            // Each preset round-trips through from_id
            let parsed = ClientProfile::from_id(preset.id()).expect("parses from canonical id");
            assert_eq!(&parsed, preset);

            // from_user_agent detects standard presets
            if *preset != ClientProfile::Copilot {
                let detected = ClientProfile::from_user_agent(preset.user_agent());
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
    fn client_profile_headers_attached_for_emulated_clients() {
        let zcode = ClientProfile::ZCode;
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

        let claude = ClientProfile::ClaudeCode;
        assert!(
            claude
                .headers()
                .iter()
                .any(|(k, v)| *k == "x-app" && *v == "claude-code")
        );

        let cline = ClientProfile::Cline;
        assert!(
            cline
                .headers()
                .iter()
                .any(|(k, v)| *k == "X-Title" && *v == "Cline")
        );

        let cursor = ClientProfile::Cursor;
        assert!(
            cursor
                .headers()
                .iter()
                .any(|(k, v)| *k == "X-Title" && *v == "Cursor")
        );

        let agy = ClientProfile::Antigravity;
        assert!(
            agy.headers()
                .iter()
                .any(|(k, v)| *k == "x-goog-api-client" && *v == "gl-go/1.23.2 gdcl/0.1")
        );

        let custom = ClientProfile::custom(
            "custom-agent/1.0",
            vec![("X-Custom-Foo".to_string(), "Bar".to_string())],
        );
        let custom_headers = custom.headers();
        assert_eq!(custom_headers.len(), 1);
        assert_eq!(custom_headers[0], ("X-Custom-Foo", "Bar"));
    }
}
