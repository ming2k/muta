//! The egress seam: one request, one transport, one trace.
//!
//! ADR-0200 replaces `reqwest` with an owned transport, and the switch must not
//! reach the protocol adapters. The seam is drawn at "given a fully formed
//! request, give me a response": [`Egress`] is implemented by [`MutaNetEgress`]
//! (production) and, behind the `reqwest-oracle` feature, by [`ReqwestEgress`]
//! — which exists only so the differential and shadow comparisons have a
//! reference implementation. `reqwest` is therefore a *dev/test* dependency;
//! it is not in the production graph.
//!
//! [`HttpResponse`] is deliberately small — status, headers, and a byte stream —
//! because that is all a protocol adapter may depend on. Everything richer
//! (retry classification, JSON diagnostics) is layered on top in
//! [`crate::transport`], so every transport gets identical behaviour by
//! construction rather than by duplicated effort.

use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
use http::{HeaderMap, Method, StatusCode};
use muta_contracts::{ProviderError, ProviderErrorKind};

#[cfg(feature = "reqwest-oracle")]
use crate::transport::transport_error;

/// A request in transport-neutral form.
#[derive(Debug, Clone)]
pub struct RequestParts {
    /// Provider label used for error attribution and retry classification.
    pub label: &'static str,
    pub method: Method,
    /// Absolute URL.
    pub url: String,
    pub headers: HeaderMap,
    pub body: Option<Bytes>,
    /// Whole-request timeout, applied only to non-streaming requests by the
    /// caller (a streaming request must never carry one).
    pub timeout: Option<Duration>,
}

/// A response: status, headers, and a body that has not been read yet.
pub struct HttpResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: BoxStream<'static, Result<Bytes, ProviderError>>,
}

impl HttpResponse {
    /// The body as a byte stream. The errors are already provider errors: the
    /// transport classified them.
    pub fn into_byte_stream(self) -> BoxStream<'static, Result<Bytes, ProviderError>> {
        self.body
    }

    /// Read the whole body.
    pub async fn into_bytes(self) -> Result<Bytes, ProviderError> {
        let mut stream = self.into_byte_stream();
        let mut out = Vec::new();
        while let Some(chunk) = stream.next().await {
            out.extend_from_slice(&chunk?);
        }
        Ok(Bytes::from(out))
    }

    /// Read the whole body as text (lossy for non-UTF-8, as the diagnostics
    /// expect).
    pub async fn into_text(self) -> Result<String, ProviderError> {
        let bytes = self.into_bytes().await?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

impl std::fmt::Debug for HttpResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HttpResponse({})", self.status)
    }
}

/// The transport that executes a request.
#[async_trait::async_trait]
pub trait Egress: Send + Sync {
    async fn send(&self, parts: RequestParts) -> Result<HttpResponse, ProviderError>;

    /// Optional pre-warm: prime a connection to `url` into the idle pool in advance.
    async fn prewarm(&self, _url: &str) -> Result<bool, ProviderError> {
        Ok(false)
    }
}

/// The `reqwest` transport, used **only** as the differential/shadow oracle.
///
/// Compiled in with the `reqwest-oracle` feature; never part of a production
/// build.
#[cfg(feature = "reqwest-oracle")]
pub struct ReqwestEgress {
    http: reqwest::Client,
}

#[cfg(feature = "reqwest-oracle")]
impl ReqwestEgress {
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }
}

#[cfg(feature = "reqwest-oracle")]
#[async_trait::async_trait]
impl Egress for ReqwestEgress {
    async fn send(&self, parts: RequestParts) -> Result<HttpResponse, ProviderError> {
        let mut builder = self.http.request(parts.method.clone(), &parts.url);
        builder = builder.headers(parts.headers.clone());
        if let Some(timeout) = parts.timeout {
            builder = builder.timeout(timeout);
        }
        let builder = match parts.body {
            Some(body) => builder.body(body),
            None => builder,
        };
        let response = builder
            .send()
            .await
            .map_err(|error| transport_error(parts.label, error))?;
        let status = response.status();
        let headers = response.headers().clone();
        let label = parts.label;
        let body = response
            .bytes_stream()
            .map(move |item| item.map_err(|error| transport_error(label, error)))
            .boxed();
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}

pub use owned::{MutaNetEgress, TraceSink};

/// A slot the owned transport writes the last attempt's timings into.
pub type TimingsSlot = std::sync::Arc<std::sync::Mutex<Option<muta_contracts::TransportTimings>>>;

/// Build an empty timings slot.
pub fn timings_slot() -> TimingsSlot {
    std::sync::Arc::new(std::sync::Mutex::new(None))
}

/// Map an owned-transport failure onto the provider error the retry classifier
/// reads, so both transports classify identically.
fn net_error(label: &'static str, error: netune::NetError) -> ProviderError {
    let retryable = error.is_retryable();
    let kind = match error.class() {
        "resolve" | "connect" | "io" => ProviderErrorKind::Transport,
        "encoding" | "decode" => ProviderErrorKind::Decode,
        _ => ProviderErrorKind::Protocol,
    };
    let mapped = ProviderError::new(label, kind, error.to_string());
    if retryable {
        mapped.retryable(None)
    } else {
        mapped
    }
}

/// A retryable overall-timeout error, matching the reqwest transport's framing.
fn timeout_error(label: &'static str) -> ProviderError {
    ProviderError::new(
        label,
        ProviderErrorKind::Timeout,
        "transport error: request timed out".to_string(),
    )
    .retryable(None)
}

mod owned {
    use std::sync::{Arc, Mutex};

    use bytes::Bytes;
    use futures::StreamExt;
    use futures::stream::BoxStream;
    use muta_contracts::ProviderError;
    use netune::{Connector, Proxy, ProxyConnector, TcpConnector, TlsConnector};
    use netune_trace::{
        AttemptRef, ConnectionInfo, EndpointRef, Fidelity, Recorder, RequestTrace, TraceId,
    };

    use super::{Egress, HttpResponse, RequestParts, net_error, timeout_error};

    /// Capacity of one request's trace ring on the owned path.
    const TRACE_CAPACITY: usize = 16_384;

    /// Called once per completed request with its trace.
    pub type TraceSink = Arc<dyn Fn(RequestTrace) + Send + Sync>;

    /// The owned transport (ADR-0200).
    ///
    /// Every request produces a [`RequestTrace`]; when a [`TraceSink`] is
    /// installed it receives it as soon as the body ends.
    pub struct MutaNetEgress<C: Connector = TlsConnector<TcpConnector>> {
        client: netune::Client<C>,
        sink: Option<TraceSink>,
        timings: super::TimingsSlot,
    }

    impl MutaNetEgress<TlsConnector<TcpConnector>> {
        /// The production configuration: platform trust store, direct.
        pub fn new() -> Result<Self, String> {
            TlsConnector::platform(TcpConnector::new())
                .map(Self::from_connector)
                .map_err(|error| error.to_string())
        }
    }

    impl MutaNetEgress<TlsConnector<ProxyConnector<TcpConnector>>> {
        /// The production configuration routed through `proxy`.
        pub fn with_proxy(proxy: Proxy) -> Result<Self, String> {
            TlsConnector::platform(ProxyConnector::new(TcpConnector::new(), proxy))
                .map(Self::from_connector)
                .map_err(|error| error.to_string())
        }
    }

    impl MutaNetEgress<TcpConnector> {
        /// Plaintext, no proxy: tests and local servers only.
        pub fn plain() -> Self {
            Self::from_connector(TcpConnector::new())
        }
    }

    impl<C: Connector> MutaNetEgress<C> {
        pub fn from_connector(connector: C) -> Self {
            Self {
                client: netune::Client::new(
                    connector,
                    netune::Pool::default(),
                    netune::ClientConfig::default(),
                ),
                sink: None,
                timings: super::timings_slot(),
            }
        }

        /// Install a sink for completed traces.
        pub fn with_trace_sink(mut self, sink: TraceSink) -> Self {
            self.sink = Some(sink);
            self
        }

        /// Write the attempt's timings into `slot` when its body ends.
        pub fn with_timings_slot(mut self, slot: super::TimingsSlot) -> Self {
            self.timings = slot;
            self
        }
    }

    #[async_trait::async_trait]
    impl<C: Connector> Egress for MutaNetEgress<C> {
        async fn send(&self, parts: RequestParts) -> Result<HttpResponse, ProviderError> {
            let (target, path) = netune::Target::from_url(&parts.url).map_err(|error| {
                ProviderError::invalid_request(parts.label, format!("invalid url: {error}"))
            })?;
            let authority = target.authority.clone();

            let mut head = netune::RequestHead::new(parts.method.clone(), path);
            for (name, value) in parts.headers.iter() {
                if let Ok(value) = value.to_str() {
                    head = head.with_header(name.as_str(), value);
                }
            }

            // A non-streaming request carries an overall deadline; it bounds
            // the body read too, exactly as the reqwest transport's per-request
            // timeout does.
            let deadline = parts
                .timeout
                .map(|timeout| tokio::time::Instant::now() + timeout);
            let recorder = Arc::new(Mutex::new(Recorder::start(TRACE_CAPACITY)));
            let send = self
                .client
                .send(&target, Arc::clone(&recorder), head, parts.body.clone());
            let response = match deadline {
                Some(deadline) => tokio::time::timeout_at(deadline, send)
                    .await
                    .map_err(|_| timeout_error(parts.label))?
                    .map_err(|error| net_error(parts.label, error))?,
                None => send.await.map_err(|error| net_error(parts.label, error))?,
            };
            let status = response.head.status;
            let headers = response.head.headers.clone();

            let sink = self.sink.clone();
            let timings = Arc::clone(&self.timings);
            let endpoint = EndpointRef {
                provider: parts.label.to_string(),
                model: String::new(),
                authority,
            };
            let label = parts.label;
            let body: BoxStream<'static, Result<Bytes, ProviderError>> = futures::stream::unfold(
                (
                    response.body,
                    recorder,
                    sink,
                    endpoint,
                    deadline,
                    label,
                    timings,
                    false,
                ),
                |(mut body, recorder, sink, endpoint, deadline, label, timings, failed)| async move {
                    if failed {
                        return None;
                    }
                    let chunk = match deadline {
                        Some(deadline) => {
                            match tokio::time::timeout_at(deadline, body.next_chunk()).await {
                                Ok(result) => result,
                                Err(_) => Err(netune::NetError::Io(std::io::Error::new(
                                    std::io::ErrorKind::TimedOut,
                                    "request timed out while reading the body",
                                ))),
                            }
                        }
                        None => body.next_chunk().await,
                    };
                    match chunk {
                        Ok(Some(chunk)) => Some((
                            Ok(chunk),
                            (body, recorder, sink, endpoint, deadline, label, timings, false),
                        )),
                        Ok(None) => {
                            if let Some(sink) = sink {
                                let log = recorder
                                    .lock()
                                    .unwrap_or_else(|error| error.into_inner())
                                    .log()
                                    .clone();
                                sink(RequestTrace {
                                    id: TraceId::new("egress"),
                                    attempt: AttemptRef {
                                        round: 0,
                                        turn: 0,
                                        attempt: 0,
                                    },
                                    endpoint,
                                    connection: ConnectionInfo::default(),
                                    fidelity: Fidelity::l1(),
                                    log,
                                });
                            }
                            None
                        }
                        Err(error) => Some((
                            Err(net_error(label, error)),
                            (body, recorder, sink, endpoint, deadline, label, timings, true),
                        )),
                    }
                },
            )
            .boxed();

            Ok(HttpResponse {
                status,
                headers,
                body,
            })
        }

        async fn prewarm(&self, url: &str) -> Result<bool, ProviderError> {
            let (target, _) = netune::Target::from_url(url).map_err(|error| {
                ProviderError::invalid_request("muta-net", format!("invalid prewarm url: {error}"))
            })?;
            self.client
                .prewarm(&target)
                .await
                .map_err(|error| net_error("muta-net", error))
        }
    }
}
