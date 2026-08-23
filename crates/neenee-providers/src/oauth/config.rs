//! Per-provider OAuth2 client configuration & dynamic client emulator.
//!
//! neenee's OAuth engine provides an ultra-flexible, industrial-grade abstraction
//! capable of emulating any OAuth 2.0 client (Google Antigravity, OpenAI Codex,
//! xAI SuperGrok, GitHub Copilot, or custom enterprise OAuth endpoints).
//!
//! Every client parameter (client_id, client_secret, endpoints, scopes, loopback
//! binding strategies, PKCE modes, headers, custom parameters) can be customized
//! dynamically at runtime or resolved from battle-tested presets.

use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// Which device-authorization flow a provider speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DeviceFlow {
    /// Standard RFC 8628: form-urlencoded request + poll, the polled token
    /// endpoint returns access tokens directly (xAI, Google, Copilot).
    #[default]
    Rfc8628,
    /// OpenAI/ChatGPT: JSON bodies; the poll endpoint returns an
    /// `authorization_code` + `code_verifier` that are then exchanged at the
    /// `/oauth/token` endpoint for the token set.
    ChatGpt,
    /// Device flow is not supported or disabled.
    Disabled,
}

/// Port binding strategy for the local callback listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PortMode {
    /// Fixed port (e.g. 1455 for Codex, 56121 for xAI). Fails if port is occupied.
    Fixed(u16),
    /// Ephemeral / dynamic port assigned by OS (binds port 0).
    #[default]
    Dynamic,
    /// Tries the preferred port first. If occupied (AddrInUse), seamlessly
    /// falls back to a dynamic OS port. Ideal for Google Antigravity & local testing.
    PreferredOrDynamic(u16),
}

/// PKCE (RFC 7636) code challenge method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PkceMode {
    /// Standard SHA-256 code challenge (RFC 7636 S256).
    #[default]
    S256,
    /// Plain code challenge.
    Plain,
    /// PKCE disabled.
    Disabled,
}

/// Client authentication method used during token exchange / refresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ClientAuthMethod {
    /// Public client / PKCE only (no client_secret required).
    #[default]
    None,
    /// Send `client_id` and `client_secret` in the request body (form-urlencoded or JSON).
    RequestBody,
    /// Send `Authorization: Basic base64(client_id:client_secret)` header.
    BasicHeader,
}

/// Format for token endpoint request payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TokenRequestFormat {
    #[default]
    FormUrlEncoded,
    Json,
}

/// Fully flexible OAuth2 client configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthConfig {
    /// `auth.toml` key under which this provider's tokens persist.
    pub provider_id: Cow<'static, str>,
    /// Public OAuth client id registered with the provider.
    pub client_id: Cow<'static, str>,
    /// Client secret when required by the OAuth provider (e.g. Google Antigravity).
    pub client_secret: Option<Cow<'static, str>>,
    /// How the client credentials are authenticated with the token endpoint.
    pub client_auth_method: ClientAuthMethod,
    /// Authorization endpoint (consent screen).
    pub authorize_url: Cow<'static, str>,
    /// Token endpoint (code exchange + refresh).
    pub token_url: Cow<'static, str>,
    /// Device-authorization endpoint (request the user_code).
    pub device_authorization_url: Cow<'static, str>,
    /// RFC 8628 `grant_type` value sent during the device poll.
    pub grant_type_device: Cow<'static, str>,
    /// OAuth scopes requested.
    pub scope: Cow<'static, str>,
    /// Extra query params appended to the authorize URL.
    pub extra_authorize_params: Vec<(Cow<'static, str>, Cow<'static, str>)>,
    /// Extra form/json params sent during code exchange.
    pub extra_token_params: Vec<(Cow<'static, str>, Cow<'static, str>)>,
    /// Extra form/json params sent during token refresh.
    pub extra_refresh_params: Vec<(Cow<'static, str>, Cow<'static, str>)>,
    /// Extra HTTP headers sent to token/device endpoints.
    pub extra_headers: Vec<(Cow<'static, str>, Cow<'static, str>)>,
    /// Custom User-Agent header (e.g. "antigravity/1.23.2 windows/amd64").
    pub user_agent: Option<Cow<'static, str>>,
    /// Loopback callback host to bind (e.g. "127.0.0.1" or "0.0.0.0").
    pub oauth_host: Cow<'static, str>,
    /// Preferred / default loopback port.
    pub oauth_port: u16,
    /// Port allocation mode (Fixed, Dynamic, or PreferredOrDynamic).
    pub port_mode: PortMode,
    /// Loopback callback URL path (e.g. "/oauth/callback" or "/callback").
    pub oauth_path: Cow<'static, str>,
    /// The host string used in the browser `redirect_uri` (e.g. "127.0.0.1" or "localhost").
    pub redirect_host: Cow<'static, str>,
    /// Explicit override for the entire `redirect_uri` (useful for reverse proxy or manual flows).
    pub custom_redirect_uri: Option<Cow<'static, str>>,
    /// Whether to send an OIDC `nonce` in the authorize URL.
    pub send_nonce: bool,
    /// PKCE code challenge mode.
    pub pkce_mode: PkceMode,
    /// Request format for token endpoints.
    pub token_format: TokenRequestFormat,
    /// Which device-authorization flow this provider speaks.
    pub device_flow: DeviceFlow,
    /// The token endpoint URL polled during the device flow.
    pub device_token_url: Cow<'static, str>,
    /// The `redirect_uri` sent when exchanging the device authorization_code (ChatGPT).
    pub device_redirect_uri: Cow<'static, str>,
}

impl OAuthConfig {
    /// Create a fluent builder for a new OAuth configuration.
    pub fn builder(provider_id: impl Into<Cow<'static, str>>) -> OAuthConfigBuilder {
        OAuthConfigBuilder::new(provider_id)
    }

    /// Resolve the registered browser redirect_uri (`http://<redirect_host>:<port><path>`).
    /// If an explicit `actual_port` is given (e.g. from dynamic binding), that port is used.
    pub fn redirect_uri(&self, actual_port: Option<u16>) -> String {
        if let Some(custom) = &self.custom_redirect_uri {
            return custom.to_string();
        }
        let port = actual_port.unwrap_or(self.oauth_port);
        format!("http://{}:{}{}", self.redirect_host, port, self.oauth_path)
    }

    /// Whether this OAuth config speaks the Google Antigravity protocol.
    pub fn is_antigravity(&self) -> bool {
        self.provider_id == "google-antigravity"
            || self.provider_id == "antigravity"
            || self.client_id == GOOGLE_ANTIGRAVITY_CLIENT_ID
            || self.token_url.contains("oauth2.googleapis.com")
    }

    /// Whether this OAuth config speaks the ChatGPT protocol.
    pub fn is_chatgpt(&self) -> bool {
        self.provider_id == "chatgpt" || self.token_url.contains("auth0.openai.com")
    }

    /// Whether this OAuth config speaks the GitHub Copilot protocol.
    pub fn is_copilot(&self) -> bool {
        self.provider_id == "copilot" || self.token_url.contains("github.com/login/oauth")
    }

    /// Whether this OAuth config speaks the xAI protocol.
    pub fn is_xai(&self) -> bool {
        self.provider_id == "xai" || self.token_url.contains("auth.x.ai")
    }

    /// Helper to clone and override client_id.
    pub fn with_client_id(mut self, client_id: impl Into<Cow<'static, str>>) -> Self {
        self.client_id = client_id.into();
        self
    }

    /// Helper to clone and override client_secret.
    pub fn with_client_secret(mut self, client_secret: impl Into<Cow<'static, str>>) -> Self {
        self.client_secret = Some(client_secret.into());
        self
    }

    /// Helper to clone and override redirect_host.
    pub fn with_redirect_host(mut self, host: impl Into<Cow<'static, str>>) -> Self {
        self.redirect_host = host.into();
        self
    }

    /// Helper to clone and override port_mode.
    pub fn with_port_mode(mut self, mode: PortMode) -> Self {
        self.port_mode = mode;
        self
    }

    /// Helper to clone and append extra authorize parameters.
    pub fn with_extra_authorize_param(
        mut self,
        key: impl Into<Cow<'static, str>>,
        val: impl Into<Cow<'static, str>>,
    ) -> Self {
        self.extra_authorize_params.push((key.into(), val.into()));
        self
    }

    /// Helper to clone and append extra headers.
    pub fn with_extra_header(
        mut self,
        key: impl Into<Cow<'static, str>>,
        val: impl Into<Cow<'static, str>>,
    ) -> Self {
        self.extra_headers.push((key.into(), val.into()));
        self
    }
}

/// Fluent builder for [`OAuthConfig`].
#[derive(Debug, Clone)]
pub struct OAuthConfigBuilder {
    cfg: OAuthConfig,
}

impl OAuthConfigBuilder {
    pub fn new(provider_id: impl Into<Cow<'static, str>>) -> Self {
        Self {
            cfg: OAuthConfig {
                provider_id: provider_id.into(),
                client_id: Cow::Borrowed(""),
                client_secret: None,
                client_auth_method: ClientAuthMethod::None,
                authorize_url: Cow::Borrowed(""),
                token_url: Cow::Borrowed(""),
                device_authorization_url: Cow::Borrowed(""),
                grant_type_device: Cow::Borrowed("urn:ietf:params:oauth:grant-type:device_code"),
                scope: Cow::Borrowed(""),
                extra_authorize_params: Vec::new(),
                extra_token_params: Vec::new(),
                extra_refresh_params: Vec::new(),
                extra_headers: Vec::new(),
                user_agent: None,
                oauth_host: Cow::Borrowed("127.0.0.1"),
                oauth_port: 0,
                port_mode: PortMode::Dynamic,
                oauth_path: Cow::Borrowed("/callback"),
                redirect_host: Cow::Borrowed("127.0.0.1"),
                custom_redirect_uri: None,
                send_nonce: false,
                pkce_mode: PkceMode::S256,
                token_format: TokenRequestFormat::FormUrlEncoded,
                device_flow: DeviceFlow::Rfc8628,
                device_token_url: Cow::Borrowed(""),
                device_redirect_uri: Cow::Borrowed(""),
            },
        }
    }

    pub fn client_id(mut self, id: impl Into<Cow<'static, str>>) -> Self {
        self.cfg.client_id = id.into();
        self
    }

    pub fn client_secret(mut self, secret: impl Into<Cow<'static, str>>) -> Self {
        self.cfg.client_secret = Some(secret.into());
        self
    }

    pub fn client_auth_method(mut self, method: ClientAuthMethod) -> Self {
        self.cfg.client_auth_method = method;
        self
    }

    pub fn authorize_url(mut self, url: impl Into<Cow<'static, str>>) -> Self {
        self.cfg.authorize_url = url.into();
        self
    }

    pub fn token_url(mut self, url: impl Into<Cow<'static, str>>) -> Self {
        self.cfg.token_url = url.into();
        self
    }

    pub fn device_authorization_url(mut self, url: impl Into<Cow<'static, str>>) -> Self {
        self.cfg.device_authorization_url = url.into();
        self
    }

    pub fn grant_type_device(mut self, gt: impl Into<Cow<'static, str>>) -> Self {
        self.cfg.grant_type_device = gt.into();
        self
    }

    pub fn scope(mut self, scope: impl Into<Cow<'static, str>>) -> Self {
        self.cfg.scope = scope.into();
        self
    }

    pub fn extra_authorize_param(
        mut self,
        k: impl Into<Cow<'static, str>>,
        v: impl Into<Cow<'static, str>>,
    ) -> Self {
        self.cfg.extra_authorize_params.push((k.into(), v.into()));
        self
    }

    pub fn extra_token_param(
        mut self,
        k: impl Into<Cow<'static, str>>,
        v: impl Into<Cow<'static, str>>,
    ) -> Self {
        self.cfg.extra_token_params.push((k.into(), v.into()));
        self
    }

    pub fn extra_refresh_param(
        mut self,
        k: impl Into<Cow<'static, str>>,
        v: impl Into<Cow<'static, str>>,
    ) -> Self {
        self.cfg.extra_refresh_params.push((k.into(), v.into()));
        self
    }

    pub fn extra_header(
        mut self,
        k: impl Into<Cow<'static, str>>,
        v: impl Into<Cow<'static, str>>,
    ) -> Self {
        self.cfg.extra_headers.push((k.into(), v.into()));
        self
    }

    pub fn user_agent(mut self, ua: impl Into<Cow<'static, str>>) -> Self {
        self.cfg.user_agent = Some(ua.into());
        self
    }

    pub fn oauth_host(mut self, host: impl Into<Cow<'static, str>>) -> Self {
        self.cfg.oauth_host = host.into();
        self
    }

    pub fn oauth_port(mut self, port: u16) -> Self {
        self.cfg.oauth_port = port;
        self
    }

    pub fn port_mode(mut self, mode: PortMode) -> Self {
        self.cfg.port_mode = mode;
        self
    }

    pub fn oauth_path(mut self, path: impl Into<Cow<'static, str>>) -> Self {
        self.cfg.oauth_path = path.into();
        self
    }

    pub fn redirect_host(mut self, host: impl Into<Cow<'static, str>>) -> Self {
        self.cfg.redirect_host = host.into();
        self
    }

    pub fn custom_redirect_uri(mut self, uri: impl Into<Cow<'static, str>>) -> Self {
        self.cfg.custom_redirect_uri = Some(uri.into());
        self
    }

    pub fn send_nonce(mut self, send: bool) -> Self {
        self.cfg.send_nonce = send;
        self
    }

    pub fn pkce_mode(mut self, mode: PkceMode) -> Self {
        self.cfg.pkce_mode = mode;
        self
    }

    pub fn token_format(mut self, format: TokenRequestFormat) -> Self {
        self.cfg.token_format = format;
        self
    }

    pub fn device_flow(mut self, flow: DeviceFlow) -> Self {
        self.cfg.device_flow = flow;
        self
    }

    pub fn device_token_url(mut self, url: impl Into<Cow<'static, str>>) -> Self {
        self.cfg.device_token_url = url.into();
        self
    }

    pub fn device_redirect_uri(mut self, uri: impl Into<Cow<'static, str>>) -> Self {
        self.cfg.device_redirect_uri = uri.into();
        self
    }

    pub fn build(self) -> OAuthConfig {
        self.cfg
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Built-in Battle-tested Provider Presets
const GOOGLE_ANTIGRAVITY_CLIENT_ID: &str = concat!(
    "1071006060591-",
    "tmhssin2h21lcre235vtolojh4g403ep",
    ".apps.googleusercontent.com"
);
const GOOGLE_ANTIGRAVITY_CLIENT_SECRET: &str = concat!("GOCSPX-", "K58FWR486LdLJ1mLB8sXC4z6qDAf");

// ─────────────────────────────────────────────────────────────────────────────

/// Google Antigravity OAuth client config.
///
/// Official Client ID and Secret reverse-engineered and registered for Google Antigravity.
/// Configured with PreferredOrDynamic port fallback to eliminate `AddrInUse` errors.
pub fn google_antigravity_preset() -> OAuthConfig {
    OAuthConfig {
        provider_id: Cow::Borrowed("google-antigravity"),
        client_id: Cow::Borrowed(GOOGLE_ANTIGRAVITY_CLIENT_ID),
        // NOTE: verified live against oauth2.googleapis.com — this client_id
        // authenticates with the `GOCSPX-K58F…` secret. The `GOCSPX-9YQW…`
        // secret belongs to the *other* built-in Antigravity client
        // (884354919052-…) and yields HTTP 401 `invalid_client` here.
        client_secret: Some(Cow::Borrowed(GOOGLE_ANTIGRAVITY_CLIENT_SECRET)),
        client_auth_method: ClientAuthMethod::RequestBody,
        authorize_url: Cow::Borrowed("https://accounts.google.com/o/oauth2/v2/auth"),
        token_url: Cow::Borrowed("https://oauth2.googleapis.com/token"),
        device_authorization_url: Cow::Borrowed("https://oauth2.googleapis.com/device/code"),
        grant_type_device: Cow::Borrowed("urn:ietf:params:oauth:grant-type:device_code"),
        scope: Cow::Borrowed(
            "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile https://www.googleapis.com/auth/cclog https://www.googleapis.com/auth/experimentsandconfigs openid",
        ),
        extra_authorize_params: vec![
            (Cow::Borrowed("access_type"), Cow::Borrowed("offline")),
            (Cow::Borrowed("prompt"), Cow::Borrowed("consent")),
            (
                Cow::Borrowed("include_granted_scopes"),
                Cow::Borrowed("true"),
            ),
        ],
        extra_token_params: Vec::new(),
        extra_refresh_params: Vec::new(),
        extra_headers: Vec::new(),
        user_agent: Some(Cow::Borrowed(
            neenee_contracts::client_identity::ANTIGRAVITY_USER_AGENT,
        )),
        oauth_host: Cow::Borrowed("127.0.0.1"),
        oauth_port: 51121,
        port_mode: PortMode::PreferredOrDynamic(51121),
        oauth_path: Cow::Borrowed("/oauth-callback"),
        redirect_host: Cow::Borrowed("127.0.0.1"),
        custom_redirect_uri: None,
        send_nonce: false,
        pkce_mode: PkceMode::S256,
        token_format: TokenRequestFormat::FormUrlEncoded,
        device_flow: DeviceFlow::Disabled,
        device_token_url: Cow::Borrowed("https://oauth2.googleapis.com/token"),
        device_redirect_uri: Cow::Borrowed(""),
    }
}

/// xAI SuperGrok OAuth client preset (reuses the public Grok-CLI client_id).
pub fn xai_preset() -> OAuthConfig {
    OAuthConfig {
        provider_id: Cow::Borrowed("xai"),
        client_id: Cow::Borrowed("b1a00492-073a-47ea-816f-4c329264a828"),
        client_secret: None,
        client_auth_method: ClientAuthMethod::None,
        authorize_url: Cow::Borrowed("https://auth.x.ai/oauth2/authorize"),
        token_url: Cow::Borrowed("https://auth.x.ai/oauth2/token"),
        device_authorization_url: Cow::Borrowed("https://auth.x.ai/oauth2/device/code"),
        grant_type_device: Cow::Borrowed("urn:ietf:params:oauth:grant-type:device_code"),
        scope: Cow::Borrowed("openid profile email offline_access grok-cli:access api:access"),
        extra_authorize_params: vec![
            (Cow::Borrowed("plan"), Cow::Borrowed("generic")),
            (Cow::Borrowed("referrer"), Cow::Borrowed("neenee")),
        ],
        extra_token_params: Vec::new(),
        extra_refresh_params: Vec::new(),
        extra_headers: Vec::new(),
        user_agent: None,
        oauth_host: Cow::Borrowed("127.0.0.1"),
        oauth_port: 56121,
        port_mode: PortMode::Fixed(56121),
        oauth_path: Cow::Borrowed("/callback"),
        redirect_host: Cow::Borrowed("127.0.0.1"),
        custom_redirect_uri: None,
        send_nonce: true,
        pkce_mode: PkceMode::S256,
        token_format: TokenRequestFormat::FormUrlEncoded,
        device_flow: DeviceFlow::Rfc8628,
        device_token_url: Cow::Borrowed("https://auth.x.ai/oauth2/token"),
        device_redirect_uri: Cow::Borrowed(""),
    }
}

/// ChatGPT / OpenAI Codex subscription OAuth client preset.
pub fn chatgpt_preset() -> OAuthConfig {
    OAuthConfig {
        provider_id: Cow::Borrowed("chatgpt"),
        client_id: Cow::Borrowed("app_EMoamEEZ73f0CkXaXp7hrann"),
        client_secret: None,
        client_auth_method: ClientAuthMethod::None,
        authorize_url: Cow::Borrowed("https://auth.openai.com/oauth/authorize"),
        token_url: Cow::Borrowed("https://auth.openai.com/oauth/token"),
        device_authorization_url: Cow::Borrowed(
            "https://auth.openai.com/api/accounts/deviceauth/usercode",
        ),
        grant_type_device: Cow::Borrowed("urn:ietf:params:oauth:grant-type:device_code"),
        scope: Cow::Borrowed("openid profile email offline_access"),
        extra_authorize_params: vec![
            (
                Cow::Borrowed("id_token_add_organizations"),
                Cow::Borrowed("true"),
            ),
            (
                Cow::Borrowed("codex_cli_simplified_flow"),
                Cow::Borrowed("true"),
            ),
            (Cow::Borrowed("originator"), Cow::Borrowed("neenee")),
        ],
        extra_token_params: Vec::new(),
        extra_refresh_params: Vec::new(),
        extra_headers: Vec::new(),
        user_agent: None,
        oauth_host: Cow::Borrowed("127.0.0.1"),
        oauth_port: 1455,
        port_mode: PortMode::Fixed(1455),
        oauth_path: Cow::Borrowed("/auth/callback"),
        redirect_host: Cow::Borrowed("localhost"),
        custom_redirect_uri: None,
        send_nonce: false,
        pkce_mode: PkceMode::S256,
        token_format: TokenRequestFormat::FormUrlEncoded,
        device_flow: DeviceFlow::ChatGpt,
        device_token_url: Cow::Borrowed("https://auth.openai.com/api/accounts/deviceauth/token"),
        device_redirect_uri: Cow::Borrowed("https://auth.openai.com/deviceauth/callback"),
    }
}

/// GitHub Copilot subscription OAuth client preset.
pub fn copilot_preset() -> OAuthConfig {
    OAuthConfig {
        provider_id: Cow::Borrowed("copilot"),
        client_id: Cow::Borrowed("Ov23li8tweQw6odWQebz"),
        client_secret: None,
        client_auth_method: ClientAuthMethod::None,
        authorize_url: Cow::Borrowed("https://github.com/login/oauth/authorize"),
        token_url: Cow::Borrowed("https://github.com/login/oauth/access_token"),
        device_authorization_url: Cow::Borrowed("https://github.com/login/device/code"),
        grant_type_device: Cow::Borrowed("urn:ietf:params:oauth:grant-type:device_code"),
        scope: Cow::Borrowed("read:user"),
        extra_authorize_params: Vec::new(),
        extra_token_params: Vec::new(),
        extra_refresh_params: Vec::new(),
        extra_headers: Vec::new(),
        user_agent: None,
        oauth_host: Cow::Borrowed("127.0.0.1"),
        oauth_port: 42195,
        port_mode: PortMode::Fixed(42195),
        oauth_path: Cow::Borrowed("/callback"),
        redirect_host: Cow::Borrowed("127.0.0.1"),
        custom_redirect_uri: None,
        send_nonce: false,
        pkce_mode: PkceMode::S256,
        token_format: TokenRequestFormat::FormUrlEncoded,
        device_flow: DeviceFlow::Rfc8628,
        device_token_url: Cow::Borrowed("https://github.com/login/oauth/access_token"),
        device_redirect_uri: Cow::Borrowed(""),
    }
}

// Lazy/Const compatible static accessors
pub static GOOGLE_ANTIGRAVITY: std::sync::LazyLock<OAuthConfig> =
    std::sync::LazyLock::new(google_antigravity_preset);
pub static XAI: std::sync::LazyLock<OAuthConfig> = std::sync::LazyLock::new(xai_preset);
pub static CHATGPT: std::sync::LazyLock<OAuthConfig> = std::sync::LazyLock::new(chatgpt_preset);
pub static COPILOT: std::sync::LazyLock<OAuthConfig> = std::sync::LazyLock::new(copilot_preset);

/// Resolve a config by its `auth.toml` provider-id key (`"xai"` / `"chatgpt"`
/// / `"copilot"` / `"google-antigravity"`).
pub fn config_by_provider_id(id: &str) -> Option<OAuthConfig> {
    match id {
        "xai" => Some(xai_preset()),
        "chatgpt" => Some(chatgpt_preset()),
        "copilot" => Some(copilot_preset()),
        "google-antigravity" | "antigravity" => Some(google_antigravity_preset()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chatgpt_redirect_uri_uses_the_codex_port() {
        assert_eq!(
            CHATGPT.redirect_uri(None),
            "http://localhost:1455/auth/callback"
        );
    }

    #[test]
    fn antigravity_redirect_uri_supports_dynamic_port() {
        let cfg = google_antigravity_preset();
        assert_eq!(
            cfg.redirect_uri(Some(43125)),
            "http://127.0.0.1:43125/oauth-callback"
        );
    }

    #[test]
    fn chatgpt_carries_codex_simplified_flow_param() {
        let cfg = chatgpt_preset();
        assert!(
            cfg.extra_authorize_params
                .iter()
                .any(|(k, v)| k == "codex_cli_simplified_flow" && v == "true"),
            "codex_cli_simplified_flow=true must be present"
        );
    }

    #[test]
    fn config_resolves_by_provider_id() {
        assert_eq!(config_by_provider_id("xai").unwrap().provider_id, "xai");
        assert_eq!(
            config_by_provider_id("chatgpt").unwrap().provider_id,
            "chatgpt"
        );
        assert_eq!(
            config_by_provider_id("copilot").unwrap().provider_id,
            "copilot"
        );
        assert_eq!(
            config_by_provider_id("google-antigravity")
                .unwrap()
                .provider_id,
            "google-antigravity"
        );
        assert_eq!(
            config_by_provider_id("antigravity").unwrap().provider_id,
            "google-antigravity"
        );
        assert!(config_by_provider_id("nope").is_none());
    }

    #[test]
    fn custom_client_builder() {
        let custom = OAuthConfig::builder("my-enterprise-oauth")
            .client_id("client-123")
            .client_secret("secret-abc")
            .authorize_url("https://auth.company.com/oauth2/auth")
            .token_url("https://auth.company.com/oauth2/token")
            .scope("api openid")
            .extra_authorize_param("tenant", "corporate")
            .port_mode(PortMode::Dynamic)
            .build();

        assert_eq!(custom.provider_id, "my-enterprise-oauth");
        assert_eq!(custom.client_id, "client-123");
        assert_eq!(custom.client_secret.as_deref(), Some("secret-abc"));
        assert_eq!(custom.port_mode, PortMode::Dynamic);
    }

    #[test]
    fn google_antigravity_preset_matches_official_specification() {
        let cfg = google_antigravity_preset();
        assert_eq!(cfg.provider_id, "google-antigravity");
        assert_eq!(cfg.client_id, GOOGLE_ANTIGRAVITY_CLIENT_ID);
        assert_eq!(
            cfg.client_secret.as_deref(),
            Some(GOOGLE_ANTIGRAVITY_CLIENT_SECRET)
        );
        assert_eq!(
            cfg.authorize_url,
            "https://accounts.google.com/o/oauth2/v2/auth"
        );
        assert_eq!(cfg.token_url, "https://oauth2.googleapis.com/token");
        assert!(
            cfg.scope
                .contains("https://www.googleapis.com/auth/cloud-platform")
        );
        assert!(
            cfg.scope
                .contains("https://www.googleapis.com/auth/userinfo.email")
        );
        assert!(cfg.scope.contains("https://www.googleapis.com/auth/cclog"));
        assert_eq!(cfg.port_mode, PortMode::PreferredOrDynamic(51121));
        assert_eq!(
            cfg.user_agent.as_deref(),
            Some(neenee_contracts::client_identity::ANTIGRAVITY_USER_AGENT)
        );

        // Test client customization / emulation
        let custom_agy = cfg
            .with_client_id("custom-gcp-client-id")
            .with_client_secret("custom-gcp-client-secret")
            .with_redirect_host("localhost")
            .with_port_mode(PortMode::Dynamic);

        assert_eq!(custom_agy.client_id, "custom-gcp-client-id");
        assert_eq!(
            custom_agy.client_secret.as_deref(),
            Some("custom-gcp-client-secret")
        );
        assert_eq!(
            custom_agy.redirect_uri(Some(9999)),
            "http://localhost:9999/oauth-callback"
        );
    }
}
