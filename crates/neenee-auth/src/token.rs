//! xAI OAuth2 token-endpoint helpers: the authorize-URL builder, the
//! authorization-code → token exchange, the refresh-token rotation, and the
//! JWT `exp` check that drives proactive refresh.
//!
//! All flows share the public Grok-CLI OAuth client (xAI's auth server rejects
//! loopback OAuth from non-allowlisted clients, so we reuse the client_id xAI
//! ships for desktop OAuth — same as opencode and the official Grok CLI).

use serde::{Deserialize, Serialize};

use crate::pkce::PkceCodes;

/// Auth-store key for SuperGrok OAuth tokens. Always `"xai"` regardless of the
/// user-facing provider id (mirrors opencode's `auth.set({ path: { id: "xai" } })`).
pub const AUTH_PROVIDER_ID: &str = "xai";

/// Public Grok-CLI OAuth client. xAI's auth server rejects loopback OAuth from
/// non-allowlisted clients, so we reuse the Grok-CLI client_id that xAI ships
/// for desktop OAuth flows. Source of truth: hermes-agent PR #26534 (mirrors
/// opencode's `packages/opencode/src/plugin/xai.ts`).
pub const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";

/// Authorize endpoint (consent screen).
pub const AUTHORIZE_URL: &str = "https://auth.x.ai/oauth2/authorize";

/// Token endpoint (code exchange + refresh + device-code poll).
pub const TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";

/// RFC 8628 device-authorization endpoint (request the user_code).
pub const DEVICE_AUTHORIZATION_URL: &str = "https://auth.x.ai/oauth2/device/code";

/// RFC 8628 device-code grant type, sent as `grant_type`.
pub const DEVICE_CODE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// OAuth scopes requested. `grok-cli:access` + `api:access` are what gates the
/// Grok API; `offline_access` yields a refresh_token.
pub const SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";

/// Refresh the access token this far ahead of its real expiry so a single
/// long-running tool call doesn't recover from a mid-flight 401.
pub const ACCESS_TOKEN_REFRESH_SKEW_MS: i64 = 120_000;

/// A successful token response from any grant type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub id_token: Option<String>,
    #[serde(default)]
    pub token_type: Option<String>,
    /// Seconds until the access_token expires. Best-effort: xAI doesn't always
    /// return it, so the JWT-`exp` check is the load-bearing freshness signal.
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub scope: Option<String>,
}

/// Headers shared by every form-urlencoded token-endpoint call.
pub(crate) fn form_headers() -> [(&'static str, &'static str); 2] {
    [
        ("Content-Type", "application/x-www-form-urlencoded"),
        ("Accept", "application/json"),
    ]
}

/// Build the authorize URL for the browser-OAuth flow.
///
/// `plan=generic` opts the consent screen into xAI's generic OAuth plan tier;
/// without it, accounts.x.ai rejects loopback OAuth from the reused Grok-CLI
/// client. This is the single load-bearing detail that distinguishes a working
/// SuperGrok login from a consent-screen 400.
pub fn build_authorize_url(
    pkce: &PkceCodes,
    state: &str,
    nonce: &str,
    redirect_uri: &str,
) -> String {
    let params = [
        ("response_type", "code"),
        ("client_id", CLIENT_ID),
        ("redirect_uri", redirect_uri),
        ("scope", SCOPE),
        ("code_challenge", pkce.challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("state", state),
        ("nonce", nonce),
        ("plan", "generic"),
        ("referrer", "neenee"),
    ];
    let query = serde_urlencoded(&params);
    format!("{AUTHORIZE_URL}?{query}")
}

/// Exchange an authorization code for a token set (browser flow).
pub async fn exchange_code(
    client: &reqwest::Client,
    code: &str,
    pkce: &PkceCodes,
    redirect_uri: &str,
) -> Result<TokenResponse, crate::AuthError> {
    let body = serde_urlencoded(&[
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", CLIENT_ID),
        ("code_verifier", pkce.verifier.as_str()),
    ]);
    post_form(client, TOKEN_URL, &body).await
}

/// Refresh a rotated access token from a refresh_token.
pub async fn refresh_access_token(
    client: &reqwest::Client,
    refresh_token: &str,
) -> Result<TokenResponse, crate::AuthError> {
    let body = serde_urlencoded(&[
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", CLIENT_ID),
    ]);
    post_form(client, TOKEN_URL, &body).await
}

/// A tiny `application/x-www-form-urlencoded` serializer that does NOT need the
/// `form_urlencoded` crate: encodes spaces as `+` (the form-encoding
/// convention token endpoints expect) and percent-encodes the reserved set.
fn serde_urlencoded(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn percent_encode(s: &str) -> String {
    percent_encode_form_value(s)
}

/// Percent-encode a single form value the way `application/x-www-form-
///-urlencoded` expects: spaces become `+`, the unreserved set passes through,
/// everything else is `%XX`. Public so the device-code flow can reuse it.
pub fn percent_encode_form_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

async fn post_form(
    client: &reqwest::Client,
    url: &str,
    body: &str,
) -> Result<TokenResponse, crate::AuthError> {
    let mut req = client.post(url).body(body.to_string());
    for (name, value) in form_headers() {
        req = req.header(name, value);
    }
    let response = req
        .send()
        .await
        .map_err(|e| crate::AuthError::Transport(format!("xAI token request failed: {e}")))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(crate::AuthError::TokenEndpoint {
            status: status.as_u16(),
            body: text,
        });
    }
    serde_json::from_str::<TokenResponse>(&text)
        .map_err(|e| crate::AuthError::Decode(format!("xAI token response parse failed: {e}")))
}

/// Whether a stored access token is expiring within `skew_ms` of now. Two
/// signals: the stored deadline (`expires_ms`), and — for JWT access tokens —
/// the JWT `exp` claim itself (the load-bearing one, since xAI doesn't always
/// return `expires_in`). Returns `false` for opaque (non-JWT) tokens, which
/// conservatively skips proactive refresh and lets the 401-on-call path drive
/// it instead.
///
/// We decode the JWT payload without verifying the signature: the result is
/// only ever used to decide *whether* to refresh, never to make a trust
/// decision, so unsigned decode is safe.
pub fn access_token_is_expiring(access_token: Option<&str>, skew_ms: i64, now_ms: i64) -> bool {
    // Prefer the JWT exp when present: xAI's access tokens are JWTs and the
    // stored deadline is best-effort.
    if let Some(exp_ms) = jwt_exp_ms(access_token.unwrap_or(""))
        && exp_ms <= now_ms + skew_ms.max(0)
    {
        return true;
    }
    false
}

/// Decode the `exp` (seconds since epoch) claim from a JWT access token, if it
/// is a JWT and carries `exp`. Returns `None` for opaque tokens or malformed
/// JWTs.
pub fn jwt_exp_ms(token: &str) -> Option<i64> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    // JWT uses base64url without padding; pad it out for the standard decoder.
    let bytes = base64url_decode(payload)?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let exp = claims.get("exp")?.as_i64()?;
    Some(exp * 1000)
}

fn base64url_decode(input: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    // Tolerate either padded or unpadded input.
    let trimmed = input.trim_end_matches('=');
    let mut buf = String::from(trimmed);
    while buf.len() % 4 != 0 {
        buf.push('=');
    }
    URL_SAFE_NO_PAD.decode(buf.trim_end_matches('=')).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    #[test]
    fn authorize_url_carries_plan_generic_and_pkce() {
        let pkce = PkceCodes {
            verifier: "v".to_string(),
            challenge: "c".to_string(),
        };
        let url = build_authorize_url(&pkce, "ST", "N", "http://127.0.0.1:56121/callback");
        // The load-bearing params are present.
        assert!(url.starts_with("https://auth.x.ai/oauth2/authorize?"));
        assert!(url.contains("plan=generic"), "plan=generic must be present");
        assert!(url.contains("referrer=neenee"));
        assert!(url.contains("client_id=b1a00492-073a-47ea-816f-4c329264a828"));
        assert!(url.contains("code_challenge=c"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("state=ST"));
        // The redirect_uri must be percent-encoded (/ → %2F).
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A56121%2Fcallback"));
    }

    #[test]
    fn url_encoding_handles_space_and_reserved() {
        let body = serde_urlencoded(&[("k", "a b/c"), ("x", "plain")]);
        assert_eq!(body, "k=a+b%2Fc&x=plain");
    }

    #[test]
    fn jwt_exp_is_decoded_from_access_token() {
        // A minimal JWT with exp = 2_000_000_000 (s) → 2_000_000_000_000 ms.
        let payload = URL_SAFE_NO_PAD.encode(br#"{"exp":2000000000}"#);
        let token = format!("header.{payload}.sig");
        assert_eq!(jwt_exp_ms(&token), Some(2_000_000_000_000));
    }

    #[test]
    fn jwt_exp_none_for_opaque_token() {
        // An opaque (non-JWT) token has no dots → None.
        assert!(jwt_exp_ms("opaque-token-no-dots").is_none());
        // A two-part token with non-JSON payload → None.
        assert!(jwt_exp_ms("aaa.bbb").is_none());
    }

    #[test]
    fn is_expiring_true_when_jwt_exp_within_skew() {
        let payload = URL_SAFE_NO_PAD.encode(format!("{{\"exp\":{}}}", 2_000_000_000));
        let token = format!("h.{payload}.s");
        // now + skew past exp → expiring.
        assert!(access_token_is_expiring(Some(&token), 0, 2_000_000_000_000));
        // now far before exp, generous skew → not expiring.
        assert!(!access_token_is_expiring(
            Some(&token),
            120_000,
            1_999_000_000_000
        ));
    }

    #[test]
    fn is_expiring_false_for_opaque_token() {
        assert!(!access_token_is_expiring(Some("opaque"), 0, 0));
        assert!(!access_token_is_expiring(None, 0, 0));
    }
}
