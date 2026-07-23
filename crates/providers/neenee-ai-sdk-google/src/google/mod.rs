//! Google Gemini native provider (REST to the versioned Gemini base).
//!
//! A thin executor over the pure [`request`] and [`response`] layers plus the
//! shared transport helpers. Gemini's transport is distinctive in two ways:
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
use neenee_core::{Message, ModelRequest, Provider, ProviderPromptHints, ProviderStreamEvent};
use serde_json::{Map, Value};
use std::sync::{Arc, Mutex};

use neenee_ai_sdk_core::{Endpoint, TurnState};
use neenee_ai_sdk_core::{decode_response_json, ensure_success, transport_error};

pub mod request;
pub mod response;

/// Official Gemini REST base, versioned. The provider appends the per-call
/// model path (`/models/{id}:generateContent` / `:streamGenerateContent`), so a
/// 中转站/relay overrides this with its own host carrying the `/v1beta` prefix.
pub const GOOGLE_DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Google Gemini native provider.
///
/// Embeds the shared [`Endpoint`] (connection config) and [`TurnState`] (tool
/// schemas + last usage). The provider holds no wire-format-unique fields:
/// Gemini's transport differences (key as query param, versioned base, native
/// function declarations) are confined to [`request`] / [`response`].
pub struct GoogleProvider {
    pub endpoint: Endpoint,
    pub turn: TurnState,
    /// Stash for Gemini thought signatures attached to streamed function-call
    /// parts, drained into `provider_meta` for exact stateless replay.
    pub last_thought_signatures: Arc<Mutex<Map<String, Value>>>,
    /// Stash for the latest streamed text-part thought signature.
    pub last_text_thought_signature: Arc<Mutex<Option<String>>>,
    /// Channel-scoped capability view. A trusted remote catalogue overrides the
    /// static baseline only for this provider/model route.
    pub capabilities: neenee_core::ModelCapabilities,
}

impl GoogleProvider {
    pub fn new(api_key: String, model: String) -> Self {
        Self::with_base_url(api_key, model, GOOGLE_DEFAULT_BASE_URL)
    }

    /// Build a provider targeting a custom versioned base URL (e.g. a
    /// Gemini-format relay). A trailing slash on `base_url` is tolerated
    /// (stripped).
    pub fn with_base_url(api_key: String, model: String, base_url: &str) -> Self {
        Self::with_base_url_and_user_agent(
            api_key,
            model,
            base_url,
            neenee_ai_sdk_core::NEENEE_USER_AGENT,
        )
    }

    /// Build a provider targeting a custom versioned base URL with an explicit
    /// `User-Agent`. A trailing slash on `base_url` is tolerated (stripped).
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
                base_url: base_url.trim_end_matches('/').to_string(),
                user_agent: user_agent.to_string(),
                id: "google".to_string(),
            },
            turn: TurnState::new(),
            last_thought_signatures: Arc::new(Mutex::new(Map::new())),
            last_text_thought_signature: Arc::new(Mutex::new(None)),
            capabilities,
        }
    }

    /// Set the attribution id (provider/solution id) so assistant responses are
    /// attributed to the logical model.
    pub fn with_id(mut self, id: String) -> Self {
        self.endpoint.set_id(id);
        self
    }

    /// Attach the effective provider-channel capability view.
    pub fn with_model_capabilities(mut self, capabilities: neenee_core::ModelCapabilities) -> Self {
        self.capabilities = capabilities;
        self
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

    fn model_capabilities(&self) -> neenee_core::ModelCapabilities {
        self.capabilities.clone()
    }

    fn prompt_hints(&self) -> ProviderPromptHints {
        // No protocol hint: Gemini's wire surface uses native function calls by
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

    fn take_last_usage(&self) -> Option<neenee_core::TokenUsage> {
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
        let client = reqwest::Client::new();
        let url = request::url(
            &self.endpoint.base_url,
            &self.endpoint.model,
            &self.endpoint.api_key,
        );

        let include_thoughts = self.capabilities.reasoning();
        let (messages, tool_specs) = request.into_parts();
        let body = request::body(
            messages,
            request::BodyInput {
                tool_specs: (!tool_specs.is_empty()).then_some(tool_specs.as_slice()),
                include_thoughts,
            },
        );

        let response = client
            .post(&url)
            .header("User-Agent", &self.endpoint.user_agent)
            .json(&body)
            .send()
            .await
            .map_err(|error| transport_error("Gemini", error))?;
        let response = ensure_success(response, "Gemini").await.map_err(|e| {
            response::clarify_error(e, &self.endpoint.model, &self.endpoint.base_url)
        })?;

        let response_json: serde_json::Value = decode_response_json(response, "Gemini").await?;

        if let Some(err) = response_json.get("error") {
            return Err(response::clarify_error(
                format!("Gemini Error: {}", err),
                &self.endpoint.model,
                &self.endpoint.base_url,
            ));
        }

        if let Some(usage) = response::usage(&response_json["usageMetadata"]) {
            self.turn.stash_usage(usage);
        }

        response::message(&response_json)
    }

    async fn stream_chat(
        &self,
        request: ModelRequest,
    ) -> Result<BoxStream<'static, Result<String, String>>, String> {
        let client = reqwest::Client::new();
        let url = request::stream_url(
            &self.endpoint.base_url,
            &self.endpoint.model,
            &self.endpoint.api_key,
        );

        let include_thoughts = self.capabilities.reasoning();
        let (messages, tool_specs) = request.into_parts();
        let body = request::body(
            messages,
            request::BodyInput {
                tool_specs: (!tool_specs.is_empty()).then_some(tool_specs.as_slice()),
                include_thoughts,
            },
        );

        let response = client
            .post(&url)
            .header("User-Agent", &self.endpoint.user_agent)
            .json(&body)
            .send()
            .await
            .map_err(|error| transport_error("Gemini", error))?;
        let response = ensure_success(response, "Gemini").await.map_err(|e| {
            response::clarify_error(e, &self.endpoint.model, &self.endpoint.base_url)
        })?;

        // SSE byte reassembly (incl. multi-byte UTF-8 split across chunks) is
        // handled by `sse::data_payloads`; here we only map each payload to the
        // Gemini `streamGenerateContent` text shape.
        let stream = neenee_ai_sdk_core::sse::data_payloads(response, "Gemini")
            .map(|item| item.map(|payload| response::stream_text(&payload)));

        Ok(stream.boxed())
    }

    async fn stream_chat_events(
        &self,
        request: ModelRequest,
    ) -> Result<BoxStream<'static, Result<ProviderStreamEvent, String>>, String> {
        let client = reqwest::Client::new();
        let url = request::stream_url(
            &self.endpoint.base_url,
            &self.endpoint.model,
            &self.endpoint.api_key,
        );

        let include_thoughts = self.capabilities.reasoning();
        let (messages, tool_specs) = request.into_parts();
        let body = request::body(
            messages,
            request::BodyInput {
                tool_specs: (!tool_specs.is_empty()).then_some(tool_specs.as_slice()),
                include_thoughts,
            },
        );

        let response = client
            .post(&url)
            .header("User-Agent", &self.endpoint.user_agent)
            .json(&body)
            .send()
            .await
            .map_err(|error| transport_error("Gemini", error))?;
        let response = ensure_success(response, "Gemini").await.map_err(|e| {
            response::clarify_error(e, &self.endpoint.model, &self.endpoint.base_url)
        })?;

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
        let stream = neenee_ai_sdk_core::sse::data_payloads(response, "Gemini").flat_map({
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
                                .unwrap_or_else(|e| e.into_inner()) = Some(signature);
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

        Ok(stream.boxed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_constructor_targets_official_base() {
        // `new` resolves the official versioned base; the per-call path is
        // appended at request time, not stored on the base.
        let p = GoogleProvider::new("k".to_string(), "gemini-2.5-flash".to_string());
        assert_eq!(
            p.endpoint.base_url(),
            "https://generativelanguage.googleapis.com/v1beta"
        );
        assert_eq!(
            p.endpoint.user_agent(),
            neenee_ai_sdk_core::NEENEE_USER_AGENT
        );
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
}
