//! OAuth2 token-endpoint helpers: URL builders, PKCE code exchange, token refresh,
//! JWT claim/expiration inspection, and provider-specific onboarding handlers (Google Antigravity & ChatGPT).

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STD;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

use crate::oauth::config::{ClientAuthMethod, OAuthConfig, PkceMode, TokenRequestFormat};
use crate::oauth::pkce::PkceCodes;
use muta_contracts::SecretString;

/// Refresh the access token ahead of expiry so long-running calls don't hit a 401.
pub const ACCESS_TOKEN_REFRESH_SKEW_MS: i64 = 120_000;

/// Standard Antigravity User-Agent matching official Google Cloud Code / Antigravity CLI.
pub const ANTIGRAVITY_USER_AGENT: &str = muta_contracts::client_identity::ANTIGRAVITY_USER_AGENT;
/// Antigravity Google API client header.
pub const ANTIGRAVITY_API_CLIENT_HEADER: &str = "gl-go/1.23.2 gdcl/0.1";
/// Endpoint for Antigravity loadCodeAssist account metadata.
pub const ANTIGRAVITY_LOAD_CODE_ASSIST_URL: &str =
    "https://daily-cloudcode-pa.googleapis.com/v1internal:loadCodeAssist";
/// Endpoint for Antigravity onboardUser account initialization.
pub const ANTIGRAVITY_ONBOARD_USER_URL: &str =
    "https://daily-cloudcode-pa.googleapis.com/v1internal:onboardUser";
/// Endpoint for Antigravity user quota summary inspection.
pub const ANTIGRAVITY_RETRIEVE_QUOTA_SUMMARY_URL: &str =
    "https://daily-cloudcode-pa.googleapis.com/v1internal:retrieveUserQuotaSummary";
/// Endpoint for Antigravity available models discovery.
pub const ANTIGRAVITY_FETCH_AVAILABLE_MODELS_URL: &str =
    "https://daily-cloudcode-pa.googleapis.com/v1internal:fetchAvailableModels";
/// Google UserInfo endpoint.
pub const GOOGLE_USERINFO_URL: &str = "https://www.googleapis.com/oauth2/v3/userinfo";

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
    /// Seconds until the access_token expires.
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub scope: Option<String>,
}

impl TokenResponse {
    pub fn validate(self) -> Result<Self, crate::oauth::AuthError> {
        if self.access_token.expose_secret().trim().is_empty() {
            return Err(crate::oauth::AuthError::Decode(
                "token endpoint returned an empty access_token".to_string(),
            ));
        }
        Ok(self)
    }
}

/// Google UserInfo response.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GoogleUserInfo {
    pub sub: Option<String>,
    pub email: Option<String>,
    pub email_verified: Option<bool>,
    pub name: Option<String>,
    pub picture: Option<String>,
}

/// Build the authorize URL for the browser-OAuth flow.
pub fn build_authorize_url(
    cfg: &OAuthConfig,
    pkce: &PkceCodes,
    state: &str,
    nonce: &str,
    redirect_uri: &str,
) -> String {
    let mut params: Vec<(&str, &str)> = vec![
        ("response_type", "code"),
        ("client_id", cfg.client_id.as_ref()),
        ("redirect_uri", redirect_uri),
        ("scope", cfg.scope.as_ref()),
        ("state", state),
    ];

    match cfg.pkce_mode {
        PkceMode::S256 => {
            params.push(("code_challenge", pkce.challenge.as_str()));
            params.push(("code_challenge_method", "S256"));
        }
        PkceMode::Plain => {
            params.push(("code_challenge", pkce.verifier.expose_secret()));
            params.push(("code_challenge_method", "plain"));
        }
        PkceMode::Disabled => {}
    }

    if cfg.send_nonce {
        params.push(("nonce", nonce));
    }

    for (k, v) in &cfg.extra_authorize_params {
        params.push((k.as_ref(), v.as_ref()));
    }

    let query = serde_urlencoded(&params);
    format!("{}?{query}", cfg.authorize_url)
}

/// Exchange an authorization code for a token set (browser / manual flow).
pub async fn exchange_code(
    client: &reqwest::Client,
    cfg: &OAuthConfig,
    code: &str,
    pkce: &PkceCodes,
    redirect_uri: &str,
) -> Result<TokenResponse, crate::oauth::AuthError> {
    let mut params: Vec<(&str, &str)> = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", cfg.client_id.as_ref()),
    ];

    if cfg.pkce_mode != PkceMode::Disabled {
        params.push(("code_verifier", pkce.verifier.expose_secret()));
    }

    let mut basic_auth: Option<String> = None;
    match cfg.client_auth_method {
        ClientAuthMethod::RequestBody => {
            if let Some(secret) = &cfg.client_secret {
                params.push(("client_secret", secret.as_ref()));
            }
        }
        ClientAuthMethod::BasicHeader => {
            if let Some(secret) = &cfg.client_secret {
                let raw = format!("{}:{}", cfg.client_id, secret);
                basic_auth = Some(format!("Basic {}", BASE64_STD.encode(raw)));
            }
        }
        ClientAuthMethod::None => {
            // Optional fallback: if client_secret is set, send in body
            if let Some(secret) = &cfg.client_secret {
                params.push(("client_secret", secret.as_ref()));
            }
        }
    }

    for (k, v) in &cfg.extra_token_params {
        params.push((k.as_ref(), v.as_ref()));
    }

    execute_token_request(client, cfg, &params, basic_auth.as_deref()).await
}

/// Refresh a rotated access token from a refresh_token.
pub async fn refresh_access_token(
    client: &reqwest::Client,
    cfg: &OAuthConfig,
    refresh_token: &str,
) -> Result<TokenResponse, crate::oauth::AuthError> {
    let mut params: Vec<(&str, &str)> = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", cfg.client_id.as_ref()),
    ];

    let mut basic_auth: Option<String> = None;
    match cfg.client_auth_method {
        ClientAuthMethod::RequestBody => {
            if let Some(secret) = &cfg.client_secret {
                params.push(("client_secret", secret.as_ref()));
            }
        }
        ClientAuthMethod::BasicHeader => {
            if let Some(secret) = &cfg.client_secret {
                let raw = format!("{}:{}", cfg.client_id, secret);
                basic_auth = Some(format!("Basic {}", BASE64_STD.encode(raw)));
            }
        }
        ClientAuthMethod::None => {
            if let Some(secret) = &cfg.client_secret {
                params.push(("client_secret", secret.as_ref()));
            }
        }
    }

    for (k, v) in &cfg.extra_refresh_params {
        params.push((k.as_ref(), v.as_ref()));
    }

    execute_token_request(client, cfg, &params, basic_auth.as_deref()).await
}

async fn execute_token_request(
    client: &reqwest::Client,
    cfg: &OAuthConfig,
    params: &[(&str, &str)],
    basic_auth: Option<&str>,
) -> Result<TokenResponse, crate::oauth::AuthError> {
    match cfg.token_format {
        TokenRequestFormat::FormUrlEncoded => {
            let body = serde_urlencoded(params);
            let mut req = client
                .post(cfg.token_url.as_ref())
                .header("Content-Type", "application/x-www-form-urlencoded")
                .header("Accept", "application/json")
                .body(body);

            if let Some(ua) = &cfg.user_agent {
                req = req.header("User-Agent", ua.as_ref());
            }
            if let Some(auth) = basic_auth {
                req = req.header("Authorization", auth);
            }
            for (k, v) in &cfg.extra_headers {
                req = req.header(k.as_ref(), v.as_ref());
            }

            let resp = req.send().await.map_err(|e| {
                crate::oauth::AuthError::Transport(format!("token request failed: {e}"))
            })?;
            let status = resp.status();
            let text = read_response_text(resp, "token response").await?;
            if !status.is_success() {
                return Err(crate::oauth::AuthError::TokenEndpoint {
                    status: status.as_u16(),
                    body: text,
                });
            }
            let parsed = serde_json::from_str::<TokenResponse>(&text).map_err(|e| {
                crate::oauth::AuthError::Decode(format!("token response parse failed: {e}"))
            })?;
            parsed.validate()
        }
        TokenRequestFormat::Json => {
            let mut map = serde_json::Map::new();
            for (k, v) in params {
                map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
            }
            let mut req = client
                .post(cfg.token_url.as_ref())
                .header("Content-Type", "application/json")
                .header("Accept", "application/json")
                .json(&serde_json::Value::Object(map));

            if let Some(ua) = &cfg.user_agent {
                req = req.header("User-Agent", ua.as_ref());
            }
            if let Some(auth) = basic_auth {
                req = req.header("Authorization", auth);
            }
            for (k, v) in &cfg.extra_headers {
                req = req.header(k.as_ref(), v.as_ref());
            }

            let resp = req.send().await.map_err(|e| {
                crate::oauth::AuthError::Transport(format!("token request failed: {e}"))
            })?;
            let status = resp.status();
            let text = read_response_text(resp, "token response").await?;
            if !status.is_success() {
                return Err(crate::oauth::AuthError::TokenEndpoint {
                    status: status.as_u16(),
                    body: text,
                });
            }
            let parsed = serde_json::from_str::<TokenResponse>(&text).map_err(|e| {
                crate::oauth::AuthError::Decode(format!("token response parse failed: {e}"))
            })?;
            parsed.validate()
        }
    }
}

/// A tiny `application/x-www-form-urlencoded` serializer.
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

/// Percent-encode a single form value.
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
    let response = client
        .post(url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("Accept", "application/json")
        .body(body.to_string())
        .send()
        .await
        .map_err(|e| crate::oauth::AuthError::Transport(format!("token request failed: {e}")))?;
    let status = response.status();
    let text = read_response_text(response, "token response").await?;
    if !status.is_success() {
        return Err(crate::oauth::AuthError::TokenEndpoint {
            status: status.as_u16(),
            body: text,
        });
    }
    let parsed = serde_json::from_str::<TokenResponse>(&text)
        .map_err(|e| crate::oauth::AuthError::Decode(format!("token response parse failed: {e}")))?;
    parsed.validate()
}

/// Whether a stored access token is expiring within `skew_ms` of now.
pub fn access_token_is_expiring(access_token: Option<&str>, skew_ms: i64, now_ms: i64) -> bool {
    if let Some(exp_ms) = jwt_exp_ms(access_token.unwrap_or(""))
        && exp_ms <= now_ms + skew_ms.max(0)
    {
        return true;
    }
    false
}

/// Decode the `exp` claim from a JWT access token.
pub fn jwt_exp_ms(token: &str) -> Option<i64> {
    let claims = jwt_claims(token)?;
    let exp = claims.get("exp")?.as_i64()?;
    Some(exp * 1000)
}

/// Resolve an access token's absolute expiration without inventing a TTL.
/// Explicit OAuth metadata wins, JWT `exp` is the fallback, and an opaque
/// token with neither is treated as non-expiring.
pub fn access_token_expiry_ms(access_token: &str, expires_in: Option<u64>, now_ms: i64) -> i64 {
    expires_in
        .and_then(|seconds| {
            i64::try_from(seconds)
                .ok()
                .and_then(|seconds| seconds.checked_mul(1_000))
                .and_then(|ttl| now_ms.checked_add(ttl))
        })
        .or_else(|| jwt_exp_ms(access_token))
        .unwrap_or(i64::MAX)
}

/// Decode a JWT's payload claims (without signature verification) as JSON.
pub(crate) fn jwt_claims(token: &str) -> Option<serde_json::Value> {
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let bytes = base64url_decode(payload)?;
    serde_json::from_slice(&bytes).ok()
}

/// Extract the ChatGPT account id from a JWT (id_token or access_token).
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

/// Fetch Google UserInfo (email, name, sub, picture) using access token.
pub async fn fetch_google_userinfo(
    client: &reqwest::Client,
    access_token: &str,
) -> Result<GoogleUserInfo, crate::oauth::AuthError> {
    let resp = client
        .get(GOOGLE_USERINFO_URL)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| crate::oauth::AuthError::Transport(format!("userinfo request failed: {e}")))?;

    if resp.status().is_success() {
        let info = resp
            .json::<GoogleUserInfo>()
            .await
            .map_err(|e| crate::oauth::AuthError::Decode(format!("userinfo parse failed: {e}")))?;
        Ok(info)
    } else {
        let status = resp.status().as_u16();
        Err(crate::oauth::AuthError::TokenEndpoint {
            status,
            body: read_response_text(resp, "userinfo error response").await?,
        })
    }
}

pub(crate) async fn read_response_text(
    response: reqwest::Response,
    context: &str,
) -> Result<String, crate::oauth::AuthError> {
    response.text().await.map_err(|error| {
        crate::oauth::AuthError::Transport(format!("could not read {context}: {error}"))
    })
}

/// Discover or onboard the user's Antigravity `cloudaicompanionProject`.
pub async fn resolve_antigravity_project(
    client: &reqwest::Client,
    access_token: &str,
) -> Result<String, crate::oauth::AuthError> {
    let load_body = serde_json::json!({
        "metadata": {
            "ideType": "ANTIGRAVITY",
            "ideVersion": "1.23.2",
            "ideName": "antigravity",
            "platform": "LINUX_AMD64",
            "pluginType": "GEMINI"
        }
    });

    let resp = client
        .post(ANTIGRAVITY_LOAD_CODE_ASSIST_URL)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("User-Agent", ANTIGRAVITY_USER_AGENT)
        .header("x-goog-api-client", ANTIGRAVITY_API_CLIENT_HEADER)
        .header("Content-Type", "application/json")
        .json(&load_body)
        .send()
        .await
        .map_err(|e| crate::oauth::AuthError::Transport(format!("loadCodeAssist failed: {e}")))?;

    if resp.status().is_success()
        && let Ok(val) = resp.json::<serde_json::Value>().await
    {
        if let Some(p) = extract_cloudaicompanion_project(&val) {
            tracing::info!(project = %p, "resolved existing Antigravity cloudaicompanionProject");
            return Ok(p);
        }

        // Project missing; attempt onboardUser with detected tier (defaulting to g1-pro-tier)
        let tier_id = val
            .get("paidTier")
            .or_else(|| val.get("paid_tier"))
            .and_then(|p| p.get("id").or(Some(p)))
            .and_then(|id| id.as_str())
            .or_else(|| {
                val.get("currentTier")
                    .or_else(|| val.get("current_tier"))
                    .and_then(|c| c.get("id").or(Some(c)))
                    .and_then(|id| id.as_str())
            })
            .or_else(|| {
                val.get("allowedTiers")
                    .or_else(|| val.get("allowed_tiers"))
                    .and_then(|a| a.as_array())
                    .and_then(|arr| arr.first())
                    .and_then(|t| t.get("id").or(Some(t)))
                    .and_then(|id| id.as_str())
            })
            .unwrap_or("g1-pro-tier");

        let onboard_body = serde_json::json!({
            "tierId": tier_id,
            "metadata": {
                "ideType": "ANTIGRAVITY",
                "ideVersion": "1.23.2",
                "ideName": "antigravity",
                "platform": "LINUX_AMD64",
                "pluginType": "GEMINI"
            }
        });

        let onboard_resp = client
            .post(ANTIGRAVITY_ONBOARD_USER_URL)
            .header("Authorization", format!("Bearer {access_token}"))
            .header("User-Agent", ANTIGRAVITY_USER_AGENT)
            .header("x-goog-api-client", ANTIGRAVITY_API_CLIENT_HEADER)
            .header("Content-Type", "application/json")
            .json(&onboard_body)
            .send()
            .await
            .map_err(|e| crate::oauth::AuthError::Transport(format!("onboardUser failed: {e}")))?;

        if onboard_resp.status().is_success() {
            if let Ok(onboard_val) = onboard_resp.json::<serde_json::Value>().await
                && let Some(p) = extract_cloudaicompanion_project(&onboard_val)
            {
                tracing::info!(project = %p, tier = %tier_id, "onboarded Antigravity cloudaicompanionProject");
                return Ok(p);
            }

            // If onboardUser completed, retry loadCodeAssist to read the freshly provisioned project
            if let Ok(second_resp) = client
                .post(ANTIGRAVITY_LOAD_CODE_ASSIST_URL)
                .header("Authorization", format!("Bearer {access_token}"))
                .header("User-Agent", ANTIGRAVITY_USER_AGENT)
                .header("x-goog-api-client", ANTIGRAVITY_API_CLIENT_HEADER)
                .header("Content-Type", "application/json")
                .json(&load_body)
                .send()
                .await
                && second_resp.status().is_success()
                && let Ok(second_val) = second_resp.json::<serde_json::Value>().await
                && let Some(p) = extract_cloudaicompanion_project(&second_val)
            {
                tracing::info!(project = %p, "resolved newly onboarded Antigravity cloudaicompanionProject");
                return Ok(p);
            }
        }
    }

    Ok(String::new())
}

/// Retrieve user quota summary from Google Antigravity CodeAssist backend.
pub async fn retrieve_antigravity_quota_summary(
    client: &reqwest::Client,
    access_token: &str,
    project: Option<&str>,
) -> Result<crate::usage::AntigravityQuotaSummaryResponse, crate::oauth::AuthError> {
    let req_body = serde_json::json!({
        "project": project.unwrap_or("")
    });

    let resp = client
        .post(ANTIGRAVITY_RETRIEVE_QUOTA_SUMMARY_URL)
        .header("Authorization", format!("Bearer {access_token}"))
        .header("User-Agent", ANTIGRAVITY_USER_AGENT)
        .header("x-goog-api-client", ANTIGRAVITY_API_CLIENT_HEADER)
        .header("Content-Type", "application/json")
        .json(&req_body)
        .send()
        .await
        .map_err(|e| crate::oauth::AuthError::Transport(format!("retrieveUserQuotaSummary failed: {e}")))?;

    if resp.status().is_success() {
        let quota = resp
            .json::<crate::usage::AntigravityQuotaSummaryResponse>()
            .await
            .map_err(|e| crate::oauth::AuthError::Decode(format!("retrieveUserQuotaSummary parse failed: {e}")))?;
        Ok(quota)
    } else {
        let status = resp.status().as_u16();
        Err(crate::oauth::AuthError::TokenEndpoint {
            status,
            body: read_response_text(resp, "retrieveUserQuotaSummary error response").await?,
        })
    }
}

/// Extract the Antigravity `cloudaicompanionProject` ID / name from any Google CodeAssist JSON response.
pub fn extract_cloudaicompanion_project(val: &serde_json::Value) -> Option<String> {
    let target = val.get("response").unwrap_or(val);
    let project = target
        .get("cloudaicompanionProject")
        .or_else(|| target.get("cloudaicompanion_project"))
        .or_else(|| target.get("project"))
        .or_else(|| target.get("duetProject"))
        .or_else(|| target.get("duet_project"))
        .or(
            if target.is_object()
                && (target.get("id").is_some()
                    || target.get("projectNumber").is_some()
                    || target.get("name").is_some())
            {
                Some(target)
            } else {
                None
            },
        )?;

    if let Some(p) = project.as_str().filter(|p| !p.trim().is_empty()) {
        let trimmed = p.trim();
        return Some(if trimmed.starts_with("projects/") {
            trimmed.to_string()
        } else if trimmed.chars().all(|c| c.is_ascii_digit()) {
            format!("projects/{trimmed}")
        } else {
            trimmed.to_string()
        });
    }

    if let Some(name) = project
        .get("name")
        .and_then(|n| n.as_str())
        .filter(|n| !n.trim().is_empty())
    {
        let trimmed = name.trim();
        return Some(if trimmed.starts_with("projects/") {
            trimmed.to_string()
        } else if trimmed.chars().all(|c| c.is_ascii_digit()) {
            format!("projects/{trimmed}")
        } else {
            trimmed.to_string()
        });
    }

    if let Some(id) = project
        .get("id")
        .and_then(|i| i.as_str())
        .filter(|id| !id.trim().is_empty())
    {
        let trimmed = id.trim();
        return Some(if trimmed.starts_with("projects/") {
            trimmed.to_string()
        } else if trimmed.chars().all(|c| c.is_ascii_digit()) {
            format!("projects/{trimmed}")
        } else {
            trimmed.to_string()
        });
    }

    if let Some(num) = project
        .get("projectNumber")
        .or_else(|| project.get("project_number"))
        .and_then(|n| n.as_str())
        .filter(|n| !n.trim().is_empty())
    {
        return Some(format!("projects/{}", num.trim()));
    }

    if let Some(num) = project
        .get("projectNumber")
        .or_else(|| project.get("project_number"))
        .and_then(|n| n.as_i64())
    {
        return Some(format!("projects/{num}"));
    }

    if let Some(num) = project.as_i64() {
        return Some(format!("projects/{num}"));
    }
    if let Some(id_num) = project.get("id").and_then(|i| i.as_i64()) {
        return Some(format!("projects/{id_num}"));
    }
    None
}

fn base64url_decode(input: &str) -> Option<Vec<u8>> {
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

    #[test]
    fn xai_authorize_url_carries_plan_generic_and_pkce() {
        let pkce = PkceCodes {
            verifier: "v".into(),
            challenge: "c".to_string(),
        };
        let url = build_authorize_url(&XAI, &pkce, "ST", "N", "http://127.0.0.1:56121/callback");
        assert!(url.starts_with("https://auth.x.ai/oauth2/authorize?"));
        assert!(url.contains("plan=generic"), "plan=generic must be present");
        assert!(url.contains("referrer=muta"));
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
        assert!(url.contains("originator=muta"));
        assert!(url.contains("scope=openid+profile+email+offline_access"));
        assert!(!url.contains("nonce="), "nonce must be absent for ChatGPT");
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
    fn token_expiry_never_invents_a_ttl_for_opaque_tokens() {
        assert_eq!(access_token_expiry_ms("opaque", None, 123), i64::MAX);
        assert_eq!(access_token_expiry_ms("opaque", Some(60), 1_000), 61_000);
    }

    #[test]
    fn token_expiry_uses_jwt_exp_when_oauth_ttl_is_absent() {
        let payload = URL_SAFE_NO_PAD.encode(br#"{"exp":2000000000}"#);
        let token = format!("header.{payload}.sig");
        assert_eq!(access_token_expiry_ms(&token, None, 1), 2_000_000_000_000);
    }

    #[test]
    fn chatgpt_account_id_decoded_from_top_level_claim() {
        let payload = URL_SAFE_NO_PAD.encode(r#"{"chatgpt_account_id":"acct-123"}"#);
        let token = format!("h.{payload}.s");
        assert_eq!(chatgpt_account_id(&token), Some("acct-123".to_string()));
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
