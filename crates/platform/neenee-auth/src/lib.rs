//! OAuth2 + PKCE authentication for providers that need it.
//!
//! neenee's API-key providers authenticate with a static key resolved from env
//! → credentials.toml → inline config. Subscription providers — xAI SuperGrok
//! and ChatGPT/Codex — authenticate via OAuth2: the user runs a login flow
//! (browser loopback OAuth on a desktop, or a device-code flow on a headless
//! box), and the harness thereafter refreshes the access token as it nears
//! expiry. This crate owns that flow; the harness calls
//! [`OAuth::resolve_access_token`] at activate/switch time so the catalog
//! snapshots a live bearer.
//!
//! Per-provider constants (client id, endpoints, scopes, redirect port) live on
//! [`config::OAuthConfig`] (`config::XAI`, `config::CHATGPT`); the two login
//! flows live in [`device`] (RFC 8628) / [`chatgpt_device`] (OpenAI JSON) and
//! [`browser`]; the on-disk token store lives in [`store`].

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod browser;
pub mod chatgpt_device;
pub mod config;
pub mod device;
pub mod pkce;
pub mod store;
pub mod token;

pub use browser::{CallbackOutcome, CallbackServer};
pub use chatgpt_device::{
    ChatGptDeviceCode, ChatGptDeviceToken, exchange_device_code as exchange_chatgpt_device_code,
    poll_device_code as poll_chatgpt_device_code,
    request_device_code as request_chatgpt_device_code,
    verification_url as chatgpt_verification_url,
};
pub use config::{CHATGPT, COPILOT, OAuthConfig, XAI, config_by_provider_id};
pub use device::{DeviceCodeResponse, poll_device_code, request_device_code};
pub use pkce::{PkceCodes, new_nonce, new_state};
pub use store::{AuthStore, TokenSet};
pub use token::{
    ACCESS_TOKEN_REFRESH_SKEW_MS, TokenResponse, access_token_is_expiring, build_authorize_url,
    chatgpt_account_id, exchange_code, jwt_exp_ms, refresh_access_token,
};

use std::sync::{Arc, Mutex};

/// Errors from the auth flows. `Display` is human-readable for surfacing in the
/// login UI / CLI.
#[derive(Debug)]
pub enum AuthError {
    Transport(String),
    TokenEndpoint { status: u16, body: String },
    Decode(String),
    DeviceCode(String),
    Cancelled,
    Timeout,
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::Transport(msg) => write!(f, "network error: {msg}"),
            AuthError::TokenEndpoint { status, body } => {
                write!(f, "token endpoint returned HTTP {status}: {body}")
            }
            AuthError::Decode(msg) => write!(f, "could not parse response: {msg}"),
            AuthError::DeviceCode(msg) => write!(f, "device authorization: {msg}"),
            AuthError::Cancelled => write!(f, "login was cancelled"),
            AuthError::Timeout => write!(f, "login timed out"),
        }
    }
}

impl std::error::Error for AuthError {}

/// Which login flow to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginMethod {
    /// Browser loopback OAuth (desktop). Binds the provider's registered
    /// loopback port, opens the authorize URL, and waits for the callback.
    Browser,
    /// Device-code (headless / VPS / SSH / Docker). Prints a verification URL +
    /// user code and long-polls the token endpoint.
    Device,
}

/// The high-level OAuth entry point. Owns the provider [`OAuthConfig`], the
/// HTTP client, and a single-flight refresh guard so concurrent channel builds
/// collapse onto one refresh HTTP call (xAI rotates the refresh_token, so
/// replaying it on two concurrent fetches would burn one of them).
#[derive(Clone)]
pub struct OAuth {
    config: OAuthConfig,
    client: reqwest::Client,
    refresh_in_flight: Arc<RefreshSlot>,
}

/// The single-flight refresh slot: `Some` while a refresh is in progress, with
/// an inner mutex holding its result so concurrent waiters share one HTTP call.
type RefreshSlot = Mutex<Option<Arc<tokio::sync::Mutex<Option<TokenSet>>>>>;

impl OAuth {
    /// Construct for a specific provider config.
    pub fn new(config: OAuthConfig) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(concat!("neenee/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            config,
            client,
            refresh_in_flight: Arc::new(Mutex::new(None)),
        }
    }

    /// Convenience constructor for the xAI SuperGrok provider.
    pub fn xai() -> Self {
        Self::new(XAI)
    }

    /// Convenience constructor for the ChatGPT/Codex subscription provider.
    pub fn chatgpt() -> Self {
        Self::new(CHATGPT)
    }

    /// Convenience constructor for the GitHub Copilot subscription provider.
    pub fn copilot() -> Self {
        Self::new(COPILOT)
    }

    /// The provider config this OAuth instance authenticates against.
    pub fn config(&self) -> &OAuthConfig {
        &self.config
    }

    /// Borrow the HTTP client (the `login` CLI uses it for the device-code
    /// request + poll so it can print the user code between the two steps).
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// Run a login flow and return the resulting token response. Does NOT
    /// persist; the caller writes the [`TokenSet`] to [`AuthStore`].
    pub async fn login(&self, method: LoginMethod) -> Result<TokenResponse, AuthError> {
        match method {
            LoginMethod::Browser => self.login_browser().await,
            LoginMethod::Device => self.login_device().await,
        }
    }

    async fn login_device(&self) -> Result<TokenResponse, AuthError> {
        match self.config.device_flow {
            config::DeviceFlow::Rfc8628 => {
                let device = request_device_code(&self.client, &self.config).await?;
                poll_device_code(&self.client, &self.config, &device).await
            }
            config::DeviceFlow::ChatGpt => {
                let device = request_chatgpt_device_code(&self.client, &self.config).await?;
                let token = poll_chatgpt_device_code(&self.client, &self.config, &device).await?;
                exchange_chatgpt_device_code(&self.client, &self.config, &token).await
            }
        }
    }

    /// Start the browser loopback flow and return the authorize URL the caller
    /// should open (or surface to the user). The companion [`BrowserLogin`]
    /// waits for the callback and exchanges the code.
    pub async fn begin_browser_login(&self) -> Result<BrowserLogin, AuthError> {
        let server = CallbackServer::start_for(&self.config)
            .await
            .map_err(|e| AuthError::Transport(format!("could not bind loopback server: {e}")))?;
        let pkce = PkceCodes::generate();
        let state = new_state();
        let nonce = new_nonce();
        let redirect = self.config.redirect_uri();
        let url = build_authorize_url(&self.config, &pkce, &state, &nonce, &redirect);
        tracing::info!(url = %url, provider = %self.config.provider_id, "open this URL to authorize");
        let rx = server.wait_for_code(state);
        Ok(BrowserLogin {
            config: self.config,
            url,
            pkce,
            redirect,
            rx,
            _server: server,
        })
    }

    async fn login_browser(&self) -> Result<TokenResponse, AuthError> {
        let login = self.begin_browser_login().await?;
        login.complete(&self.client).await
    }

    /// Resolve a live access token from a stored [`TokenSet`], refreshing it
    /// if it is expiring (within the skew window, or per its JWT `exp`). The
    /// refresh is single-flight: concurrent callers share one HTTP call and the
    /// rotated token set it produces. Returns the access token to send as a
    /// bearer, plus the (possibly updated) token set the caller should persist.
    /// The `account_id` is carried through unchanged on refresh (OpenAI does
    /// not rotate the account).
    pub async fn resolve_access_token(
        &self,
        stored: TokenSet,
    ) -> Result<(String, TokenSet), AuthError> {
        let now = now_ms();
        // Cheap path: token is fresh.
        if !access_token_is_expiring(Some(&stored.access), ACCESS_TOKEN_REFRESH_SKEW_MS, now)
            && stored.expires_ms > now + ACCESS_TOKEN_REFRESH_SKEW_MS
        {
            return Ok((stored.access.clone(), stored));
        }

        // Single-flight: grab the shared refresh slot, or join the in-flight one.
        let guard = {
            let mut slot = self
                .refresh_in_flight
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(shared) = slot.as_ref() {
                Arc::clone(shared)
            } else {
                let shared = Arc::new(tokio::sync::Mutex::new(None));
                *slot = Some(Arc::clone(&shared));
                shared
            }
        };

        let mut inner = guard.lock().await;
        // Another caller already completed the refresh while we waited.
        if let Some(refreshed) = inner.as_ref() {
            return Ok((refreshed.access.clone(), refreshed.clone()));
        }
        let refreshed = refresh_access_token(&self.client, &self.config, &stored.refresh).await?;
        let new_refresh = refreshed
            .refresh_token
            .clone()
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| stored.refresh.clone());
        let expires_ms = now + (refreshed.expires_in.unwrap_or(3600) as i64) * 1000;
        // Preserve the captured account id across refresh; OpenAI's refresh
        // response is an opaque JWT without the chatgpt_account_id claim, so
        // the value captured at login is the durable source of truth.
        let account_id = refreshed
            .id_token
            .as_deref()
            .or(Some(refreshed.access_token.as_str()))
            .and_then(chatgpt_account_id)
            .or(stored.account_id.clone());
        let tokens = TokenSet {
            access: refreshed.access_token.clone(),
            refresh: new_refresh,
            expires_ms,
            account_id,
        };
        *inner = Some(tokens.clone());
        // Clear the single-flight slot so the next stale token starts a fresh
        // refresh rather than reusing a (now older) cached result.
        let mut slot = self
            .refresh_in_flight
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *slot = None;
        let access = tokens.access.clone();
        Ok((access, tokens))
    }
}

/// In-flight browser OAuth login: the authorize URL plus the state needed to
/// finish the code exchange after the user consents.
pub struct BrowserLogin {
    config: OAuthConfig,
    /// Authorize URL the user (or `webbrowser::open`) should visit.
    pub url: String,
    pkce: PkceCodes,
    redirect: String,
    rx: tokio::sync::oneshot::Receiver<CallbackOutcome>,
    /// Keep the loopback server alive until exchange completes.
    _server: CallbackServer,
}

impl BrowserLogin {
    /// Wait for the loopback callback (up to 5 minutes) and exchange the code.
    pub async fn complete(self, client: &reqwest::Client) -> Result<TokenResponse, AuthError> {
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(5 * 60), self.rx)
            .await
            .map_err(|_| AuthError::Timeout)?
            .map_err(|_| AuthError::Cancelled)?;
        match outcome {
            CallbackOutcome::Code(code) => {
                exchange_code(client, &self.config, &code, &self.pkce, &self.redirect).await
            }
            CallbackOutcome::Failed(msg) => Err(AuthError::Transport(msg)),
        }
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
