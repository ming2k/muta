//! OpenAI **Responses** API provider — the wire format spoken by the ChatGPT
//! Subscription backend (`chatgpt.com/backend-api/codex/responses`).
//!
//! Unlike the chat-completions [`OpenAiChatCompletionsProvider`](crate::OpenAiChatCompletionsProvider),
//! this provider:
//! - sends dynamic OAuth credentials or static tokens,
//! - attaches the optional `ChatGPT-Account-Id` header for ChatGPT Subscriptions,
//! - builds a Responses request (`instructions` + `input` items) and parses
//!   `response.*` streaming events,
//! - supports self-healing reactive force-refresh on HTTP 401 Unauthorized for OAuth channels.

pub mod request;
pub mod response;
mod tool_trace;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use muta_contracts::{
    CredentialSource, Effort, ModelRequest, Provider, ProviderError, ProviderErrorKind,
    ProviderPromptHints, ProviderStreamEvent, ResolvedAuth,
};
use std::sync::{Arc, Mutex};

use crate::transport::{decode_response_json, ensure_success, transport_error};
use crate::{Client, ClientProfile, Endpoint};

fn parse_retry_after_from_message(message: &str) -> Option<u64> {
    let lower = message.to_ascii_lowercase();
    let idx = lower.find("try again in")?;
    let remainder = lower[idx + "try again in".len()..].trim_start();
    let num_end = remainder.find(|c: char| !c.is_ascii_digit() && c != '.')?;
    let val_str = &remainder[..num_end];
    let unit_part = remainder[num_end..].trim_start();
    let val = val_str.parse::<f64>().ok()?;
    if unit_part.starts_with("ms") {
        Some(val as u64)
    } else if unit_part.starts_with('s') || unit_part.starts_with("sec") {
        Some((val * 1000.0) as u64)
    } else {
        None
    }
}

fn parse_responses_stream_error(
    error_val: &serde_json::Value,
    label: &'static str,
) -> ProviderError {
    let code = error_val
        .get("code")
        .and_then(serde_json::Value::as_str);
    let message = error_val
        .get("message")
        .and_then(serde_json::Value::as_str)
        .filter(|m| !m.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            if let Some(code) = code {
                format!("Responses stream error: {code}")
            } else if let Some(s) = error_val.as_str() {
                s.to_string()
            } else {
                format!("Responses stream error: {error_val}")
            }
        });

    let retry_delay = parse_retry_after_from_message(&message);

    match code {
        Some("server_is_overloaded" | "slow_down") => {
            ProviderError::new(label, ProviderErrorKind::Unavailable, message)
                .with_status(503)
                .retryable(retry_delay.or(Some(2000)))
        }
        Some("rate_limit_exceeded") => {
            ProviderError::new(label, ProviderErrorKind::RateLimited, message)
                .with_status(429)
                .retryable(retry_delay)
        }
        Some("context_length_exceeded" | "context_window_exceeded") => {
            ProviderError::new(label, ProviderErrorKind::ContextOverflow, message)
                .with_status(400)
        }
        Some("insufficient_quota" | "usage_not_included") => {
            ProviderError::new(label, ProviderErrorKind::Unavailable, message)
                .with_status(402)
        }
        Some("cyber_policy" | "misalignment_policy_violation" | "invalid_prompt" | "bio_policy") => {
            ProviderError::new(label, ProviderErrorKind::InvalidRequest, message)
                .with_status(400)
        }
        _ => {
            let lower = message.to_ascii_lowercase();
            if lower.contains("overloaded") || lower.contains("capacity") {
                ProviderError::new(label, ProviderErrorKind::Unavailable, message)
                    .with_status(503)
                    .retryable(retry_delay.or(Some(2000)))
            } else if lower.contains("rate limit") {
                ProviderError::new(label, ProviderErrorKind::RateLimited, message)
                    .with_status(429)
                    .retryable(retry_delay)
            } else {
                let mut err = ProviderError::new(label, ProviderErrorKind::Protocol, message);
                if let Some(delay) = retry_delay {
                    err = err.retryable(Some(delay));
                }
                err
            }
        }
    }
}

fn decode_stream_payload(
    data: &str,
    label: &'static str,
) -> Result<serde_json::Value, ProviderError> {
    let value = serde_json::from_str::<serde_json::Value>(data).map_err(|error| {
        ProviderError::new(
            label,
            ProviderErrorKind::Decode,
            format!("Invalid JSON in Responses stream: {error}"),
        )
    })?;
    match value["type"].as_str() {
        Some("response.failed") => {
            let err_val = value
                .get("response")
                .and_then(|r| r.get("error"))
                .unwrap_or(&value["response"]);
            Err(parse_responses_stream_error(err_val, label))
        }
        Some("error") => {
            let err_val = value.get("error").unwrap_or(&value);
            Err(parse_responses_stream_error(err_val, label))
        }
        _ => {
            if let Some(err_val) = value.get("error") {
                if !err_val.is_null() {
                    return Err(parse_responses_stream_error(err_val, label));
                }
            }
            Ok(value)
        }
    }
}

fn models_etag(headers: &reqwest::header::HeaderMap) -> Option<String> {
    headers
        .get("x-models-etag")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// OpenAI Responses-API provider (ChatGPT Subscription backend).
pub struct OpenAiResponsesProvider {
    pub endpoint: Endpoint,
    pub reasoning_effort: Option<Effort>,
    /// Channel-scoped capability view. A trusted remote catalogue overrides the
    /// static baseline only for this provider/model route.
    pub capabilities: muta_contracts::ModelCapabilities,
    /// When `true`, attach ChatGPT Subscription headers (`originator: muta` and
    /// `ChatGPT-Account-Id`).
    pub dialect: muta_contracts::OpenAiResponsesDialect,
    /// Whether upstream persists response state and accepts
    /// `previous_response_id`. Subscription backends force this off.
    pub store: bool,
    /// Route-scoped prompt-cache capabilities, defaults, and affinity.
    pub prompt_cache: crate::PromptCacheConfig,
    /// Pooled HTTP client reused across every request this provider makes.
    pub client: Client,
}

impl OpenAiResponsesProvider {
    pub fn new(
        credentials: std::sync::Arc<dyn CredentialSource>,
        model: String,
        base_url: &str,
    ) -> Self {
        let capabilities = muta_contracts::ModelCapabilities::for_channel(&model, None);
        Self {
            endpoint: Endpoint::with_credentials(credentials, model, base_url, "chatgpt"),
            client: Client::new(),
            reasoning_effort: None,
            capabilities,
            dialect: muta_contracts::OpenAiResponsesDialect::Standard,
            store: true,
            prompt_cache: crate::PromptCacheConfig::default(),
        }
    }

    /// Build a provider with static API key string.
    pub fn from_static_key(api_key: String, model: String, base_url: &str) -> Self {
        Self::new(muta_contracts::static_credential(api_key), model, base_url)
    }

    /// Build a provider with dynamic credentials.
    pub fn with_credentials(
        credentials: std::sync::Arc<dyn CredentialSource>,
        model: String,
        base_url: &str,
    ) -> Self {
        Self::new(credentials, model, base_url)
    }

    pub fn with_user_agent(mut self, user_agent: &str) -> Self {
        self.endpoint.client_profile = ClientProfile::from_user_agent(user_agent);
        self
    }

    pub fn with_client_profile(mut self, profile: impl Into<ClientProfile>) -> Self {
        self.endpoint.client_profile = profile.into();
        self
    }

    pub fn with_id(mut self, id: String) -> Self {
        self.endpoint.set_id(id);
        self
    }

    pub fn with_reasoning_effort(mut self, effort: Option<Effort>) -> Self {
        self.reasoning_effort = effort;
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

    pub fn with_dialect(mut self, dialect: muta_contracts::OpenAiResponsesDialect) -> Self {
        self.dialect = dialect;
        if dialect != muta_contracts::OpenAiResponsesDialect::Standard {
            self.store = false;
        }
        self
    }

    pub fn with_store(mut self, store: bool) -> Self {
        self.store = store;
        self
    }

    pub fn with_prompt_cache(mut self, prompt_cache: crate::PromptCacheConfig) -> Self {
        self.prompt_cache = prompt_cache;
        self
    }

    /// Human-readable backend label for error messages and logs.
    fn label(&self) -> &'static str {
        match self.dialect {
            muta_contracts::OpenAiResponsesDialect::Copilot => "Copilot",
            muta_contracts::OpenAiResponsesDialect::ChatGpt => "ChatGPT",
            muta_contracts::OpenAiResponsesDialect::Standard => "OpenAI Responses",
        }
    }

    /// Build the HTTP request for the given payload and resolved credentials.
    fn build_request_for_auth(
        &self,
        body: &serde_json::Value,
        auth: &ResolvedAuth,
    ) -> reqwest::RequestBuilder {
        let mut req = self
            .client
            .http()
            .post(self.endpoint.base_url())
            .header(reqwest::header::USER_AGENT, self.endpoint.user_agent())
            .json(body);
        let copilot = self.dialect == muta_contracts::OpenAiResponsesDialect::Copilot;
        let chatgpt = self.dialect == muta_contracts::OpenAiResponsesDialect::ChatGpt;
        let is_copilot_vision = copilot && request::has_input_image(body);
        for (name, value) in request::headers(
            auth.token.expose_secret(),
            auth.account_id.as_deref(),
            copilot,
            chatgpt,
        ) {
            req = req.header(name, value);
        }
        if is_copilot_vision {
            req = req.header("Copilot-Vision-Request", "true");
        }
        if chatgpt {
            req = req.header("x-codex-routing-hint", format!("model={}", self.endpoint.model));
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
        req
    }

    /// Send a request with automatic token resolution, timeout stamping,
    /// and reactive force-refresh on HTTP 401 Unauthorized for OAuth channels.
    async fn send_request(
        &self,
        body: &serde_json::Value,
        is_stream: bool,
    ) -> Result<reqwest::Response, ProviderError> {
        let auth = self
            .endpoint
            .resolve_auth()
            .await
            .map_err(|e| ProviderError::authentication(self.label(), e))?;
        let mut req = self.build_request_for_auth(body, &auth);
        if !is_stream {
            req = req.timeout(self.client.request_timeout());
        }
        let response = req
            .send()
            .await
            .map_err(|error| transport_error(self.label(), error))?;

        if response.status() == reqwest::StatusCode::UNAUTHORIZED && self.endpoint.is_oauth() {
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
            let mut retry_req = self.build_request_for_auth(body, &refreshed_auth);
            if !is_stream {
                retry_req = retry_req.timeout(self.client.request_timeout());
            }
            let retried_resp = retry_req
                .send()
                .await
                .map_err(|error| transport_error(self.label(), error))?;
            return ensure_success(retried_resp, self.label()).await;
        }

        ensure_success(response, self.label()).await
    }

    fn build_body(
        &self,
        request: ModelRequest,
        stream: bool,
    ) -> Result<serde_json::Value, ProviderError> {
        let cache_plan = self
            .prompt_cache
            .resolve(&request)
            .map_err(|e| ProviderError::invalid_request(self.label(), e))?;
        let ModelRequest {
            instructions,
            messages,
            tool_specs,
            delivery,
            ..
        } = request;
        request::body_with_capabilities(
            messages,
            request::BodyInput {
                model: &self.endpoint.model,
                stream,
                instructions: Some(&instructions),
                tool_specs: (!tool_specs.is_empty()).then_some(tool_specs.as_slice()),
                reasoning_effort: self.reasoning_effort,
                delivery: &delivery,
                store: self.store,
                cache_plan: &cache_plan,
            },
            &self.capabilities,
        )
        .map_err(|error| ProviderError::invalid_request(self.label(), error.to_string()))
    }

    /// The ChatGPT Subscription Responses endpoint is streaming-only. Collect
    /// its canonical event stream for callers of the provider's completion
    /// interface instead of maintaining a second, unsupported wire path.
    async fn collect_streaming_completion(
        &self,
        request: ModelRequest,
    ) -> Result<muta_contracts::ProviderCompletion, ProviderError> {
        let mut stream = self.stream_chat_events(request).await?;
        let mut streamed_usage = None;
        let mut completion_meta = None;

        while let Some(event) = stream.next().await {
            let event = event?;
            match event {
                ProviderStreamEvent::Usage(usage) => streamed_usage = Some(usage),
                ProviderStreamEvent::Completed(mut meta) => {
                    if completion_meta.is_some() {
                        return Err(ProviderError::protocol(
                            self.label(),
                            "Responses stream emitted more than one terminal completion event.",
                        ));
                    }
                    if meta.usage.is_none() {
                        meta.usage = streamed_usage;
                    }
                    completion_meta = Some(meta);
                }
                ProviderStreamEvent::ModelCatalogEtag(_)
                | ProviderStreamEvent::TextDelta(_)
                | ProviderStreamEvent::ReasoningDelta(_)
                | ProviderStreamEvent::ToolCallDelta { .. } => {
                    if completion_meta.is_some() {
                        return Err(ProviderError::protocol(
                            self.label(),
                            "Responses stream emitted data after its terminal completion event.",
                        ));
                    }
                }
            }
        }

        let meta = completion_meta.ok_or_else(|| {
            ProviderError::protocol(
                self.label(),
                "Responses stream ended without a terminal completion event.",
            )
        })?;
        let output = meta
            .artifacts
            .as_ref()
            .and_then(|artifacts| {
                artifacts.get(muta_contracts::OPENAI_RESPONSE_OUTPUT_ARTIFACT_KEY)
            })
            .filter(|output| output.as_array().is_some_and(|items| !items.is_empty()))
            .ok_or_else(|| {
                ProviderError::protocol(
                    self.label(),
                    "Responses stream completed without a valid output artifact.",
                )
            })?;
        let message = response::message(output);

        Ok(muta_contracts::ProviderCompletion { message, meta })
    }
}

#[async_trait]
impl Provider for OpenAiResponsesProvider {
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

    fn route_fingerprint(&self) -> muta_contracts::RouteFingerprint {
        muta_contracts::RouteFingerprint(format!(
            "openai-responses:{}:{}:{}",
            self.endpoint.base_url,
            self.endpoint.model,
            if self.store { "stored" } else { "local" }
        ))
    }

    fn continuation_mode(&self) -> muta_contracts::ContinuationMode {
        if self.store {
            muta_contracts::ContinuationMode::RemoteStored
        } else {
            muta_contracts::ContinuationMode::OpaqueReplay
        }
    }

    fn prompt_hints(&self) -> ProviderPromptHints {
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
        if self.dialect == muta_contracts::OpenAiResponsesDialect::ChatGpt {
            return self.collect_streaming_completion(request).await;
        }
        let label = self.label();
        let body = self.build_body(request, false)?;
        let resp = self.send_request(&body, false).await?;
        let value: serde_json::Value = decode_response_json(resp, label).await?;
        if let Some(err) = value.get("error") {
            return Err(ProviderError::new(
                label,
                ProviderErrorKind::Protocol,
                format!("{label} Error: {}", err),
            ));
        }
        let output = value
            .get("output")
            .and_then(serde_json::Value::as_array)
            .filter(|items| !items.is_empty())
            .ok_or_else(|| {
                ProviderError::protocol(label, "Responses completion contains no output items.")
            })?;
        let output = serde_json::Value::Array(output.clone());
        let mut artifacts = serde_json::Map::new();
        artifacts.insert(
            muta_contracts::OPENAI_RESPONSE_OUTPUT_ARTIFACT_KEY.to_string(),
            output.clone(),
        );
        let continuation =
            value["id"]
                .as_str()
                .map(|response_id| muta_contracts::ContinuationCursor {
                    route: self.route_fingerprint(),
                    local_head: String::new(),
                    response_id: response_id.to_string(),
                });
        Ok(muta_contracts::ProviderCompletion {
            message: response::message(&output),
            meta: muta_contracts::ProviderCompletionMeta {
                usage: response::usage(&value["usage"]),
                artifacts: Some(artifacts),
                continuation,
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
        let label = self.label();
        let body = self.build_body(request, true)?;
        let resp = self.send_request(&body, true).await?;
        let stream = crate::sse::data_payloads(resp, label).map(|item| {
            let data = item?;
            let value = decode_stream_payload(&data, label)?;
            // Accumulate only output_text deltas on the text-only path.
            if value["type"].as_str() == Some("response.output_text.delta") {
                Ok(value["delta"].as_str().unwrap_or("").to_string())
            } else {
                Ok(String::new())
            }
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
        let label = self.label();
        let body = self.build_body(request, true)?;
        let resp = self.send_request(&body, true).await?;
        let model_catalog_etag = models_etag(resp.headers());

        // One stateful parser threads the function-call item state across the
        // whole stream; each SSE payload becomes zero or more events. Terminal
        // usage arrives as a `Usage` event here, which the harness books
        // directly (mirrors the chat-completions streaming path) — no stashing
        // into the turn is needed.
        let parser = Arc::new(Mutex::new(response::ResponsesStream::new()));
        let route = self.route_fingerprint();
        let stream = crate::sse::data_payloads(resp, label).map(move |item| {
            let data = item?;
            let value = decode_stream_payload(&data, label)?;
            let mut p = parser.lock().unwrap_or_else(|e| e.into_inner());
            let mut events = p
                .parse_value(&value)
                .map_err(|error| ProviderError::protocol(label, error))?;
            for event in &mut events {
                if let ProviderStreamEvent::Completed(meta) = event
                    && let Some(artifacts) = meta.artifacts.as_mut()
                    && let Some(response_id) = artifacts
                        .remove(muta_contracts::OPENAI_RESPONSE_ID_ARTIFACT_KEY)
                        .and_then(|value| value.as_str().map(str::to_string))
                {
                    meta.continuation = Some(muta_contracts::ContinuationCursor {
                        route: route.clone(),
                        local_head: String::new(),
                        response_id,
                    });
                }
            }
            Ok::<_, ProviderError>(events)
        });
        let events = stream.flat_map(|result| match result {
            Ok(events) => futures::stream::iter(events.into_iter().map(Ok).collect::<Vec<_>>()),
            Err(error) => futures::stream::iter(vec![Err(error)]),
        });
        let controls = futures::stream::iter(
            model_catalog_etag
                .into_iter()
                .map(|etag| Ok(ProviderStreamEvent::ModelCatalogEtag(etag))),
        );
        Ok(controls.chain(events).boxed())
    }
}

#[cfg(test)]
mod stream_protocol_tests {
    use super::*;

    #[test]
    fn malformed_sse_payload_is_a_decode_error() {
        let error = decode_stream_payload("{", "ChatGPT").unwrap_err();
        assert_eq!(error.kind(), ProviderErrorKind::Decode);
    }

    #[test]
    fn terminal_sse_failures_are_not_silently_ignored() {
        for payload in [
            r#"{"type":"response.failed","response":{"error":{"message":"denied"}}}"#,
            r#"{"type":"error","error":{"message":"denied"}}"#,
        ] {
            let error = decode_stream_payload(payload, "ChatGPT").unwrap_err();
            assert_eq!(error.kind(), ProviderErrorKind::Protocol);
        }
    }

    #[test]
    fn server_is_overloaded_and_slow_down_are_classified_as_unavailable_and_retryable() {
        for payload in [
            r#"{"type":"error","error":{"code":"server_is_overloaded","message":"Our servers are currently overloaded. Please try again later.","type":"service_unavailable_error"}}"#,
            r#"{"type":"response.failed","response":{"error":{"code":"slow_down","message":"Please slow down."}}}"#,
        ] {
            let error = decode_stream_payload(payload, "ChatGPT").unwrap_err();
            assert_eq!(error.kind(), ProviderErrorKind::Unavailable);
            assert_eq!(error.status(), Some(503));
            assert!(matches!(
                error.retry_disposition(),
                muta_contracts::RetryDisposition::Retry { .. }
            ));
        }
    }

    #[test]
    fn rate_limit_with_parsed_duration_is_classified_as_rate_limited_and_retryable() {
        let payload = r#"{"type":"response.failed","response":{"error":{"code":"rate_limit_exceeded","message":"Rate limit reached. Please try again in 11.054s."}}}"#;
        let error = decode_stream_payload(payload, "ChatGPT").unwrap_err();
        assert_eq!(error.kind(), ProviderErrorKind::RateLimited);
        assert_eq!(error.status(), Some(429));
        assert_eq!(
            error.retry_disposition(),
            muta_contracts::RetryDisposition::Retry {
                retry_after_ms: Some(11054)
            }
        );
    }

    #[test]
    fn context_length_exceeded_is_classified_as_context_overflow() {
        let payload = r#"{"type":"response.failed","response":{"error":{"code":"context_length_exceeded","message":"Your input exceeds the context window."}}}"#;
        let error = decode_stream_payload(payload, "ChatGPT").unwrap_err();
        assert_eq!(error.kind(), ProviderErrorKind::ContextOverflow);
        assert_eq!(error.status(), Some(400));
    }

    #[test]
    fn models_etag_header_becomes_a_catalog_control_event() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("X-Models-Etag", "  etag-42  ".parse().unwrap());
        assert_eq!(models_etag(&headers).as_deref(), Some("etag-42"));
    }
}
