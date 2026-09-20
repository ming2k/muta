//! Transport middleware pipeline for request transformation and stream framing (ADR-0267).
//!
//! Generic protocol clients (OpenAI, Claude, Gemini) execute this phased pipeline rather than
//! branching on provider names or dialects. Every wire customization (request envelope,
//! codec, cryptographic signature, stream unwrap) is partitioned into strictly ordered phases:
//!
//!   Phase 1: `EnvelopePhase` (Structural JSON body transformation)
//!   Phase 2: `BodyCodecPhase` (Payload byte encoding/compression)
//!   Phase 3: `RequestSignerPhase` (Final immutable request inspection and cryptographic signing)

use crate::request::RequestBuilder;
use muta_contracts::{
    OpenAiChatDialect, PreflightValidator, ProviderError, ProviderErrorKind, ResolvedAuth,
};
use std::fmt;

/// Phase 1: Structural JSON body transformation (e.g. AgentChat envelope).
pub trait EnvelopePhase: Send + Sync {
    fn reshape_body(&self, body: &serde_json::Value) -> Result<serde_json::Value, ProviderError>;
}

/// Phase 2: Payload byte encoding or compression.
pub trait BodyCodecPhase: Send + Sync {
    fn encode_body(&self, body_bytes: &[u8]) -> Result<Vec<u8>, ProviderError>;
}

/// Phase 3: Final immutable request inspection and cryptographic signing.
pub trait RequestSignerPhase: Send + Sync {
    fn sign_request(
        &self,
        req: RequestBuilder,
        signed_path: &str,
        body_bytes: &[u8],
        auth: &ResolvedAuth,
    ) -> Result<RequestBuilder, ProviderError>;
}

/// Upstream execution and latency metrics emitted by terminal events.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamMetrics {
    pub first_token_duration_ms: Option<u64>,
    pub total_duration_ms: Option<u64>,
    pub server_duration_ms: Option<u64>,
}

/// Upstream fault returned inside an HTTP SSE envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderFault {
    pub status_code: u16,
    pub message: String,
}

/// Algebraic outcome of parsing an inbound raw SSE event (ADR-0267).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransformedFrame {
    /// Standard LLM chunk payload (canonical JSON string).
    Delta(String),
    /// Control / heartbeat / false `[DONE]` frame to safely skip.
    Skip,
    /// Authoritative stream close with upstream duration and performance telemetry.
    Terminal(StreamMetrics),
    /// Upstream error wrapped inside an envelope (e.g. quota, auth expired).
    UpstreamFault(ProviderFault),
}

/// Intercepts and decodes raw streaming frames into canonical protocol events.
pub trait StreamTransformer: Send + Sync {
    /// Transforms an inbound raw SSE event into a canonical frame outcome.
    fn transform_event(
        &self,
        event_type: Option<&str>,
        data: &str,
    ) -> Result<TransformedFrame, ProviderError>;
}

/// Default pass-through stream transformer.
#[derive(Debug, Clone, Default)]
pub struct PassThroughStreamTransformer;

impl StreamTransformer for PassThroughStreamTransformer {
    fn transform_event(
        &self,
        _event_type: Option<&str>,
        data: &str,
    ) -> Result<TransformedFrame, ProviderError> {
        if data.trim() == "[DONE]" {
            Ok(TransformedFrame::Terminal(StreamMetrics::default()))
        } else {
            Ok(TransformedFrame::Delta(data.to_string()))
        }
    }
}

/// Composable transport pipeline for request and stream transformations.
pub struct TransportPipeline {
    envelope_phase: Option<Box<dyn EnvelopePhase>>,
    codec_phase: Option<Box<dyn BodyCodecPhase>>,
    signer_phase: Option<Box<dyn RequestSignerPhase>>,
    stream_transformer: Box<dyn StreamTransformer>,
    validators: Vec<Box<dyn PreflightValidator>>,
}

impl Default for TransportPipeline {
    fn default() -> Self {
        Self {
            envelope_phase: None,
            codec_phase: None,
            signer_phase: None,
            stream_transformer: Box::new(PassThroughStreamTransformer),
            validators: Vec::new(),
        }
    }
}

impl fmt::Debug for TransportPipeline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TransportPipeline")
            .field("has_envelope", &self.envelope_phase.is_some())
            .field("has_codec", &self.codec_phase.is_some())
            .field("has_signer", &self.signer_phase.is_some())
            .field("validators", &self.validators.len())
            .finish()
    }
}

impl TransportPipeline {
    /// Create a fluent builder for phased pipeline construction.
    pub fn builder() -> TransportPipelineBuilder {
        TransportPipelineBuilder::default()
    }

    /// Perform preflight contract validation before an in-flight request.
    pub fn preflight_assert(&self, auth: &ResolvedAuth) -> Result<(), ProviderError> {
        for validator in &self.validators {
            validator
                .validate_auth(auth)
                .map_err(|msg| ProviderError::authentication("pipeline", msg))?;
        }
        Ok(())
    }

    /// Transform an outbound raw body through Envelope and Codec phases.
    pub fn prepare_body(&self, body: &serde_json::Value) -> Result<Vec<u8>, ProviderError> {
        let reshaped = match &self.envelope_phase {
            Some(phase) => phase.reshape_body(body)?,
            None => body.clone(),
        };
        let raw = serde_json::to_vec(&reshaped).map_err(|e| {
            ProviderError::new(
                "pipeline",
                ProviderErrorKind::Protocol,
                format!("failed to serialize body: {e}"),
            )
        })?;
        match &self.codec_phase {
            Some(phase) => phase.encode_body(&raw),
            None => Ok(raw),
        }
    }

    /// Execute the RequestSigner phase.
    pub fn sign_request(
        &self,
        req: RequestBuilder,
        signed_path: &str,
        body_bytes: &[u8],
        auth: &ResolvedAuth,
    ) -> Result<RequestBuilder, ProviderError> {
        match &self.signer_phase {
            Some(phase) => phase.sign_request(req, signed_path, body_bytes, auth),
            None => Ok(req),
        }
    }

    /// Unwrap an incoming stream frame into canonical frame outcome.
    pub fn transform_event(
        &self,
        event_type: Option<&str>,
        data: &str,
    ) -> Result<TransformedFrame, ProviderError> {
        self.stream_transformer.transform_event(event_type, data)
    }

    /// Unwrap an incoming payload for backward-compatible call sites.
    pub fn unwrap_payload(
        &self,
        data: &str,
        provider_label: &'static str,
    ) -> Result<String, ProviderError> {
        match self.transform_event(None, data)? {
            TransformedFrame::Delta(chunk) => Ok(chunk),
            TransformedFrame::Skip | TransformedFrame::Terminal(_) => Ok(String::new()),
            TransformedFrame::UpstreamFault(fault) => Err(ProviderError::new(
                provider_label,
                ProviderErrorKind::Upstream,
                format!(
                    "Upstream error ({}): {}",
                    fault.status_code, fault.message
                ),
            )),
        }
    }

    /// Create the appropriate default pipeline for an OpenAI chat completions dialect.
    pub fn for_openai_chat_dialect(_dialect: OpenAiChatDialect) -> Self {
        Self::default()
    }
}

/// Fluent builder for [`TransportPipeline`].
#[derive(Default)]
pub struct TransportPipelineBuilder {
    envelope_phase: Option<Box<dyn EnvelopePhase>>,
    codec_phase: Option<Box<dyn BodyCodecPhase>>,
    signer_phase: Option<Box<dyn RequestSignerPhase>>,
    stream_transformer: Option<Box<dyn StreamTransformer>>,
    validators: Vec<Box<dyn PreflightValidator>>,
}

impl TransportPipelineBuilder {
    pub fn with_envelope(mut self, phase: impl EnvelopePhase + 'static) -> Self {
        self.envelope_phase = Some(Box::new(phase));
        self
    }

    pub fn with_codec(mut self, phase: impl BodyCodecPhase + 'static) -> Self {
        self.codec_phase = Some(Box::new(phase));
        self
    }

    pub fn with_signer(mut self, phase: impl RequestSignerPhase + 'static) -> Self {
        self.signer_phase = Some(Box::new(phase));
        self
    }

    pub fn with_validator(mut self, validator: impl PreflightValidator + 'static) -> Self {
        self.validators.push(Box::new(validator));
        self
    }

    pub fn with_stream_transformer(
        mut self,
        transformer: impl StreamTransformer + 'static,
    ) -> Self {
        self.stream_transformer = Some(Box::new(transformer));
        self
    }

    pub fn build(self) -> TransportPipeline {
        TransportPipeline {
            envelope_phase: self.envelope_phase,
            codec_phase: self.codec_phase,
            signer_phase: self.signer_phase,
            stream_transformer: self
                .stream_transformer
                .unwrap_or_else(|| Box::new(PassThroughStreamTransformer)),
            validators: self.validators,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_stream_transformer_handles_delta_and_done() {
        let transformer = PassThroughStreamTransformer;

        let delta = transformer.transform_event(None, "hello world").unwrap();
        assert_eq!(delta, TransformedFrame::Delta("hello world".to_string()));

        let done = transformer.transform_event(None, "[DONE]").unwrap();
        assert_eq!(done, TransformedFrame::Terminal(StreamMetrics::default()));
    }
}
