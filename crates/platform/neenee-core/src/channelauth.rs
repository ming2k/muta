//! How a user-defined channel authenticates — the discriminating field that
//! lets a channel declare it resolves its bearer from OAuth (xAI SuperGrok)
//! rather than from an API key.
//!
//! Defined in `neenee-core` (not `neenee-store`) because the
//! [`crate::events::AgentRequest::AddProvider`] domain event carries it; the
//! store depends on core, not the reverse. It round-trips through TOML, so it
//! derives [`serde`] like the other config-shaped domain enums.

use serde::{Deserialize, Serialize};

/// How a user-defined channel authenticates.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChannelAuth {
    /// Bearer from `api_key_env` (env first) or inline `api_key`. The
    /// historical behavior — every provider except OAuth ones.
    #[default]
    ApiKey,
    /// xAI SuperGrok subscription: resolve the live OAuth access token from
    /// `auth.toml` (key `"xai"`), refreshed at activate/switch time (see
    /// `neenee_auth`). Any user provider channel may set this; the catalog
    /// always reads the shared xAI token set.
    XaiOAuth,
    /// ChatGPT/Codex subscription: resolve the live OAuth access token from
    /// `auth.toml` (key `"chatgpt"`) and the `chatgpt_account_id`, then route
    /// inference to the Responses backend
    /// (`https://chatgpt.com/backend-api/codex/responses`). Refreshed at
    /// activate/switch time.
    ChatGptOAuth,
    /// GitHub Copilot subscription: resolve the live OAuth access token from
    /// `auth.toml` (key `"copilot"`) and route inference to the Copilot
    /// Responses backend (`https://api.githubcopilot.com/responses`). The token
    /// is the GitHub OAuth access token from the RFC 8628 device flow; it does
    /// not expire on a schedule, so the refresh path is a no-op until the user
    /// revokes the app. Copilot-specific request headers
    /// (`x-initiator`, `Openai-Intent`, `X-GitHub-Api-Version`) are injected by
    /// the Responses provider when it detects this auth mode.
    CopilotOAuth,
}

impl ChannelAuth {
    /// Whether this variant resolves its bearer from the OAuth token store
    /// rather than from an API key. Covers every subscription/OAuth provider.
    pub fn is_oauth(self) -> bool {
        matches!(
            self,
            ChannelAuth::XaiOAuth | ChannelAuth::ChatGptOAuth | ChannelAuth::CopilotOAuth
        )
    }

    /// The `auth.toml` provider-id key for this OAuth variant, or `None` for
    /// API-key channels. Used to load/refresh the shared token set.
    pub fn oauth_provider_id(self) -> Option<&'static str> {
        match self {
            ChannelAuth::XaiOAuth => Some("xai"),
            ChannelAuth::ChatGptOAuth => Some("chatgpt"),
            ChannelAuth::CopilotOAuth => Some("copilot"),
            ChannelAuth::ApiKey => None,
        }
    }

    /// The default login flow for this OAuth provider. **Device flow is the
    /// default** for every subscription provider: it works headless (SSH, VPS,
    /// Docker) and needs no registered browser callback URL, so it cannot hit
    /// "redirect_uri is not associated with this application". The browser flow
    /// remains available as an opt-in only when the provider's callback is
    /// registered and the user is on a desktop with a reachable loopback port.
    ///
    /// Returns `None` for API-key channels (no OAuth login to run).
    pub fn default_login_method(self) -> Option<LoginMethod> {
        match self {
            ChannelAuth::XaiOAuth | ChannelAuth::ChatGptOAuth | ChannelAuth::CopilotOAuth => {
                Some(LoginMethod::Device)
            }
            ChannelAuth::ApiKey => None,
        }
    }
}

/// Which OAuth login flow to run. Carried by [`crate::events::AgentRequest::
/// ConnectProvider`] so the TUI picks the method, not the harness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
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
    fn oauth_providers_default_to_device_flow() {
        // The device flow is the universal default for every subscription
        // provider: it works headless and needs no registered callback URL, so
        // it cannot hit "redirect_uri is not associated with this application".
        // This guards against the TUI regressing back to a hardcoded browser
        // flow that breaks Copilot (whose callback is not a loopback URL).
        assert_eq!(
            ChannelAuth::CopilotOAuth.default_login_method(),
            Some(LoginMethod::Device)
        );
        assert_eq!(
            ChannelAuth::ChatGptOAuth.default_login_method(),
            Some(LoginMethod::Device)
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
