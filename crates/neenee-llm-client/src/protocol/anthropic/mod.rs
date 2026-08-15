//! Anthropic-compatible `/messages` provider with native tool-call support.
//!
//! A thin executor over the pure [`request`], [`response`], [`signature`], and
//! [`thinking`] layers plus the shared transport helpers. The provider struct
//! holds the shared [`Endpoint`] (connection config), the shared [`TurnState`]
//! (tool schemas + last usage), and only two Anthropic-unique fields:
//! `max_tokens` (the Messages API requires it) and `thinking` (the resolved
//! reasoning config). Everything else — body construction, header assembly,
//! cache breakpoints, thinking stamps, response/stream parsing — lives in a
//! pure, independently testable module.
//!
//! Module layout (mirrors the Google and OpenAI providers):
//! - [`request`] — body / headers / cache-breakpoint + thinking stamping (pure)
//! - [`response`] — usage, message assembly, stream-payload parsing (pure)
//! - [`signature`] — thinking-signature fragment accumulator (stateful, no I/O)
//! - [`thinking`] — the resolved `ThinkingConfig` knobs
//! - this file — the [`AnthropicMessagesProvider`] executor + `Provider` impl

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::BoxStream;
use neenee_contracts::{Message, ModelRequest, Provider, ProviderPromptHints, ProviderStreamEvent};
use std::sync::Arc;

use crate::{Client, Endpoint, TurnState};

pub mod request;
pub mod response;
pub mod signature;
pub mod thinking;

// Re-export the model-capability enums so callers reaching them through the
// provider crate keep a stable path. They live in `neenee-contracts` because they
// are model capabilities, not transport details.
pub use neenee_contracts::effort::Effort;
pub use neenee_contracts::{ThinkingMode, ThinkingSupport};
pub use thinking::ThinkingConfig;

/// Anthropic-compatible `/messages` provider.
///
/// Embeds the shared [`Endpoint`] and [`TurnState`], plus the two
/// Anthropic-unique fields: `max_tokens` (the Messages API requires it) and
/// `thinking` (the resolved reasoning/effort config).
pub struct AnthropicMessagesProvider {
    pub endpoint: Endpoint,
    pub turn: TurnState,
    /// Pooled HTTP client reused across every request this provider makes.
    pub client: Client,
    /// `max_tokens` sent on every `/messages` request. The Messages API
    /// requires this field; it caps the response length.
    pub max_tokens: u32,
    /// Resolved thinking/effort knobs stamped onto every request body.
    pub thinking: ThinkingConfig,
    /// Channel-scoped capability view. A trusted remote catalogue overrides the
    /// static baseline only for this provider/model route.
    pub capabilities: neenee_contracts::ModelCapabilities,
    /// Use GitHub Copilot's bearer authentication and client headers for its
    /// `/v1/messages` adapter instead of stock Anthropic API-key headers.
    pub copilot: bool,
    /// Stash for the signature of the most recent assistant `thinking` block,
    /// accumulated across SSE chunks (streaming) or read once (non-streaming),
    /// drained into the message's `provider_meta` for the next replay.
    pub last_thinking_signature: Arc<signature::SignatureStash>,
}

impl AnthropicMessagesProvider {
    pub fn new(api_key: String, model: String, base_url: &str) -> Self {
        Self::with_base_url_and_user_agent(api_key, model, base_url, crate::NEENEE_USER_AGENT)
    }

    /// Build a provider targeting a custom `/messages` base URL with the
    /// default `User-Agent`.
    pub fn with_base_url(api_key: String, model: String, base_url: &str) -> Self {
        Self::with_base_url_and_user_agent(api_key, model, base_url, crate::NEENEE_USER_AGENT)
    }

    /// Build a provider targeting a custom `/messages` base URL with an
    /// explicit `User-Agent`.
    pub fn with_base_url_and_user_agent(
        api_key: String,
        model: String,
        base_url: &str,
        user_agent: &str,
    ) -> Self {
        // Default the thinking/effort config to opt-in off (ADR-0046).
        let thinking = ThinkingConfig::for_model(&neenee_contracts::model::resolve(&model));
        let capabilities = neenee_contracts::ModelCapabilities::for_channel(&model, None);
        Self {
            endpoint: Endpoint {
                api_key,
                model,
                base_url: base_url.to_string(),
                user_agent: user_agent.to_string(),
                id: "anthropic".to_string(),
            },
            turn: TurnState::new(),
            client: Client::new(),
            max_tokens: 8192,
            thinking,
            capabilities,
            copilot: false,
            last_thinking_signature: signature::SignatureStash::shared(),
        }
    }

    /// Set the `max_tokens` sent on every `/messages` request.
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    /// Override the thinking/effort configuration.
    pub fn with_thinking(mut self, thinking: ThinkingConfig) -> Self {
        self.thinking = thinking;
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

    /// Enable GitHub Copilot's Anthropic Messages adapter headers.
    pub fn with_copilot(mut self, copilot: bool) -> Self {
        self.copilot = copilot;
        self
    }

    /// Stamp the attribution id. Returns `self` for chaining.
    pub fn with_id(mut self, id: String) -> Self {
        self.endpoint.set_id(id);
        self
    }

    /// Apply the per-request auth + version + beta headers to a request builder.
    fn build_request(&self, body: &serde_json::Value) -> reqwest::RequestBuilder {
        let mut req = self
            .client
            .http()
            .post(self.endpoint.base_url())
            .header(reqwest::header::USER_AGENT, self.endpoint.user_agent())
            .json(body);
        for (name, value) in request::headers(
            self.endpoint.api_key(),
            &self.capabilities,
            self.thinking,
            self.copilot,
        ) {
            req = req.header(name, value);
        }
        req
    }
}

#[async_trait]
impl Provider for AnthropicMessagesProvider {
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
        // No protocol hint: thinking signatures are carried as opaque
        // `provider_meta` and replayed only into the wire `thinking` block's
        // `signature` field — they never enter any content channel the model
        // can read, so there is nothing for a prompt note to guard against.
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

    fn take_last_provider_meta(&self) -> Option<serde_json::Map<String, serde_json::Value>> {
        // Drain the thinking signature accumulated during the last turn into
        // the provider-opaque sidecar the harness stamps on the assistant
        // message.
        self.last_thinking_signature.take().map(|sig| {
            let mut map = serde_json::Map::new();
            map.insert(
                "thinking_signature".to_string(),
                serde_json::Value::String(sig),
            );
            map
        })
    }

    async fn chat(&self, request: ModelRequest) -> Result<Message, String> {
        let (messages, tool_specs) = request.into_parts();
        let body = request::body_with_capabilities(
            messages,
            request::BodyInput {
                model: &self.endpoint.model,
                stream: false,
                tool_specs: (!tool_specs.is_empty()).then_some(tool_specs.as_slice()),
                max_tokens: self.max_tokens,
                thinking: self.thinking,
            },
            &self.capabilities,
        );

        let response_json: serde_json::Value = self
            .client
            .send_json(self.build_request(&body), "Anthropic")
            .await?;

        let assembled = response::assemble_message(&response_json)?;

        if let Some(usage) = response::usage(&response_json["usage"]) {
            self.turn.stash_usage(usage);
        }
        if let Some(sig) = assembled.thinking_signature.clone() {
            self.last_thinking_signature.set(sig);
        }

        Ok(response::into_message(assembled))
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
                max_tokens: self.max_tokens,
                thinking: self.thinking,
            },
            &self.capabilities,
        );

        let response = self
            .client
            .send(self.build_request(&body), "Anthropic")
            .await?;

        // Reuse the shared SSE byte reassembly; each payload is one Anthropic
        // event JSON. Map to text deltas only (this is the simple stream path).
        let stream = crate::sse::data_payloads(response, "Anthropic")
            .map(|item| item.map(|payload| response::stream_text(&payload)));
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
                max_tokens: self.max_tokens,
                thinking: self.thinking,
            },
            &self.capabilities,
        );

        let response = self
            .client
            .send(self.build_request(&body), "Anthropic")
            .await?;

        // Side-channel: hoover up signature fragments before the typed parser
        // (which ignores `signature_delta`), so the assembled assistant turn can
        // carry the full signature in `provider_meta` for the next replay.
        let sig_stash = self.last_thinking_signature.clone();
        // Usage arrives split across `message_start` (input + cache counters)
        // and `message_delta` (output); the accumulator merges them so the
        // emitted Usage events carry the full counts.
        let mut usage_state = response::StreamUsage::default();
        let stream = crate::sse::data_payloads(response, "Anthropic").flat_map(move |item| {
            let events: Vec<Result<ProviderStreamEvent, String>> = match item {
                Ok(payload) => {
                    sig_stash.capture(&payload);
                    match response::stream_events(&payload, &mut usage_state) {
                        Ok(parsed) => parsed.into_iter().map(Ok).collect(),
                        Err(e) => vec![Err(e)],
                    }
                }
                Err(e) => vec![Err(e)],
            };
            futures::stream::iter(events)
        });
        Ok(stream.boxed())
    }
}

#[cfg(test)]
mod tests;
