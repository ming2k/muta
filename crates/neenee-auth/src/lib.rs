//! OAuth2 + PKCE authentication for providers that need it (xAI SuperGrok).
//!
//! neenee's other providers authenticate with a static API key resolved from
//! env → credentials.toml → inline config. SuperGrok subscriptions, however,
//! authenticate via OAuth2: the user runs a login flow (browser loopback OAuth
//! on a desktop, or RFC 8628 device-code on a headless box), and the harness
//! thereafter refreshes the access token as it nears expiry. This crate owns
//! that flow; the harness calls [`XaiOAuth::resolve_access_token`] at
//! activate/switch time so the catalog snapshots a live bearer.
//!
//! The xAI specifics (client id, endpoints, scopes, `plan=generic`) live in
//! [`token`]; the two login flows live in [`device`] and [`browser`]; the
//! on-disk token store lives in [`store`].

pub mod browser;
pub mod device;
pub mod pkce;
pub mod store;
pub mod token;

pub use browser::{CallbackOutcome, CallbackServer, redirect_uri};
pub use device::{DeviceCodeResponse, poll_device_code, request_device_code};
pub use pkce::{PkceCodes, new_nonce, new_state};
pub use store::{AuthStore, TokenSet};
pub use token::{
    ACCESS_TOKEN_REFRESH_SKEW_MS, AUTHORIZE_URL, AUTH_PROVIDER_ID, CLIENT_ID,
    DEVICE_AUTHORIZATION_URL, DEVICE_CODE_GRANT_TYPE, SCOPE, TOKEN_URL, TokenResponse,
    access_token_is_expiring, build_authorize_url, exchange_code, jwt_exp_ms, refresh_access_token,
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
                write!(f, "xAI token endpoint returned HTTP {status}: {body}")
            }
            AuthError::Decode(msg) => write!(f, "could not parse xAI response: {msg}"),
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
    /// Browser loopback OAuth (desktop). Binds `127.0.0.1:56121`, opens the
    /// authorize URL, and waits for the callback.
    Browser,
    /// RFC 8628 device-code (headless / VPS / SSH / Docker). Prints a
    /// verification URL + user code and long-polls the token endpoint.
    Device,
}

/// The high-level xAI OAuth entry point. Owns the HTTP client and a
/// single-flight refresh guard so concurrent channel builds collapse onto one
/// refresh HTTP call (xAI rotates the refresh_token, so replaying it on two
/// concurrent fetches would burn one of them).
#[derive(Clone)]
pub struct XaiOAuth {
    client: reqwest::Client,
    refresh_in_flight: Arc<RefreshSlot>,
}

/// The single-flight refresh slot: `Some` while a refresh is in progress, with
/// an inner mutex holding its result so concurrent waiters share one HTTP call.
type RefreshSlot = Mutex<Option<Arc<tokio::sync::Mutex<Option<TokenSet>>>>>;

impl Default for XaiOAuth {
    fn default() -> Self {
        Self::new()
    }
}

impl XaiOAuth {
    /// Construct with a fresh HTTP client (rustls, no system certs needed).
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .user_agent(concat!("neenee/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            client,
            refresh_in_flight: Arc::new(Mutex::new(None)),
        }
    }

    /// Borrow the HTTP client (the `login` CLI uses it for the device-code
    /// request + poll so it can print the user code between the two steps).
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// Run a login flow and return the resulting token set. Does NOT persist;
    /// the caller writes the [`TokenSet`] to [`AuthStore`].
    pub async fn login(&self, method: LoginMethod) -> Result<TokenResponse, AuthError> {
        match method {
            LoginMethod::Browser => self.login_browser().await,
            LoginMethod::Device => self.login_device().await,
        }
    }

    async fn login_device(&self) -> Result<TokenResponse, AuthError> {
        let device = request_device_code(&self.client).await?;
        poll_device_code(&self.client, &device).await
    }

    /// Start the browser loopback flow and return the authorize URL the caller
    /// should open (or surface to the user). The companion
    /// [`complete_browser_login`] waits for the callback and exchanges the code.
    pub async fn begin_browser_login(&self) -> Result<BrowserLogin, AuthError> {
        let server = CallbackServer::start()
            .await
            .map_err(|e| AuthError::Transport(format!("could not bind loopback server: {e}")))?;
        let pkce = PkceCodes::generate();
        let state = new_state();
        let nonce = new_nonce();
        let redirect = redirect_uri();
        let url = build_authorize_url(&pkce, &state, &nonce, &redirect);
        tracing::info!(url = %url, "open this URL to authorize xAI");
        let rx = server.wait_for_code(state);
        Ok(BrowserLogin {
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
        let refreshed = refresh_access_token(&self.client, &stored.refresh).await?;
        let new_refresh = refreshed
            .refresh_token
            .clone()
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| stored.refresh.clone());
        let expires_ms = now + (refreshed.expires_in.unwrap_or(3600) as i64) * 1000;
        let tokens = TokenSet {
            access: refreshed.access_token.clone(),
            refresh: new_refresh,
            expires_ms,
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
                exchange_code(client, &code, &self.pkce, &self.redirect).await
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
