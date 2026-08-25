//! Google native provider (REST to the versioned Google base).
//!
//! A thin executor over the pure [`request`] and [`response`] layers plus the
//! shared transport helpers. Google's transport is distinctive in two ways:
//! the API key rides as a `?key=` query param (never a header), and the base
//! URL is versioned (`.../v1beta`) with the per-call model path appended at
//! request time. Everything else reuses the shared SSE decoder and HTTP
//! helpers.
//!
//! Module layout (mirrors the OpenAI and Anthropic providers):
//! - [`request`] — body / url construction (pure, no I/O)
//! - [`response`] — usage, message, and stream-payload parsing (pure)
//! - this file — the [`GoogleProvider`] executor + `Provider` impl

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use muta_contracts::{Message, ModelRequest, Provider, ProviderPromptHints, ProviderStreamEvent};
use serde_json::{Map, Value};
use std::sync::{Arc, Mutex};

use crate::{Client, Endpoint, TurnState};
use crate::{decode_response_json, ensure_success, transport_error};

pub mod request;
pub mod response;

/// Official Google REST base, versioned. The provider appends the per-call
/// model path (`/models/{id}:generateContent` / `:streamGenerateContent`), so a
/// 中转站/relay overrides this with its own host carrying the `/v1beta` prefix.
pub const GOOGLE_DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Google native provider.
///
/// Embeds the shared [`Endpoint`] (connection config) and [`TurnState`] (tool
/// schemas + last usage). The provider holds no wire-format-unique fields:
/// Google's transport differences (key as query param, versioned base, native
/// function declarations) are confined to [`request`] / [`response`].
pub struct GoogleProvider {
    pub endpoint: Endpoint,
    pub turn: TurnState,
    /// Pooled HTTP client reused across every request this provider makes.
    pub client: Client,
    /// Stash for Google thought signatures attached to streamed function-call
    /// parts, drained into `provider_meta` for exact stateless replay.
    pub last_thought_signatures: Arc<Mutex<Map<String, Value>>>,
    /// Stash for the latest streamed text-part thought signature.
    pub last_text_thought_signature: Arc<Mutex<Option<String>>>,
    /// Channel-scoped capability view. A trusted remote catalogue overrides the
    /// static baseline only for this provider/model route.
    pub capabilities: muta_contracts::ModelCapabilities,
    /// Channel-scoped reasoning-effort override. `None` leaves the model's
    /// server-default thinking level in place; `Some(e)` pins it, translated
    /// onto `thinkingConfig` (`thinkingLevel` for Gemini 3.x, a
    /// `thinkingBudget` bucket for Gemini 2.5) at request-build time.
    pub reasoning_effort: Option<muta_contracts::Effort>,
    /// Antigravity Google Cloud companion project ID (`cloudaicompanionProject`).
    /// When set, the provider routes requests in `v1internal` envelope shape with
    /// `Authorization: Bearer` authentication to the Antigravity backend.
    pub project_id: Option<String>,
    /// Sticky, channel-scoped flag: this upstream has refused our
    /// `thinkingConfig` at least once (INVALID_ARGUMENT naming the field), so
    /// the thinking-disclosure surface is unavailable on this route. Reasoning
    /// itself continues — the model still thinks server-side — but the chain
    /// cannot be disclosed and requests stop asking for it. See
    /// [`Self::note_thinking_rejected`].
    thinking_rejected: Arc<Mutex<bool>>,
}

impl GoogleProvider {
    pub fn new(api_key: String, model: String) -> Self {
        Self::with_base_url(api_key, model, GOOGLE_DEFAULT_BASE_URL)
    }

    /// Build a provider targeting a custom versioned base URL (e.g. a
    /// Google-format relay). A trailing slash on `base_url` is tolerated
    /// (stripped).
    pub fn with_base_url(api_key: String, model: String, base_url: &str) -> Self {
        Self::with_base_url_and_user_agent(api_key, model, base_url, crate::MUTA_USER_AGENT)
    }

    /// Build a provider targeting a custom versioned base URL with an explicit
    /// `User-Agent`. A trailing slash on `base_url` is tolerated (stripped).
    pub fn with_base_url_and_user_agent(
        api_key: String,
        model: String,
        base_url: &str,
        user_agent: &str,
    ) -> Self {
        let capabilities = muta_contracts::ModelCapabilities::for_channel(&model, None);
        Self {
            endpoint: Endpoint {
                api_key,
                model,
                base_url: base_url.trim_end_matches('/').to_string(),
                user_agent: user_agent.to_string(),
                id: "google".to_string(),
            },
            turn: TurnState::new(),
            client: Client::new(),
            last_thought_signatures: Arc::new(Mutex::new(Map::new())),
            last_text_thought_signature: Arc::new(Mutex::new(None)),
            capabilities,
            reasoning_effort: None,
            project_id: None,
            thinking_rejected: Arc::new(Mutex::new(false)),
        }
    }

    /// Set the attribution id (provider/solution id) so assistant responses are
    /// attributed to the logical model.
    pub fn with_id(mut self, id: String) -> Self {
        self.endpoint.set_id(id);
        self
    }

    /// Attach the effective provider-channel capability view.
    pub fn with_model_capabilities(
        mut self,
        capabilities: muta_contracts::ModelCapabilities,
    ) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Set the channel-scoped reasoning-effort override. `None` leaves the
    /// server default in place. Clamped to the resolved model's supported
    /// `effort_levels` at request-build time, then translated onto Google's
    /// `thinkingConfig` (`thinkingLevel` for Gemini 3.x, a `thinkingBudget`
    /// bucket for Gemini 2.5).
    pub fn with_reasoning_effort(mut self, effort: Option<muta_contracts::Effort>) -> Self {
        self.reasoning_effort = effort;
        self
    }

    /// Attach the Antigravity project ID (`cloudaicompanionProject`).
    pub fn with_project_id(mut self, project_id: impl Into<String>) -> Self {
        self.project_id = Some(project_id.into());
        self
    }

    /// Whether this provider is configured for Google Antigravity `v1internal` protocol.
    pub fn is_antigravity(&self) -> bool {
        self.project_id.is_some()
            || self
                .endpoint
                .base_url
                .contains("cloudcode-pa.googleapis.com")
    }

    /// Build the thinkingless streaming request for a channel whose upstream
    /// refused `thinkingConfig`. Shared shape with
    /// [`Self::stream_chat_events`] minus the thinking-disclosure surface; kept
    /// as a separate method so the downgrade path is explicit and testable.
    async fn stream_chat_events_without_thinking(
        &self,
        request: ModelRequest,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent, String>>, String> {
        let client = self.client.http();
        let (url, headers, body) = self.prepare_request(request, true, true);

        let response = client
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|error| transport_error("Google", error))?;
        let response = ensure_success(response, "Google").await.map_err(|e| {
            response::clarify_error(e, &self.endpoint.model, &self.endpoint.base_url)
        })?;
        Ok(self.wrap_event_stream(response))
    }

    /// Wrap a successful SSE response into the event stream the harness
    /// consumes: Google thought-signature stashing plus per-call index
    /// assignment. Extracted from `stream_chat_events` so the downgrade path
    /// shares it exactly.
    fn wrap_event_stream(
        &self,
        response: reqwest::Response,
    ) -> BoxStream<'static, Result<ProviderStreamEvent, String>> {
        self.last_thought_signatures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        *self
            .last_text_thought_signature
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        let next_tool_index = std::sync::Arc::new(std::sync::Mutex::new(0usize));
        let thought_signatures = self.last_thought_signatures.clone();
        let text_thought_signature = self.last_text_thought_signature.clone();
        let stream = crate::sse::data_payloads(response, "Google").flat_map({
            let next_tool_index = next_tool_index.clone();
            move |item| {
                let events: Vec<Result<ProviderStreamEvent, String>> = match item {
                    Ok(payload) => {
                        let parsed = response::stream_payload(&payload);
                        if !parsed.thought_signatures.is_empty() {
                            let mut guard =
                                thought_signatures.lock().unwrap_or_else(|e| e.into_inner());
                            for (id, signature) in parsed.thought_signatures {
                                guard.insert(id, Value::String(signature));
                            }
                        }
                        if let Some(signature) = parsed.text_thought_signature {
                            *text_thought_signature
                                .lock()
                                .unwrap_or_else(|e| e.into_inner()) = Some(signature.clone());
                            let mut guard =
                                thought_signatures.lock().unwrap_or_else(|e| e.into_inner());
                            for event in &parsed.events {
                                if let ProviderStreamEvent::ToolCallDelta { id: Some(id), .. } =
                                    event
                                {
                                    guard
                                        .entry(id.clone())
                                        .or_insert_with(|| Value::String(signature.clone()));
                                }
                            }
                        }
                        parsed
                            .events
                            .into_iter()
                            .map(|event| match event {
                                ProviderStreamEvent::ToolCallDelta {
                                    id,
                                    name,
                                    arguments,
                                    ..
                                } => {
                                    let mut guard =
                                        next_tool_index.lock().unwrap_or_else(|e| e.into_inner());
                                    let index = *guard;
                                    *guard += 1;
                                    Ok(ProviderStreamEvent::ToolCallDelta {
                                        index,
                                        id,
                                        name,
                                        arguments,
                                    })
                                }
                                event => Ok(event),
                            })
                            .collect()
                    }
                    Err(error) => vec![Err(error)],
                };
                futures::stream::iter(events)
            }
        });
        stream.boxed()
    }

    /// Record that this channel's upstream rejected our `thinkingConfig` and
    /// report whether this is new information (first observation). The flag is
    /// channel-scoped and sticky for the process lifetime: the rejection is a
    /// property of the upstream route, not of an individual request, so later
    /// turns skip straight to the thinkingless form instead of paying one
    /// failed request each. A new observation is exactly-once so the caller
    /// logs/notices it once per process.
    fn note_thinking_rejected(&self) -> bool {
        let mut guard = self
            .thinking_rejected
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if *guard {
            false
        } else {
            *guard = true;
            true
        }
    }

    /// Whether this channel's upstream has already refused our
    /// `thinkingConfig` (sticky, per process).
    fn thinking_was_rejected(&self) -> bool {
        *self
            .thinking_rejected
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Build the thinkingless non-streaming request for a channel whose
    /// upstream refused `thinkingConfig`. The model still thinks server-side
    /// (Gemini 3.x cannot reason with thinking off); what changes is that we
    /// stop asking for the chain to be disclosed, which is the upstream's
    /// prerogative — the turn itself is not an error.
    async fn chat_without_thinking(&self, request: ModelRequest) -> Result<Message, String> {
        tracing::warn!(
            model = %self.endpoint.model,
            "upstream rejected thinkingConfig; retrying without disclosed thinking (chain withheld by upstream)"
        );
        let client = self.client.http();
        let (url, headers, body) = self.prepare_request(request, false, true);
        let response = client
            .post(&url)
            .headers(headers)
            .json(&body)
            .timeout(self.client.request_timeout())
            .send()
            .await
            .map_err(|error| transport_error("Google", error))?;
        let response = ensure_success(response, "Google").await.map_err(|e| {
            response::clarify_error(e, &self.endpoint.model, &self.endpoint.base_url)
        })?;
        let response_json: serde_json::Value = decode_response_json(response, "Google").await?;
        let root = response_json.get("response").unwrap_or(&response_json);
        if let Some(err) = response_json.get("error").or_else(|| root.get("error")) {
            return Err(response::clarify_error(
                format!("Google Error: {}", err),
                &self.endpoint.model,
                &self.endpoint.base_url,
            ));
        }
        if let Some(usage) = response::usage(&root["usageMetadata"]) {
            self.turn.stash_usage(usage);
        }
        response::message(&response_json)
    }

    fn prepare_request(
        &self,
        request: ModelRequest,
        is_stream: bool,
        omit_thinking: bool,
    ) -> (String, reqwest::header::HeaderMap, serde_json::Value) {
        let include_thoughts = self.capabilities.reasoning() && !omit_thinking;
        let thinking = if omit_thinking {
            None
        } else {
            request::resolve_thinking(
                self.reasoning_effort,
                &self.capabilities.effort_levels,
                request::max_thinking_budget(&self.endpoint.model),
            )
        };
        let (messages, tool_specs) = request.into_parts();
        let raw_body = request::body(
            messages,
            request::BodyInput {
                tool_specs: (!tool_specs.is_empty()).then_some(tool_specs.as_slice()),
                include_thoughts,
                thinking,
            },
        );

        let mut headers = reqwest::header::HeaderMap::new();
        if let Ok(ua) = self.endpoint.user_agent.parse() {
            headers.insert("User-Agent", ua);
        }
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );

        if self.is_antigravity() {
            let action = if is_stream {
                "streamGenerateContent"
            } else {
                "generateContent"
            };
            let base = self.endpoint.base_url.trim_end_matches('/');
            let base = base.strip_suffix("/v1internal").unwrap_or(base);
            let url = if is_stream {
                format!("{base}/v1internal:{action}?alt=sse")
            } else {
                format!("{base}/v1internal:{action}")
            };

            if !self.endpoint.api_key.is_empty()
                && let Ok(bearer) = format!("Bearer {}", self.endpoint.api_key).parse()
            {
                headers.insert("Authorization", bearer);
            }

            headers.insert(
                reqwest::header::HeaderName::from_static("x-goog-api-client"),
                reqwest::header::HeaderValue::from_static("gl-go/1.23.2 gdcl/0.1"),
            );

            for (k, v) in self.endpoint.client_identity().headers() {
                if let (Ok(hname), Ok(hval)) = (
                    reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                    reqwest::header::HeaderValue::from_str(v),
                ) {
                    headers.insert(hname, hval);
                }
            }

            let project = self.project_id.as_deref().unwrap_or("");
            if project.is_empty() {
                tracing::warn!(
                    model = %self.endpoint.model,
                    "Antigravity request missing project_id; requests without cloudaicompanionProject may hit HTTP 429 RESOURCE_EXHAUSTED"
                );
            }

            // Normalize model names for Antigravity backend: Antigravity routes 3.7 Flash
            // through its tiered wire identifier `gemini-3.7-flash-tiered`.
            let wire_model = if self.endpoint.model == "gemini-3.7-flash" {
                "gemini-3.7-flash-tiered"
            } else {
                self.endpoint.model.as_str()
            };

            let wrapped_body = serde_json::json!({
                "project": project,
                "requestId": uuid::Uuid::new_v4().to_string(),
                "userAgent": self.endpoint.user_agent,
                "model": wire_model,
                "request": raw_body
            });

            (url, headers, wrapped_body)
        } else {
            let url = if is_stream {
                request::stream_url(
                    &self.endpoint.base_url,
                    &self.endpoint.model,
                    &self.endpoint.api_key,
                )
            } else {
                request::url(
                    &self.endpoint.base_url,
                    &self.endpoint.model,
                    &self.endpoint.api_key,
                )
            };
            (url, headers, raw_body)
        }
    }

    // Accessors (base_url / model_id / user_agent / api_key / id) are forwarded
    // from the embedded [`Endpoint`]; see `self.endpoint.*`.
}

#[async_trait]
impl Provider for GoogleProvider {
    fn provider_id(&self) -> String {
        self.endpoint.id.clone()
    }

    fn model(&self) -> String {
        self.endpoint.model.clone()
    }

    // `effort()` keeps its default (`None`): the Gemini `thinkingLevel` /
    // `thinkingBudget` mapping has no user-facing depth vocabulary that
    // matches the shared `Effort` tiers one-to-one, so the transcript stays
    // quiet rather than showing a translated label that could mislead.

    fn model_capabilities(&self) -> muta_contracts::ModelCapabilities {
        self.capabilities.clone()
    }
    fn prompt_hints(&self) -> ProviderPromptHints {
        // No protocol hint: Google's wire surface uses native function calls by
        // construction, and tool-result replay as `functionResponse` parts is
        // the provider's own convention that the model already follows. An
        // in-prompt note would only restate facts the harness already enforces.
        ProviderPromptHints {
            system_guidance: "",
        }
    }

    fn usage_supported(&self) -> bool {
        true
    }

    fn take_last_usage(&self) -> Option<muta_contracts::TokenUsage> {
        self.turn.take_usage()
    }

    fn take_last_provider_meta(&self) -> Option<Map<String, Value>> {
        let mut signatures = self
            .last_thought_signatures
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let text_signature = self
            .last_text_thought_signature
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if signatures.is_empty() && text_signature.is_none() {
            return None;
        }
        let mut provider_meta = Map::new();
        if !signatures.is_empty() {
            provider_meta.insert(
                response::THOUGHT_SIGNATURES_META_KEY.to_string(),
                Value::Object(std::mem::take(&mut *signatures)),
            );
        }
        if let Some(signature) = text_signature {
            provider_meta.insert(
                response::TEXT_THOUGHT_SIGNATURE_META_KEY.to_string(),
                Value::String(signature),
            );
        }
        Some(provider_meta)
    }

    async fn chat(&self, request: ModelRequest) -> Result<Message, String> {
        let client = self.client.http();
        let omit = self.thinking_was_rejected();
        let (url, headers, body) = self.prepare_request(request.clone(), false, omit);

        // Non-streaming, sent through `Client::http` directly (the error
        // clarification below needs the raw helpers), so stamp the shared
        // non-streaming request bound here — `Client::send_json` applies it
        // for the other protocols. The streaming paths deliberately carry no
        // overall timeout (see the `client` module docs).
        let response = client
            .post(&url)
            .headers(headers)
            .json(&body)
            .timeout(self.client.request_timeout())
            .send()
            .await
            .map_err(|error| transport_error("Google", error))?;
        let response = match ensure_success(response, "Google").await {
            Ok(response) => response,
            Err(e) => {
                // Elastic downgrade (see `stream_chat_events`): when the
                // upstream rejects our `thinkingConfig`, retry the identical
                // turn with thinking omitted rather than failing it.
                let e = response::clarify_error(e, &self.endpoint.model, &self.endpoint.base_url);
                if response::rejects_thinking_config(&e) && self.note_thinking_rejected() {
                    return Box::pin(self.chat_without_thinking(request)).await;
                }
                return Err(e);
            }
        };

        let response_json: serde_json::Value = decode_response_json(response, "Google").await?;
        let root = response_json.get("response").unwrap_or(&response_json);

        if let Some(err) = response_json.get("error").or_else(|| root.get("error")) {
            return Err(response::clarify_error(
                format!("Google Error: {}", err),
                &self.endpoint.model,
                &self.endpoint.base_url,
            ));
        }

        if let Some(usage) = response::usage(&root["usageMetadata"]) {
            self.turn.stash_usage(usage);
        }

        response::message(&response_json)
    }

    async fn stream_chat(
        &self,
        request: ModelRequest,
    ) -> Result<BoxStream<'static, Result<String, String>>, String> {
        let client = self.client.http();
        let omit = self.thinking_was_rejected();
        let (url, headers, body) = self.prepare_request(request.clone(), true, omit);

        let response = client
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|error| transport_error("Google", error))?;
        let response = match ensure_success(response, "Google").await {
            Ok(response) => response,
            Err(e) => {
                // Elastic downgrade (see `stream_chat_events`): an upstream
                // refusing `thinkingConfig` withholds the chain; the text
                // stream itself is still served. The event-stream path is the
                // one the agent actually drives (and the one that would
                // downgrade-and-retry), so here it suffices to surface the
                // classified error — the sticky flag below is still set so the
                // *next* text stream skips the doomed stamp instead of paying
                // a failed request.
                let e = response::clarify_error(e, &self.endpoint.model, &self.endpoint.base_url);
                if response::rejects_thinking_config(&e) {
                    self.note_thinking_rejected();
                }
                return Err(e);
            }
        };

        // SSE byte reassembly (incl. multi-byte UTF-8 split across chunks) is
        // handled by `sse::data_payloads`; here we only map each payload to the
        // Google `streamGenerateContent` text shape.
        let stream = crate::sse::data_payloads(response, "Google")
            .map(|item| item.map(|payload| response::stream_text(&payload)));

        Ok(stream.boxed())
    }

    async fn stream_chat_events(
        &self,
        request: ModelRequest,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent, String>>, String> {
        let client = self.client.http();
        // A channel whose upstream already refused `thinkingConfig` stops
        // asking for it: one probe request per process pays for the knowledge,
        // and every later turn streams thinkingless from the start.
        let omit = self.thinking_was_rejected();
        let (url, headers, body) = self.prepare_request(request.clone(), true, omit);

        let response = client
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|error| transport_error("Google", error))?;
        let response = match ensure_success(response, "Google").await {
            Ok(response) => response,
            Err(e) => {
                // Elastic downgrade: an upstream that rejects
                // `thinkingConfig` is withholding the chain, not failing the
                // turn. Retry the identical request with the
                // thinking-disclosure surface omitted so the answer still
                // streams; the sticky flag (`note_thinking_rejected`) makes
                // this at most one extra request per channel per process.
                let e = response::clarify_error(e, &self.endpoint.model, &self.endpoint.base_url);
                if response::rejects_thinking_config(&e) && self.note_thinking_rejected() {
                    tracing::warn!(
                        model = %self.endpoint.model,
                        "upstream rejected thinkingConfig; streaming without disclosed thinking (chain withheld by upstream)"
                    );
                    return Box::pin(self.stream_chat_events_without_thinking(request)).await;
                }
                return Err(e);
            }
        };

        Ok(self.wrap_event_stream(response))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Baseline capability view for tests that need to stamp an effort ladder:
    /// `GoogleProvider::new` derives capabilities from the registry, which the
    /// llm-client crate's tests see as the fallback (`effort_levels == []`).
    fn p_caps(model: &str) -> muta_contracts::ModelCapabilities {
        muta_contracts::ModelCapabilities::for_channel(model, None)
    }

    #[test]
    fn default_constructor_targets_official_base() {
        // `new` resolves the official versioned base; the per-call path is
        // appended at request time, not stored on the base.
        let p = GoogleProvider::new("k".to_string(), "gemini-2.5-flash".to_string());
        assert_eq!(
            p.endpoint.base_url(),
            "https://generativelanguage.googleapis.com/v1beta"
        );
        assert_eq!(p.endpoint.user_agent(), crate::MUTA_USER_AGENT);
    }

    #[test]
    fn custom_base_url_strips_trailing_slash() {
        // A relay/中转站 base supplied with a trailing slash must not yield a
        // double slash in the appended model path.
        let p = GoogleProvider::with_base_url_and_user_agent(
            "k".to_string(),
            "gemini-2.5-flash".to_string(),
            "https://relay.example.com/v1beta/",
            "relay-agent/1.0",
        );
        assert_eq!(p.endpoint.base_url(), "https://relay.example.com/v1beta");
        assert_eq!(p.endpoint.user_agent(), "relay-agent/1.0");
    }

    #[test]
    fn prompt_hints_emit_no_system_guidance() {
        let p = GoogleProvider::new("k".to_string(), "gemini-2.5-flash".to_string());
        // No protocol note: native function calls are the wire default and the
        // model already follows its own functionResponse replay convention.
        assert!(p.prompt_hints().system_guidance.is_empty());
    }

    #[test]
    fn antigravity_envelope_normalizes_gemini_37_flash_wire_name() {
        let p = GoogleProvider::with_base_url_and_user_agent(
            "k".to_string(),
            "gemini-3.7-flash".to_string(),
            "https://daily-cloudcode-pa.googleapis.com/v1internal",
            "ua",
        )
        .with_project_id("proj-1");
        let (_, _, body) = p.prepare_request(ModelRequest::new(Vec::new()), true, false);
        assert_eq!(body["model"], "gemini-3.7-flash-tiered");
        assert!(body["request"].is_object());
    }

    #[test]
    fn antigravity_envelope_preserves_canonical_antigravity_wire_ids() {
        for (id, expected) in [
            ("gemini-3.7-flash-tiered", "gemini-3.7-flash-tiered"),
            ("gemini-pro-agent", "gemini-pro-agent"),
            ("gemini-3.1-pro-low", "gemini-3.1-pro-low"),
            ("gemini-3.1-flash-lite", "gemini-3.1-flash-lite"),
            ("gemini-2.5-flash", "gemini-2.5-flash"),
            ("claude-sonnet-4-6", "claude-sonnet-4-6"),
            ("claude-opus-4-6-thinking", "claude-opus-4-6-thinking"),
        ] {
            let p = GoogleProvider::with_base_url_and_user_agent(
                "k".to_string(),
                id.to_string(),
                "https://daily-cloudcode-pa.googleapis.com/v1internal",
                "ua",
            )
            .with_project_id("proj-1");
            let (_, _, body) = p.prepare_request(ModelRequest::new(Vec::new()), false, false);
            assert_eq!(
                body["model"], expected,
                "model id must match expected wire id"
            );
        }
    }

    #[test]
    fn thinking_rejected_flag_is_sticky_and_latches_once() {
        // One observation latches the channel-scoped flag; the exactly-once
        // return lets the caller log the downgrade exactly once per process.
        let p = GoogleProvider::new("k".to_string(), "gemini-3.7-flash".to_string());
        assert!(!p.thinking_was_rejected());
        assert!(p.note_thinking_rejected(), "first observation reports new");
        assert!(p.thinking_was_rejected());
        assert!(
            !p.note_thinking_rejected(),
            "later observations report already-known"
        );
        assert!(p.thinking_was_rejected());
    }

    #[test]
    fn omit_thinking_strips_the_whole_thinking_surface() {
        // The downgrade must drop BOTH the disclosure request and the depth
        // directive: `includeThoughts` alone is what the upstream refused, but
        // `thinkingLevel`/`thinkingBudget` ride the same rejected object and
        // re-sending either would fail the retry identically.
        let capabilities = {
            let mut caps = p_caps("gemini-3.7-flash");
            caps.thinking = muta_contracts::thinking::ThinkingSupport::ReasoningContent;
            caps.effort_levels = muta_contracts::effort::EFFORT_GEMINI_LEVEL
                .iter()
                .copied()
                .map(Into::into)
                .collect();
            caps
        };
        let p = GoogleProvider::new("k".to_string(), "gemini-3.7-flash".to_string())
            .with_model_capabilities(capabilities)
            .with_reasoning_effort(Some(muta_contracts::Effort::High));
        let (_, _, with) = p.prepare_request(ModelRequest::new(Vec::new()), true, false);
        assert_eq!(
            with["generationConfig"]["thinkingConfig"]["includeThoughts"],
            true
        );
        assert_eq!(
            with["generationConfig"]["thinkingConfig"]["thinkingLevel"],
            "high"
        );

        let (_, _, without) = p.prepare_request(ModelRequest::new(Vec::new()), true, true);
        assert!(
            without["generationConfig"]["thinkingConfig"].is_null(),
            "the entire thinkingConfig object must be absent after downgrade"
        );
    }

    #[test]
    fn omit_thinking_is_orthogonal_to_non_antigravity_path() {
        // The native (non-envelope) path stamps thinkingConfig on the raw
        // body; the downgrade applies there identically.
        let capabilities = {
            let mut caps = p_caps("gemini-2.5-pro");
            caps.thinking = muta_contracts::thinking::ThinkingSupport::ReasoningContent;
            caps.effort_levels = muta_contracts::effort::EFFORT_GEMINI_BUDGET
                .iter()
                .copied()
                .map(Into::into)
                .collect();
            caps
        };
        let p = GoogleProvider::new("k".to_string(), "gemini-2.5-pro".to_string())
            .with_model_capabilities(capabilities)
            .with_reasoning_effort(Some(muta_contracts::Effort::High));
        let (_, _, with) = p.prepare_request(ModelRequest::new(Vec::new()), true, false);
        assert_eq!(
            with["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            32768
        );
        let (_, _, without) = p.prepare_request(ModelRequest::new(Vec::new()), true, true);
        assert!(without["generationConfig"]["thinkingConfig"].is_null());
    }
}
