//! OpenAI **Responses** API provider — the wire format spoken by the ChatGPT
//! subscription backend (`chatgpt.com/backend-api/codex/responses`).
//!
//! Unlike the chat-completions [`OpenAiChatCompletionsProvider`](crate::OpenAiChatCompletionsProvider),
//! this provider:
//! - sends the OAuth access token as the bearer (not an API key),
//! - attaches the optional `ChatGPT-Account-Id` header,
//! - builds a Responses request (`instructions` + `input` items) and parses
//!   `response.*` streaming events.
//!
//! The pure request/response mechanics live in [`request`] and [`response`];
//! this file is the executor + `Provider` impl.

pub mod request;
pub mod response;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use neenee_contracts::{
    Effort, Message, ModelRequest, Provider, ProviderPromptHints, ProviderStreamEvent,
};
use std::sync::{Arc, Mutex};

use crate::{Client, Endpoint, TurnState};

/// OpenAI Responses-API provider (ChatGPT subscription backend).
pub struct OpenAiResponsesProvider {
    pub endpoint: Endpoint,
    pub turn: TurnState,
    pub reasoning_effort: Option<Effort>,
    /// The ChatGPT account id, sent as `ChatGPT-Account-Id`. `None` is valid
    /// for single-account users (the header is simply omitted).
    pub account_id: Option<String>,
    /// Channel-scoped capability view. A trusted remote catalogue overrides the
    /// static baseline only for this provider/model route.
    pub capabilities: neenee_contracts::ModelCapabilities,
    /// When `true`, inject GitHub Copilot's required per-request headers
    /// (`x-initiator`, `Openai-Intent`, `X-GitHub-Api-Version`, and
    /// `Copilot-Vision-Request` for vision turns) instead of the ChatGPT
    /// account-id header. Flipped on by the catalog for Copilot OAuth channels.
    pub copilot: bool,
    /// Pooled HTTP client reused across every request this provider makes.
    pub client: Client,
}

impl OpenAiResponsesProvider {
    pub fn new(
        access_token: String,
        model: String,
        base_url: &str,
        account_id: Option<String>,
    ) -> Self {
        let capabilities = neenee_contracts::ModelCapabilities::for_channel(&model, None);
        Self {
            endpoint: Endpoint {
                api_key: access_token,
                model,
                base_url: base_url.to_string(),
                user_agent: crate::NEENEE_USER_AGENT.to_string(),
                id: "chatgpt".to_string(),
            },
            turn: TurnState::new(),
            client: Client::new(),
            reasoning_effort: None,
            account_id,
            capabilities,
            copilot: false,
        }
    }

    pub fn with_user_agent(mut self, user_agent: &str) -> Self {
        self.endpoint.user_agent = user_agent.to_string();
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
        capabilities: neenee_contracts::ModelCapabilities,
    ) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Flip on Copilot-mode request headers (see [`Self::copilot`]).
    pub fn with_copilot(mut self, copilot: bool) -> Self {
        self.copilot = copilot;
        self
    }

    /// Human-readable backend label for error messages and logs. The Responses
    /// provider serves two distinct backends behind one wire format — the
    /// ChatGPT subscription backend and the GitHub Copilot backend — and
    /// surfacing the right name in errors (e.g. "Copilot HTTP 400" vs
    /// "ChatGPT HTTP 400") is essential for diagnosing which one rejected a
    /// request.
    fn label(&self) -> &'static str {
        if self.copilot { "Copilot" } else { "ChatGPT" }
    }

    /// Apply the per-request auth + user-agent headers. In Copilot mode the
    /// header set is Copilot's (`x-initiator`, `Openai-Intent`,
    /// `X-GitHub-Api-Version`) and the ChatGPT account-id header is omitted.
    /// Copilot also requires `Copilot-Vision-Request: true` on any turn that
    /// carries an image, detected here by scanning the Responses `input` array
    /// for an `input_image` part.
    fn build_request(&self, body: &serde_json::Value) -> reqwest::RequestBuilder {
        let mut req = self
            .client
            .http()
            .post(self.endpoint.base_url())
            .header(reqwest::header::USER_AGENT, self.endpoint.user_agent())
            .json(body);
        let is_copilot_vision = self.copilot && request::has_input_image(body);
        for (name, value) in request::headers(
            self.endpoint.api_key(),
            self.account_id.as_deref(),
            self.copilot,
        ) {
            req = req.header(name, value);
        }
        if is_copilot_vision {
            req = req.header("Copilot-Vision-Request", "true");
        }
        req
    }

    fn build_body(&self, request: ModelRequest, stream: bool) -> serde_json::Value {
        let (messages, tool_specs) = request.into_parts();
        request::body_with_capabilities(
            messages,
            request::BodyInput {
                model: &self.endpoint.model,
                stream,
                tool_specs: (!tool_specs.is_empty()).then_some(tool_specs.as_slice()),
                reasoning_effort: self.reasoning_effort,
            },
            &self.capabilities,
        )
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

    fn model_capabilities(&self) -> neenee_contracts::ModelCapabilities {
        self.capabilities.clone()
    }

    fn prompt_hints(&self) -> ProviderPromptHints {
        // No protocol note: the Responses surface uses native function_call
        // items by construction.
        ProviderPromptHints {
            system_guidance: "",
        }
    }

    fn usage_supported(&self) -> bool {
        true
    }

    fn take_last_usage(&self) -> Option<neenee_contracts::TokenUsage> {
        self.turn.take_usage()
    }

    async fn chat(&self, request: ModelRequest) -> Result<Message, String> {
        let label = self.label();
        let body = self.build_body(request, false);
        let value: serde_json::Value = self
            .client
            .send_json(self.build_request(&body), label)
            .await?;
        if let Some(err) = value.get("error") {
            return Err(format!("{label} Error: {}", err));
        }
        if let Some(usage) = response::usage(&value["usage"]) {
            self.turn.stash_usage(usage);
        }
        Ok(response::message(&value["output"]))
    }

    async fn stream_chat(
        &self,
        request: ModelRequest,
    ) -> Result<BoxStream<'static, Result<String, String>>, String> {
        let label = self.label();
        let body = self.build_body(request, true);
        let resp = self.client.send(self.build_request(&body), label).await?;

        let stream = crate::sse::data_payloads(resp, label).map(|item| {
            let data = item?;
            let value: serde_json::Value = serde_json::from_str(&data).unwrap_or_default();
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
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent, String>>, String> {
        let label = self.label();
        let body = self.build_body(request, true);
        let resp = self.client.send(self.build_request(&body), label).await?;

        // One stateful parser threads the function-call item state across the
        // whole stream; each SSE payload becomes zero or more events. Terminal
        // usage arrives as a `Usage` event here, which the harness books
        // directly (mirrors the chat-completions streaming path) — no stashing
        // into the turn is needed.
        let parser = Arc::new(Mutex::new(response::ResponsesStream::new()));
        let stream = crate::sse::data_payloads(resp, label).map(move |item| {
            let data = item?;
            let mut p = parser.lock().unwrap_or_else(|e| e.into_inner());
            Ok::<_, String>(p.parse(&data))
        });
        Ok(stream
            .flat_map(|result| match result {
                Ok(events) => futures::stream::iter(events.into_iter().map(Ok).collect::<Vec<_>>()),
                Err(error) => futures::stream::iter(vec![Err(error)]),
            })
            .boxed())
    }
}
