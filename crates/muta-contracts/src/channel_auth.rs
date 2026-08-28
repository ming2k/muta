//! How a user-defined channel authenticates — the discriminating field that
//! lets a channel declare it resolves its bearer from OAuth (xAI SuperGrok)
//! rather than from an API key.
//!
//! Defined in `muta-contracts` (not `muta-persistence`) because the
//! [`crate::events::AgentRequest::AddProvider`] domain event carries it; the
//! store depends on core, not the reverse. It round-trips through TOML, so it
//! derives [`serde`] like the other config-shaped domain enums.

use serde::{Deserialize, Serialize};

/// How a user-defined channel authenticates.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub enum ChannelAuth {
    /// Bearer from `api_key_env` (env first) or inline `api_key`. The
    /// historical behavior — every provider except OAuth ones.
    #[default]
    ApiKey,
    /// xAI SuperGrok subscription: resolve the live OAuth access token from
    /// `auth.toml` (key `"xai"`), refreshed at activate/switch time (see
    /// `muta_providers::oauth`). Any user provider channel may set this; the
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

impl ChannelAuth {
    /// Whether this variant resolves its bearer from the OAuth token store
    /// rather than from an API key. Covers every subscription/OAuth provider.
    pub fn is_oauth(self) -> bool {
        matches!(
            self,
            ChannelAuth::XaiOAuth
                | ChannelAuth::ChatGptOAuth
                | ChannelAuth::CopilotOAuth
                | ChannelAuth::AntigravityOAuth
        )
    }

    /// Whether this variant is Google Antigravity OAuth.
    pub fn is_antigravity(self) -> bool {
        matches!(self, ChannelAuth::AntigravityOAuth)
    }

    /// Whether this variant is ChatGPT OAuth.
    pub fn is_chatgpt(self) -> bool {
        matches!(self, ChannelAuth::ChatGptOAuth)
    }

    /// Whether this variant is GitHub Copilot OAuth.
    pub fn is_copilot(self) -> bool {
        matches!(self, ChannelAuth::CopilotOAuth)
    }

    /// Whether this variant is xAI OAuth.
    pub fn is_xai(self) -> bool {
        matches!(self, ChannelAuth::XaiOAuth)
    }

    /// The `auth.toml` provider-id key for this OAuth variant, or `None` for
    /// API-key channels. Used to load/refresh the shared token set.
    pub fn oauth_provider_id(self) -> Option<&'static str> {
        match self {
            ChannelAuth::XaiOAuth => Some("xai"),
            ChannelAuth::ChatGptOAuth => Some("chatgpt"),
            ChannelAuth::CopilotOAuth => Some("copilot"),
            ChannelAuth::AntigravityOAuth => Some("google-antigravity"),
            ChannelAuth::ApiKey => None,
        }
    }

    /// The default login flow for this OAuth provider.
    ///
    /// Returns `None` for API-key channels (no OAuth login to run).
    pub fn default_login_method(self) -> Option<LoginMethod> {
        match self {
            ChannelAuth::ChatGptOAuth | ChannelAuth::AntigravityOAuth => Some(LoginMethod::Browser),
            ChannelAuth::XaiOAuth | ChannelAuth::CopilotOAuth => Some(LoginMethod::Device),
            ChannelAuth::ApiKey => None,
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
            ChannelAuth::CopilotOAuth.default_login_method(),
            Some(LoginMethod::Device)
        );
        assert_eq!(
            ChannelAuth::ChatGptOAuth.default_login_method(),
            Some(LoginMethod::Browser)
        );
        assert_eq!(
            ChannelAuth::XaiOAuth.default_login_method(),
            Some(LoginMethod::Device)
        );
    }

    #[test]
    fn api_key_channels_have_no_login_method() {
        assert_eq!(ChannelAuth::ApiKey.default_login_method(), None);
    }
}
