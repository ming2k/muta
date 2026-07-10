//! Live model-list discovery from each provider's API.
//!
//! A provider instance created from a template can either mirror the
//! template's *compiled-in* model list ([`crate::registry::ProviderTemplateSpec`])
//! or fetch the list *live* from the provider's own `GET /models` endpoint.
//! This module owns the live path: it speaks the three wire protocols
//! (`openai` / `anthropic` / `gemini`), authenticates the same way a chat
//! request would, and parses the returned model ids.
//!
//! ## Priority
//!
//! The catalog reconciliation layer decides which instances use live
//! discovery via a `ModelSource` flag on `UserProviderConfig` (see
//! `neenee-store::config` and `neenee_agent::catalog::reconcile_provider_models`).
//! There the priority is explicit: **(1)** live API query when `ModelSource::Api`,
//! falling back to **(2)** the template's compiled-in snapshot on any error
//! (network failure, non-2xx, unparseable body, empty list). `ModelSource::Fixed`
//! skips the network entirely and uses the snapshot.
//!
//! ## Protocol details
//!
//! - **OpenAI-compatible** (OpenAI, DeepSeek, xAI, Kimi, Z.AI, sub2api
//!   relays): `GET {base}/v1/models`, `Authorization: Bearer <key>`, body
//!   `{data: [{id}, …]}`. Auth matches the chat path: a keyless relay sends
//!   no bearer header at all.
//! - **Anthropic**: `GET {base}/v1/models`, `x-api-key` + `anthropic-version`,
//!   body `{data: [{id}, …]}`.
//! - **Gemini native**: `GET {base}/v1beta/models?key=<key>`,
//!   body `{models: [{name: "models/<id>", supportedGenerationMethods: […]}, …]}`
//!   — only `generateContent`-capable text models are kept.
//!
//! ## Endpoint derivation
//!
//! The chat endpoint a channel already carries is the source of truth; this
//! module strips the path suffix (`/chat/completions`, `/messages`, or the
//! bare `/v1beta` root) and re-appends the models path. A caller that already
//! has a bare API root can pass it directly.

use std::time::Duration;

use serde_json::Value;

/// The protocol a discovery request speaks. Kept as a string (not the
/// `neenee_store::UserTransport` enum) so this module has zero dependency on
/// `neenee-store` — the calling layer maps its transport to one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryProtocol {
    /// OpenAI-compatible chat completions → `GET /v1/models`.
    OpenAi,
    /// Anthropic `/messages` → `GET /v1/models` with `x-api-key`.
    Anthropic,
    /// Google Gemini native → `GET /v1beta/models?key=`.
    Gemini,
}

impl DiscoveryProtocol {
    /// Parse a template registry protocol string into a discovery protocol.
    /// Unknown values fall back to [`DiscoveryProtocol::OpenAi`] (the most
    /// common relay shape) rather than erroring, so a future protocol label
    /// degrades to "best effort" discovery.
    pub fn from_template_protocol(protocol: &str) -> Self {
        match protocol {
            "anthropic" => Self::Anthropic,
            "gemini" => Self::Gemini,
            _ => Self::OpenAi,
        }
    }
}

/// Everything a live discovery request needs, borrowed from the instance's
/// first channel. Fields mirror what a chat request would use so the auth
/// matches exactly.
#[derive(Debug, Clone)]
pub struct ModelDiscoveryRequest<'a> {
    pub protocol: DiscoveryProtocol,
    /// The channel's chat endpoint base URL (e.g.
    /// `https://api.openai.com/v1/chat/completions`). The models path is
    /// derived from it via [`models_endpoint_for`].
    pub base_url: &'a str,
    pub api_key: &'a str,
    pub user_agent: Option<&'a str>,
}

/// Why a live model list could not be obtained. The catalog layer treats every
/// variant the same way — fall back to the compiled-in snapshot — so the
/// variants exist only for diagnostics/logging.
#[derive(Debug)]
pub enum ModelListError {
    /// The chat base URL could not be turned into a models URL (e.g. it was
    /// empty or had an unexpected shape).
    BadEndpoint(String),
    /// The HTTP request failed (network/DNS/TLS). Carries the underlying
    /// reqwest error for logging.
    Http(reqwest::Error),
    /// The API returned a non-2xx status. Carries the status code and body
    /// snippet so a misconfigured key surfaces a readable reason.
    Status(u16, String),
    /// The response body could not be parsed into a model list (missing
    /// `data`/`models`, wrong types). Carries a short description.
    Parse(String),
    /// The list parsed but contained zero usable model ids. Treated as a
    /// failure so the catalog keeps the snapshot rather than blanking the
    /// instance — an empty live list is almost always an auth/parsing issue,
    /// never a genuinely model-less provider.
    Empty,
}

impl std::fmt::Display for ModelListError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadEndpoint(msg) => write!(f, "bad model-list endpoint: {msg}"),
            Self::Http(e) => write!(f, "model-list HTTP request failed: {e}"),
            Self::Status(code, body) => {
                let snippet = body.chars().take(200).collect::<String>();
                write!(f, "model-list request returned HTTP {code}: {snippet}")
            }
            Self::Parse(msg) => write!(f, "could not parse model list: {msg}"),
            Self::Empty => write!(f, "model list parsed but contained no models"),
        }
    }
}

impl std::error::Error for ModelListError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Http(e) => Some(e),
            _ => None,
        }
    }
}

/// How long a live model-list request may take before it is abandoned. Kept
/// short: discovery runs at startup and must never block the app on a slow or
/// unreachable relay — the snapshot fallback covers the gap.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

/// Derive the `GET /models` endpoint from a chat endpoint base URL.
///
/// The chat endpoint is the authority (it is what the channel actually calls),
/// so discovery reuses its host and scheme rather than guessing a separate
/// models host. Path handling per protocol:
///
/// - `openai`: strip a trailing `/chat/completions` (and any `/v1/chat/…`),
///   keep the API root, append `/models`. Accepts both a full
///   `…/v1/chat/completions` URL and a bare `…/v1` root.
/// - `anthropic`: strip a trailing `/messages`, append `/models`.
/// - `gemini`: the base is already the API root (`…/v1beta`); append
///   `/models`.
///
/// Returns [`ModelListError::BadEndpoint`] only for an empty/whitespace base.
pub fn models_endpoint_for(
    protocol: DiscoveryProtocol,
    base_url: &str,
) -> Result<String, ModelListError> {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return Err(ModelListError::BadEndpoint("base URL is empty".to_string()));
    }

    // Split into the API root (scheme + host + version path) and drop any
    // method-specific suffix. We look for the known suffixes from the right so
    // a path like `/v1/chat/completions` keeps its `/v1` root.
    let root = match protocol {
        DiscoveryProtocol::OpenAi => {
            // Accept both `…/chat/completions` and a bare `…/v1` root.
            trimmed
                .strip_suffix("/chat/completions")
                .or_else(|| trimmed.strip_suffix("/chat/completions/"))
                .unwrap_or(trimmed)
        }
        DiscoveryProtocol::Anthropic => trimmed
            .strip_suffix("/messages")
            .or_else(|| trimmed.strip_suffix("/messages/"))
            .unwrap_or(trimmed),
        DiscoveryProtocol::Gemini => trimmed.strip_suffix('/').unwrap_or(trimmed),
    };
    let root = root.strip_suffix('/').unwrap_or(root);

    Ok(match protocol {
        DiscoveryProtocol::OpenAi => format!("{root}/models"),
        DiscoveryProtocol::Anthropic => format!("{root}/models"),
        DiscoveryProtocol::Gemini => format!("{root}/models"),
    })
}

/// Fetch the live model list for `req`. Pure network + parse; the fallback
/// decision lives in the caller. Sorted + de-duplicated so the resulting
/// channel set is stable across runs regardless of API ordering.
///
/// Empty results are reported as [`ModelListError::Empty`] (never an empty
/// `Ok`) so a broken endpoint can never blank out a working instance.
pub async fn list_models(req: ModelDiscoveryRequest<'_>) -> Result<Vec<String>, ModelListError> {
    let endpoint = models_endpoint_for(req.protocol, req.base_url)?;
    let user_agent = req.user_agent.unwrap_or(crate::NEENEE_USER_AGENT);

    let client = reqwest::Client::builder()
        .user_agent(user_agent)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(ModelListError::Http)?;

    let response = match req.protocol {
        DiscoveryProtocol::OpenAi => {
            // OpenAI auth: a bearer when a key is set, NO header when keyless
            // (some relays reject a malformed bearer). Mirrors the chat path.
            let mut builder = client.get(&endpoint);
            if !req.api_key.trim().is_empty() {
                builder = builder.bearer_auth(req.api_key);
            }
            builder.send().await.map_err(ModelListError::Http)?
        }
        DiscoveryProtocol::Anthropic => {
            // Anthropic auth: x-api-key + the pinned API version. The version
            // header is mandatory on every Anthropic request including the
            // models list endpoint.
            let mut builder = client
                .get(&endpoint)
                .header("x-api-key", req.api_key)
                .header("anthropic-version", anthropic_version());
            if req.api_key.trim().is_empty() {
                // A keyless request still sends the headers (harmless) but
                // most Anthropic relays require a key; the snapshot fallback
                // covers the keyless-misconfigured case.
                builder = builder.header("x-api-key", "");
            }
            builder.send().await.map_err(ModelListError::Http)?
        }
        DiscoveryProtocol::Gemini => {
            // Gemini auth: the key is a query param, never a header. A keyless
            // request omits it entirely (Google rejects keyless, but a relay
            // might not require it).
            let mut builder = client.get(&endpoint);
            if !req.api_key.trim().is_empty() {
                builder = builder.query(&[("key", req.api_key)]);
            }
            builder.send().await.map_err(ModelListError::Http)?
        }
    };

    let status = response.status();
    if !status.is_success() {
        let code = status.as_u16();
        let body = response.text().await.unwrap_or_default();
        return Err(ModelListError::Status(code, body));
    }

    let body = response.text().await.map_err(ModelListError::Http)?;
    let json: Value = serde_json::from_str(&body)
        .map_err(|e| ModelListError::Parse(format!("response is not valid JSON: {e}")))?;

    let mut models = parse_models(req.protocol, &json);
    if models.is_empty() {
        return Err(ModelListError::Empty);
    }
    // Stable order regardless of API ordering: sort then de-dup. This makes
    // the reseed idempotent across runs (a re-fetch that returns the same set
    // in a different order does not register as a change).
    models.sort();
    models.dedup();
    Ok(models)
}

/// The pinned Anthropic API version sent on every request. Mirrors the chat
/// request header so a relay that pins a version accepts the models call too.
fn anthropic_version() -> &'static str {
    neenee_ai_sdk_anthropic::anthropic::request::ANTHROPIC_VERSION
}

/// Parse a `GET /models` response body into a list of model ids, per protocol.
/// Pure function so the per-protocol shapes are unit-testable without any HTTP.
fn parse_models(protocol: DiscoveryProtocol, json: &Value) -> Vec<String> {
    match protocol {
        DiscoveryProtocol::OpenAi => parse_data_ids(json),
        DiscoveryProtocol::Anthropic => parse_data_ids(json),
        DiscoveryProtocol::Gemini => parse_gemini_models(json),
    }
}

/// Extract `data[].id` as strings. Used by both OpenAI-compat and Anthropic,
/// which share the `{data: [{id}, …]}` shape on their models endpoints.
fn parse_data_ids(json: &Value) -> Vec<String> {
    let Some(data) = json.get("data").and_then(Value::as_array) else {
        return Vec::new();
    };
    data.iter()
        .filter_map(|entry| entry.get("id").and_then(Value::as_str))
        .map(|id| id.to_string())
        .collect()
}

/// Extract Gemini `models[]`, keeping only `generateContent`-capable text
/// models and stripping the `models/` name prefix to a bare id.
fn parse_gemini_models(json: &Value) -> Vec<String> {
    let Some(models) = json.get("models").and_then(Value::as_array) else {
        return Vec::new();
    };
    models
        .iter()
        .filter_map(|entry| {
            // Only keep text-generation models. A Gemini model entry advertises
            // its capabilities via `supportedGenerationMethods`; entries that
            // list `generateContent` are the chat/text models an agent uses.
            // Embeddings/embedding-only and image/video models are excluded.
            let methods = entry
                .get("supportedGenerationMethods")
                .and_then(Value::as_array);
            let is_text =
                methods.is_none_or(|arr| arr.iter().any(|m| m.as_str() == Some("generateContent")));
            if !is_text {
                return None;
            }
            entry
                .get("name")
                .and_then(Value::as_str)
                .map(|name| name.strip_prefix("models/").unwrap_or(name).to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_openai_models_endpoint_from_chat_url() {
        assert_eq!(
            models_endpoint_for(
                DiscoveryProtocol::OpenAi,
                "https://api.openai.com/v1/chat/completions"
            )
            .unwrap(),
            "https://api.openai.com/v1/models"
        );
        // A bare API root (no chat suffix) is accepted as-is.
        assert_eq!(
            models_endpoint_for(DiscoveryProtocol::OpenAi, "https://api.openai.com/v1").unwrap(),
            "https://api.openai.com/v1/models"
        );
        // A relay that uses a non-standard path keeps its host/root.
        assert_eq!(
            models_endpoint_for(
                DiscoveryProtocol::OpenAi,
                "https://relay.example.com/v1/chat/completions"
            )
            .unwrap(),
            "https://relay.example.com/v1/models"
        );
    }

    #[test]
    fn derives_anthropic_models_endpoint_from_messages_url() {
        assert_eq!(
            models_endpoint_for(
                DiscoveryProtocol::Anthropic,
                "https://api.anthropic.com/v1/messages"
            )
            .unwrap(),
            "https://api.anthropic.com/v1/models"
        );
        // A relay messages URL keeps its host.
        assert_eq!(
            models_endpoint_for(
                DiscoveryProtocol::Anthropic,
                "https://relay.example.com/v1/messages"
            )
            .unwrap(),
            "https://relay.example.com/v1/models"
        );
    }

    #[test]
    fn derives_gemini_models_endpoint_from_root() {
        // Gemini channels carry the API root (…/v1beta), not a method URL.
        assert_eq!(
            models_endpoint_for(
                DiscoveryProtocol::Gemini,
                "https://generativelanguage.googleapis.com/v1beta"
            )
            .unwrap(),
            "https://generativelanguage.googleapis.com/v1beta/models"
        );
        // A trailing slash is normalized away.
        assert_eq!(
            models_endpoint_for(
                DiscoveryProtocol::Gemini,
                "https://generativelanguage.googleapis.com/v1beta/"
            )
            .unwrap(),
            "https://generativelanguage.googleapis.com/v1beta/models"
        );
    }

    #[test]
    fn rejects_empty_base_url() {
        assert!(matches!(
            models_endpoint_for(DiscoveryProtocol::OpenAi, ""),
            Err(ModelListError::BadEndpoint(_))
        ));
        assert!(matches!(
            models_endpoint_for(DiscoveryProtocol::OpenAi, "   "),
            Err(ModelListError::BadEndpoint(_))
        ));
    }

    #[test]
    fn parses_openai_data_ids() {
        let json = serde_json::json!({
            "data": [
                { "id": "gpt-5.6-sol", "object": "model" },
                { "id": "gpt-5.5", "object": "model" },
                { "id": "gpt-5.4-mini", "object": "model" }
            ]
        });
        let mut got = parse_models(DiscoveryProtocol::OpenAi, &json);
        got.sort();
        assert_eq!(got, vec!["gpt-5.4-mini", "gpt-5.5", "gpt-5.6-sol"]);
    }

    #[test]
    fn parses_anthropic_data_ids() {
        // Anthropic's /v1/models returns the same {data:[{id}]} shape.
        let json = serde_json::json!({
            "data": [
                { "id": "claude-opus-4-8", "display_name": "Claude Opus 4.8" },
                { "id": "claude-sonnet-5", "display_name": "Claude Sonnet 5" }
            ]
        });
        let mut got = parse_models(DiscoveryProtocol::Anthropic, &json);
        got.sort();
        assert_eq!(got, vec!["claude-opus-4-8", "claude-sonnet-5"]);
    }

    #[test]
    fn parses_gemini_models_stripping_prefix_and_filtering_non_text() {
        let json = serde_json::json!({
            "models": [
                {
                    "name": "models/gemini-2.5-flash",
                    "supportedGenerationMethods": ["generateContent", "countTokens"]
                },
                {
                    "name": "models/gemini-2.5-pro",
                    "supportedGenerationMethods": ["generateContent"]
                },
                // An embedding-only model must be excluded.
                {
                    "name": "models/text-embedding-004",
                    "supportedGenerationMethods": ["embedContent"]
                },
                // A model with no methods array is kept (best-effort).
                { "name": "models/gemini-3-pro-preview" }
            ]
        });
        let mut got = parse_models(DiscoveryProtocol::Gemini, &json);
        got.sort();
        assert_eq!(
            got,
            vec!["gemini-2.5-flash", "gemini-2.5-pro", "gemini-3-pro-preview"]
        );
    }

    #[test]
    fn parse_returns_empty_when_shape_is_wrong() {
        // No `data` array → empty (the caller reports Empty as a discovery failure).
        assert!(
            parse_models(
                DiscoveryProtocol::OpenAi,
                &serde_json::json!({ "error": "unauthorized" })
            )
            .is_empty()
        );
        // No `models` array → empty.
        assert!(
            parse_models(
                DiscoveryProtocol::Gemini,
                &serde_json::json!({ "error": "bad key" })
            )
            .is_empty()
        );
    }

    #[test]
    fn discovery_protocol_maps_template_strings() {
        assert_eq!(
            DiscoveryProtocol::from_template_protocol("anthropic"),
            DiscoveryProtocol::Anthropic
        );
        assert_eq!(
            DiscoveryProtocol::from_template_protocol("gemini"),
            DiscoveryProtocol::Gemini
        );
        assert_eq!(
            DiscoveryProtocol::from_template_protocol("openai"),
            DiscoveryProtocol::OpenAi
        );
        // Unknown → OpenAI (most common relay shape), never an error.
        assert_eq!(
            DiscoveryProtocol::from_template_protocol("future"),
            DiscoveryProtocol::OpenAi
        );
    }
}
