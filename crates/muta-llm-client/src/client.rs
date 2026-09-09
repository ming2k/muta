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
//! (streaming) or [`Client::send_json`] (non-streaming). The builder is only a
//! convenient way to *describe* a request: execution goes through the
//! [`Egress`] seam, so the same adapter code runs on `reqwest` today and on the
//! owned transport (ADR-0200) by configuration.
//!
//! ## Timeouts
//!
//! Two bounds, deliberately scoped:
//!
//! - `CONNECT_TIMEOUT` applies client-wide, so every request — streaming or
//!   not — fails fast when the peer black-holes the TCP/TLS handshake (no
//!   RST, no bytes). Without it a dead endpoint hangs until the OS TCP stack
//!   gives up (on the order of minutes), and the retry classifier in
//!   [`crate::transport`] never sees an error to classify.
//! - `CHAT_REQUEST_TIMEOUT` bounds one whole non-streaming request
//!   (connect → full body). It is stamped per request by [`Client::send_json`]
//!   (and by Google's `chat`, which sends through [`Client::http`] directly),
//!   never on the streaming path: an overall timeout would cut a long SSE
//!   generation mid-stream, and reqwest's per-read `read_timeout` would kill
//!   legitimate streams whose token gaps exceed the bound. Stall policy for a
//!   live stream belongs to the harness (muta-agent's `STREAM_IDLE_TIMEOUT`).
//!
//! Both bounds surface as `reqwest` timeout errors, which
//! [`transport_error`] classifies as retryable, so a stall feeds the retry
//! loop instead of hanging the turn forever.

use std::sync::Arc;
use std::time::Duration;

use muta_contracts::ProviderError;

#[cfg(feature = "reqwest-oracle")]
use crate::egress::ReqwestEgress;
use crate::egress::{Egress, HttpResponse};
use crate::transport::{decode_response_json, ensure_success};

/// Bound on the connect phase of every *oracle* request.
///
/// The owned transport enforces its own connect behaviour; this exists only so
/// the differential oracle matches the client it replaced.
#[cfg(feature = "reqwest-oracle")]
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Overall bound for a single non-streaming chat request, applied per request
/// by [`Client::send_json`] — never client-wide and never to streaming.
///
/// A non-streaming response delivers zero bytes until the model has finished
/// generating, so a reasoning model can legitimately take minutes; 5 minutes
/// is generous enough for that while still catching a genuinely stalled
/// endpoint, which now surfaces as a retryable timeout instead of hanging
/// the turn forever.
const CHAT_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// Environment variable selecting the transport: `net` (owned) or anything else
/// (`reqwest`, the default).
pub const EGRESS_ENV: &str = "MUTA_EGRESS";

/// Environment variable carrying a proxy URL for the owned transport
/// (`http://user:pass@host:port` or `socks5://host:port`).
pub const PROXY_ENV: &str = "MUTA_PROXY";

/// Which transport the environment asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressChoice {
    Reqwest,
    Owned { proxy: Option<String> },
}

/// Whether the owned transport is the default when `MUTA_EGRESS` is unset.
///
/// The P2 cutover: `net-transport-default` is a default feature, so an unset
/// variable means the owned transport. `MUTA_EGRESS=reqwest` still selects the
/// oracle in builds that compile it (`reqwest-oracle`); without it, the request
/// stays on the owned transport and says so.
#[cfg(feature = "net-transport-default")]
const OWNED_BY_DEFAULT: bool = true;
#[cfg(not(feature = "net-transport-default"))]
const OWNED_BY_DEFAULT: bool = false;

/// Resolve the transport choice from raw environment values.
///
/// Pure so it can be tested without mutating process state; [`Client::new`]
/// feeds it the real environment. `MUTA_EGRESS=reqwest` always wins, so an
/// operator can escape the default without a rebuild.
pub fn egress_choice(value: Option<&str>, proxy: Option<&str>) -> EgressChoice {
    let owned = match value {
        Some("net") | Some("owned") | Some("muta-net") => true,
        Some("reqwest") => false,
        _ => OWNED_BY_DEFAULT,
    };
    if owned {
        EgressChoice::Owned {
            proxy: proxy
                .filter(|url| !url.trim().is_empty())
                .map(str::to_string),
        }
    } else {
        EgressChoice::Reqwest
    }
}

#[cfg(feature = "reqwest-oracle")]
fn build_reqwest() -> reqwest::Client {
    // `build` fails only on invalid TLS/proxy configuration, none of which this
    // crate sets; fall back to stock defaults rather than panic if a future
    // builder knob ever makes it fallible here.
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

fn egress_from_env(timings: crate::egress::TimingsSlot) -> Arc<dyn Egress> {
    let choice = egress_choice(
        std::env::var(EGRESS_ENV).ok().as_deref(),
        std::env::var(PROXY_ENV).ok().as_deref(),
    );
    match choice {
        #[cfg(feature = "reqwest-oracle")]
        EgressChoice::Reqwest => Arc::new(ReqwestEgress::new(build_reqwest())),
        #[cfg(not(feature = "reqwest-oracle"))]
        EgressChoice::Reqwest => {
            tracing::warn!(
                "{EGRESS_ENV}=reqwest needs the `reqwest-oracle` feature; using the owned transport"
            );
            owned_egress(None, timings)
        }
        EgressChoice::Owned { proxy } => owned_egress(proxy.as_deref(), timings),
    }
}

/// Build the owned egress, optionally through a proxy.
fn owned_egress(proxy: Option<&str>, timings: crate::egress::TimingsSlot) -> Arc<dyn Egress> {
    let built: Result<Arc<dyn Egress>, String> = match proxy {
        Some(url) => muta_net::Proxy::parse(url)
            .map_err(|error| error.to_string())
            .and_then(crate::MutaNetEgress::with_proxy)
            .map(|egress| Arc::new(egress.with_timings_slot(timings)) as Arc<dyn Egress>),
        None => crate::MutaNetEgress::new()
            .map(|egress| Arc::new(egress.with_timings_slot(timings)) as Arc<dyn Egress>),
    };
    match built {
        Ok(egress) => egress,
        Err(error) => {
            tracing::warn!(%error, "owned transport unavailable");
            fallback_egress()
        }
    }
}

/// Last resort when the owned transport cannot be built: the oracle, if it is
/// compiled in. A production build has no fallback and says so loudly.
fn fallback_egress() -> Arc<dyn Egress> {
    #[cfg(feature = "reqwest-oracle")]
    {
        Arc::new(ReqwestEgress::new(build_reqwest()))
    }
    #[cfg(not(feature = "reqwest-oracle"))]
    {
        panic!("no egress available: the owned transport failed and no oracle is compiled in")
    }
}

/// Pooled HTTP client owning one `reqwest::Client` and one [`Egress`] for the
/// provider's lifetime.
pub struct Client {
    /// Only the oracle needs a `reqwest` client; production has none.
    #[cfg(feature = "reqwest-oracle")]
    http: reqwest::Client,
    egress: Arc<dyn Egress>,
    /// Filled by the owned transport when an attempt's body ends; taken once
    /// per attempt by whoever books it.
    transport_timings: crate::egress::TimingsSlot,
    /// Overall timeout stamped on non-streaming requests; see
    /// `CHAT_REQUEST_TIMEOUT`. A field rather than a call-site constant so
    /// tests can shrink it and observe a stall without waiting out the
    /// production bound.
    request_timeout: Duration,
}

impl Client {
    /// Construct a pooled client: `reqwest` defaults plus `CONNECT_TIMEOUT`
    /// on the connect phase. No overall or read timeout is set client-wide —
    /// see the module docs for why streaming forbids both; the non-streaming
    /// bound is applied per request by [`Client::send_json`].
    ///
    /// The transport is `reqwest` unless `MUTA_EGRESS=net` selects the owned
    /// path (ADR-0200), optionally through `MUTA_PROXY`.
    pub fn new() -> Self {
        let transport_timings = crate::egress::timings_slot();
        Self {
            #[cfg(feature = "reqwest-oracle")]
            http: build_reqwest(),
            egress: egress_from_env(Arc::clone(&transport_timings)),
            transport_timings,
            request_timeout: CHAT_REQUEST_TIMEOUT,
        }
    }

    /// Take the transport-level timings of the most recent attempt.
    ///
    /// `None` when the egress could not observe them (or they were already
    /// taken): a timing is attributed to exactly one attempt.
    pub fn take_transport_timings(&self) -> Option<muta_contracts::TransportTimings> {
        self.transport_timings
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
    }

    /// Construct a client that executes requests through `egress`.
    ///
    /// The `reqwest` client is still constructed (protocols build request
    /// builders with it) but is only used as a builder factory.
    pub fn with_egress(egress: Arc<dyn Egress>) -> Self {
        Self {
            #[cfg(feature = "reqwest-oracle")]
            http: build_reqwest(),
            egress,
            transport_timings: crate::egress::timings_slot(),
            request_timeout: CHAT_REQUEST_TIMEOUT,
        }
    }

    /// The oracle's underlying `reqwest` client. Only available with the
    /// `reqwest-oracle` feature; production builds construct requests with
    /// [`crate::RequestBuilder`].
    #[cfg(feature = "reqwest-oracle")]
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// The overall timeout non-streaming call sites stamp per request.
    /// [`Self::send_json`] applies it automatically; protocols that send
    /// non-streaming requests through [`Self::http`] directly (Google's
    /// `chat`) stamp it on the builder themselves.
    pub(crate) fn request_timeout(&self) -> Duration {
        self.request_timeout
    }

    /// Shrink the non-streaming request timeout so tests can observe a stall
    /// without waiting out the production bound.
    #[cfg(test)]
    fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Send a fully-built request **without** validating the status.
    ///
    /// Needed by the OAuth self-heal paths, which must observe a `401` and
    /// retry with a refreshed token rather than have it raised as an error.
    /// No overall timeout is applied: this is the streaming path, and a long
    /// SSE generation must not be cut mid-stream.
    pub async fn send_raw(
        &self,
        request: crate::request::RequestBuilder,
        label: &'static str,
    ) -> Result<HttpResponse, ProviderError> {
        let parts = request.build(label)?;
        // ADR-0200: when the runtime shadow is enabled, replay this exact
        // request through the owned transport in a detached task. The
        // production path below is untouched by its outcome.
        #[cfg(feature = "net-shadow")]
        if crate::shadow::enabled() {
            crate::shadow::spawn(parts.clone());
        }
        self.egress.send(parts).await
    }

    /// Send a fully-built request and enforce HTTP success. Returns the
    /// response for the caller to decode or feed to [`crate::sse`].
    pub async fn send(
        &self,
        request: crate::request::RequestBuilder,
        label: &'static str,
    ) -> Result<HttpResponse, ProviderError> {
        let response = self.send_raw(request, label).await?;
        ensure_success(response, label, None).await
    }

    /// Send a fully-built request, enforce success, and decode the body as
    /// JSON. Convenience for non-streaming chat.
    ///
    /// Unlike [`Self::send_raw`], stamps an overall per-request timeout
    /// (`CHAT_REQUEST_TIMEOUT`): a non-streaming response delivers nothing
    /// until generation completes, so a stalled endpoint would otherwise hang
    /// the turn forever. The timeout surfaces as a retryable transport error
    /// via [`transport_error`].
    pub async fn send_json(
        &self,
        request: crate::request::RequestBuilder,
        label: &'static str,
    ) -> Result<serde_json::Value, ProviderError> {
        let response = self
            .send(request.timeout(self.request_timeout), label)
            .await?;
        decode_response_json(response, label).await
    }
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};

    /// A server that accepts connections and then says nothing — the
    /// black-hole stall (connection held open, zero response bytes) the
    /// timeouts exist to catch. [`Blackhole::drop_connections`] closes every
    /// accepted socket so pending clients observe EOF and the test can end.
    struct Blackhole {
        addr: std::net::SocketAddr,
        held: Arc<Mutex<Vec<TcpStream>>>,
    }

    impl Blackhole {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let held: Arc<Mutex<Vec<TcpStream>>> = Arc::new(Mutex::new(Vec::new()));
            let held_in_thread = Arc::clone(&held);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    match stream {
                        Ok(stream) => held_in_thread
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .push(stream),
                        Err(_) => break,
                    }
                }
            });
            Self { addr, held }
        }

        fn url(&self) -> String {
            format!("http://{}/chat/completions", self.addr)
        }

        fn drop_connections(&self) {
            self.held
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clear();
        }
    }

    fn post_json(_client: &Client, url: &str) -> crate::request::RequestBuilder {
        crate::request::RequestBuilder::new(http::Method::POST, url).json(&serde_json::json!({}))
    }

    /// The core regression test: a stalled non-streaming request must time
    /// out (previously it hung forever), and the timeout must classify as
    /// retryable so the harness retries instead of surfacing a dead turn.
    #[tokio::test]
    async fn non_streaming_timeout_fires_and_is_retryable() {
        let server = Blackhole::start();
        let client = Client::new().with_request_timeout(Duration::from_millis(300));
        let started = std::time::Instant::now();
        let error = client
            .send_json(post_json(&client, &server.url()), "Test")
            .await
            .unwrap_err();
        let elapsed = started.elapsed();
        assert!(
            elapsed >= Duration::from_millis(300),
            "the error must be the 300ms request timeout, not an instant failure: \
             {elapsed:?} / {error}"
        );
        let retryable = error.retry_disposition();
        assert!(
            matches!(retryable, muta_contracts::RetryDisposition::Retry { .. }),
            "a timeout must classify as retryable: {error}"
        );
        assert!(
            error.message().contains("transport error"),
            "transport error framing expected: {}",
            error.message()
        );
    }

    /// The connect phase's other failure mode (nothing listening → fast
    /// ECONNREFUSED) must classify as retryable via `is_connect`, matching
    /// what a connect timeout produces.
    #[tokio::test]
    async fn connect_phase_failure_is_retryable() {
        // Bind then release a port so nothing listens on it.
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let client = Client::new();
        let error = client
            .send(
                post_json(&client, &format!("http://127.0.0.1:{port}/v1/messages")),
                "Test",
            )
            .await
            .unwrap_err();
        assert!(
            matches!(
                error.retry_disposition(),
                muta_contracts::RetryDisposition::Retry { .. }
            ),
            "connect-phase errors must be retryable: {error}"
        );
    }

    #[test]
    fn the_transport_choice_is_pure_and_total() {
        // With the cutover feature off (the default), an unset variable keeps
        // reqwest; with it on, the owned transport is the default and
        // `MUTA_EGRESS=reqwest` is the escape hatch.
        let default = if cfg!(feature = "net-transport-default") {
            EgressChoice::Owned { proxy: None }
        } else {
            EgressChoice::Reqwest
        };
        assert_eq!(egress_choice(None, None), default);
        assert_eq!(egress_choice(Some("reqwest"), None), EgressChoice::Reqwest);
        assert_eq!(egress_choice(Some("reqwest"), None), EgressChoice::Reqwest);
        assert_eq!(
            egress_choice(Some("net"), None),
            EgressChoice::Owned { proxy: None }
        );
        assert_eq!(
            egress_choice(Some("net"), Some("http://127.0.0.1:8080")),
            EgressChoice::Owned {
                proxy: Some("http://127.0.0.1:8080".into())
            }
        );
        // An empty proxy is the same as none.
        assert_eq!(
            egress_choice(Some("net"), Some("  ")),
            EgressChoice::Owned { proxy: None }
        );
    }

    /// The streaming send path must NOT apply the non-streaming request
    /// timeout: a long generation must not be cut mid-stream. The client runs
    /// on its own runtime thread so this thread can observe whether `send`
    /// returns on its own while the server stalls.
    #[test]
    fn streaming_send_carries_no_overall_timeout() {
        let server = Blackhole::start();
        let url = server.url();
        let handle = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let client = Client::new().with_request_timeout(Duration::from_millis(300));
                let request = post_json(&client, &url);
                client.send(request, "Test").await
            })
        });
        std::thread::sleep(Duration::from_millis(1_000));
        assert!(
            !handle.is_finished(),
            "send must outlast the 300ms non-streaming timeout; stream stall \
             policy belongs to the harness (STREAM_IDLE_TIMEOUT)"
        );
        server.drop_connections();
        let result = handle.join().unwrap();
        assert!(
            result.is_err(),
            "EOF before response headers is a transport error"
        );
    }
}
