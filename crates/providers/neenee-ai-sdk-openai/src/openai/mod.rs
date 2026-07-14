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
//!   - this file — the [`OpenAiCompatProvider`] executor + `Provider` impl

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use neenee_core::{Effort, ModelRequest, Provider, ProviderPromptHints, ProviderStreamEvent};
use std::sync::Arc;
use std::sync::Mutex;

use neenee_ai_sdk_core::{Endpoint, TurnState};
use neenee_ai_sdk_core::{decode_response_json, ensure_success, transport_error};

pub mod echo;
pub mod request;
pub mod response;

/// OpenAI-compatible chat-completions provider.
///
/// Embeds the shared [`Endpoint`] (connection config) and [`TurnState`] (tool
/// schemas + last usage) plus the optional OpenAI `reasoning_effort` override.
pub struct OpenAiCompatProvider {
    pub endpoint: Endpoint,
    pub turn: TurnState,
    pub reasoning_effort: Option<Effort>,
}

impl OpenAiCompatProvider {
    pub fn new(api_key: String, model: String) -> Self {
        Self::with_base_url(api_key, model, "https://api.openai.com/v1/chat/completions")
    }

    pub fn with_base_url(api_key: String, model: String, base_url: &str) -> Self {
        Self::with_base_url_and_user_agent(
            api_key,
            model,
            base_url,
            neenee_ai_sdk_core::NEENEE_USER_AGENT,
        )
    }

    pub fn with_base_url_and_user_agent(
        api_key: String,
        model: String,
        base_url: &str,
        user_agent: &str,
    ) -> Self {
        Self {
            endpoint: Endpoint {
                api_key,
                model,
                base_url: base_url.to_string(),
                user_agent: user_agent.to_string(),
                id: "openai".to_string(),
            },
            turn: TurnState::new(),
            reasoning_effort: None,
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

    // Accessors (base_url / model_id / user_agent / api_key / id) are forwarded
    // from the embedded [`Endpoint`]; see `self.endpoint.*`.

    /// Apply the per-request auth + user-agent headers to a request builder.
    fn build_request(
        &self,
        client: &reqwest::Client,
        body: &serde_json::Value,
    ) -> reqwest::RequestBuilder {
        let mut req = client
            .post(self.endpoint.base_url())
            .header(reqwest::header::USER_AGENT, self.endpoint.user_agent())
            .json(body);
        for (name, value) in request::headers(self.endpoint.api_key()) {
            req = req.header(name, value);
        }
        req
    }
}

#[async_trait]
impl Provider for OpenAiCompatProvider {
    fn provider_id(&self) -> String {
        self.endpoint.id.clone()
    }

    fn model(&self) -> String {
        self.endpoint.model.clone()
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
        let client = reqwest::Client::new();
        let (messages, tool_specs) = request.into_parts();
        let body = request::body(
            messages,
            request::BodyInput {
                model: &self.endpoint.model,
                stream: false,
                tool_specs: (!tool_specs.is_empty()).then_some(tool_specs.as_slice()),
                reasoning_effort: self.reasoning_effort,
            },
        );

        let response = self
            .build_request(&client, &body)
            .send()
            .await
            .map_err(|error| transport_error("OpenAI", error))?;
        let response = ensure_success(response, "OpenAI").await?;
        let response_json: serde_json::Value = decode_response_json(response, "OpenAI").await?;

        if let Some(err) = response_json.get("error") {
            return Err(format!("OpenAI Error: {}", err));
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
        let client = reqwest::Client::new();
        let (messages, tool_specs) = request.into_parts();
        let body = request::body(
            messages,
            request::BodyInput {
                model: &self.endpoint.model,
                stream: true,
                tool_specs: (!tool_specs.is_empty()).then_some(tool_specs.as_slice()),
                reasoning_effort: self.reasoning_effort,
            },
        );

        let response = self
            .build_request(&client, &body)
            .send()
            .await
            .map_err(|error| transport_error("OpenAI", error))?;
        let response = ensure_success(response, "OpenAI").await?;

        let stream = neenee_ai_sdk_core::sse::data_payloads(response, "OpenAI").map(|item| {
            let data = item?;
            Ok(response::stream_text(&data))
        });

        Ok(stream.boxed())
    }

    async fn stream_chat_events(
        &self,
        request: ModelRequest,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent, String>>, String> {
        let client = reqwest::Client::new();
        let (messages, tool_specs) = request.into_parts();
        let body = request::body(
            messages,
            request::BodyInput {
                model: &self.endpoint.model,
                stream: true,
                tool_specs: (!tool_specs.is_empty()).then_some(tool_specs.as_slice()),
                reasoning_effort: self.reasoning_effort,
            },
        );

        let response = self
            .build_request(&client, &body)
            .send()
            .await
            .map_err(|error| transport_error("OpenAI", error))?;
        let response = ensure_success(response, "OpenAI").await?;

        // Tool-call echo filter shared between the body and the end-of-stream
        // flush: it suppresses any content that mirrors a native tool call
        // before it becomes a `TextDelta`. SSE byte reassembly (incl.
        // multi-byte UTF-8 split across chunks) is handled by
        // `sse::data_payloads`; each payload is then parsed into the OpenAI
        // event shape and fed through the echo filter.
        let echo_filter = Arc::new(Mutex::new(echo::ToolCallEchoFilter::new()));
        let filter_for_body = Arc::clone(&echo_filter);
        let body = neenee_ai_sdk_core::sse::data_payloads(response, "OpenAI").map(move |item| {
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
        let provider = OpenAiCompatProvider::new("test-key".to_string(), "test-model".to_string());
        // No protocol note: native tool calls are the wire default and the
        // ToolCallEchoFilter strips text-mirrored calls regardless.
        assert!(provider.prompt_hints().system_guidance.is_empty());
    }
}
