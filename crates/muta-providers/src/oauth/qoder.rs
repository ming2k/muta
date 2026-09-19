//! Alibaba Qoder device-authorization flow.
//!
//! Qoder's device flow is a Qoder-flavored PKCE protocol, not RFC 8628 and
//! not ChatGPT's two-step exchange:
//!
//! 1. The client generates a `verifier` (43–128 chars), `challenge =
//!    base64url(SHA256(verifier))`, a `nonce` (uuid), and its machine id.
//! 2. It opens the browser page
//!    `https://qoder.com/device/selectAccounts?challenge=…&challenge_method=S256
//!    &nonce=…&machine_id=…&client_id=…` — there is **no** device-code
//!    endpoint; the browser URL itself is the device code.
//! 3. It polls `GET openapi.qoder.sh/api/v1/deviceToken/poll?…`. A `404`
//!    means still pending (keep polling at 1s); a `200` carries the `dt-`
//!    prefixed device token plus its `jrt-` refresh token (~30-day device
//!    token lifetime per the protocol docs).
//! 4. Inference needs the *inference* token, which the device token
//!    exchanges via the OpenAPI surface (same `jobToken` family a pasted
//!    personal-access token uses).
//!
//! Timeout is 300s (the CLI's own deadline), not the 15-minute RFC default.

use serde::Deserialize;

use super::store::{AuthStore, QoderStoredIdentity, TokenSet};
use sha2::Digest;

use muta_contracts::{ResolvedAuth, SecretString};

use crate::oauth::token::TokenResponse;

/// Poll cadence while the user has not approved yet (the CLI's own value).
const QODER_POLL_INTERVAL_MS: u64 = 1_000;
/// Total device-flow deadline (the CLI's own 300s).
const QODER_FLOW_DEADLINE_MS: u64 = 300 * 1_000;
/// Safety margin added to each sleep so we never wake exactly on a boundary.
const POLLING_SAFETY_MARGIN_MS: u64 = 100;

/// The nonce + verifier pair that identifies one pending authorization.
/// Qoder's flow has no server-side "device code" — these client-side values
/// ARE the correlation key.
#[derive(Debug, Clone)]
pub struct QoderDeviceSession {
    pub nonce: String,
    pub verifier: String,
    pub challenge: String,
    pub machine_id: String,
    pub client_id: String,
}

impl QoderDeviceSession {
    /// Generate a fresh session. `machine_id` must be the connection's
    /// stable machine UUID (36 chars) — it is what the server fingerprints.
    pub fn new(machine_id: &str, client_id: &str) -> Self {
        let verifier = new_verifier();
        let challenge = base64_url_no_pad(&sha2::Sha256::digest(verifier.as_bytes()));
        Self {
            nonce: uuid::Uuid::new_v4().to_string(),
            verifier,
            challenge,
            machine_id: machine_id.to_string(),
            client_id: client_id.to_string(),
        }
    }

    /// The browser URL the user opens to approve. This doubles as the
    /// "device code" prompt (no separate user_code exists).
    pub fn user_url(&self) -> String {
        format!(
            "https://qoder.com/device/selectAccounts?challenge={}&challenge_method=S256\
&nonce={}&machine_id={}&client_id={}",
            self.challenge, self.nonce, self.machine_id, self.client_id
        )
    }
}

/// 43-character verifier: 86 hex chars of entropy trimmed — the CLI uses
/// random hex too, so the shape matches what the server accepts.
fn new_verifier() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let mut state = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E3779B97F4A7C15);
    let mut out = String::with_capacity(43);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    while out.len() < 43 {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        out.push(HEX[((state >> 33) & 0xf) as usize] as char);
    }
    out
}

fn base64_url_no_pad(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// The device-token poll response (`dt-` access, `jrt-` refresh).
#[derive(Debug, Deserialize)]
pub struct QoderDeviceToken {
    #[serde(alias = "accessToken", alias = "device_token")]
    pub access_token: SecretString,
    #[serde(default, alias = "refreshToken")]
    pub refresh_token: Option<SecretString>,
    #[serde(default)]
    pub uid: Option<String>,
    #[serde(default)]
    pub expire_time: Option<u64>,
}

/// The device-token poll endpoint (OpenAPI surface, international line).
const DEVICE_POLL_URL: &str = "https://openapi.qoder.sh/api/v1/deviceToken/poll";
/// The jobToken exchange endpoint (OpenAPI surface).
const JOB_TOKEN_EXCHANGE_URL: &str = "https://openapi.qoder.sh/api/v1/jobToken/exchange";

/// Poll the deviceToken endpoint until approval or deadline. `404` = pending.
pub async fn poll_device_token(
    client: &crate::http::Http,
    session: &QoderDeviceSession,
) -> Result<QoderDeviceToken, crate::oauth::AuthError> {
    poll_device_token_at(
        client,
        DEVICE_POLL_URL,
        session,
        sleep_ms,
        QODER_FLOW_DEADLINE_MS,
    )
    .await
}

/// Test-injectable variant: explicit endpoint, sleep, and clock.
pub async fn poll_device_token_at<S, Fut>(
    client: &crate::http::Http,
    endpoint: &str,
    session: &QoderDeviceSession,
    sleep: S,
    deadline_ms: u64,
) -> Result<QoderDeviceToken, crate::oauth::AuthError>
where
    S: Fn(u64) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = ()> + Send,
{
    let start = std::time::Instant::now();
    loop {
        if start.elapsed().as_millis() as u64 >= deadline_ms {
            return Err(crate::oauth::AuthError::Timeout);
        }
        let url = format!(
            "{endpoint}?nonce={}&verifier={}&challenge_method=S256",
            percent(&session.nonce),
            percent(&session.verifier)
        );
        let request = crate::http::Request::new(netune::Method::GET, &url)
            .header("accept", "application/json");
        let response = client
            .send(request)
            .await
            .map_err(|e| crate::oauth::AuthError::Transport(format!("device poll failed: {e}")))?;
        let status = response.status;
        let text = response.body;
        if status.is_success() {
            let token: QoderDeviceToken = serde_json::from_str(&text).map_err(|e| {
                crate::oauth::AuthError::Decode(format!("device token parse failed: {e}"))
            })?;
            return Ok(token);
        }
        let code = status.as_u16();
        if code != 404 {
            return Err(crate::oauth::AuthError::TokenEndpoint { status: code, body: text });
        }
        sleep(QODER_POLL_INTERVAL_MS + POLLING_SAFETY_MARGIN_MS).await;
    }
}

/// Production polling loop with the CLI's 300s deadline.
pub async fn poll_device_token_with<S, Fut>(
    client: &crate::http::Http,
    session: &QoderDeviceSession,
    sleep: S,
) -> Result<QoderDeviceToken, crate::oauth::AuthError>
where
    S: Fn(u64) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = ()> + Send,
{
    poll_device_token_at(client, DEVICE_POLL_URL, session, sleep, QODER_FLOW_DEADLINE_MS).await
}

/// Exchange a Qoder credential (a `pt-` personal-access token or a `dt-`
/// device token) for the `jt-` inference token the COSY surface consumes.
/// Idempotent; the `jt-` token lives ~24h and is refreshed the same way.
pub async fn exchange_inference_token(
    client: &crate::http::Http,
    credential: &str,
) -> Result<TokenResponse, crate::oauth::AuthError> {
    exchange_inference_token_at(client, JOB_TOKEN_EXCHANGE_URL, credential).await
}

/// Test-injectable variant with an explicit endpoint.
pub async fn exchange_inference_token_at(
    client: &crate::http::Http,
    endpoint: &str,
    credential: &str,
) -> Result<TokenResponse, crate::oauth::AuthError> {
    let request = crate::http::Request::new(netune::Method::POST, endpoint)
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .json(&serde_json::json!({ "personal_token": credential }));
    let response = client
        .send(request)
        .await
        .map_err(|e| crate::oauth::AuthError::Transport(format!("exchange failed: {e}")))?;
    let status = response.status;
    let text = response.body;
    if !status.is_success() {
        return Err(crate::oauth::AuthError::TokenEndpoint {
            status: status.as_u16(),
            body: text,
        });
    }
    let parsed: TokenResponse = serde_json::from_str(&text)
        .map_err(|e| crate::oauth::AuthError::Decode(format!("exchange parse failed: {e}")))?;
    parsed.validate()
}

fn percent(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

async fn sleep_ms(ms: u64) {
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
}

/// Generate a fresh machine AES key as 32 lowercase hex chars. The key is
/// generated once per device at login and persisted with the connection —
/// rotating it would look like device churn to Qoder's risk layer.
pub fn generate_machine_key_hex() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let mut state = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E3779B97F4A7C15);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(32);
    while out.len() < 32 {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        out.push(HEX[((state >> 33) & 0xf) as usize] as char);
    }
    out
}

/// Credential source for a pasted Qoder personal-access token (`pt-…`).
///
/// Qoder runs this as ApiKey auth on the COSY surface, but the surface still
/// demands the full typed request identity (machine key, uid, org scope).
/// This source owns that material: the machine key is a per-device AES key
/// persisted beside the credentials (stable across processes — rotating it
/// would look like device churn), and the uid is resolved once from the
/// userinfo endpoint, cached in the same slot.
pub struct QoderApiKeyCredentialSource {
    connection_id: String,
    token: SecretString,
    identity: std::sync::Mutex<Option<muta_contracts::QoderRequestIdentity>>,
}

impl std::fmt::Debug for QoderApiKeyCredentialSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QoderApiKeyCredentialSource")
            .field("connection_id", &self.connection_id)
            .field("token", &"[REDACTED]")
            .finish()
    }
}

impl QoderApiKeyCredentialSource {
    pub fn new(connection_id: impl Into<String>, token: SecretString) -> Self {
        Self {
            connection_id: connection_id.into(),
            token,
            identity: std::sync::Mutex::new(None),
        }
    }

    /// The per-device identity slot, persisted in the auth store under the
    /// connection id (same file OAuth credentials use — one identity home
    /// per connection).
    async fn load_or_create_identity(&self) -> Result<muta_contracts::QoderRequestIdentity, String> {
        if let Some(existing) = self.identity.lock().unwrap_or_else(|e| e.into_inner()).clone()
        {
            return Ok(existing);
        }
        let store = AuthStore::load().map_err(|e| e.to_string())?;
        if let Some(stored) = store
            .tokens
            .get(&self.connection_id)
            .and_then(|t| t.qoder.as_ref())
        {
            let identity = muta_contracts::QoderRequestIdentity {
                uid: stored.uid.clone(),
                machine_key_hex: stored.machine_key_hex.clone(),
                data_policy_agreed: stored.data_policy_agreed,
                organization_id: stored.organization_id.clone(),
                organization_tags: stored.organization_tags.clone(),
            };
            *self.identity.lock().unwrap_or_else(|e| e.into_inner()) = Some(identity.clone());
            return Ok(identity);
        }
        // First use: mint the device identity and persist it through the
        // cross-process lock (the store's transactional write path).
        let identity = muta_contracts::QoderRequestIdentity {
            uid: String::new(),
            machine_key_hex: SecretString::from(generate_machine_key_hex()),
            data_policy_agreed: true,
            organization_id: None,
            organization_tags: Vec::new(),
        };
        let mut locked = AuthStore::lock()
            .await
            .map_err(|e| format!("could not lock auth store: {e}"))?;
        let mut entry = locked
            .get(&self.connection_id)
            .cloned()
            .unwrap_or_else(|| TokenSet {
                access: self.token.clone(),
                refresh: String::new().into(),
                expires_ms: i64::MAX,
                account_id: None,
                id_token: None,
                token_type: None,
                scope: None,
                project_id: None,
                user_email: None,
                qoder: None,
            });
        // Never clobber a live bearer with our placeholder if OAuth rotated
        // concurrently: keep the existing access token when present.
        if entry.access.expose_secret().trim().is_empty() {
            entry.access = self.token.clone();
        }
        entry.qoder = Some(QoderStoredIdentity {
            uid: String::new(),
            machine_key_hex: identity.machine_key_hex.clone(),
            data_policy_agreed: true,
            organization_id: None,
            organization_tags: Vec::new(),
        });
        locked.set(&self.connection_id, entry);
        locked.save().map_err(|e| e.to_string())?;
        *self.identity.lock().unwrap_or_else(|e| e.into_inner()) = Some(identity.clone());
        Ok(identity)
    }
}
impl muta_contracts::CredentialSource for QoderApiKeyCredentialSource {
    fn resolve_auth<'a>(&'a self) -> futures::future::BoxFuture<'a, Result<ResolvedAuth, String>> {
        Box::pin(async move {
            let identity = self.load_or_create_identity().await?;
            Ok(ResolvedAuth::new(self.token.clone()).with_qoder_identity(identity))
        })
    }

    fn force_refresh<'a>(&'a self) -> futures::future::BoxFuture<'a, Result<ResolvedAuth, String>> {
        // Static PAT: nothing to rotate; the identity is already minted.
        Box::pin(async move {
            let identity = self.load_or_create_identity().await?;
            Ok(ResolvedAuth::new(self.token.clone()).with_qoder_identity(identity))
        })
    }

    fn is_ready(&self) -> bool {
        !self.token.expose_secret().trim().is_empty()
    }

    fn is_oauth(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_url_carries_the_full_pkce_contract() {
        let session = QoderDeviceSession::new(
            "0f8e2b1a-1111-4222-8333-444455556666",
            "e93fe488-5778-4c35-a6fc-0f54ed7b3139",
        );
        let url = session.user_url();
        assert!(url.starts_with("https://qoder.com/device/selectAccounts?"), "{url}");
        assert!(url.contains("challenge_method=S256"));
        assert!(url.contains("client_id=e93fe488-5778-4c35-a6fc-0f54ed7b3139"));
        assert!(url.contains("machine_id=0f8e2b1a"));
        assert!(url.contains(&format!("nonce={}", session.nonce)));
        // Challenge is unpadded base64url of SHA256(verifier).
        let expected = base64_url_no_pad(&sha2::Sha256::digest(session.verifier.as_bytes()));
        assert!(url.contains(&format!("challenge={expected}")));
    }

    #[test]
    fn verifier_meets_the_length_floor() {
        let session = QoderDeviceSession::new("m", "c");
        assert!(session.verifier.len() >= 43);
        assert!(session.verifier.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn percent_encodes_query_values() {
        assert_eq!(percent("abc-_.~"), "abc-_.~");
        assert_eq!(percent("a b+c"), "a%20b%2Bc");
    }

    #[tokio::test]
    async fn poll_surfaces_non_404_errors_immediately() {
        // The owned transport has no per-client base-URL override, so bind a
        // raw TCP server and hand its URL to the injectable endpoint.
        let (addr, hits, _keep) =
            spawn_scripted_server(vec![(500, "boom".to_string())]).await;
        let session = QoderDeviceSession::new("m", "c");
        let client = crate::http::Http::control_plane().unwrap();
        let url = format!("http://{addr}/api/v1/deviceToken/poll");
        let result = poll_device_token_at(&client, &url, &session, |_| async {}, 5_000).await;
        assert!(matches!(
            result,
            Err(crate::oauth::AuthError::TokenEndpoint { status: 500, .. })
        ));
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn poll_treats_404_as_pending_then_succeeds() {
        let (addr, hits, _keep) = spawn_scripted_server(vec![
            (404, "not found".to_string()),
            (
                200,
                r#"{"access_token":"dt-abc","refresh_token":"jrt-x","uid":"u1"}"#.to_string(),
            ),
        ])
        .await;
        let session = QoderDeviceSession::new("m", "c");
        let client = crate::http::Http::control_plane().unwrap();
        let url = format!("http://{addr}/api/v1/deviceToken/poll");
        let result = poll_device_token_at(&client, &url, &session, |_| async {}, 5_000).await;
        let token = result.unwrap();
        assert_eq!(token.access_token.expose_secret(), "dt-abc");
        assert_eq!(token.uid.as_deref(), Some("u1"));
        // One pending poll + one success.
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn exchange_maps_the_job_token_response() {
        let addr = server_single(
            200,
            r#"{"access_token":"jt-infer","refresh_token":"jrt-refresh","expires_in":86400,"token_type":"Bearer"}"#,
        )
        .await;
        let client = crate::http::Http::control_plane().unwrap();
        let url = format!("http://{addr}/api/v1/jobToken/exchange");
        let result = exchange_inference_token_at(&client, &url, "pt-user-token").await;
        let token = result.unwrap();
        assert_eq!(token.access_token.expose_secret(), "jt-infer");
    }

    // ── test helpers ────────────────────────────────────────────────────────

    /// Minimal HTTP test server serving a scripted list of (status, body)
    /// responses, one per connection, repeating the last one indefinitely.
    async fn spawn_scripted_server(
        script: Vec<(u16, String)>,
    ) -> (std::net::SocketAddr, std::sync::Arc<std::sync::atomic::AtomicUsize>, tokio::task::JoinHandle<()>) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_task = Arc::clone(&hits);
        let script = Arc::new(tokio::sync::Mutex::new(script));
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else { break };
                let mut buf = vec![0u8; 2048];
                let _ = tokio::io::AsyncReadExt::read(&mut socket, &mut buf).await;
                let step = {
                    let mut script = script.lock().await;
                    if script.len() > 1 {
                        script.remove(0)
                    } else {
                        script[0].clone()
                    }
                };
                hits_task.fetch_add(1, Ordering::SeqCst);
                let reason = if step.0 == 200 { "OK" } else { "ERR" };
                let payload = format!(
                    "HTTP/1.1 {} {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    step.0, reason, step.1.len(), step.1
                );
                let _ = tokio::io::AsyncWriteExt::write_all(&mut socket, payload.as_bytes()).await;
                let _ = tokio::io::AsyncWriteExt::shutdown(&mut socket).await;
            }
        });
        (addr, hits, task)
    }

    async fn server_single(status: u16, body: &str) -> std::net::SocketAddr {
        spawn_scripted_server(vec![(status, body.to_string())]).await.0
    }
}
