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
