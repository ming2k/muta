//! Per-provider OAuth2 client configuration.
//!
//! neenee's OAuth providers (xAI SuperGrok, ChatGPT/Codex subscription) share
//! the same mechanics — PKCE browser flow, token exchange, refresh, JWT-exp
//! freshness — and differ only in their registered client constants
//! (`client_id`, endpoints, scopes, redirect port). [`OAuthConfig`] captures
//! those constants as a compile-time value so the generic flows in [`token`],
//! [`browser`], and [`device`] can serve any provider from one implementation.
//!
//! The ChatGPT device flow is structurally distinct from RFC 8628 (JSON bodies,
//! a two-step authorization_code → token exchange), so [`DeviceFlow`] lets the
//! orchestrator pick the right device implementation per provider.

/// Which device-authorization flow a provider speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceFlow {
    /// Standard RFC 8628: form-urlencoded request + poll, the polled token
    /// endpoint returns access tokens directly (xAI).
    Rfc8628,
    /// OpenAI/ChatGPT: JSON bodies; the poll endpoint returns an
    /// `authorization_code` + `code_verifier` that are then exchanged at the
    /// `/oauth/token` endpoint for the token set.
    ChatGpt,
}

/// Static OAuth2 client configuration for one provider. Every field is a
/// compile-time constant — it is part of the provider's registered OAuth
/// client and never varies at runtime.
#[derive(Debug, Clone, Copy)]
pub struct OAuthConfig {
    /// `auth.toml` key under which this provider's tokens persist.
    pub provider_id: &'static str,
    /// Public OAuth client id registered with the provider.
    pub client_id: &'static str,
    /// Authorization endpoint (consent screen).
    pub authorize_url: &'static str,
    /// Token endpoint (code exchange + refresh).
    pub token_url: &'static str,
    /// Device-authorization endpoint (request the user_code).
    pub device_authorization_url: &'static str,
    /// RFC 8628 `grant_type` value sent during the device poll.
    pub grant_type_device: &'static str,
    /// OAuth scopes requested.
    pub scope: &'static str,
    /// Extra query params appended to the authorize URL (provider-specific
    /// quirks like xAI's `plan=generic` or OpenAI's `codex_cli_simplified_flow`).
    pub extra_authorize_params: &'static [(&'static str, &'static str)],
    /// Loopback callback host:port:path. The server binds `oauth_host`
    /// (always `127.0.0.1` — IPv4 loopback, works everywhere), while
    /// [`redirect_host`](Self::redirect_host) is the host string sent in the
    /// `redirect_uri` to the provider. These differ for OpenAI, which
    /// registered its Codex client against `http://localhost:1455/...` (not
    /// `127.0.0.1`); a `redirect_uri` host mismatch yields "Invalid authorize
    /// request". `localhost` resolves to `127.0.0.1`, so the bound listener
    /// still receives the browser callback.
    pub oauth_host: &'static str,
    pub oauth_port: u16,
    pub oauth_path: &'static str,
    /// The host string used in the browser `redirect_uri`. Usually equal to
    /// `oauth_host`, except for OpenAI where it MUST be `localhost` to match
    /// the registered Codex redirect.
    pub redirect_host: &'static str,
    /// Whether to send an OIDC `nonce` in the authorize URL. xAI's flow accepts
    /// it; OpenAI's `codex_cli_simplified_flow` rejects the request when an
    /// unexpected `nonce` is present.
    pub send_nonce: bool,
    /// Which device-authorization flow this provider speaks.
    pub device_flow: DeviceFlow,
    /// The token endpoint URL polled during the device flow. For [`DeviceFlow::
    /// Rfc8628`] this equals [`token_url`](Self::token_url); for ChatGPT it is
    /// the `deviceauth/token` endpoint that yields an authorization_code.
    pub device_token_url: &'static str,
    /// The `redirect_uri` sent when exchanging the device authorization_code
    /// (ChatGPT device flow). Unused for RFC 8628.
    pub device_redirect_uri: &'static str,
}

impl OAuthConfig {
    /// The exact registered browser redirect_uri (`http://<redirect_host>:<port><path>`).
    pub fn redirect_uri(&self) -> String {
        format!(
            "http://{}:{}{}",
            self.redirect_host, self.oauth_port, self.oauth_path
        )
    }
}

/// xAI SuperGrok OAuth client (reuses the public Grok-CLI client_id).
pub const XAI: OAuthConfig = OAuthConfig {
    provider_id: "xai",
    client_id: "b1a00492-073a-47ea-816f-4c329264a828",
    authorize_url: "https://auth.x.ai/oauth2/authorize",
    token_url: "https://auth.x.ai/oauth2/token",
    device_authorization_url: "https://auth.x.ai/oauth2/device/code",
    grant_type_device: "urn:ietf:params:oauth:grant-type:device_code",
    scope: "openid profile email offline_access grok-cli:access api:access",
    extra_authorize_params: &[("plan", "generic"), ("referrer", "neenee")],
    oauth_host: "127.0.0.1",
    oauth_port: 56121,
    oauth_path: "/callback",
    redirect_host: "127.0.0.1",
    send_nonce: true,
    device_flow: DeviceFlow::Rfc8628,
    device_token_url: "https://auth.x.ai/oauth2/token",
    device_redirect_uri: "",
};

/// ChatGPT / OpenAI Codex subscription OAuth client. Reuses the public Codex
/// CLI client_id (`app_EMoamEEZ73f0CkXaXp7hrann`) — the same one opencode and
/// the official Codex CLI ship — so `auth.openai.com` accepts the loopback
/// redirect. The inference backend
/// (`https://chatgpt.com/backend-api/codex/responses`) is wired in the catalog
/// layer, not here; this config covers only the OAuth dance.
pub const CHATGPT: OAuthConfig = OAuthConfig {
    provider_id: "chatgpt",
    client_id: "app_EMoamEEZ73f0CkXaXp7hrann",
    authorize_url: "https://auth.openai.com/oauth/authorize",
    token_url: "https://auth.openai.com/oauth/token",
    device_authorization_url: "https://auth.openai.com/api/accounts/deviceauth/usercode",
    grant_type_device: "urn:ietf:params:oauth:grant-type:device_code",
    scope: "openid profile email offline_access",
    extra_authorize_params: &[
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("originator", "neenee"),
    ],
    // The server binds 127.0.0.1, but the redirect_uri MUST carry `localhost`
    // to match OpenAI's registered Codex redirect exactly.
    oauth_host: "127.0.0.1",
    oauth_port: 1455,
    oauth_path: "/auth/callback",
    redirect_host: "localhost",
    // OpenAI's simplified flow rejects an unexpected `nonce`.
    send_nonce: false,
    device_flow: DeviceFlow::ChatGpt,
    device_token_url: "https://auth.openai.com/api/accounts/deviceauth/token",
    device_redirect_uri: "https://auth.openai.com/deviceauth/callback",
};

/// GitHub Copilot subscription OAuth client. neenee reuses the public
/// Copilot OAuth App client id (`Ov23li8tweQw6odWQebz`) that opencode and
/// several third-party Copilot integrations use. GitHub's Copilot backend
/// maintains a per-client-ID model allowlist, so a mismatched or self-registered
/// OAuth App often returns only the always-available GPT-4o family instead of
/// the account's real subscription models. The flow is plain RFC 8628 —
/// `read:user` scope is all Copilot's token endpoint needs to mint a
/// subscription-scoped token; the returned access token is sent verbatim as a
/// bearer to `api.githubcopilot.com` (the Responses backend URL is wired in the
/// catalog layer, not here). The token does not expire on a schedule (GitHub
/// returns no `expires_in`), so the refresh path is effectively a no-op — it
/// stays valid until the user revokes the app.
pub const COPILOT: OAuthConfig = OAuthConfig {
    provider_id: "copilot",
    client_id: "Ov23li8tweQw6odWQebz",
    authorize_url: "https://github.com/login/oauth/authorize",
    token_url: "https://github.com/login/oauth/access_token",
    device_authorization_url: "https://github.com/login/device/code",
    grant_type_device: "urn:ietf:params:oauth:grant-type:device_code",
    scope: "read:user",
    extra_authorize_params: &[],
    oauth_host: "127.0.0.1",
    // Unused by the device flow; kept consistent with the other configs. A
    // Copilot browser flow is not offered, so this port is never bound.
    oauth_port: 42195,
    oauth_path: "/callback",
    redirect_host: "127.0.0.1",
    send_nonce: false,
    device_flow: DeviceFlow::Rfc8628,
    device_token_url: "https://github.com/login/oauth/access_token",
    device_redirect_uri: "",
};

/// Google Antigravity OAuth client config. Public Client ID registered for Google Antigravity / Cloud SDK.
pub const GOOGLE_ANTIGRAVITY: OAuthConfig = OAuthConfig {
    provider_id: "google-antigravity",
    client_id: "1070200057404-36h00infrjh2h81p4g0t47a98v1a21qg.apps.googleusercontent.com",
    authorize_url: "https://accounts.google.com/o/oauth2/v2/auth",
    token_url: "https://oauth2.googleapis.com/token",
    device_authorization_url: "https://oauth2.googleapis.com/device/code",
    grant_type_device: "urn:ietf:params:oauth:grant-type:device_code",
    scope: "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email openid profile offline_access",
    extra_authorize_params: &[("access_type", "offline"), ("prompt", "consent")],
    oauth_host: "127.0.0.1",
    oauth_port: 51121,
    oauth_path: "/oauth/callback",
    redirect_host: "127.0.0.1",
    send_nonce: false,
    device_flow: DeviceFlow::Rfc8628,
    device_token_url: "https://oauth2.googleapis.com/token",
    device_redirect_uri: "",
};

/// Resolve a config by its `auth.toml` provider-id key (`"xai"` / `"chatgpt"`
/// / `"copilot"` / `"google-antigravity"`).
pub fn config_by_provider_id(id: &str) -> Option<&'static OAuthConfig> {
    match id {
        "xai" => Some(&XAI),
        "chatgpt" => Some(&CHATGPT),
        "copilot" => Some(&COPILOT),
        "google-antigravity" | "antigravity" => Some(&GOOGLE_ANTIGRAVITY),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chatgpt_redirect_uri_uses_the_codex_port() {
        // Must be `localhost` (not 127.0.0.1) to match OpenAI's registration.
        assert_eq!(
            CHATGPT.redirect_uri(),
            "http://localhost:1455/auth/callback"
        );
    }

    #[test]
    fn chatgpt_carries_codex_simplified_flow_param() {
        let params = CHATGPT.extra_authorize_params;
        assert!(
            params
                .iter()
                .any(|(k, v)| *k == "codex_cli_simplified_flow" && *v == "true"),
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
            config_by_provider_id("google-antigravity").unwrap().provider_id,
            "google-antigravity"
        );
        assert_eq!(
            config_by_provider_id("antigravity").unwrap().provider_id,
            "google-antigravity"
        );
        assert!(config_by_provider_id("nope").is_none());
    }

    #[test]
    fn copilot_speaks_rfc8628_with_read_user_scope() {
        // Copilot uses the standard device flow (not ChatGPT's JSON variant)
        // and the minimal scope the Copilot token endpoint requires.
        assert_eq!(COPILOT.device_flow, DeviceFlow::Rfc8628);
        assert_eq!(COPILOT.scope, "read:user");
        assert_eq!(
            COPILOT.device_authorization_url,
            "https://github.com/login/device/code"
        );
        // The polled token endpoint equals the regular token endpoint for
        // RFC 8628 (unlike ChatGPT, which polls a separate deviceauth/token).
        assert_eq!(COPILOT.device_token_url, COPILOT.token_url);
        // Use the public Copilot OAuth App client id so the backend allowlist
        // matches opencode and other community integrations.
        assert_eq!(COPILOT.client_id, "Ov23li8tweQw6odWQebz");
    }
}
