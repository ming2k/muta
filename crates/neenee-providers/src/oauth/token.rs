//! OAuth2 token-endpoint helpers shared by every provider: the authorize-URL
//! builder, the authorization-code → token exchange, the refresh-token
//! rotation, and the JWT `exp`/`chatgpt_account_id` decoding that drives
//! proactive refresh and account-id capture.
//!
//! Provider specifics (client id, endpoints, scopes, extra authorize params)
//! live on [`crate::oauth::config::OAuthConfig`]; these functions are the generic
//! mechanics parameterized by it.

use serde::{Deserialize, Serialize};

use neenee_core::SecretString;

use crate::oauth::config::OAuthConfig;
use crate::oauth::pkce::PkceCodes;

/// Refresh the access token this far ahead of its real expiry so a single
/// long-running tool call doesn't recover from a mid-flight 401.
pub const ACCESS_TOKEN_REFRESH_SKEW_MS: i64 = 120_000;

/// A successful token response from any grant type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenResponse {
    pub access_token: SecretString,
    #[serde(default)]
    pub refresh_token: Option<SecretString>,
    #[serde(default)]
    pub id_token: Option<SecretString>,
    #[serde(default)]
    pub token_type: Option<String>,
    /// Seconds until the access_token expires. Best-effort: providers don't
    /// always return it, so the JWT-`exp` check is the load-bearing freshness
    /// signal.
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
/// Provider-specific extras (xAI's `plan=generic`, OpenAI's
/// `codex_cli_simplified_flow`) ride on [`OAuthConfig::extra_authorize_params`].
pub fn build_authorize_url(
    cfg: &OAuthConfig,
    pkce: &PkceCodes,
    state: &str,
    nonce: &str,
    redirect_uri: &str,
) -> String {
    let mut params: Vec<(&str, &str)> = vec![
        ("response_type", "code"),
        ("client_id", cfg.client_id),
        ("redirect_uri", redirect_uri),
        ("scope", cfg.scope),
        ("code_challenge", pkce.challenge.as_str()),
        ("code_challenge_method", "S256"),
        ("state", state),
    ];
    // xAI's flow carries an OIDC nonce; OpenAI's simplified flow rejects it.
    if cfg.send_nonce {
        params.push(("nonce", nonce));
    }
    params.extend_from_slice(cfg.extra_authorize_params);
    let query = serde_urlencoded(&params);
    format!("{}?{query}", cfg.authorize_url)
}

/// Exchange an authorization code for a token set (browser flow).
pub async fn exchange_code(
    client: &reqwest::Client,
    cfg: &OAuthConfig,
    code: &str,
    pkce: &PkceCodes,
    redirect_uri: &str,
) -> Result<TokenResponse, crate::oauth::AuthError> {
    let body = serde_urlencoded(&[
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", cfg.client_id),
        ("code_verifier", pkce.verifier.expose_secret()),
    ]);
    post_form(client, cfg.token_url, &body).await
}

/// Refresh a rotated access token from a refresh_token.
pub async fn refresh_access_token(
    client: &reqwest::Client,
    cfg: &OAuthConfig,
    refresh_token: &str,
) -> Result<TokenResponse, crate::oauth::AuthError> {
    let body = serde_urlencoded(&[
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", cfg.client_id),
    ]);
    post_form(client, cfg.token_url, &body).await
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
/// -urlencoded` expects: spaces become `+`, the unreserved set passes through,
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

/// Serialize `&[(&str,&str)]` into an `application/x-www-form-urlencoded` body.
/// Public so the device flows can build exchange bodies without duplicating the
/// encoder.
pub fn percent_encode_form_pairs(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| {
            format!(
                "{}={}",
                percent_encode_form_value(k),
                percent_encode_form_value(v)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

pub(crate) async fn post_form(
    client: &reqwest::Client,
    url: &str,
    body: &str,
) -> Result<TokenResponse, crate::oauth::AuthError> {
    let mut req = client.post(url).body(body.to_string());
    for (name, value) in form_headers() {
        req = req.header(name, value);
    }
    let response = req
        .send()
        .await
        .map_err(|e| crate::oauth::AuthError::Transport(format!("token request failed: {e}")))?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(crate::oauth::AuthError::TokenEndpoint {
            status: status.as_u16(),
            body: text,
        });
    }
    serde_json::from_str::<TokenResponse>(&text)
        .map_err(|e| crate::oauth::AuthError::Decode(format!("token response parse failed: {e}")))
}

/// Whether a stored access token is expiring within `skew_ms` of now. Two
/// signals: the stored deadline (`expires_ms`), and — for JWT access tokens —
/// the JWT `exp` claim itself. Returns `false` for opaque (non-JWT) tokens,
/// which conservatively skips proactive refresh and lets the 401-on-call path
/// drive it instead.
///
/// We decode the JWT payload without verifying the signature: the result is
/// only ever used to decide *whether* to refresh, never to make a trust
/// decision, so unsigned decode is safe.
pub fn access_token_is_expiring(access_token: Option<&str>, skew_ms: i64, now_ms: i64) -> bool {
    // Prefer the JWT exp when present: many providers' access tokens are JWTs
    // and the stored deadline is best-effort.
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
    let claims = jwt_claims(token)?;
    let exp = claims.get("exp")?.as_i64()?;
    Some(exp * 1000)
}

/// Decode a JWT's payload claims (without signature verification) as JSON.
pub(crate) fn jwt_claims(token: &str) -> Option<serde_json::Value> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let bytes = base64url_decode(payload)?;
    serde_json::from_slice(&bytes).ok()
}

/// Extract the ChatGPT account id from a JWT (the `id_token` or
/// `access_token`). OpenAI encodes it as `chatgpt_account_id`, or nested under
/// `https://api.openai.com/auth` → `chatgpt_account_id`. Returns `None` for
/// opaque tokens or when the claim is absent — the caller then sends requests
/// without the `ChatGPT-Account-Id` header (still valid for single-account
/// users).
pub fn chatgpt_account_id(token: &str) -> Option<String> {
    let claims = jwt_claims(token)?;
    if let Some(id) = claims.get("chatgpt_account_id").and_then(|v| v.as_str()) {
        return Some(id.to_string());
    }
    claims
        .get("https://api.openai.com/auth")
        .and_then(|v| v.get("chatgpt_account_id"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
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
    use crate::oauth::config::{CHATGPT, XAI};
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;

    #[test]
    fn xai_authorize_url_carries_plan_generic_and_pkce() {
        let pkce = PkceCodes {
            verifier: "v".into(),
            challenge: "c".to_string(),
        };
        let url = build_authorize_url(&XAI, &pkce, "ST", "N", "http://127.0.0.1:56121/callback");
        assert!(url.starts_with("https://auth.x.ai/oauth2/authorize?"));
        assert!(url.contains("plan=generic"), "plan=generic must be present");
        assert!(url.contains("referrer=neenee"));
        assert!(url.contains("client_id=b1a00492-073a-47ea-816f-4c329264a828"));
        assert!(url.contains("code_challenge=c"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("state=ST"));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A56121%2Fcallback"));
    }

    #[test]
    fn chatgpt_authorize_url_carries_codex_flow_param() {
        let pkce = PkceCodes {
            verifier: "v".into(),
            challenge: "c".to_string(),
        };
        let url = build_authorize_url(
            &CHATGPT,
            &pkce,
            "ST",
            "N",
            "http://localhost:1455/auth/callback",
        );
        assert!(url.starts_with("https://auth.openai.com/oauth/authorize?"));
        assert!(url.contains("client_id=app_EMoamEEZ73f0CkXaXp7hrann"));
        assert!(url.contains("codex_cli_simplified_flow=true"));
        assert!(url.contains("id_token_add_organizations=true"));
        assert!(url.contains("originator=neenee"));
        assert!(url.contains("scope=openid+profile+email+offline_access"));
        // OpenAI's simplified flow must NOT carry a nonce.
        assert!(!url.contains("nonce="), "nonce must be absent for ChatGPT");
        // The redirect host must be localhost (not 127.0.0.1) to match the
        // registered Codex client.
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"));
    }

    #[test]
    fn url_encoding_handles_space_and_reserved() {
        let body = serde_urlencoded(&[("k", "a b/c"), ("x", "plain")]);
        assert_eq!(body, "k=a+b%2Fc&x=plain");
    }

    #[test]
    fn jwt_exp_is_decoded_from_access_token() {
        let payload = URL_SAFE_NO_PAD.encode(br#"{"exp":2000000000}"#);
        let token = format!("header.{payload}.sig");
        assert_eq!(jwt_exp_ms(&token), Some(2_000_000_000_000));
    }

    #[test]
    fn jwt_exp_none_for_opaque_token() {
        assert!(jwt_exp_ms("opaque-token-no-dots").is_none());
        assert!(jwt_exp_ms("aaa.bbb").is_none());
    }

    #[test]
    fn chatgpt_account_id_decoded_from_top_level_claim() {
        let payload = URL_SAFE_NO_PAD.encode(r#"{"chatgpt_account_id":"acct-123"}"#);
        let token = format!("h.{payload}.s");
        assert_eq!(chatgpt_account_id(&token), Some("acct-123".to_string()));
    }

    #[test]
    fn chatgpt_account_id_decoded_from_nested_claim() {
        let payload = URL_SAFE_NO_PAD
            .encode(r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acct-9"}}"#);
        let token = format!("h.{payload}.s");
        assert_eq!(chatgpt_account_id(&token), Some("acct-9".to_string()));
    }

    #[test]
    fn chatgpt_account_id_none_when_absent() {
        let payload = URL_SAFE_NO_PAD.encode(r#"{"sub":"x"}"#);
        let token = format!("h.{payload}.s");
        assert!(chatgpt_account_id(&token).is_none());
    }

    #[test]
    fn is_expiring_true_when_jwt_exp_within_skew() {
        let payload = URL_SAFE_NO_PAD.encode(format!("{{\"exp\":{}}}", 2_000_000_000));
        let token = format!("h.{payload}.s");
        assert!(access_token_is_expiring(Some(&token), 0, 2_000_000_000_000));
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
