//! How a user-defined connection authenticates — the discriminating field that
//! lets a connection declare it resolves its bearer from OAuth (ChatGPT, Copilot,
//! Google Antigravity, xAI) rather than from an API key.
//!
//! Defined in `muta-contracts` (not `muta-persistence`) because domain events
//! carry it; the store depends on core, not the reverse. It round-trips through TOML,
//! so it derives [`serde`] like the other config-shaped domain enums.

use serde::{Deserialize, Serialize};

/// How a user-defined connection authenticates.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum ConnectionAuth {
    /// Bearer from `api_key_env` (env first) or inline `api_key`. The
    /// historical behavior — every provider except OAuth ones.
    #[default]
    ApiKey,
    /// xAI SuperGrok subscription: resolve the live OAuth access token from
    /// `auth.toml` (key `"xai"`), refreshed at activate/switch time (see
    /// `muta_providers::oauth`). Any user provider connection may set this; the
    /// catalog always reads the shared xAI token set.
    XaiOAuth,
    /// ChatGPT/Codex subscription: resolve the live OAuth access token from
    /// `auth.toml` (key `"chatgpt"`) and the `chatgpt_account_id`, then route
    /// inference to the Responses backend
    /// (`https://chatgpt.com/backend-api/codex/responses`). Refreshed at
    /// activate/switch time.
    ChatGptOAuth,
    /// GitHub Copilot subscription: resolve the live OAuth access token from
    /// `auth.toml` (key `"copilot"`) and route inference to the Copilot
    /// Responses backend (`https://api.githubcopilot.com/responses`).
    CopilotOAuth,
    /// Google Antigravity subscription: resolve the live OAuth access token from
    /// `auth.toml` (key `"google-antigravity"`), refreshed at activate/switch
    /// time. Connects Google Antigravity models over native Google REST.
    AntigravityOAuth,
}

/// Backwards-compatible alias for [`ConnectionAuth`].
pub type ChannelAuth = ConnectionAuth;

impl ConnectionAuth {
    /// Whether this variant resolves its bearer from the OAuth token store
    /// rather than from an API key. Covers every subscription/OAuth provider.
    pub fn is_oauth(self) -> bool {
        matches!(
            self,
            ConnectionAuth::XaiOAuth
                | ConnectionAuth::ChatGptOAuth
                | ConnectionAuth::CopilotOAuth
                | ConnectionAuth::AntigravityOAuth
        )
    }

    /// Whether this variant is Google Antigravity OAuth.
    pub fn is_antigravity(self) -> bool {
        matches!(self, ConnectionAuth::AntigravityOAuth)
    }

    /// Whether this variant is ChatGPT OAuth.
    pub fn is_chatgpt(self) -> bool {
        matches!(self, ConnectionAuth::ChatGptOAuth)
    }

    /// Whether this variant is GitHub Copilot OAuth.
    pub fn is_copilot(self) -> bool {
        matches!(self, ConnectionAuth::CopilotOAuth)
    }

    /// Whether this variant is xAI OAuth.
    pub fn is_xai(self) -> bool {
        matches!(self, ConnectionAuth::XaiOAuth)
    }

    /// The `auth.toml` provider-id key for this OAuth variant, or `None` for
    /// API-key connections. Used to load/refresh the shared token set.
    pub fn oauth_provider_id(self) -> Option<&'static str> {
        match self {
            ConnectionAuth::XaiOAuth => Some("xai"),
            ConnectionAuth::ChatGptOAuth => Some("chatgpt"),
            ConnectionAuth::CopilotOAuth => Some("copilot"),
            ConnectionAuth::AntigravityOAuth => Some("google-antigravity"),
            ConnectionAuth::ApiKey => None,
        }
    }

    /// The default login flow for this OAuth provider.
    ///
    /// Returns `None` for API-key connections (no OAuth login to run).
    pub fn default_login_method(self) -> Option<LoginMethod> {
        match self {
            ConnectionAuth::ChatGptOAuth | ConnectionAuth::AntigravityOAuth => {
                Some(LoginMethod::Browser)
            }
            ConnectionAuth::XaiOAuth | ConnectionAuth::CopilotOAuth => Some(LoginMethod::Device),
            ConnectionAuth::ApiKey => None,
        }
    }
}

/// Which OAuth login flow to run. Carried by [`crate::events::AgentRequest::
/// ConnectProvider`] so the TUI picks the method, not the harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum LoginMethod {
    /// RFC 8628 device-code grant — headless / VPS / SSH / Docker. The default:
    /// works anywhere, prints a URL + short code the user enters on any device.
    #[default]
    Device,
    /// Browser loopback OAuth — local desktop. Binds `127.0.0.1:56121` and
    /// opens the authorize URL.
    Browser,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_providers_choose_a_registration_supported_default() {
        // ChatGPT and Antigravity register localhost callbacks and prefer the
        // desktop PKCE flow. Copilot's public application is device-only; xAI
        // keeps device authorization as its portable default.
        assert_eq!(
            ConnectionAuth::CopilotOAuth.default_login_method(),
            Some(LoginMethod::Device)
        );
        assert_eq!(
            ConnectionAuth::ChatGptOAuth.default_login_method(),
            Some(LoginMethod::Browser)
        );
        assert_eq!(
            ConnectionAuth::XaiOAuth.default_login_method(),
            Some(LoginMethod::Device)
        );
    }

    #[test]
    fn api_key_connections_have_no_login_method() {
        assert_eq!(ConnectionAuth::ApiKey.default_login_method(), None);
    }
}
