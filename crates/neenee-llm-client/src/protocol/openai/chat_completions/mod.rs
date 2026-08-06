//! OpenAI-compatible chat-completions provider with native tool-call support
//! and a streaming filter that strips tool-call "echo" text (GLM/Qwen style).
//!
//! A thin executor over the pure [`request`], [`response`], and [`echo`]
//! layers plus the shared transport helpers. The provider struct holds only
//! the shared [`Endpoint`] (connection config) and [`TurnState`] (tool schemas
//! and last usage) — every wire-format detail lives in a pure, independently
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
use neenee_core::{Effort, ModelRequest, Provider, ProviderPromptHints, ProviderStreamEvent};
use std::sync::Arc;
use std::sync::Mutex;

use crate::{Client, Endpoint, TurnState};

pub mod echo;
pub mod request;
pub mod response;

/// OpenAI-compatible chat-completions provider.
///
/// Embeds the shared [`Endpoint`] (connection config) and [`TurnState`] (tool
/// schemas + last usage) plus the optional OpenAI `reasoning_effort` override.
pub struct OpenAiChatCompletionsProvider {
    pub endpoint: Endpoint,
    pub turn: TurnState,
    pub reasoning_effort: Option<Effort>,
    /// Optional session-scoped prompt-cache key (Moonshot / Kimi). Set once per
    /// session via [`with_prompt_cache_key`](Self::with_prompt_cache_key); when
    /// present, every request carries `prompt_cache_key` so the server-side
    /// cache namespaces per session and repeated prefixes hit at a discount.
    /// Resolved from the model's [`neenee_core::CachePolicy`] by the registry.
    pub prompt_cache_key: Option<String>,
    /// Channel-scoped capability view. A trusted remote catalogue overrides the
    /// static baseline only for this provider/model route.
    pub capabilities: neenee_core::ModelCapabilities,
    /// When `true`, inject GitHub Copilot's required per-request headers
    /// (`x-initiator`, `Openai-Intent`, `X-GitHub-Api-Version`) in addition to
    /// the bearer. Flipped on by the catalog for Copilot OAuth channels that
    /// speak the chat-completions surface (the GPT-4o family and Copilot Free
    /// accounts, which do not have Responses-API access). Mirrors the same flag
    /// on [`OpenAiResponsesProvider`](crate::OpenAiResponsesProvider).
    pub copilot: bool,
    /// Pooled HTTP client reused across every request this provider makes.
    pub client: Client,
}

impl OpenAiChatCompletionsProvider {
    pub fn new(api_key: String, model: String) -> Self {
        Self::with_base_url(api_key, model, "https://api.openai.com/v1/chat/completions")
    }

    pub fn with_base_url(api_key: String, model: String, base_url: &str) -> Self {
        Self::with_base_url_and_user_agent(api_key, model, base_url, crate::NEENEE_USER_AGENT)
    }

    pub fn with_base_url_and_user_agent(
        api_key: String,
        model: String,
        base_url: &str,
        user_agent: &str,
    ) -> Self {
        let capabilities = neenee_core::ModelCapabilities::for_channel(&model, None);
        Self {
            endpoint: Endpoint {
                api_key,
                model,
                base_url: base_url.to_string(),
                user_agent: user_agent.to_string(),
                id: "openai".to_string(),
            },
            turn: TurnState::new(),
            client: Client::new(),
            reasoning_effort: None,
            prompt_cache_key: None,
            capabilities,
            copilot: false,
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

    /// Set the session-scoped `prompt_cache_key` (Moonshot / Kimi). Typically the
    /// session id, so all turns in a session share a server-side cache namespace.
    /// Only takes effect for model families whose [`neenee_core::CachePolicy`] is
    /// [`SessionKey`](neenee_core::CachePolicy::SessionKey); the registry decides
    /// whether to set this. Returns `self` for chaining.
    pub fn with_prompt_cache_key(mut self, key: Option<String>) -> Self {
        self.prompt_cache_key = key;
        self
    }

    /// Attach the effective provider-channel capability view.
    pub fn with_model_capabilities(mut self, capabilities: neenee_core::ModelCapabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Flip on Copilot-mode request headers (see [`Self::copilot`]).
    pub fn with_copilot(mut self, copilot: bool) -> Self {
        self.copilot = copilot;
        self
    }

    /// Human-readable backend label for error messages and logs. The OpenAI
    /// chat-completions provider serves both the generic OpenAI-compatible
    /// surface and the GitHub Copilot chat surface behind one wire format;
    /// surfacing the right name in errors ("Copilot HTTP 400" vs "OpenAI HTTP
    /// 400") is essential for diagnosing which backend rejected a request.
    fn label(&self) -> &'static str {
        if self.copilot { "Copilot" } else { "OpenAI" }
    }

    // Accessors (base_url / model_id / user_agent / api_key / id) are forwarded
    // from the embedded [`Endpoint`]; see `self.endpoint.*`.

    /// Apply the per-request auth + user-agent headers to a request builder.
    fn build_request(&self, body: &serde_json::Value) -> reqwest::RequestBuilder {
        let mut req = self
            .client
            .http()
            .post(self.endpoint.base_url())
            .header(reqwest::header::USER_AGENT, self.endpoint.user_agent())
            .json(body);
        for (name, value) in request::headers(self.endpoint.api_key(), self.copilot) {
            req = req.header(name, value);
        }
        req
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

    fn model_capabilities(&self) -> neenee_core::ModelCapabilities {
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

    fn take_last_usage(&self) -> Option<neenee_core::TokenUsage> {
        self.turn.take_usage()
    }

    async fn chat(&self, request: ModelRequest) -> Result<neenee_core::Message, String> {
        let (messages, tool_specs) = request.into_parts();
        let body = request::body_with_capabilities(
            messages,
            request::BodyInput {
                model: &self.endpoint.model,
                stream: false,
                tool_specs: (!tool_specs.is_empty()).then_some(tool_specs.as_slice()),
                reasoning_effort: self.reasoning_effort,
                prompt_cache_key: self.prompt_cache_key.as_deref(),
            },
            &self.capabilities,
        );

        let label = self.label();
        let response_json: serde_json::Value = self
            .client
            .send_json(self.build_request(&body), label)
            .await?;

        if let Some(err) = response_json.get("error") {
            return Err(format!("{label} Error: {}", err));
        }

        if let Some(usage) = response::usage(&response_json["usage"]) {
            self.turn.stash_usage(usage);
        }

        let choice = &response_json["choices"][0]["message"];
        Ok(response::message(choice, |raw, had_native| {
            let emitted = echo::ToolCallEchoFilter::filter_content(raw, had_native);
            tracing::debug!(
                target: "neenee_core::provider",
                provider = %self.endpoint.id,
                model = %self.endpoint.model,
                raw_chars = raw.len(),
                emitted_chars = emitted.len(),
                suppressed_chars = raw.len().saturating_sub(emitted.len()),
                native_tool_calls = had_native,
                "openai chat echo summary",
            );
            emitted
        }))
    }

    async fn stream_chat(
        &self,
        request: ModelRequest,
    ) -> Result<BoxStream<'static, Result<String, String>>, String> {
        let (messages, tool_specs) = request.into_parts();
        let body = request::body_with_capabilities(
            messages,
            request::BodyInput {
                model: &self.endpoint.model,
                stream: true,
                tool_specs: (!tool_specs.is_empty()).then_some(tool_specs.as_slice()),
                reasoning_effort: self.reasoning_effort,
                prompt_cache_key: self.prompt_cache_key.as_deref(),
            },
            &self.capabilities,
        );

        let response = self
            .client
            .send(self.build_request(&body), self.label())
            .await?;

        let stream = crate::sse::data_payloads(response, self.label()).map(|item| {
            let data = item?;
            Ok(response::stream_text(&data))
        });

        Ok(stream.boxed())
    }

    async fn stream_chat_events(
        &self,
        request: ModelRequest,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent, String>>, String> {
        let (messages, tool_specs) = request.into_parts();
        let body = request::body_with_capabilities(
            messages,
            request::BodyInput {
                model: &self.endpoint.model,
                stream: true,
                tool_specs: (!tool_specs.is_empty()).then_some(tool_specs.as_slice()),
                reasoning_effort: self.reasoning_effort,
                prompt_cache_key: self.prompt_cache_key.as_deref(),
            },
            &self.capabilities,
        );

        let response = self
            .client
            .send(self.build_request(&body), self.label())
            .await?;

        // Tool-call echo filter shared between the body and the end-of-stream
        // flush: it suppresses any content that mirrors a native tool call
        // before it becomes a `TextDelta`. SSE byte reassembly (incl.
        // multi-byte UTF-8 split across chunks) is handled by
        // `sse::data_payloads`; each payload is then parsed into the OpenAI
        // event shape and fed through the echo filter.
        let echo_filter = Arc::new(Mutex::new(echo::ToolCallEchoFilter::new()));
        let filter_for_body = Arc::clone(&echo_filter);
        let body = crate::sse::data_payloads(response, self.label()).map(move |item| {
            let data = item?;
            let parsed = response::stream_events(&data);
            // Recover from a poisoned mutex: a prior panic in this critical
            // section must not take down subsequent stream chunks.
            let mut filter = filter_for_body
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let mut events: Vec<Result<ProviderStreamEvent, String>> = Vec::new();
            for event in parsed {
                events.extend(filter.observe(event).into_iter().map(Ok));
            }
            Ok::<_, String>(events)
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
                target: "neenee_core::provider",
                provider = %provider_id,
                model = %model,
                content_fed_chars = filter.fed_chars,
                content_emitted_chars = filter.emitted_chars,
                echo_suppressed_chars = filter.fed_chars.saturating_sub(filter.emitted_chars),
                reasoning_chars = filter.reasoning_chars,
                tool_call_deltas = filter.tool_call_deltas,
                "openai stream summary",
            );
            let events: Vec<Result<ProviderStreamEvent, String>> = if emitted.is_empty() {
                Vec::new()
            } else {
                vec![Ok(ProviderStreamEvent::TextDelta(emitted))]
            };
            Ok::<_, String>(events)
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
    use neenee_core::{Message, Role, Tool};

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
        request::body(
            messages,
            request::BodyInput {
                model: "test-model",
                stream: false,
                tool_specs: Some(&tool_specs),
                reasoning_effort: None,
                prompt_cache_key: None,
            },
        )
    }

    #[test]
    fn model_request_emits_the_selected_variants_schema() {
        // A `read_text` capability with two variants; the agent resolves a
        // selection before handing the toolset to the provider, so whichever
        // variant is selected is the one whose schema reaches the request body.
        let toolset = neenee_core::ToolSet::from_tools(vec![
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
        let mut selection = neenee_core::VariantSelection::new();
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
