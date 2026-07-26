//! Pooled HTTP client shared by every protocol adapter.
//!
//! Each provider embeds a [`Client`] instead of calling `reqwest::Client::new()`
//! per request. Constructing a client per call discarded the connection pool
//! and TLS session cache on every turn, so keep-alive and TLS resumption never
//! carried across requests. One [`Client`] lives for the provider's lifetime
//! (a provider is built once per session), so a single pool is reused across
//! every chat, stream, and ReAct turn.
//!
//! A protocol builds a fully-formed [`reqwest::RequestBuilder`] — URL, auth
//! headers, JSON body, all vendor-specific — and hands it to [`Client::send`]
//! (streaming) or [`Client::send_json`] (non-streaming). The send → HTTP
//! success → decode pipeline is shared, so the twelve provider call sites no
//! longer restate it.

use crate::transport::{decode_response_json, ensure_success, transport_error};

/// Pooled HTTP client owning one `reqwest::Client` for the provider's lifetime.
pub struct Client {
    http: reqwest::Client,
}

impl Client {
    /// Construct a client with `reqwest` defaults (the same configuration the
    /// prior per-call `reqwest::Client::new()` used, now pooled).
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
        }
    }

    /// The underlying pooled client. Protocol code uses this to build a
    /// [`reqwest::RequestBuilder`] (`.post(url).header(...).json(body)`); the
    /// builder holds its own reference to the pool, so it can be passed to
    /// [`Self::send`] without borrowing `self`.
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// Send a fully-built request and enforce HTTP success. Returns the
    /// response for the caller to decode or feed to [`crate::sse`].
    pub async fn send(
        &self,
        request: reqwest::RequestBuilder,
        label: &str,
    ) -> Result<reqwest::Response, String> {
        let response = request
            .send()
            .await
            .map_err(|error| transport_error(label, error))?;
        ensure_success(response, label).await
    }

    /// Send a fully-built request, enforce success, and decode the body as
    /// JSON. Convenience for non-streaming chat.
    pub async fn send_json(
        &self,
        request: reqwest::RequestBuilder,
        label: &str,
    ) -> Result<serde_json::Value, String> {
        let response = self.send(request, label).await?;
        decode_response_json(response, label).await
    }
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}
