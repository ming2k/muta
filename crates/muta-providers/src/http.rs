//! The provider-side HTTP client, on the owned transport (ADR-0200).
//!
//! Every non-model egress in this crate — OAuth token exchange, usage/quota
//! probes, model-list discovery — used to build its own `reqwest::Client`. They
//! now share one handle over [`muta_net`], which is what makes them visible to
//! the same trace, the same retry classification and the same timeout policy as
//! the model path.
//!
//! The surface is deliberately small and *bounded*: a request has an overall
//! deadline (these are all short control-plane calls), the body is returned as
//! text, and content-encoding is already decoded by the transport.

use std::sync::Arc;
use std::time::Duration;

use http::{HeaderMap, Method, StatusCode};
use muta_net::{Client, ClientConfig, Pool, Target, TcpConnector, TlsConnector};

/// Request body shapes these call sites use.
#[derive(Debug, Clone)]
pub enum Body {
    Json(serde_json::Value),
    Form(Vec<(String, String)>),
    /// An already-encoded body (the OAuth module's form serializer predates
    /// this helper and is exercised by its own tests).
    Raw(String),
}

/// A control-plane request.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Body>,
}

impl Request {
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: Vec::new(),
            body: None,
        }
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_string(), value.into()));
        self
    }

    pub fn json(mut self, value: &serde_json::Value) -> Self {
        self.body = Some(Body::Json(value.clone()));
        self
    }

    pub fn form(mut self, form: Vec<(String, String)>) -> Self {
        self.body = Some(Body::Form(form));
        self
    }

    pub fn raw_body(mut self, body: String) -> Self {
        self.body = Some(Body::Raw(body));
        self
    }
}

/// A control-plane reply with the body already read.
#[derive(Debug, Clone)]
pub struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: String,
}

impl Reply {
    pub fn is_success(&self) -> bool {
        self.status.is_success()
    }

    /// Parse the body as JSON, with a diagnostic that names the endpoint.
    pub fn json(&self) -> Result<serde_json::Value, String> {
        serde_json::from_str(&self.body)
            .map_err(|error| format!("invalid JSON response: {error} (body: {})", self.body))
    }
}

/// A pooled HTTP handle over the owned transport.
#[derive(Clone)]
pub struct Http {
    client: Arc<Client<TlsConnector<TcpConnector>>>,
    timeout: Duration,
}

impl Http {
    /// Build a handle with an overall per-request deadline.
    pub fn new(timeout: Duration) -> Result<Self, String> {
        let connector =
            TlsConnector::platform(TcpConnector::new()).map_err(|error| error.to_string())?;
        Ok(Self {
            client: Arc::new(Client::new(
                connector,
                Pool::default(),
                ClientConfig::default(),
            )),
            timeout,
        })
    }

    /// Build a handle with this crate's control-plane deadline (10 s).
    pub fn control_plane() -> Result<Self, String> {
        Self::new(Duration::from_secs(10))
    }

    pub async fn send(&self, request: Request) -> Result<Reply, String> {
        let (target, path) = Target::from_url(&request.url).map_err(|error| error.to_string())?;
        let mut head = muta_net::RequestHead::new(request.method.clone(), path);
        let mut content_type = None;
        for (name, value) in &request.headers {
            if name.eq_ignore_ascii_case("content-type") {
                content_type = Some(value.clone());
            }
            head = head.with_header(name.as_str(), value.as_str());
        }
        let body = match &request.body {
            None => None,
            Some(Body::Json(value)) => {
                if content_type.is_none() {
                    head = head.with_header("content-type", "application/json");
                }
                Some(
                    serde_json::to_vec(value)
                        .map_err(|error| format!("could not encode request: {error}"))?
                        .into(),
                )
            }
            Some(Body::Form(fields)) => {
                if content_type.is_none() {
                    head = head.with_header("content-type", "application/x-www-form-urlencoded");
                }
                Some(form_encode(fields).into())
            }
            Some(Body::Raw(body)) => Some(body.clone().into()),
        };

        let exchange = async {
            let mut response = self.client.request(&target, head, body).await?;
            let status = response.head.status;
            let headers = response.head.headers.clone();
            let bytes = response.body.read_to_end().await?;
            Ok::<_, muta_net::NetError>((status, headers, bytes))
        };
        let (status, headers, bytes) = tokio::time::timeout(self.timeout, exchange)
            .await
            .map_err(|_| format!("request to {} timed out", request.url))?
            .map_err(|error| error.to_string())?;
        Ok(Reply {
            status,
            headers,
            body: String::from_utf8_lossy(&bytes).into_owned(),
        })
    }

    pub async fn get(&self, url: &str, headers: &[(&str, &str)]) -> Result<Reply, String> {
        let mut request = Request::new(Method::GET, url);
        for (name, value) in headers {
            request = request.header(name, *value);
        }
        self.send(request).await
    }
}

/// Percent-encode one URL component (query names and values, form fields).
pub fn encode_component(input: &str) -> String {
    percent_encode(input)
}

/// `application/x-www-form-urlencoded` encoding, per WHATWG URL.
fn form_encode(fields: &[(String, String)]) -> String {
    let mut out = String::new();
    for (index, (name, value)) in fields.iter().enumerate() {
        if index > 0 {
            out.push('&');
        }
        out.push_str(&percent_encode(name));
        out.push('=');
        out.push_str(&percent_encode(value));
    }
    out
}

fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_encoding_matches_the_url_standard() {
        assert_eq!(
            form_encode(&[
                ("grant_type".into(), "authorization_code".into()),
                ("code".into(), "a b+c/d".into()),
            ]),
            "grant_type=authorization_code&code=a+b%2Bc%2Fd"
        );
    }

    #[test]
    fn a_reply_parses_json_and_reports_failures() {
        let reply = Reply {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            body: "{\"ok\":true}".into(),
        };
        assert!(reply.is_success());
        assert_eq!(reply.json().expect("json")["ok"], serde_json::json!(true));

        let broken = Reply {
            status: StatusCode::BAD_GATEWAY,
            headers: HeaderMap::new(),
            body: "not json".into(),
        };
        assert!(!broken.is_success());
        assert!(broken.json().is_err());
    }
}
