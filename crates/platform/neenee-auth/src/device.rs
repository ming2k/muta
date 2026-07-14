//! RFC 8628 device authorization grant — the headless / VPS / SSH / Docker
//! login path. No loopback callback server runs on the host, so this works
//! anywhere `127.0.0.1:56121` isn't reachable from the user's browser: the CLI
//! prints a verification URL + short user_code, the user opens it on any
//! device, and the CLI long-polls the token endpoint.

use serde::{Deserialize, Serialize};

use crate::config::OAuthConfig;
use crate::token::TokenResponse;

/// Response from the device-authorization endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub interval: Option<u64>,
}

impl DeviceCodeResponse {
    /// The URL the user should open: the `complete` form (embeds the user_code)
    /// when xAI returns it, otherwise the bare verification URI.
    pub fn user_url(&self) -> &str {
        self.verification_uri_complete
            .as_deref()
            .unwrap_or(&self.verification_uri)
    }
}

// ── Poll-loop bounds (mirror opencode's xai.ts) ─────────────────────────────
/// Default poll interval when the server doesn't return `interval` (seconds).
const DEVICE_CODE_DEFAULT_INTERVAL_MS: u64 = 5_000;
/// Floor the poll interval so we never hammer the token endpoint.
const DEVICE_CODE_MIN_INTERVAL_MS: u64 = 1_000;
/// RFC 8628 §3.5: on `slow_down`, bump the interval by ≥5s.
const DEVICE_CODE_SLOW_DOWN_INCREMENT_MS: u64 = 5_000;
/// Default device-code lifetime when the server doesn't return `expires_in`.
const DEVICE_CODE_DEFAULT_EXPIRES_MS: u64 = 5 * 60 * 1000;
/// Safety margin added to each sleep so we never wake exactly on the deadline.
const OAUTH_POLLING_SAFETY_MARGIN_MS: i64 = 3_000;

/// Request a device code (RFC 8628). Prints nothing; the caller surfaces the
/// `user_code` + `verification_uri` to the operator.
pub async fn request_device_code(
    client: &reqwest::Client,
    cfg: &OAuthConfig,
) -> Result<DeviceCodeResponse, crate::AuthError> {
    request_device_code_at(client, cfg, cfg.device_authorization_url).await
}

/// Same as [`request_device_code`] but with an explicit endpoint (tests).
pub async fn request_device_code_at(
    client: &reqwest::Client,
    cfg: &OAuthConfig,
    endpoint: &str,
) -> Result<DeviceCodeResponse, crate::AuthError> {
    let body = format!(
        "client_id={}&scope={}",
        cfg.client_id,
        crate::token::percent_encode_form_value(cfg.scope)
    );
    let response = client
        .post(endpoint)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .body(body)
        .send()
        .await
        .map_err(|e| crate::AuthError::Transport(format!("device code request failed: {e}")))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(crate::AuthError::TokenEndpoint {
            status: status.as_u16(),
            body: text,
        });
    }
    let json: DeviceCodeResponse = serde_json::from_str(&text)
        .map_err(|e| crate::AuthError::Decode(format!("device code response parse failed: {e}")))?;
    if json.device_code.is_empty() || json.user_code.is_empty() || json.verification_uri.is_empty()
    {
        return Err(crate::AuthError::Decode(
            "device code response missing device_code / user_code / verification_uri".to_string(),
        ));
    }
    Ok(json)
}

/// Poll the token endpoint until the user completes authorization, the code
/// expires, or a terminal error arrives. Honors RFC 8628 §3.5:
/// `authorization_pending` → keep polling; `slow_down` → bump the interval.
pub async fn poll_device_code(
    client: &reqwest::Client,
    cfg: &OAuthConfig,
    device: &DeviceCodeResponse,
) -> Result<TokenResponse, crate::AuthError> {
    poll_device_code_with(client, cfg, device, sleep_ms, now_ms).await
}

/// Test-injectable variant of [`poll_device_code`] so unit tests can drive the
/// `authorization_pending` / `slow_down` branches without real waits.
pub async fn poll_device_code_with<S, Fut>(
    client: &reqwest::Client,
    cfg: &OAuthConfig,
    device: &DeviceCodeResponse,
    sleep: S,
    now: impl Fn() -> i64 + Send + Sync,
) -> Result<TokenResponse, crate::AuthError>
where
    S: Fn(u64) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = ()> + Send,
{
    let expires_ms =
        positive_seconds_to_ms(device.expires_in, DEVICE_CODE_DEFAULT_EXPIRES_MS) as i64;
    let deadline = now() + expires_ms;
    let mut interval_ms = positive_seconds_to_ms(device.interval, DEVICE_CODE_DEFAULT_INTERVAL_MS)
        .max(DEVICE_CODE_MIN_INTERVAL_MS);

    loop {
        if now() >= deadline {
            return Err(crate::AuthError::DeviceCode(
                "device authorization timed out".to_string(),
            ));
        }

        let body = format!(
            "grant_type={}&client_id={}&device_code={}",
            cfg.grant_type_device,
            cfg.client_id,
            crate::token::percent_encode_form_value(&device.device_code)
        );
        let response = client
            .post(cfg.device_token_url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .header("Accept", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| crate::AuthError::Transport(format!("device token poll failed: {e}")))?;

        let status = response.status();
        let text = response.text().await.unwrap_or_default();

        if status.is_success() {
            return serde_json::from_str::<TokenResponse>(&text).map_err(|e| {
                crate::AuthError::Decode(format!("device token response parse failed: {e}"))
            });
        }

        // Parse the OAuth2 error body.
        let err: DeviceTokenError = serde_json::from_str(&text).unwrap_or_default();
        let remaining = (deadline - now()).max(0);

        match err.error.as_deref() {
            Some("authorization_pending") => {
                sleep(min_with_margin(interval_ms, remaining)).await;
                continue;
            }
            Some("slow_down") => {
                interval_ms += DEVICE_CODE_SLOW_DOWN_INCREMENT_MS;
                sleep(min_with_margin(interval_ms, remaining)).await;
                continue;
            }
            Some("access_denied" | "authorization_denied") => {
                return Err(crate::AuthError::DeviceCode(
                    "device authorization was denied".to_string(),
                ));
            }
            Some("expired_token") => {
                return Err(crate::AuthError::DeviceCode(
                    "device code expired - please re-run login".to_string(),
                ));
            }
            _ => {
                let detail = err
                    .error_description
                    .as_deref()
                    .or(err.error.as_deref())
                    .unwrap_or("");
                return Err(crate::AuthError::TokenEndpoint {
                    status: status.as_u16(),
                    body: detail.to_string(),
                });
            }
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct DeviceTokenError {
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

/// Normalize a server-supplied seconds value to milliseconds, falling back to
/// `default_ms` when missing, non-positive, or not finite.
fn positive_seconds_to_ms(value: Option<u64>, default_ms: u64) -> u64 {
    value
        .filter(|s| *s > 0)
        .map(|s| s * 1000)
        .unwrap_or(default_ms)
}

/// Clamp an interval to the remaining deadline, then add the safety margin so
/// we never wake exactly on the deadline.
fn min_with_margin(interval_ms: u64, remaining_ms: i64) -> u64 {
    let capped = (remaining_ms as u64).min(interval_ms);
    capped.saturating_add(OAUTH_POLLING_SAFETY_MARGIN_MS as u64)
}

async fn sleep_ms(ms: u64) {
    // Async sleep so the agent loop stays free while we wait between polls.
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positive_seconds_normalizes_garbage() {
        assert_eq!(positive_seconds_to_ms(Some(5), 7_000), 5_000);
        assert_eq!(positive_seconds_to_ms(None, 7_000), 7_000);
        assert_eq!(positive_seconds_to_ms(Some(0), 7_000), 7_000);
    }

    #[test]
    fn user_url_prefers_complete_form() {
        let bare = DeviceCodeResponse {
            device_code: "dc".into(),
            user_code: "UC".into(),
            verification_uri: "https://x.ai/device".into(),
            verification_uri_complete: None,
            expires_in: None,
            interval: None,
        };
        assert_eq!(bare.user_url(), "https://x.ai/device");
        let complete = DeviceCodeResponse {
            verification_uri_complete: Some("https://x.ai/device?user_code=UC".into()),
            ..bare
        };
        assert_eq!(complete.user_url(), "https://x.ai/device?user_code=UC");
    }

    #[test]
    fn min_with_margin_clamps_to_remaining() {
        // Interval larger than remaining → capped to remaining, plus margin.
        assert_eq!(min_with_margin(10_000, 2_000), 5_000);
        // Interval smaller than remaining → interval plus margin.
        assert_eq!(min_with_margin(1_000, 60_000), 4_000);
    }
}
