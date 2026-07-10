//! OpenAI **Responses** API provider — the wire format spoken by the ChatGPT
//! subscription backend (`chatgpt.com/backend-api/codex/responses`).
//!
//! Unlike the chat-completions [`OpenAiCompatProvider`](crate::OpenAiCompatProvider),
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
use neenee_core::{Effort, Message, Provider, ProviderPromptHints, ProviderStreamEvent};
use std::sync::{Arc, Mutex};

use neenee_ai_sdk_core::{Endpoint, TurnState};
use neenee_ai_sdk_core::{decode_response_json, ensure_success, transport_error};

/// OpenAI Responses-API provider (ChatGPT subscription backend).
pub struct ResponsesProvider {
    pub endpoint: Endpoint,
    pub turn: TurnState,
    pub reasoning_effort: Option<Effort>,
    /// The ChatGPT account id, sent as `ChatGPT-Account-Id`. `None` is valid
    /// for single-account users (the header is simply omitted).
    pub account_id: Option<String>,
}

impl ResponsesProvider {
    pub fn new(
        access_token: String,
        model: String,
        base_url: &str,
        account_id: Option<String>,
    ) -> Self {
        Self {
            endpoint: Endpoint {
                api_key: access_token,
                model,
                base_url: base_url.to_string(),
                user_agent: neenee_ai_sdk_core::NEENEE_USER_AGENT.to_string(),
                id: "chatgpt".to_string(),
            },
            turn: TurnState::new(),
            reasoning_effort: None,
            account_id,
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

    /// Apply the per-request auth + user-agent headers.
    fn build_request(
        &self,
        client: &reqwest::Client,
        body: &serde_json::Value,
    ) -> reqwest::RequestBuilder {
        let mut req = client
            .post(self.endpoint.base_url())
            .header(reqwest::header::USER_AGENT, self.endpoint.user_agent())
            .json(body);
        for (name, value) in request::headers(self.endpoint.api_key(), self.account_id.as_deref()) {
            req = req.header(name, value);
        }
        req
    }

    fn build_body(&self, messages: Vec<Message>, stream: bool) -> serde_json::Value {
        self.turn.with_tool_schemas(|tool_specs| {
            request::body(
                messages,
                request::BodyInput {
                    model: &self.endpoint.model,
                    stream,
                    tool_specs,
                    reasoning_effort: self.reasoning_effort,
                },
            )
        })
    }
}

#[async_trait]
impl Provider for ResponsesProvider {
    fn prepare_tools(&self, tools: &[Arc<dyn neenee_core::Tool>]) {
        self.turn.prepare(tools);
    }

    fn provider_id(&self) -> String {
        self.endpoint.id.clone()
    }

    fn model(&self) -> String {
        self.endpoint.model.clone()
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

    fn take_last_usage(&self) -> Option<neenee_core::TokenUsage> {
        self.turn.take_usage()
    }

    async fn chat(&self, messages: Vec<Message>) -> Result<Message, String> {
        let client = reqwest::Client::new();
        let body = self.build_body(messages, false);
        let resp = self
            .build_request(&client, &body)
            .send()
            .await
            .map_err(|e| transport_error("ChatGPT", e))?;
        let resp = ensure_success(resp, "ChatGPT").await?;
        let value: serde_json::Value = decode_response_json(resp, "ChatGPT").await?;
        if let Some(err) = value.get("error") {
            return Err(format!("ChatGPT Error: {}", err));
        }
        if let Some(usage) = response::usage(&value["usage"]) {
            self.turn.stash_usage(usage);
        }
        Ok(response::message(&value["output"]))
    }

    async fn stream_chat(
        &self,
        messages: Vec<Message>,
    ) -> Result<BoxStream<'static, Result<String, String>>, String> {
        let client = reqwest::Client::new();
        let body = self.build_body(messages, true);
        let resp = self
            .build_request(&client, &body)
            .send()
            .await
            .map_err(|e| transport_error("ChatGPT", e))?;
        let resp = ensure_success(resp, "ChatGPT").await?;

        let stream = neenee_ai_sdk_core::sse::data_payloads(resp, "ChatGPT").map(|item| {
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
        messages: Vec<Message>,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent, String>>, String> {
        let client = reqwest::Client::new();
        let body = self.build_body(messages, true);
        let resp = self
            .build_request(&client, &body)
            .send()
            .await
            .map_err(|e| transport_error("ChatGPT", e))?;
        let resp = ensure_success(resp, "ChatGPT").await?;

        // One stateful parser threads the function-call item state across the
        // whole stream; each SSE payload becomes zero or more events. Terminal
        // usage arrives as a `Usage` event here, which the harness books
        // directly (mirrors the chat-completions streaming path) — no stashing
        // into the turn is needed.
        let parser = Arc::new(Mutex::new(response::ResponsesStream::new()));
        let stream = neenee_ai_sdk_core::sse::data_payloads(resp, "ChatGPT").map(move |item| {
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
