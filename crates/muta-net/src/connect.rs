//! Connection establishment with per-phase timing.
//!
//! Each phase is bracketed by events in the attempt's recorder, which is what
//! makes `dns_us` / `tcp_us` / `tls_us` measurements rather than guesses — and
//! what makes them *absent* (`ConnectionReused`) when the pool answered instead.

use std::sync::{Arc, Mutex};

use muta_trace::{EventKind, Recorder};
use tokio::net::TcpStream;

use crate::error::NetError;

/// A stream the HTTP codec can drive: anything readable, writable and boxable.
pub trait Transport: AsyncReadWrite + Send + Unpin {}
impl<T: AsyncReadWrite + Send + Unpin> Transport for T {}

/// Blanket supertrait so `Box<dyn Transport>` satisfies the codec's bounds.
pub trait AsyncReadWrite: tokio::io::AsyncRead + tokio::io::AsyncWrite {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite> AsyncReadWrite for T {}

/// How to reach a peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// `host:port`.
    pub authority: String,
    /// Server name for TLS; `None` means plaintext.
    pub tls_server_name: Option<String>,
}

impl Target {
    /// A plaintext target.
    pub fn plain(authority: impl Into<String>) -> Self {
        Self {
            authority: authority.into(),
            tls_server_name: None,
        }
    }

    /// A TLS target: `authority` is the socket address, `server_name` is the
    /// name verified against the certificate.
    pub fn tls(authority: impl Into<String>, server_name: impl Into<String>) -> Self {
        Self {
            authority: authority.into(),
            tls_server_name: Some(server_name.into()),
        }
    }

    /// Split an absolute URL into a target and an origin-form path.
    ///
    /// Keeps the HTTP stack's URL parsing in one place: callers migrating from
    /// a raw client should not each hand-roll scheme/authority/path handling.
    pub fn from_url(url: &str) -> Result<(Self, String), NetError> {
        let (scheme, rest) = url
            .split_once("://")
            .ok_or_else(|| NetError::Connect(format!("url has no scheme: {url}")))?;
        let (authority, path) = match rest.find('/') {
            Some(index) => (&rest[..index], &rest[index..]),
            None => (rest, "/"),
        };
        if authority.is_empty() {
            return Err(NetError::Connect(format!("url has no host: {url}")));
        }
        let default_port = if scheme == "https" { 443 } else { 80 };
        let has_port = authority
            .rsplit_once(':')
            .is_some_and(|(_, port)| port.chars().all(|character| character.is_ascii_digit()));
        let authority = if has_port {
            authority.to_string()
        } else {
            format!("{authority}:{default_port}")
        };
        let target = match scheme {
            "https" => {
                let host = authority
                    .rsplit_once(':')
                    .map_or(authority.as_str(), |(host, _)| host)
                    .to_string();
                Self::tls(authority, host)
            }
            "http" => Self::plain(authority),
            other => {
                return Err(NetError::Connect(format!("unsupported scheme: {other}")));
            }
        };
        Ok((target, path.to_string()))
    }
}

/// A freshly established connection.
pub struct Established {
    pub stream: Box<dyn Transport>,
    /// Local port, when the platform exposes it. Distinguishes concurrent
    /// connections to the same authority.
    pub local_port: Option<u16>,
    /// A duplicate handle to the same socket, kept so `TCP_INFO` can be sampled
    /// while the connection is in use. `None` when the platform refused the
    /// duplicate, which only costs the RTT/retransmit scopes.
    pub socket: Option<std::net::TcpStream>,
}

/// Opens connections. Implemented by `TcpConnector` today; the TLS layer wraps
/// its output rather than replacing it.
pub trait Connector: Send + Sync {
    fn connect(
        &self,
        target: &Target,
        recorder: &Arc<Mutex<Recorder>>,
    ) -> impl std::future::Future<Output = Result<Established, NetError>> + Send;
}

/// Resolves, then connects over TCP.
#[derive(Debug, Default, Clone, Copy)]
pub struct TcpConnector;

impl Connector for TcpConnector {
    async fn connect(
        &self,
        target: &Target,
        recorder: &Arc<Mutex<Recorder>>,
    ) -> Result<Established, NetError> {
        {
            let mut recorder = recorder.lock().unwrap_or_else(|e| e.into_inner());
            recorder.mark(EventKind::DnsStart, 0, 0);
        }
        let addresses: Vec<std::net::SocketAddr> = tokio::net::lookup_host(&target.authority)
            .await
            .map_err(|error| NetError::Resolve(error.to_string()))?
            .collect();
        {
            let mut recorder = recorder.lock().unwrap_or_else(|e| e.into_inner());
            recorder.mark(EventKind::DnsEnd, addresses.len() as u32, 0);
        }
        let Some(address) = addresses.first().copied() else {
            return Err(NetError::Resolve(format!(
                "no addresses for {}",
                target.authority
            )));
        };

        {
            let mut recorder = recorder.lock().unwrap_or_else(|e| e.into_inner());
            recorder.mark(EventKind::TcpStart, 0, 0);
        }
        let stream = TcpStream::connect(address)
            .await
            .map_err(|error| NetError::Connect(error.to_string()))?;
        // Streaming responses are small, latency-sensitive writes: Nagle would
        // add up to 40 ms per flush.
        let _ = stream.set_nodelay(true);
        let local_port = stream.local_addr().ok().map(|address| address.port());
        {
            let mut recorder = recorder.lock().unwrap_or_else(|e| e.into_inner());
            recorder.mark(EventKind::TcpEnd, 0, 0);
        }

        // A duplicated descriptor for the same socket: the HTTP path owns the
        // stream, the sampler owns this one, and neither blocks the other.
        let socket = crate::tcp_info::duplicate(&stream);

        Ok(Established {
            stream: Box::new(stream),
            local_port,
            socket,
        })
    }
}
