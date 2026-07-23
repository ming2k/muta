//! Live model-list discovery from each provider's API.
//!
//! A provider instance created from a template can either mirror the
//! template's *compiled-in* model list ([`crate::registry::ProviderTemplateSpec`])
//! or fetch the list *live* from the provider's own `GET /models` endpoint.
//! This module owns the live path: it speaks the three wire protocols
//! (`openai` / `anthropic` / `gemini`), authenticates the same way a chat
//! request would, and parses the returned model entries. Beyond the id,
//! endpoints may advertise per-model capability hints (context length,
//! reasoning, image input, effort tiers — the Kimi Code platform is the rich
//! case); these ride along on [`DiscoveredModel`] as `Option`s, and the
//! catalog decides per template whether to trust and persist them.
//!
//! ## Priority
//!
//! The catalog reconciliation layer decides which instances use live
//! discovery via a `ModelSource` flag on `UserProviderConfig` (see
//! `neenee-store::config` and `neenee_agent::catalog::reconcile_provider_models`).
//! For `ModelSource::Api`, the catalog either intersects the live result with
//! its protocol-compatible model registry (the default) or — for
//! fitting-enabled trusted templates — materializes every advertised id and
//! persists the capability hints of registry-unknown ones (ADR-0065). On an
//! error or empty result it retains the last valid subset either way.
//! `ModelSource::Fixed` skips the network entirely and uses the template
//! snapshot.
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

use neenee_core::{
    Effort, RemoteModelEndpoint, RemoteModelMetadata, SecretString, ThinkingSupport,
};
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
    pub api_key: &'a SecretString,
    pub user_agent: Option<&'a str>,
    /// Extra request headers a provider requires beyond standard auth —
    /// e.g. GitHub Copilot's `x-initiator` / `Openai-Intent` /
    /// `X-GitHub-Api-Version`. Empty for stock OpenAI/Anthropic/Gemini.
    /// Applied to every protocol; a provider that needs per-header logic can
    /// still set them here since discovery is read-only (GET).
    pub extra_headers: &'a [(&'a str, &'a str)],
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

/// A model entry discovered from a provider's live `GET /models` list. The
/// `id` is always present; every capability field is `None` when the endpoint
/// does not advertise it. The stock OpenAI/Anthropic/Gemini shapes carry no
/// capability data. Two rich shapes are recognized: the Kimi Code platform
/// (`api.kimi.com/coding`), advertising flat `context_length` /
/// `supports_reasoning` / `supports_image_in` / `think_efforts` fields per
/// entry, and GitHub Copilot (`api.githubcopilot.com`), advertising the same
/// information nested under `capabilities.{limits,supports}` (see
/// `discovered_model_from_entry`). Consumers decide per template whether
/// these hints may be trusted (see `ProviderTemplateSpec::fitting`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscoveredModel {
    pub id: String,
    /// Whether the provider allows this model to appear in its interactive
    /// model picker. `None` means the endpoint did not distinguish picker
    /// models, so callers may include it.
    pub picker_enabled: Option<bool>,
    /// Exact API surface advertised for the model. This is provider-scoped: a
    /// Copilot model can use Messages, Responses, or Chat Completions while the
    /// same id elsewhere uses another route.
    pub endpoint: Option<RemoteModelEndpoint>,
    /// Provider model family, when advertised.
    pub family: Option<String>,
    /// Advertised context window in tokens (Kimi's `context_length`, or
    /// Copilot's `capabilities.limits.max_context_window_tokens`).
    pub context_window: Option<usize>,
    /// Maximum generated tokens, when advertised.
    pub max_output_tokens: Option<u32>,
    /// Reasoning support. For Kimi, an explicit `supports_thinking_type`
    /// (`"only"`/`"both"` → true, `"no"` → false) wins over the legacy
    /// `supports_reasoning` boolean. For Copilot, a non-empty
    /// `capabilities.supports.reasoning_effort` list means true.
    pub reasoning: Option<bool>,
    /// The precise reasoning wire representation when advertised. This is
    /// stronger than the coarse [`Self::reasoning`] display flag.
    pub thinking: Option<ThinkingSupport>,
    /// Native tool/function calling support, when advertised.
    pub tool_call: Option<bool>,
    /// Image-input support (Kimi's `supports_image_in`, or Copilot's
    /// `capabilities.supports.vision`).
    pub vision: Option<bool>,
    /// Advertised reasoning-effort tiers (Kimi's
    /// `think_efforts.valid_efforts`, or Copilot's
    /// `capabilities.supports.reasoning_effort`).
    pub effort_levels: Option<Vec<String>>,
    /// Display name when the endpoint provides one (Kimi's `display_name`,
    /// or Copilot's `name`).
    pub display_name: Option<String>,
}

impl DiscoveredModel {
    /// Convert live provider facts into the persisted channel-scoped snapshot.
    /// `None` fields intentionally remain absent so the static baseline may
    /// provide a conservative fallback for fields the endpoint does not expose.
    pub fn remote_metadata(&self) -> RemoteModelMetadata {
        RemoteModelMetadata {
            endpoint: self.endpoint,
            display_name: self.display_name.clone(),
            family: self.family.clone(),
            context_window: self.context_window,
            max_output_tokens: self.max_output_tokens,
            thinking: self.thinking.or_else(|| {
                self.reasoning.map(|reasoning| {
                    if reasoning {
                        ThinkingSupport::ReasoningContent
                    } else {
                        ThinkingSupport::None
                    }
                })
            }),
            tool_call: self.tool_call,
            vision: self.vision,
            effort_levels: self.effort_levels.as_ref().map(|levels| {
                levels
                    .iter()
                    .filter_map(|level| Effort::parse(level))
                    .collect()
            }),
        }
    }
}

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
/// decision lives in the caller. Sorted + de-duplicated by id so the
/// resulting channel set is stable across runs regardless of API ordering.
///
/// Empty results are reported as [`ModelListError::Empty`] (never an empty
/// `Ok`) so a broken endpoint can never blank out a working instance.
pub async fn list_models(
    req: ModelDiscoveryRequest<'_>,
) -> Result<Vec<DiscoveredModel>, ModelListError> {
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
            if !req.api_key.expose_secret().trim().is_empty() {
                builder = builder.bearer_auth(req.api_key.expose_secret());
            }
            for (name, value) in req.extra_headers {
                builder = builder.header(*name, *value);
            }
            builder.send().await.map_err(ModelListError::Http)?
        }
        DiscoveryProtocol::Anthropic => {
            // Anthropic auth: x-api-key + the pinned API version. The version
            // header is mandatory on every Anthropic request including the
            // models list endpoint.
            let mut builder = client
                .get(&endpoint)
                .header("x-api-key", req.api_key.expose_secret())
                .header("anthropic-version", anthropic_version());
            if req.api_key.expose_secret().trim().is_empty() {
                // A keyless request still sends the headers (harmless) but
                // most Anthropic relays require a key; the snapshot fallback
                // covers the keyless-misconfigured case.
                builder = builder.header("x-api-key", "");
            }
            for (name, value) in req.extra_headers {
                builder = builder.header(*name, *value);
            }
            builder.send().await.map_err(ModelListError::Http)?
        }
        DiscoveryProtocol::Gemini => {
            // Gemini auth: the key is a query param, never a header. A keyless
            // request omits it entirely (Google rejects keyless, but a relay
            // might not require it).
            let mut builder = client.get(&endpoint);
            if !req.api_key.expose_secret().trim().is_empty() {
                builder = builder.query(&[("key", req.api_key.expose_secret())]);
            }
            for (name, value) in req.extra_headers {
                builder = builder.header(*name, *value);
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
    // Stable order regardless of API ordering: sort by id then de-dup. This
    // makes the reseed idempotent across runs (a re-fetch that returns the
    // same set in a different order does not register as a change).
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models.dedup_by(|a, b| a.id == b.id);
    Ok(models)
}

/// The pinned Anthropic API version sent on every request. Mirrors the chat
/// request header so a relay that pins a version accepts the models call too.
fn anthropic_version() -> &'static str {
    neenee_ai_sdk_anthropic::anthropic::request::ANTHROPIC_VERSION
}

/// Parse a `GET /models` response body into a list of model entries, per
/// protocol. Pure function so the per-protocol shapes are unit-testable
/// without any HTTP.
fn parse_models(protocol: DiscoveryProtocol, json: &Value) -> Vec<DiscoveredModel> {
    match protocol {
        DiscoveryProtocol::OpenAi => parse_data_models(json),
        DiscoveryProtocol::Anthropic => parse_data_models(json),
        DiscoveryProtocol::Gemini => parse_gemini_models(json),
    }
}

/// Extract `data[]` entries. Used by both OpenAI-compat and Anthropic, which
/// share the `{data: [{id}, …]}` shape on their models endpoints. Every
/// capability field is optional: the stock endpoints omit them (yielding
/// `None`), while the Kimi Code platform advertises the full set.
fn parse_data_models(json: &Value) -> Vec<DiscoveredModel> {
    let Some(data) = json.get("data").and_then(Value::as_array) else {
        return Vec::new();
    };
    data.iter()
        .filter_map(discovered_model_from_entry)
        .collect()
}

/// Read one `data[]` entry: the mandatory `id` plus any advertised capability
/// fields (absent fields stay `None` — the caller decides whether to trust
/// and persist them). Non-chat entries (Copilot also lists embedding models,
/// tagged `capabilities.type != "chat"`) are skipped entirely rather than
/// surfaced with empty capabilities.
fn discovered_model_from_entry(entry: &Value) -> Option<DiscoveredModel> {
    let id = entry.get("id").and_then(Value::as_str)?.to_string();
    if let Some(capabilities) = entry.get("capabilities") {
        return copilot_model_from_capabilities(id, entry, capabilities);
    }
    // Thinking-type precedence mirrors the kimi-code client: the newer
    // three-state field wins over the legacy boolean when present.
    let reasoning = match entry.get("supports_thinking_type").and_then(Value::as_str) {
        Some("only") | Some("both") => Some(true),
        Some("no") => Some(false),
        _ => entry.get("supports_reasoning").and_then(Value::as_bool),
    };
    let effort_levels = entry
        .get("think_efforts")
        .and_then(|efforts| efforts.get("valid_efforts"))
        .and_then(Value::as_array)
        .map(|levels| {
            levels
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        });
    Some(DiscoveredModel {
        id,
        picker_enabled: None,
        endpoint: None,
        family: None,
        context_window: entry
            .get("context_length")
            .and_then(Value::as_u64)
            .map(|length| length as usize),
        max_output_tokens: None,
        reasoning,
        thinking: None,
        tool_call: None,
        vision: entry.get("supports_image_in").and_then(Value::as_bool),
        effort_levels,
        display_name: entry
            .get("display_name")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// Read a Copilot-shaped `data[]` entry, whose capability fields live nested
/// under `capabilities.{limits,supports}` rather than the flat Kimi layout
/// (schema per `@vscode/copilot-api`'s `CCAModel`/`CCAModelCapabilities`):
/// `capabilities.limits.max_context_window_tokens`,
/// `capabilities.supports.vision`, and `capabilities.supports.reasoning_effort`
/// (a non-empty tier list — `o1`/`o3`/GPT-5-thinking-style models — implies
/// reasoning support; its entries double as `effort_levels`). Router/tool
/// entries and non-`"chat"` capability types (embeddings, etc.) are filtered
/// out here rather than by the caller, since only Copilot's response carries
/// that distinction.
fn copilot_model_from_capabilities(
    id: String,
    entry: &Value,
    capabilities: &Value,
) -> Option<DiscoveredModel> {
    if let Some(kind) = capabilities.get("type").and_then(Value::as_str)
        && kind != "chat"
    {
        return None;
    }
    let limits = capabilities.get("limits");
    let supports = capabilities.get("supports");
    let effort_levels: Option<Vec<String>> = supports
        .and_then(|s| s.get("reasoning_effort"))
        .and_then(Value::as_array)
        .map(|levels| {
            levels
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        });
    let reasoning = effort_levels
        .as_ref()
        .map(|levels| !levels.is_empty())
        .or_else(|| {
            supports
                .and_then(|s| s.get("adaptive_thinking"))
                .and_then(Value::as_bool)
        });
    let thinking = if supports
        .and_then(|s| s.get("adaptive_thinking"))
        .and_then(Value::as_bool)
        == Some(true)
    {
        Some(ThinkingSupport::AnthropicAdaptive)
    } else if supports
        .and_then(|s| s.get("max_thinking_budget"))
        .and_then(Value::as_u64)
        .is_some()
    {
        Some(ThinkingSupport::AnthropicManual)
    } else {
        reasoning.map(|enabled| {
            if enabled {
                ThinkingSupport::ReasoningContent
            } else {
                ThinkingSupport::None
            }
        })
    };
    let endpoint = copilot_endpoint(entry.get("supported_endpoints"));
    Some(DiscoveredModel {
        id,
        picker_enabled: entry.get("model_picker_enabled").and_then(Value::as_bool),
        endpoint,
        family: capabilities
            .get("family")
            .and_then(Value::as_str)
            .map(str::to_string),
        context_window: limits
            .and_then(|l| l.get("max_context_window_tokens"))
            .and_then(Value::as_u64)
            .map(|length| length as usize),
        max_output_tokens: limits
            .and_then(|l| l.get("max_output_tokens"))
            .and_then(Value::as_u64)
            .and_then(|length| u32::try_from(length).ok()),
        reasoning,
        thinking,
        tool_call: supports
            .and_then(|s| s.get("tool_calls"))
            .and_then(Value::as_bool),
        vision: supports
            .and_then(|s| s.get("vision"))
            .and_then(Value::as_bool),
        effort_levels,
        display_name: entry
            .get("name")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// Decode Copilot's advertised route in deterministic priority order. Messages
/// is checked first because it requires a distinct wire format; Responses is
/// next; Chat Completions is the explicit final route. Missing or unfamiliar
/// entries leave the channel's configured fallback untouched.
fn copilot_endpoint(value: Option<&Value>) -> Option<RemoteModelEndpoint> {
    let endpoints = value?.as_array()?;
    let has = |needle| endpoints.iter().any(|value| value.as_str() == Some(needle));
    if has("/v1/messages") {
        Some(RemoteModelEndpoint::Messages)
    } else if has("/responses") {
        Some(RemoteModelEndpoint::Responses)
    } else if has("/chat/completions") {
        Some(RemoteModelEndpoint::ChatCompletions)
    } else {
        None
    }
}

/// Extract Gemini `models[]`, keeping only `generateContent`-capable text
/// models and stripping the `models/` name prefix to a bare id. The Gemini
/// shape advertises no per-model capability fields neenee consumes, so the
/// entries carry ids only.
fn parse_gemini_models(json: &Value) -> Vec<DiscoveredModel> {
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
                .map(|name| DiscoveredModel {
                    id: name.strip_prefix("models/").unwrap_or(name).to_string(),
                    ..DiscoveredModel::default()
                })
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
        let mut got: Vec<String> = parse_models(DiscoveryProtocol::OpenAi, &json)
            .into_iter()
            .map(|model| model.id)
            .collect();
        got.sort();
        assert_eq!(got, vec!["gpt-5.4-mini", "gpt-5.5", "gpt-5.6-sol"]);
    }

    #[test]
    fn parses_kimi_platform_capability_fields() {
        // The Kimi Code platform's live response shape (recorded 2026-07 from
        // GET https://api.kimi.com/coding/v1/models): every entry advertises
        // its context length, reasoning/thinking support, and image input;
        // K3 additionally lists its effort tiers.
        let json = serde_json::json!({
            "data": [
                {
                    "id": "kimi-for-coding",
                    "display_name": "kimi-for-coding",
                    "context_length": 262144,
                    "supports_reasoning": true,
                    "supports_image_in": true,
                    "supports_video_in": true,
                    "supports_thinking_type": "only"
                },
                {
                    "id": "k3",
                    "display_name": "k3",
                    "context_length": 1048576,
                    "supports_reasoning": true,
                    "supports_image_in": true,
                    "supports_video_in": true,
                    "supports_thinking_type": "only",
                    "think_efforts": {
                        "support": true,
                        "valid_efforts": ["max"],
                        "default_effort": "max"
                    }
                }
            ],
            "object": "list"
        });
        let models = parse_models(DiscoveryProtocol::OpenAi, &json);
        assert_eq!(models.len(), 2);
        let k3 = &models[1];
        assert_eq!(k3.id, "k3");
        assert_eq!(k3.context_window, Some(1_048_576));
        assert_eq!(k3.reasoning, Some(true));
        assert_eq!(k3.vision, Some(true));
        assert_eq!(k3.effort_levels, Some(vec!["max".to_string()]));
        assert_eq!(k3.display_name.as_deref(), Some("k3"));
        // The legacy entry has no effort field → None, not an empty vec.
        assert_eq!(models[0].id, "kimi-for-coding");
        assert_eq!(models[0].context_window, Some(262_144));
        assert_eq!(models[0].effort_levels, None);
    }

    #[test]
    fn parses_copilot_nested_capability_fields() {
        // GitHub Copilot's live `/models` shape (per `@vscode/copilot-api`'s
        // `CCAModel`): capability data is nested under `capabilities`, unlike
        // Kimi's flat fields. A reasoning model advertises a non-empty
        // `reasoning_effort` tier list; a non-reasoning chat model has none.
        let json = serde_json::json!({
            "data": [
                {
                    "id": "gpt-5",
                    "name": "GPT-5",
                    "object": "model",
                    "model_picker_enabled": true,
                    "capabilities": {
                        "type": "chat",
                        "family": "gpt-5",
                        "limits": {
                            "max_context_window_tokens": 272_000,
                            "max_output_tokens": 128_000,
                            "max_prompt_tokens": 200_000
                        },
                        "supports": {
                            "adaptive_thinking": false,
                            "streaming": true,
                            "tool_calls": true,
                            "vision": true,
                            "reasoning_effort": ["low", "medium", "high"]
                        }
                    }
                },
                {
                    "id": "gpt-4o",
                    "name": "GPT-4o",
                    "object": "model",
                    "model_picker_enabled": true,
                    "capabilities": {
                        "type": "chat",
                        "family": "gpt-4o",
                        "limits": {
                            "max_context_window_tokens": 128_000,
                            "max_output_tokens": 16_384,
                            "max_prompt_tokens": 96_000
                        },
                        "supports": {
                            "adaptive_thinking": true,
                            "streaming": true,
                            "tool_calls": true,
                            "vision": true
                        }
                    }
                },
                {
                    "id": "text-embedding-3-small",
                    "name": "Embedding V3 small",
                    "object": "model",
                    "capabilities": {
                        "type": "embeddings"
                    }
                }
            ]
        });
        let models = parse_models(DiscoveryProtocol::OpenAi, &json);
        // The embeddings entry is filtered out — only chat models surface.
        assert_eq!(models.len(), 2);
        let gpt5 = models.iter().find(|m| m.id == "gpt-5").unwrap();
        assert_eq!(gpt5.context_window, Some(272_000));
        assert_eq!(gpt5.max_output_tokens, Some(128_000));
        assert_eq!(gpt5.reasoning, Some(true));
        assert_eq!(gpt5.thinking, Some(ThinkingSupport::ReasoningContent));
        assert_eq!(gpt5.tool_call, Some(true));
        assert_eq!(gpt5.vision, Some(true));
        assert_eq!(
            gpt5.effort_levels,
            Some(vec![
                "low".to_string(),
                "medium".to_string(),
                "high".to_string()
            ])
        );
        assert_eq!(gpt5.display_name.as_deref(), Some("GPT-5"));
        let gpt4o = models.iter().find(|m| m.id == "gpt-4o").unwrap();
        assert_eq!(gpt4o.context_window, Some(128_000));
        // `adaptive_thinking` explicitly declares reasoning despite the absent
        // `reasoning_effort` vocabulary.
        assert_eq!(gpt4o.reasoning, Some(true));
        assert_eq!(gpt4o.effort_levels, None);
        assert_eq!(gpt4o.thinking, Some(ThinkingSupport::AnthropicAdaptive));
    }

    #[test]
    fn copilot_metadata_preserves_picker_and_endpoint_facts() {
        let json = serde_json::json!({
            "data": [
                {
                    "id": "claude-opus-4.7",
                    "name": "Claude Opus 4.7",
                    "model_picker_enabled": true,
                    "supported_endpoints": ["/v1/messages"],
                    "capabilities": {
                        "type": "chat",
                        "family": "claude-opus",
                        "limits": {
                            "max_context_window_tokens": 144_000,
                            "max_output_tokens": 64_000
                        },
                        "supports": {
                            "adaptive_thinking": true,
                            "tool_calls": true,
                            "vision": true,
                            "reasoning_effort": ["low", "medium", "high"]
                        }
                    }
                },
                {
                    "id": "internal-title-model",
                    "name": "Internal title model",
                    "model_picker_enabled": false,
                    "supported_endpoints": ["/responses"],
                    "capabilities": {
                        "type": "chat",
                        "family": "internal",
                        "limits": { "max_output_tokens": 1024 },
                        "supports": { "tool_calls": false }
                    }
                }
            ]
        });

        let models = parse_models(DiscoveryProtocol::OpenAi, &json);
        let claude = models
            .iter()
            .find(|model| model.id == "claude-opus-4.7")
            .unwrap();
        assert_eq!(claude.picker_enabled, Some(true));
        assert_eq!(claude.endpoint, Some(RemoteModelEndpoint::Messages));
        assert_eq!(claude.family.as_deref(), Some("claude-opus"));
        assert_eq!(claude.thinking, Some(ThinkingSupport::AnthropicAdaptive));
        assert_eq!(claude.tool_call, Some(true));
        assert_eq!(claude.max_output_tokens, Some(64_000));

        let remote = claude.remote_metadata();
        assert_eq!(remote.endpoint, Some(RemoteModelEndpoint::Messages));
        assert_eq!(
            remote.effort_levels,
            Some(vec![Effort::Low, Effort::Medium, Effort::High])
        );
        assert_eq!(remote.thinking, Some(ThinkingSupport::AnthropicAdaptive));

        let internal = models
            .iter()
            .find(|model| model.id == "internal-title-model")
            .unwrap();
        assert_eq!(internal.picker_enabled, Some(false));
        assert_eq!(internal.endpoint, Some(RemoteModelEndpoint::Responses));
    }

    #[test]
    fn thinking_type_field_wins_over_legacy_reasoning_bool() {
        // `supports_thinking_type` is the newer, authoritative field: "no"
        // must override a stray `supports_reasoning: true`.
        let json = serde_json::json!({
            "data": [
                { "id": "a", "supports_reasoning": true, "supports_thinking_type": "no" },
                { "id": "b", "supports_reasoning": false, "supports_thinking_type": "both" }
            ]
        });
        let models = parse_models(DiscoveryProtocol::OpenAi, &json);
        assert_eq!(models[0].reasoning, Some(false));
        assert_eq!(models[1].reasoning, Some(true));
    }

    #[test]
    fn parses_anthropic_data_ids() {
        // Anthropic's /v1/models returns the same {data:[{id}]} shape plus a
        // display name; no capability fields are advertised.
        let json = serde_json::json!({
            "data": [
                { "id": "claude-opus-4-8", "display_name": "Claude Opus 4.8" },
                { "id": "claude-sonnet-5", "display_name": "Claude Sonnet 5" }
            ]
        });
        let models = parse_models(DiscoveryProtocol::Anthropic, &json);
        let mut got: Vec<String> = models.iter().map(|model| model.id.clone()).collect();
        got.sort();
        assert_eq!(got, vec!["claude-opus-4-8", "claude-sonnet-5"]);
        assert_eq!(models[0].display_name.as_deref(), Some("Claude Opus 4.8"));
        assert_eq!(models[0].context_window, None);
        assert_eq!(models[0].reasoning, None);
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
        let mut got: Vec<String> = parse_models(DiscoveryProtocol::Gemini, &json)
            .into_iter()
            .map(|model| model.id)
            .collect();
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
