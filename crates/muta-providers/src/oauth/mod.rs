//! OAuth2 + PKCE authentication engine & client emulator.
//!
//! muta's OAuth subsystem provides an ultra-flexible, industrial-grade architecture
//! supporting:
//! - Multi-provider presets (Google Antigravity, OpenAI Codex, xAI SuperGrok, GitHub Copilot)
//! - Dynamic client emulation (custom client IDs, secrets, endpoints, headers, PKCE modes, port strategies)
//! - Dynamic loopback port binding (with automatic fallback on busy ports)
//! - Headless / SSH manual code and redirect URL parsing (solving Google OOB deprecation)
//! - Automatic token refreshing with JWT exp inspection, skew margins, single-flight locking, and atomic persistence.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod browser;
pub mod chatgpt_device;
pub mod config;
pub mod credential_source;
pub mod device;
pub mod manual;
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
pub use config::{
    CHATGPT, COPILOT, ClientAuthMethod, DeviceFlow, GOOGLE_ANTIGRAVITY, OAuthConfig,
    OAuthConfigBuilder, PkceMode, PortMode, TokenRequestFormat, XAI, chatgpt_preset,
    config_by_provider_id, copilot_preset, google_antigravity_preset, xai_preset,
};
pub use credential_source::OAuthCredentialSource;
pub use device::{DeviceCodeResponse, poll_device_code, request_device_code};
pub use manual::parse_authorization_response;
pub use pkce::{PkceCodes, new_nonce, new_state};
pub use store::{AuthStore, TokenSet};
pub use token::{
    ACCESS_TOKEN_REFRESH_SKEW_MS, ANTIGRAVITY_LOAD_CODE_ASSIST_URL, ANTIGRAVITY_ONBOARD_USER_URL,
    ANTIGRAVITY_USER_AGENT, GOOGLE_USERINFO_URL, GoogleUserInfo, TokenResponse,
    access_token_is_expiring, build_authorize_url, chatgpt_account_id, exchange_code,
    fetch_google_userinfo, jwt_exp_ms, refresh_access_token, resolve_antigravity_project,
};

pub use muta_contracts::LoginMethod;
use muta_contracts::SecretString;
use std::sync::{Arc, Mutex};

/// Errors from the auth flows. Surfaced in user-facing CLI and logs.
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

impl AuthError {
    /// Whether this error indicates an invalid/revoked refresh token on the identity provider.
    pub fn is_permanent_grant_error(&self) -> bool {
        match self {
            AuthError::TokenEndpoint { body, .. } => {
                let lower = body.to_lowercase();
                lower.contains("invalid_grant")
                    || lower.contains("token_revoked")
                    || lower.contains("unauthorized_client")
            }
            _ => false,
        }
    }
}

impl std::error::Error for AuthError {}

/// User-facing authorization material produced before an OAuth flow waits for
/// completion. Frontends render this one shape for both PKCE and device-code
/// sessions; a browser flow has no `user_code`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthLoginPrompt {
    pub method: LoginMethod,
    pub url: String,
    pub user_code: Option<String>,
    pub message: String,
}

/// An initiated OAuth flow. Starting and completing are deliberately separate:
/// the caller must be able to show/open the authorization URL before the flow
/// blocks on a localhost callback or device-token poll.
pub struct OAuthLoginSession {
    prompt: OAuthLoginPrompt,
    client: reqwest::Client,
    flow: OAuthLoginFlow,
}

enum OAuthLoginFlow {
    Browser(BrowserLogin),
    RfcDevice {
        config: OAuthConfig,
        device: DeviceCodeResponse,
    },
    ChatGptDevice {
        config: OAuthConfig,
        device: ChatGptDeviceCode,
    },
}

impl OAuthLoginSession {
    pub fn prompt(&self) -> &OAuthLoginPrompt {
        &self.prompt
    }

    /// Wait for authorization and exchange the resulting grant for tokens.
    pub async fn complete(self) -> Result<TokenResponse, AuthError> {
        match self.flow {
            OAuthLoginFlow::Browser(login) => login.complete(&self.client).await,
            OAuthLoginFlow::RfcDevice { config, device } => {
                poll_device_code(&self.client, &config, &device).await
            }
            OAuthLoginFlow::ChatGptDevice { config, device } => {
                let token = poll_chatgpt_device_code(&self.client, &config, &device).await?;
                exchange_chatgpt_device_code(&self.client, &config, &token).await
            }
        }
    }
}

/// The high-level OAuth orchestrator.
#[derive(Clone)]
pub struct OAuth {
    config: OAuthConfig,
    client: reqwest::Client,
    refresh_in_flight: Arc<RefreshSlot>,
}

type RefreshSlot = Mutex<Option<Arc<tokio::sync::Mutex<Option<TokenSet>>>>>;

impl OAuth {
    /// Construct with a specific provider configuration.
    pub fn new(config: OAuthConfig) -> Self {
        let client = reqwest::Client::builder()
            .user_agent(concat!("muta/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            config,
            client,
            refresh_in_flight: Arc::new(Mutex::new(None)),
        }
    }

    /// Convenience constructor for Google Antigravity.
    pub fn google_antigravity() -> Self {
        Self::new(google_antigravity_preset())
    }

    /// Convenience constructor for xAI SuperGrok.
    pub fn xai() -> Self {
        Self::new(xai_preset())
    }

    /// Convenience constructor for ChatGPT/Codex.
    pub fn chatgpt() -> Self {
        Self::new(chatgpt_preset())
    }

    /// Convenience constructor for GitHub Copilot.
    pub fn copilot() -> Self {
        Self::new(copilot_preset())
    }

    /// The provider config this OAuth instance authenticates against.
    pub fn config(&self) -> &OAuthConfig {
        &self.config
    }

    /// Borrow the HTTP client.
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    /// Run an OAuth login flow and return the resulting token response.
    pub async fn login(&self, method: LoginMethod) -> Result<TokenResponse, AuthError> {
        self.begin_login(method).await?.complete().await
    }

    /// Initiate either generic PKCE/browser login or the configured device
    /// grant and return a common pending-session handle.
    pub async fn begin_login(&self, method: LoginMethod) -> Result<OAuthLoginSession, AuthError> {
        if !self.config.supports_login_method(method) {
            return Err(AuthError::Transport(format!(
                "{} login is not supported for {}",
                match method {
                    LoginMethod::Browser => "browser PKCE",
                    LoginMethod::Device => "device-code",
                },
                self.config.provider_id
            )));
        }
        let (prompt, flow) = match method {
            LoginMethod::Browser => {
                let login = self.begin_browser_login().await?;
                let prompt = OAuthLoginPrompt {
                    method,
                    url: login.url.clone(),
                    user_code: None,
                    message: "Complete authorization in your browser (or open the link below)."
                        .to_string(),
                };
                (prompt, OAuthLoginFlow::Browser(login))
            }
            LoginMethod::Device => match self.config.device_flow {
                config::DeviceFlow::Rfc8628 => {
                    let device = request_device_code(&self.client, &self.config).await?;
                    let prompt = OAuthLoginPrompt {
                        method,
                        url: device.user_url().to_string(),
                        user_code: Some(device.user_code.clone()),
                        message: "Open the URL on any device and enter the code to authorize."
                            .to_string(),
                    };
                    (
                        prompt,
                        OAuthLoginFlow::RfcDevice {
                            config: self.config.clone(),
                            device,
                        },
                    )
                }
                config::DeviceFlow::ChatGpt => {
                    let device = request_chatgpt_device_code(&self.client, &self.config).await?;
                    let prompt = OAuthLoginPrompt {
                        method,
                        url: device.user_url(&self.config),
                        user_code: Some(device.user_code.clone()),
                        message: "Open the URL on any device and enter the code to authorize."
                            .to_string(),
                    };
                    (
                        prompt,
                        OAuthLoginFlow::ChatGptDevice {
                            config: self.config.clone(),
                            device,
                        },
                    )
                }
                config::DeviceFlow::Disabled => unreachable!("support checked above"),
            },
        };
        Ok(OAuthLoginSession {
            prompt,
            client: self.client.clone(),
            flow,
        })
    }

    /// Start the browser PKCE flow and return the authorize URL plus callback
    /// state. Prefer [`Self::begin_login`] in application code so both login
    /// families share the same orchestration path.
    pub async fn begin_browser_login(&self) -> Result<BrowserLogin, AuthError> {
        let server = CallbackServer::start_for(&self.config)
            .await
            .map_err(|e| AuthError::Transport(format!("could not bind loopback server: {e}")))?;
        let bound_port = server.bound_port();
        let redirect = self.config.redirect_uri(Some(bound_port));
        let pkce = PkceCodes::generate();
        let state = new_state();
        let nonce = new_nonce();
        let url = build_authorize_url(&self.config, &pkce, &state, &nonce, &redirect);
        tracing::info!(
            url = %url,
            provider = %self.config.provider_id,
            bound_port = bound_port,
            "open this URL to authorize"
        );
        let rx = server.wait_for_code(state.clone());
        Ok(BrowserLogin {
            config: self.config.clone(),
            url,
            state,
            pkce,
            redirect,
            rx,
            _server: server,
        })
    }

    /// Exchange an authorization code obtained manually (e.g. pasted into CLI in headless mode).
    pub async fn exchange_manual_code(
        &self,
        code: &str,
        pkce: &PkceCodes,
        redirect_uri: &str,
    ) -> Result<TokenResponse, AuthError> {
        exchange_code(&self.client, &self.config, code, pkce, redirect_uri).await
    }

    /// Resolve a live access token from a stored [`TokenSet`], refreshing it
    /// if expiring. Concurrency-safe: concurrent callers share one single-flight HTTP call.
    pub async fn resolve_access_token(
        &self,
        stored: TokenSet,
    ) -> Result<(SecretString, TokenSet), AuthError> {
        let now = now_ms();
        if !access_token_is_expiring(
            Some(stored.access.expose_secret()),
            ACCESS_TOKEN_REFRESH_SKEW_MS,
            now,
        ) && stored.expires_ms > now + ACCESS_TOKEN_REFRESH_SKEW_MS
        {
            return Ok((stored.access.clone(), stored));
        }
        self.force_resolve_access_token(stored).await
    }

    /// Force a fresh token exchange with upstream regardless of remaining expiration TTL.
    /// Concurrency-safe: concurrent callers share one single-flight HTTP call.
    pub async fn force_resolve_access_token(
        &self,
        stored: TokenSet,
    ) -> Result<(SecretString, TokenSet), AuthError> {
        let now = now_ms();
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
        if let Some(refreshed) = inner.as_ref() {
            return Ok((refreshed.access.clone(), refreshed.clone()));
        }

        let refreshed =
            refresh_access_token(&self.client, &self.config, stored.refresh.expose_secret())
                .await?;
        let new_refresh = refreshed
            .refresh_token
            .clone()
            .filter(|r| !r.is_empty())
            .unwrap_or_else(|| stored.refresh.clone());
        let expires_ms = now + (refreshed.expires_in.unwrap_or(3600) as i64) * 1000;

        let mut account_id = if self.config.is_chatgpt() {
            refreshed
                .id_token
                .as_ref()
                .map(SecretString::expose_secret)
                .or(Some(refreshed.access_token.expose_secret()))
                .and_then(chatgpt_account_id)
                .or(stored.account_id.clone())
        } else {
            stored.account_id.clone()
        };

        let mut project_id = stored.project_id.clone();
        let mut user_email = stored.user_email.clone();

        if self.config.is_antigravity() {
            if (project_id.is_none() || account_id.is_none())
                && let Ok(project) = resolve_antigravity_project(
                    &self.client,
                    refreshed.access_token.expose_secret(),
                )
                .await
                && !project.is_empty()
            {
                project_id = Some(project.clone());
                if account_id.is_none() {
                    account_id = Some(project);
                }
            }
            if user_email.is_none()
                && let Ok(info) =
                    fetch_google_userinfo(&self.client, refreshed.access_token.expose_secret())
                        .await
            {
                user_email = info.email;
            }
        }

        let tokens = TokenSet {
            access: refreshed.access_token.clone(),
            refresh: new_refresh,
            expires_ms,
            account_id,
            id_token: refreshed.id_token.clone().or(stored.id_token),
            token_type: refreshed.token_type.clone().or(stored.token_type),
            scope: refreshed.scope.clone().or(stored.scope),
            project_id,
            user_email,
        };

        *inner = Some(tokens.clone());
        let mut slot = self
            .refresh_in_flight
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *slot = None;
        let access = tokens.access.clone();
        Ok((access, tokens))
    }
}

/// In-flight browser OAuth login session.
pub struct BrowserLogin {
    pub config: OAuthConfig,
    /// Authorize URL the user should open.
    pub url: String,
    /// Expected CSRF state.
    pub state: String,
    /// Generated PKCE pair.
    pub pkce: PkceCodes,
    /// The exact registered redirect URI used for this session.
    pub redirect: String,
    rx: tokio::sync::oneshot::Receiver<CallbackOutcome>,
    _server: CallbackServer,
}

impl BrowserLogin {
    /// The port bound by the loopback server for this session.
    pub fn bound_port(&self) -> u16 {
        self._server.bound_port()
    }

    /// Manually inject a pasted authorization response (redirect URL or code).
    pub fn inject_manual_input(&self, input: &str) -> Result<(), AuthError> {
        let code = parse_authorization_response(input, Some(&self.state))?;
        if self._server.inject_outcome(CallbackOutcome::Code(code)) {
            Ok(())
        } else {
            Err(AuthError::Cancelled)
        }
    }

    /// Wait for the callback (or manual input) and exchange the authorization code for tokens.
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
