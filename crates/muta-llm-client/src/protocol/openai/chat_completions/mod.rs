//! OpenAI-compatible chat-completions provider with native tool-call support
//! and a streaming filter that strips tool-call "echo" text (GLM/Qwen style).
//!
//! A thin executor over the pure [`request`], [`response`], and [`echo`]
//! layers plus the shared transport helpers. The provider struct holds only
//! the shared [`Endpoint`] (connection config) — every wire-format detail lives in a pure, independently
//! testable module.
//!
//! Module layout (mirrors the Google and Anthropic providers):
//!   - [`request`] — body / headers / message conversion (pure, no I/O)
//!   - [`response`] — usage, message, and stream-payload parsing (pure)
//!   - [`echo`] — the tool-call "echo" suppression filter (stateful, no I/O)
//!   - this file — the [`OpenAiChatCompletionsProvider`] executor + `Provider` impl

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use muta_contracts::{
    CredentialSource, Effort, ModelRequest, Provider, ProviderError, ProviderErrorKind,
    ProviderPromptHints, ProviderStreamEvent, ResolvedAuth,
};
use std::sync::Arc;
use std::sync::Mutex;

use crate::transport::{decode_response_json, ensure_success};
use crate::{Client, ClientProfile, Endpoint};

pub mod echo;
pub mod qoder;
pub mod request;
pub mod response;

/// OpenAI-compatible chat-completions provider.
///
/// Embeds the shared [`Endpoint`] plus the optional OpenAI
/// `reasoning_effort` override.
pub struct OpenAiChatCompletionsProvider {
    pub endpoint: Endpoint,
    pub reasoning_effort: Option<Effort>,
    /// Route-scoped prompt-cache capabilities, defaults, and affinity.
    pub prompt_cache: crate::PromptCacheConfig,
    /// Channel-scoped capability view. A trusted remote catalogue overrides the
    /// static baseline only for this provider/model route.
    pub capabilities: muta_contracts::ModelCapabilities,
    /// When `true`, inject GitHub Copilot's required per-request headers
    /// (`x-initiator`, `Openai-Intent`, `X-GitHub-Api-Version`) in addition to
    /// the bearer. Flipped on by the catalog for Copilot OAuth channels that
    /// speak the chat-completions surface (the GPT-4o family and Copilot Free
    /// accounts, which do not have Responses-API access). Mirrors the same flag
    /// on [`OpenAiResponsesProvider`](crate::OpenAiResponsesProvider).
    pub dialect: muta_contracts::OpenAiChatDialect,
    /// Pooled HTTP client reused across every request this provider makes.
    pub client: Client,
}

/// Canonical alias for the Chat Completions protocol provider.
pub type ChatCompletionsProvider = OpenAiChatCompletionsProvider;

impl OpenAiChatCompletionsProvider {
    pub fn new(api_key: String, model: String) -> Self {
        Self::with_base_url(api_key, model, "https://api.openai.com/v1/chat/completions")
    }

    pub fn with_base_url(api_key: String, model: String, base_url: &str) -> Self {
        Self::with_base_url_and_user_agent(api_key, model, base_url, crate::MUTA_USER_AGENT)
    }

    pub fn with_base_url_and_user_agent(
        api_key: String,
        model: String,
        base_url: &str,
        user_agent: &str,
    ) -> Self {
        let capabilities = muta_contracts::ModelCapabilities::for_channel(&model, None);
        Self {
            endpoint: Endpoint::from_static_key(api_key, model, base_url, "openai")
                .with_user_agent(user_agent),
            client: Client::new(),
            reasoning_effort: None,
            prompt_cache: crate::PromptCacheConfig::default(),
            capabilities,
            dialect: muta_contracts::OpenAiChatDialect::Standard,
        }
    }

    /// Build a provider with dynamic credentials.
    pub fn with_credentials(
        credentials: std::sync::Arc<dyn CredentialSource>,
        model: String,
        base_url: &str,
        client_profile: impl Into<ClientProfile>,
    ) -> Self {
        let capabilities = muta_contracts::ModelCapabilities::for_channel(&model, None);
        Self {
            endpoint: Endpoint::with_credentials(credentials, model, base_url, "openai")
                .with_client_profile(client_profile),
            client: Client::new(),
            reasoning_effort: None,
            prompt_cache: crate::PromptCacheConfig::default(),
            capabilities,
            dialect: muta_contracts::OpenAiChatDialect::Standard,
        }
    }

    /// Stamp the attribution id (the registry does this with the channel entry
    /// id). Returns `self` for chaining.
    pub fn with_id(mut self, id: String) -> Self {
        self.endpoint.set_id(id);
        self
    }

    /// Set the OpenAI `reasoning_effort` override for models that expose it.
    /// `None` keeps the provider default.
    pub fn with_reasoning_effort(mut self, effort: Option<Effort>) -> Self {
        self.reasoning_effort = effort;
        self
    }

    pub fn with_prompt_cache(mut self, prompt_cache: crate::PromptCacheConfig) -> Self {
        self.prompt_cache = prompt_cache;
        self
    }

    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.endpoint = self.endpoint.with_session_id(session_id);
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

    pub fn with_dialect(mut self, dialect: muta_contracts::OpenAiChatDialect) -> Self {
        self.dialect = dialect;
        self
    }

    /// Human-readable backend label for error messages and logs. The OpenAI
    /// chat-completions provider serves both the generic OpenAI-compatible
    /// surface and the GitHub Copilot chat surface behind one wire format;
    /// surfacing the right name in errors ("Copilot HTTP 400" vs "OpenAI HTTP
    /// 400") is essential for diagnosing which backend rejected a request.
    fn label(&self) -> &'static str {
        match self.dialect {
            muta_contracts::OpenAiChatDialect::Copilot => "Copilot",
            muta_contracts::OpenAiChatDialect::OpenRouter => "OpenRouter",
            muta_contracts::OpenAiChatDialect::Qoder => "Qoder",
            muta_contracts::OpenAiChatDialect::Standard => "OpenAI",
        }
    }

    /// Apply the per-request auth + user-agent headers to a request builder.
    /// In Qoder dialect the bearer is the exchange token and the body is the
    /// already-QoderEncoding-encoded string (see `build_qoder_request`).
    fn build_request_for_auth(
        &self,
        body: &serde_json::Value,
        auth: &ResolvedAuth,
    ) -> crate::request::RequestBuilder {
        let mut req =
            crate::request::RequestBuilder::new(http::Method::POST, self.endpoint.base_url())
                .header(http::header::USER_AGENT, self.endpoint.user_agent())
                .json(body);
        let copilot = self.dialect == muta_contracts::OpenAiChatDialect::Copilot;
        for (name, value) in request::headers(auth.token.expose_secret(), self.dialect) {
            req = req.header(name, value);
        }
        for (name, value) in self.endpoint.headers() {
            if !copilot
                || !crate::COPILOT_CLIENT_HEADERS
                    .iter()
                    .any(|(k, _)| *k == name)
            {
                req = req.header(name, value);
            }
        }
        req = self
            .endpoint
            .attach_session_affinity_headers(req, self.prompt_cache.routing_key());
        req
    }

    /// Build a Qoder COSY-signed request from the chat-completions JSON.
    ///
    /// Qoder's wire differs from the plain bearer surface in three ways: the
    /// body is QoderEncoding-encoded, the `Authorization` header carries the
    /// COSY signature bundle (not the raw bearer), and identity headers
    /// (`Cosy-User`, `Cosy-Date`, `Cosy-Key`, org scope) ride along. The
    /// resolved auth's `qoder` field carries the typed request identity —
    /// the provider-owned machine key, uid, and org scope; the generic
    /// `account_id`/`project_id` fields belong to other protocols and are
    /// not consulted here.
    fn build_qoder_request(
        &self,
        body: &serde_json::Value,
        auth: &ResolvedAuth,
        now_secs: u64,
    ) -> Result<crate::request::RequestBuilder, ProviderError> {
        let qoder_identity = auth.qoder.as_ref().ok_or_else(|| {
            ProviderError::authentication(
                self.label(),
                "Qoder request identity is missing; re-authorize this connection".to_string(),
            )
        })?;
        let key_hex = qoder_identity.machine_key_hex.expose_secret();
        let identity_key = qoder::CosyIdentity::parse_key_hex(key_hex).ok_or_else(|| {
            ProviderError::authentication(
                self.label(),
                "Qoder machine key is malformed; re-authorize this connection".to_string(),
            )
        })?;
        let identity = qoder::CosyIdentity::from_key_hex(&identity_key);
        let uid = qoder_identity.uid.as_str();
        let raw = serde_json::to_vec(body)
            .map_err(|e| ProviderError::invalid_request(self.label(), e.to_string()))?;
        let encoded = qoder::encode_body(&raw[..]);
        let identity_json = qoder_identity.identity_payload_json(
            auth.token.expose_secret(),
            auth.user_email.as_deref().unwrap_or(""),
        );
        let prepared = qoder::prepare_request(&identity, &identity_json, uid, &encoded, now_secs);
        let mut req = crate::request::RequestBuilder::new(
            http::Method::POST,
            qoder::inference_url(self.endpoint.base_url()),
        )
        .header(http::header::USER_AGENT, self.endpoint.user_agent())
        .header("Content-Type", "application/json")
        .header("Authorization", prepared.authorization)
        .header("Cosy-Date", prepared.date)
        .header("Cosy-Key", prepared.key);
        if !uid.is_empty() {
            req = req.header("Cosy-User", uid);
        }
        // Organization scope: presence is conditional (20/21/22-header
        // contract — omit entirely when unset, keep order otherwise).
        if let Some(org_id) = qoder_identity.organization_id.as_deref()
            && !org_id.is_empty()
        {
            req = req.header("Cosy-Organization-Id", org_id);
        }
        if !qoder_identity.organization_tags.is_empty() {
            req = req.header(
                "Cosy-Organization-Tags",
                qoder_identity.organization_tags.join(","),
            );
        }
        for (name, value) in self.endpoint.headers() {
            req = req.header(name, value);
        }
        req = req.body(prepared.encoded_body);
        Ok(req)
    }

    /// Send a request with automatic token resolution, timeout stamping,
    /// and reactive force-refresh on HTTP 401 Unauthorized for OAuth channels.
    async fn send_request(
        &self,
        body: &serde_json::Value,
        is_stream: bool,
        telemetry: &muta_contracts::TransportTelemetry,
    ) -> Result<crate::egress::HttpResponse, ProviderError> {
        let auth = self
            .endpoint
            .resolve_auth()
            .await
            .map_err(|e| ProviderError::authentication(self.label(), e))?;
        let qoder = self.dialect == muta_contracts::OpenAiChatDialect::Qoder;
        let mut req = if qoder {
            let built = self
                .build_qoder_request(body, &auth, unix_secs())
                .map_err(|e| e)?;
            built.with_telemetry(telemetry.clone())
        } else {
            self.build_request_for_auth(body, &auth).with_telemetry(telemetry.clone())
        };
        if !is_stream {
            req = req.timeout(self.client.request_timeout());
        }
        let response = self.client.send_raw(req, self.label()).await?;

        if response.status == http::StatusCode::UNAUTHORIZED && self.endpoint.is_oauth() && !qoder
        {
            tracing::warn!(
                provider = %self.endpoint.id,
                model = %self.endpoint.model,
                "OAuth token rejected by {} (401 Unauthorized); attempting force-refresh and retry",
                self.label()
            );
            let refreshed_auth = self
                .endpoint
                .force_refresh_auth_after(&auth.token)
                .await
                .map_err(|error| ProviderError::authentication(self.label(), error))?;
            let mut retry_req = self
                .build_request_for_auth(body, &refreshed_auth)
                .with_telemetry(telemetry.clone());
            if !is_stream {
                retry_req = retry_req.timeout(self.client.request_timeout());
            }
            return self.client.send(retry_req, self.label()).await;
        }

        ensure_success(response, self.label(), Some(&self.endpoint.model)).await
    }
}

/// Current unix seconds (Qoder's `Cosy-Date` granularity).
fn unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[async_trait]
impl Provider for OpenAiChatCompletionsProvider {
    fn provider_id(&self) -> String {
        self.endpoint.id.clone()
    }

    fn model(&self) -> String {
        self.endpoint.model.clone()
    }

    fn effort(&self) -> Option<Effort> {
        self.reasoning_effort
    }

    fn model_capabilities(&self) -> muta_contracts::ModelCapabilities {
        self.capabilities.clone()
    }

    fn prompt_hints(&self) -> ProviderPromptHints {
        // No protocol hint: the OpenAI wire surface uses native tool calls by
        // construction, and the `ToolCallEchoFilter` deterministically strips
        // any text-mirrored call regardless of prompting. An in-prompt note
        // would only restate facts the model already has and the harness
        // already enforces.
        ProviderPromptHints {
            system_guidance: "",
        }
    }

    fn usage_supported(&self) -> bool {
        true
    }

    async fn chat(
        &self,
        request: ModelRequest,
    ) -> Result<muta_contracts::ProviderCompletion, muta_contracts::ProviderError> {
        let cache_plan = self
            .prompt_cache
            .resolve(&request)
            .map_err(|e| ProviderError::invalid_request(self.label(), e))?;
        let ModelRequest {
            instructions,
            mut messages,
            tool_specs,
            temporary_context,
            transport_telemetry,
            ..
        } = request;
        messages.extend(temporary_context);
        let body = request::body_with_capabilities(
            messages,
            request::BodyInput {
                model: &self.endpoint.model,
                stream: false,
                instructions: Some(&instructions),
                tool_specs: (!tool_specs.is_empty()).then_some(tool_specs.as_slice()),
                reasoning_effort: self.reasoning_effort,
                dialect: self.dialect,
                cache_plan: &cache_plan,
            },
            &self.capabilities,
        );

        let label = self.label();
        let resp = self
            .send_request(&body, false, &transport_telemetry)
            .await?;
        let response_json: serde_json::Value = decode_response_json(resp, label).await?;

        if let Some(err) = response_json.get("error") {
            return Err(ProviderError::new(
                label,
                ProviderErrorKind::Protocol,
                format!("{label} Error: {}", err),
            ));
        }

        let usage = response::usage(&response_json["usage"]);

        let choice = &response_json["choices"][0]["message"];
        let message = response::message(choice, |raw, had_native| {
            let emitted = echo::ToolCallEchoFilter::filter_content(raw, had_native);
            tracing::debug!(
                target: "muta_contracts::provider",
                provider = %self.endpoint.id,
                model = %self.endpoint.model,
                raw_chars = raw.len(),
                emitted_chars = emitted.len(),
                suppressed_chars = raw.len().saturating_sub(emitted.len()),
                native_tool_calls = had_native,
                "openai chat echo summary",
            );
            emitted
        });
        Ok(muta_contracts::ProviderCompletion {
            message,
            meta: muta_contracts::ProviderCompletionMeta {
                usage,
                ..Default::default()
            },
        })
    }

    async fn stream_chat(
        &self,
        request: ModelRequest,
    ) -> Result<
        BoxStream<'static, Result<String, muta_contracts::ProviderError>>,
        muta_contracts::ProviderError,
    > {
        let cache_plan = self
            .prompt_cache
            .resolve(&request)
            .map_err(|e| ProviderError::invalid_request(self.label(), e))?;
        let ModelRequest {
            instructions,
            mut messages,
            tool_specs,
            temporary_context,
            transport_telemetry,
            ..
        } = request;
        messages.extend(temporary_context);
        let body = request::body_with_capabilities(
            messages,
            request::BodyInput {
                model: &self.endpoint.model,
                stream: true,
                instructions: Some(&instructions),
                tool_specs: (!tool_specs.is_empty()).then_some(tool_specs.as_slice()),
                reasoning_effort: self.reasoning_effort,
                dialect: self.dialect,
                cache_plan: &cache_plan,
            },
            &self.capabilities,
        );

        let response = self.send_request(&body, true, &transport_telemetry).await?;

        let stream = crate::sse::data_payloads(response, self.label()).map(|item| {
            let data = item?;
            Ok(response::stream_text(&data))
        });

        Ok(stream.boxed())
    }

    async fn stream_chat_events(
        &self,
        request: ModelRequest,
    ) -> Result<
        BoxStream<'static, Result<ProviderStreamEvent, muta_contracts::ProviderError>>,
        muta_contracts::ProviderError,
    > {
        let cache_plan = self
            .prompt_cache
            .resolve(&request)
            .map_err(|e| ProviderError::invalid_request(self.label(), e))?;
        let ModelRequest {
            instructions,
            mut messages,
            tool_specs,
            temporary_context,
            transport_telemetry,
            ..
        } = request;
        messages.extend(temporary_context);
        let body = request::body_with_capabilities(
            messages,
            request::BodyInput {
                model: &self.endpoint.model,
                stream: true,
                instructions: Some(&instructions),
                tool_specs: (!tool_specs.is_empty()).then_some(tool_specs.as_slice()),
                reasoning_effort: self.reasoning_effort,
                dialect: self.dialect,
                cache_plan: &cache_plan,
            },
            &self.capabilities,
        );

        let response = self.send_request(&body, true, &transport_telemetry).await?;

        // Tool-call echo filter shared between the body and the end-of-stream
        // flush: it suppresses any content that mirrors a native tool call
        // before it becomes a `TextDelta`. SSE byte reassembly (incl.
        // multi-byte UTF-8 split across chunks) is handled by
        // `sse::data_payloads`; each payload is then parsed into the OpenAI
        // event shape and fed through the echo filter.
        let echo_filter = Arc::new(Mutex::new(echo::ToolCallEchoFilter::new()));
        let filter_for_body = Arc::clone(&echo_filter);
        let reasoning_details =
            Arc::new(Mutex::new(response::ReasoningDetailsAccumulator::default()));
        let reasoning_details_for_body = Arc::clone(&reasoning_details);
        let collect_reasoning_details =
            self.dialect == muta_contracts::OpenAiChatDialect::OpenRouter;
        let unwrap_qoder_envelope = self.dialect == muta_contracts::OpenAiChatDialect::Qoder;
        let label = self.label();
        let body = crate::sse::data_payloads(response, label).map(move |item| {
            let data = item?;
            // Qoder's outer envelope (`{"headers":..,"body":"<chunk>",
            // "statusCodeValue":200}`) unwraps first: the inner `body` string
            // is the chat.completion.chunk JSON that `stream_events` parses.
            // A `statusCodeValue != 200` envelope is an upstream error and
            // surfaces as-is; the `[DONE]` marker inside a 200 body is not a
            // terminator (the authoritative close is `event:finish`).
            let data = if unwrap_qoder_envelope {
                match qoder::unwrap_envelope(&data) {
                    qoder::Envelope::Chunk(inner) => inner,
                    qoder::Envelope::Done => String::new(),
                    qoder::Envelope::Skip => String::new(),
                    qoder::Envelope::Error(status, message) => {
                        return Err(ProviderError::new(
                            label,
                            ProviderErrorKind::Upstream,
                            format!("Qoder upstream error ({status}): {message}"),
                        ));
                    }
                }
            } else {
                data
            };
            if data.is_empty() {
                return Ok::<Vec<Result<ProviderStreamEvent, ProviderError>>, ProviderError>(
                    Vec::new(),
                );
            }
            // Parse-once discipline (ADR-0184): the single deserialization
            // here doubles as the validity check — a non-JSON payload is a
            // decode error, and the parsed `Value` feeds the stream parser
            // (which then fans events out through the echo filter).
            let event: serde_json::Value = serde_json::from_str(&data).map_err(|_| {
                ProviderError::new(
                    label,
                    ProviderErrorKind::Decode,
                    "Invalid JSON in stream payload",
                )
            })?;
            if collect_reasoning_details {
                reasoning_details_for_body
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .observe(&event);
            }
            let parsed = response::stream_events(&event);
            // Recover from a poisoned mutex: a prior panic in this critical
            // section must not take down subsequent stream chunks.
            let mut filter = filter_for_body
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let mut events: Vec<Result<ProviderStreamEvent, ProviderError>> = Vec::new();
            for event in parsed {
                events.extend(filter.observe(event).into_iter().map(Ok));
            }
            Ok::<_, ProviderError>(events)
        });
        // Flush any buffered non-echo text once the byte stream ends, and log a
        // per-turn stream summary so empty responses are diagnosable.
        let provider_id = self.endpoint.id.clone();
        let model = self.endpoint.model.clone();
        let tail = futures::stream::once(async move {
            let mut filter = echo_filter
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let emitted = filter.finish();
            tracing::debug!(
                target: "muta_contracts::provider",
                provider = %provider_id,
                model = %model,
                content_fed_chars = filter.fed_chars,
                content_emitted_chars = filter.emitted_chars,
                echo_suppressed_chars = filter.fed_chars.saturating_sub(filter.emitted_chars),
                reasoning_chars = filter.reasoning_chars,
                tool_call_deltas = filter.tool_call_deltas,
                "openai stream summary",
            );
            let mut events: Vec<Result<ProviderStreamEvent, ProviderError>> = Vec::new();
            if !emitted.is_empty() {
                events.push(Ok(ProviderStreamEvent::TextDelta(emitted)));
            }
            let artifacts = collect_reasoning_details
                .then(|| {
                    reasoning_details
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .artifacts()
                })
                .flatten();
            events.push(Ok(ProviderStreamEvent::Completed(
                muta_contracts::ProviderCompletionMeta {
                    artifacts,
                    ..Default::default()
                },
            )));
            Ok::<_, ProviderError>(events)
        });
        Ok(body
            .chain(tail)
            .flat_map(|result| match result {
                Ok(events) => futures::stream::iter(events),
                Err(error) => futures::stream::iter(vec![Err(error)]),
            })
            .boxed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muta_contracts::{Message, Role, Tool};

    // resolved-variant schema reaches the request body

    /// Minimal Tool stand-in carrying a variant id, so resolving a toolset and
    /// preparing its schemas can be exercised without the whole tools crate.
    struct DummyTool {
        name: &'static str,
        variant: &'static str,
        desc: &'static str,
    }
    #[async_trait]
    impl Tool for DummyTool {
        fn name(&self) -> &str {
            self.name
        }
        fn variant(&self) -> &str {
            self.variant
        }
        fn description(&self) -> &str {
            self.desc
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn call(&self, _: &str) -> Result<String, String> {
            Ok(String::new())
        }
    }

    fn tool_desc_at(body: &serde_json::Value, idx: usize) -> &str {
        body["tools"][idx]["function"]["description"]
            .as_str()
            .unwrap_or("")
    }

    fn body_with_tools(tools: &[Arc<dyn Tool>]) -> serde_json::Value {
        let request = ModelRequest::with_tools(vec![Message::new(Role::User, "go")], tools);
        let (messages, tool_specs) = request.into_parts();
        static DEFAULT_CACHE_PLAN: muta_contracts::ResolvedCachePolicy =
            muta_contracts::ResolvedCachePolicy::Unsupported;
        request::body(
            messages,
            request::BodyInput {
                model: "test-model",
                stream: false,
                instructions: None,
                tool_specs: Some(&tool_specs),
                reasoning_effort: None,
                dialect: muta_contracts::OpenAiChatDialect::Standard,
                cache_plan: &DEFAULT_CACHE_PLAN,
            },
        )
    }

    #[test]
    fn model_request_emits_the_selected_variants_schema() {
        // A `read_text` capability with two variants; the agent resolves a
        // selection before handing the toolset to the provider, so whichever
        // variant is selected is the one whose schema reaches the request body.
        let toolset = muta_contracts::ToolSet::from_tools(vec![
            Arc::new(DummyTool {
                name: "read_text",
                variant: "default",
                desc: "default wording",
            }) as Arc<dyn Tool>,
            Arc::new(DummyTool {
                name: "read_text",
                variant: "terse",
                desc: "terse wording",
            }) as Arc<dyn Tool>,
        ]);

        // Default selection → default variant's description in the body.
        let body = body_with_tools(&toolset.default_view());
        assert_eq!(tool_desc_at(&body, 0), "default wording");
        assert_eq!(body["tools"][0]["function"]["name"], "read_text");

        // Selecting the terse variant → terse description in the body, same name.
        let mut selection = muta_contracts::VariantSelection::new();
        selection.insert("read_text".to_string(), "terse".to_string());
        let body = body_with_tools(&toolset.resolve(&selection));
        assert_eq!(tool_desc_at(&body, 0), "terse wording");
        assert_eq!(body["tools"][0]["function"]["name"], "read_text");
        assert_eq!(body["tools"][0]["type"], "function");
    }

    #[test]
    fn prompt_hints_emit_no_system_guidance() {
        let provider =
            OpenAiChatCompletionsProvider::new("test-key".to_string(), "test-model".to_string());
        // No protocol note: native tool calls are the wire default and the
        // ToolCallEchoFilter strips text-mirrored calls regardless.
        assert!(provider.prompt_hints().system_guidance.is_empty());
    }

    #[test]
    fn opencode_go_request_carries_session_and_client_headers() {
        let provider = OpenAiChatCompletionsProvider::with_base_url_and_user_agent(
            "test-key".to_string(),
            "glm-5.2".to_string(),
            "https://opencode.ai/zen/go/v1/chat/completions",
            crate::OPENCODE_USER_AGENT,
        )
        .with_id("opencode-go".to_string())
        .with_session_id("ses_wire_test_123");

        let auth = muta_contracts::ResolvedAuth::new("test-token");
        let body = serde_json::json!({"model": "glm-5.2"});
        let req = provider
            .build_request_for_auth(&body, &auth)
            .build("Test")
            .unwrap();

        let headers = &req.headers;
        assert_eq!(
            headers.get("x-opencode-session").unwrap().to_str().unwrap(),
            "ses_wire_test_123"
        );
        assert_eq!(
            headers.get("x-opencode-client").unwrap().to_str().unwrap(),
            "cli"
        );
        assert!(
            headers
                .get("x-opencode-request")
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("req_")
        );
        assert_eq!(
            headers.get("user-agent").unwrap().to_str().unwrap(),
            crate::OPENCODE_USER_AGENT
        );
    }

    // ── Qoder dialect ───────────────────────────────────────────────────────

    fn qoder_provider() -> OpenAiChatCompletionsProvider {
        OpenAiChatCompletionsProvider::with_base_url(
            String::new(),
            "qoder3".to_string(),
            "https://api2.qoder.sh",
        )
        .with_dialect(muta_contracts::OpenAiChatDialect::Qoder)
    }

    fn qoder_auth() -> ResolvedAuth {
        ResolvedAuth {
            token: "pt-demo".into(),
            account_id: None,
            project_id: None,
            user_email: Some("dev@example.com".to_string()),
            qoder: Some(muta_contracts::QoderRequestIdentity {
                uid: "u-123".to_string(),
                machine_key_hex: qoder::CosyIdentity::generate().key_hex().into(),
                data_policy_agreed: true,
                organization_id: None,
                organization_tags: Vec::new(),
            }),
        }
    }

    #[test]
    fn qoder_static_headers_carry_the_identity_contract() {
        let provider = qoder_provider();
        let body = serde_json::json!({"model": "qoder3"});
        let req = provider
            .build_request_for_auth(&body, &qoder_auth())
            .build("Test")
            .unwrap();
        let header = |name: &str| {
            req.headers
                .get(name)
                .map(|v| v.to_str().unwrap().to_string())
                .unwrap_or_default()
        };
        assert_eq!(header("Cosy-ClientType"), "5");
        assert_eq!(header("Cosy-MachineType"), "5");
        assert_eq!(header("Cosy-Version"), qoder::COSY_VERSION);
        assert_eq!(header("Cosy-Business-Product"), "cli");
        assert_eq!(header("Cosy-Business-Type"), "agent");
        assert_eq!(header("Cosy-Scene"), "assistant");
        assert_eq!(header("Cosy-Data-Policy"), "agree");
        assert_eq!(header("Login-Version"), "v2");
        assert_eq!(header("accept"), "text/event-stream");
    }

    #[test]
    fn qoder_request_stamps_cosy_signature_and_encoded_body() {
        let provider = qoder_provider();
        let body = serde_json::json!({"model": "qoder3", "messages": []});
        let req = provider
            .build_qoder_request(&body, &qoder_auth(), 1_700_000_000)
            .unwrap()
            .build("Test")
            .unwrap();
        // URL is the inference path with the fixed query contract.
        assert!(
            req.url.starts_with(
                "https://api2.qoder.sh/algo/api/v2/service/pro/sse/agent_chat_generation"
            ),
            "{}",
            req.url
        );
        assert!(req.url.contains("FetchKeys=llm_model_result"));
        assert!(req.url.contains("AgentId=agent_common"));
        assert!(req.url.contains("Encode=1"));
        // COSY authorization shape.
        let auth_header = req
            .headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(auth_header.starts_with("Bearer COSY."), "{auth_header}");
        let signature = auth_header.rsplit('.').next().unwrap();
        assert_eq!(signature.len(), 32);
        // Identity headers.
        assert_eq!(
            req.headers.get("cosy-date").unwrap().to_str().unwrap(),
            "1700000000"
        );
        assert_eq!(
            req.headers.get("cosy-user").unwrap().to_str().unwrap(),
            "u-123"
        );
        assert!(!req.headers.get("cosy-key").unwrap().is_empty());
        // The body is QoderEncoding (not plain JSON).
        let body_bytes = req.body.unwrap();
        let body_text = String::from_utf8_lossy(&body_bytes).to_string();
        assert!(
            !body_text.contains("{\"model\""),
            "body must be encoded, not raw JSON: {body_text}"
        );
    }

    #[test]
    fn qoder_request_fails_closed_without_machine_identity() {
        let provider = qoder_provider();
        let auth = ResolvedAuth {
            token: "pt-demo".into(),
            account_id: None,
            project_id: None,
            user_email: None,
            qoder: None,
        };
        let body = serde_json::json!({"model": "qoder3"});
        let result = provider.build_qoder_request(&body, &auth, 0);
        assert!(result.is_err(), "missing machine key must fail closed");
    }

    #[test]
    fn qoder_request_omits_org_headers_when_unscoped() {
        let provider = qoder_provider();
        let body = serde_json::json!({"model": "qoder3"});
        let req = provider
            .build_qoder_request(&body, &qoder_auth(), 0)
            .unwrap()
            .build("Test")
            .unwrap();
        assert!(req.headers.get("cosy-organization-id").is_none());
        assert!(req.headers.get("cosy-organization-tags").is_none());
    }

    #[test]
    fn qoder_request_carries_org_scope_when_present() {
        let provider = qoder_provider();
        let mut auth = qoder_auth();
        if let Some(identity) = auth.qoder.as_mut() {
            identity.organization_id = Some("org-9".to_string());
            identity.organization_tags = vec!["team-a".to_string(), "team-b".to_string()];
        }
        let body = serde_json::json!({"model": "qoder3"});
        let req = provider
            .build_qoder_request(&body, &auth, 0)
            .unwrap()
            .build("Test")
            .unwrap();
        assert_eq!(
            req.headers.get("cosy-organization-id").unwrap().to_str().unwrap(),
            "org-9"
        );
        assert_eq!(
            req.headers.get("cosy-organization-tags").unwrap().to_str().unwrap(),
            "team-a,team-b"
        );
    }
}
