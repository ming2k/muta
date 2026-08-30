//! OpenAI **Responses** API provider — the wire format spoken by the ChatGPT
//! subscription backend (`chatgpt.com/backend-api/codex/responses`).
//!
//! Unlike the chat-completions [`OpenAiChatCompletionsProvider`](crate::OpenAiChatCompletionsProvider),
//! this provider:
//! - sends dynamic OAuth credentials or static tokens,
//! - attaches the optional `ChatGPT-Account-Id` header for ChatGPT subscriptions,
//! - builds a Responses request (`instructions` + `input` items) and parses
//!   `response.*` streaming events,
//! - supports self-healing reactive force-refresh on HTTP 401 Unauthorized for OAuth channels.

pub mod request;
pub mod response;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use muta_contracts::{
    CredentialSource, Effort, Message, ModelRequest, Provider, ProviderPromptHints,
    ProviderStreamEvent, ResolvedAuth,
};
use std::sync::{Arc, Mutex};

use crate::transport::{decode_response_json, ensure_success, transport_error};
use crate::{Client, Endpoint, TurnState};

/// OpenAI Responses-API provider (ChatGPT subscription backend).
pub struct OpenAiResponsesProvider {
    pub endpoint: Endpoint,
    pub turn: TurnState,
    pub reasoning_effort: Option<Effort>,
    /// Channel-scoped capability view. A trusted remote catalogue overrides the
    /// static baseline only for this provider/model route.
    pub capabilities: muta_contracts::ModelCapabilities,
    /// When `true`, attach ChatGPT subscription headers (`originator: muta` and
    /// `ChatGPT-Account-Id`).
    pub chatgpt: bool,
    /// When `true`, inject GitHub Copilot's required per-request headers
    /// (`x-initiator`, `Openai-Intent`, `X-GitHub-Api-Version`, and
    /// `Copilot-Vision-Request` for vision turns).
    pub copilot: bool,
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
            turn: TurnState::new(),
            client: Client::new(),
            reasoning_effort: None,
            capabilities,
            chatgpt: false,
            copilot: false,
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
        capabilities: muta_contracts::ModelCapabilities,
    ) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Enable ChatGPT subscription headers (`originator: muta` and `ChatGPT-Account-Id`).
    pub fn with_chatgpt(mut self, chatgpt: bool) -> Self {
        self.chatgpt = chatgpt;
        self
    }

    /// Flip on Copilot-mode request headers.
    pub fn with_copilot(mut self, copilot: bool) -> Self {
        self.copilot = copilot;
        self
    }

    /// Human-readable backend label for error messages and logs.
    fn label(&self) -> &'static str {
        if self.copilot {
            "Copilot"
        } else if self.chatgpt {
            "ChatGPT"
        } else {
            "OpenAI Responses"
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
        let is_copilot_vision = self.copilot && request::has_input_image(body);
        for (name, value) in request::headers(
            auth.token.expose_secret(),
            auth.account_id.as_deref(),
            self.copilot,
            self.chatgpt,
        ) {
            req = req.header(name, value);
        }
        if is_copilot_vision {
            req = req.header("Copilot-Vision-Request", "true");
        }
        for (name, value) in self.endpoint.client_identity().headers() {
            if !self.copilot
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
    ) -> Result<reqwest::Response, String> {
        let auth = self.endpoint.resolve_auth().await?;
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

    fn effort(&self) -> Option<Effort> {
        self.reasoning_effort
    }

    fn model_capabilities(&self) -> muta_contracts::ModelCapabilities {
        self.capabilities.clone()
    }

    fn prompt_hints(&self) -> ProviderPromptHints {
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

    async fn chat(&self, request: ModelRequest) -> Result<Message, String> {
        let label = self.label();
        let body = self.build_body(request, false);
        let resp = self.send_request(&body, false).await?;
        let value: serde_json::Value = decode_response_json(resp, label).await?;
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
        let resp = self.send_request(&body, true).await?;

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
        let resp = self.send_request(&body, true).await?;

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
