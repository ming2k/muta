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

use crate::transport::{decode_response_json, ensure_success, transport_error};
use crate::{Client, Endpoint};

pub mod echo;
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
        user_agent: &str,
    ) -> Self {
        let capabilities = muta_contracts::ModelCapabilities::for_channel(&model, None);
        Self {
            endpoint: Endpoint::with_credentials(credentials, model, base_url, "openai")
                .with_user_agent(user_agent),
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
        if self.dialect == muta_contracts::OpenAiChatDialect::Copilot {
            "Copilot"
        } else {
            "OpenAI"
        }
    }

    /// Apply the per-request auth + user-agent headers to a request builder.
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
        let copilot = self.dialect == muta_contracts::OpenAiChatDialect::Copilot;
        for (name, value) in request::headers(auth.token.expose_secret(), copilot) {
            req = req.header(name, value);
        }
        for (name, value) in self.endpoint.client_identity().headers() {
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
            if let Ok(refreshed_auth) = self.endpoint.force_refresh_auth().await {
                let mut retry_req = self.build_request_for_auth(body, &refreshed_auth);
                if !is_stream {
                    retry_req = retry_req.timeout(self.client.request_timeout());
                }
                if let Ok(retried_resp) = retry_req.send().await {
                    return ensure_success(retried_resp, self.label()).await;
                }
            }
        }

        ensure_success(response, self.label()).await
    }
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
        let (messages, tool_specs) = request.into_parts();
        let body = request::body_with_capabilities(
            messages,
            request::BodyInput {
                model: &self.endpoint.model,
                stream: false,
                tool_specs: (!tool_specs.is_empty()).then_some(tool_specs.as_slice()),
                reasoning_effort: self.reasoning_effort,
                cache_plan: &cache_plan,
            },
            &self.capabilities,
        );

        let label = self.label();
        let resp = self.send_request(&body, false).await?;
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
        let (messages, tool_specs) = request.into_parts();
        let body = request::body_with_capabilities(
            messages,
            request::BodyInput {
                model: &self.endpoint.model,
                stream: true,
                tool_specs: (!tool_specs.is_empty()).then_some(tool_specs.as_slice()),
                reasoning_effort: self.reasoning_effort,
                cache_plan: &cache_plan,
            },
            &self.capabilities,
        );

        let response = self.send_request(&body, true).await?;

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
        let (messages, tool_specs) = request.into_parts();
        let body = request::body_with_capabilities(
            messages,
            request::BodyInput {
                model: &self.endpoint.model,
                stream: true,
                tool_specs: (!tool_specs.is_empty()).then_some(tool_specs.as_slice()),
                reasoning_effort: self.reasoning_effort,
                cache_plan: &cache_plan,
            },
            &self.capabilities,
        );

        let response = self.send_request(&body, true).await?;

        // Tool-call echo filter shared between the body and the end-of-stream
        // flush: it suppresses any content that mirrors a native tool call
        // before it becomes a `TextDelta`. SSE byte reassembly (incl.
        // multi-byte UTF-8 split across chunks) is handled by
        // `sse::data_payloads`; each payload is then parsed into the OpenAI
        // event shape and fed through the echo filter.
        let echo_filter = Arc::new(Mutex::new(echo::ToolCallEchoFilter::new()));
        let filter_for_body = Arc::clone(&echo_filter);
        let label = self.label();
        let body = crate::sse::data_payloads(response, label).map(move |item| {
            let data = item?;
            if serde_json::from_str::<serde_json::Value>(&data).is_err() {
                return Err(ProviderError::new(
                    label,
                    ProviderErrorKind::Decode,
                    "Invalid JSON in stream payload",
                ));
            }
            let parsed = response::stream_events(&data);
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
            events.push(Ok(ProviderStreamEvent::Completed(
                muta_contracts::ProviderCompletionMeta::default(),
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

    // --- resolved-variant schema reaches the request body ---

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
        static DEFAULT_CACHE_PLAN: muta_contracts::ResolvedCachePlan =
            muta_contracts::ResolvedCachePlan::Unsupported;
        request::body(
            messages,
            request::BodyInput {
                model: "test-model",
                stream: false,
                tool_specs: Some(&tool_specs),
                reasoning_effort: None,
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
}
