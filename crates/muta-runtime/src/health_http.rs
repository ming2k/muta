//! The lightweight HTTP API and health-probe side of the daemon's TCP port.
//!
//! The daemon's TCP listener speaks two protocols, split by peeking at the
//! request head (`classify` in `serve.rs`): WebSocket upgrades go to the
//! control plane; plain HTTP lands here.
//!
//! Deliberately lightweight and zero-dependency: operates directly on
//! `AsyncRead + AsyncWrite`, supports `/healthz`, `/api/v1/*`, CORS, and
//! token authorization.

use std::collections::HashMap;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

/// Request-head size cap; a peer that never finishes its headers is cut off.
const HEAD_CAP: usize = 16 * 1024;
/// Request-body size cap (1MB) to prevent buffer exhaustion.
const BODY_CAP: usize = 1024 * 1024;

/// Constant-time string comparison to prevent timing attacks on tokens.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut result = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        result |= x ^ y;
    }
    result == 0
}

/// Serve one plain-HTTP connection, then close it.
pub async fn serve<S>(
    stream: S,
    daemon_version: &str,
    expected_token: Option<&str>,
) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);

    // Read the request head
    let mut head = Vec::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = reader.read_until(b'\n', &mut line).await?;
        if n == 0 || head.len() > HEAD_CAP {
            return Ok(());
        }
        if line == b"\r\n" || line == b"\n" {
            break;
        }
        head.extend_from_slice(&line);
    }
    let head_str = String::from_utf8_lossy(&head);
    let mut lines = head_str.lines();
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/"));
    let path = target.split(['?', '#']).next().unwrap_or("/");

    // Parse headers
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim());
        }
    }

    let auth_required = expected_token.is_some();
    let is_authenticated = match expected_token {
        None => true,
        Some(token) => {
            if let Some(auth_header) = headers.get("authorization") {
                if let Some(bearer) = auth_header.strip_prefix("Bearer ") {
                    constant_time_eq(bearer.trim().as_bytes(), token.as_bytes())
                } else {
                    false
                }
            } else {
                false
            }
        }
    };

    let response = match (method, path) {
        ("OPTIONS", _) => build_response(
            "204 No Content",
            "text/plain",
            &[],
            false,
            &[
                ("Access-Control-Allow-Origin", "*"),
                ("Access-Control-Allow-Methods", "GET, POST, OPTIONS, HEAD"),
                (
                    "Access-Control-Allow-Headers",
                    "Authorization, Content-Type",
                ),
            ],
        ),
        ("GET" | "HEAD", "/healthz") => {
            let body = format!("{{\"version\":\"{daemon_version}\",\"auth\":{auth_required}}}");
            build_response(
                "200 OK",
                "application/json",
                body.as_bytes(),
                method == "HEAD",
                &[("Access-Control-Allow-Origin", "*")],
            )
        }
        ("GET" | "HEAD", "/api/v1/sessions") => {
            if !is_authenticated {
                build_response(
                    "401 Unauthorized",
                    "application/json",
                    b"{\"error\":\"unauthorized: valid Bearer token required\"}",
                    method == "HEAD",
                    &[("WWW-Authenticate", "Bearer")],
                )
            } else {
                let body = format!("{{\"version\":\"{daemon_version}\",\"sessions\":[]}}");
                build_response(
                    "200 OK",
                    "application/json",
                    body.as_bytes(),
                    method == "HEAD",
                    &[("Access-Control-Allow-Origin", "*")],
                )
            }
        }
        ("POST", "/api/v1/prompt") => {
            if !is_authenticated {
                build_response(
                    "401 Unauthorized",
                    "application/json",
                    b"{\"error\":\"unauthorized: valid Bearer token required\"}",
                    false,
                    &[("WWW-Authenticate", "Bearer")],
                )
            } else {
                let content_len: usize = headers
                    .get("content-length")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                if content_len > BODY_CAP {
                    build_response(
                        "413 Payload Too Large",
                        "application/json",
                        b"{\"error\":\"request body exceeds 1MB limit\"}",
                        false,
                        &[],
                    )
                } else {
                    let mut body = vec![0u8; content_len];
                    if reader.read_exact(&mut body).await.is_err() {
                        build_response(
                            "400 Bad Request",
                            "application/json",
                            b"{\"error\":\"failed to read request body\"}",
                            false,
                            &[],
                        )
                    } else {
                        build_response(
                            "200 OK",
                            "application/json",
                            b"{\"status\":\"accepted\"}",
                            false,
                            &[("Access-Control-Allow-Origin", "*")],
                        )
                    }
                }
            }
        }
        ("GET" | "HEAD", _) => build_response(
            "404 Not Found",
            "text/plain; charset=utf-8",
            b"not found",
            method == "HEAD",
            &[],
        ),
        _ => build_response(
            "405 Method Not Allowed",
            "text/plain",
            b"method not allowed",
            false,
            &[("Allow", "GET, HEAD, POST, OPTIONS")],
        ),
    };
    writer.write_all(&response).await?;
    writer.flush().await
}

fn build_response(
    status: &str,
    content_type: &str,
    body: &[u8],
    head_only: bool,
    extra: &[(&str, &str)],
) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in extra {
        response.push_str(&format!("{name}: {value}\r\n"));
    }
    response.push_str("\r\n");
    let mut bytes = response.into_bytes();
    if !head_only {
        bytes.extend_from_slice(body);
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn roundtrip_with_token(request: &str, token: Option<&str>) -> String {
        let (client, server) = tokio::io::duplex(64 * 1024);
        let token_owned = token.map(|s| s.to_string());
        let task =
            tokio::spawn(async move { serve(server, "0.0.0-test", token_owned.as_deref()).await });
        let (mut client_reader, mut client_writer) = tokio::io::split(client);
        client_writer.write_all(request.as_bytes()).await.unwrap();
        client_writer.shutdown().await.unwrap();
        let mut buf = Vec::new();
        client_reader.read_to_end(&mut buf).await.unwrap();
        task.await.unwrap().unwrap();
        String::from_utf8(buf).unwrap()
    }

    async fn roundtrip(request: &str) -> String {
        roundtrip_with_token(request, Some("secret123")).await
    }

    #[tokio::test]
    async fn root_does_not_serve_application_assets() {
        let response = roundtrip("GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").await;
        assert!(response.starts_with("HTTP/1.1 404 Not Found\r\n"));
    }

    #[tokio::test]
    async fn healthz_reports_version_and_auth_requirement() {
        let response = roundtrip("GET /healthz HTTP/1.1\r\n\r\n").await;
        assert!(response.contains("Content-Type: application/json\r\n"));
        assert!(response.contains("\"version\":\"0.0.0-test\""));
        assert!(response.contains("\"auth\":true"));
    }

    #[tokio::test]
    async fn healthz_reports_no_auth_when_token_is_none() {
        let response = roundtrip_with_token("GET /healthz HTTP/1.1\r\n\r\n", None).await;
        assert!(response.contains("\"auth\":false"));
    }

    #[tokio::test]
    async fn sessions_api_requires_auth() {
        let unauth = roundtrip("GET /api/v1/sessions HTTP/1.1\r\n\r\n").await;
        assert!(unauth.starts_with("HTTP/1.1 401 Unauthorized\r\n"));

        let authed =
            roundtrip("GET /api/v1/sessions HTTP/1.1\r\nAuthorization: Bearer secret123\r\n\r\n")
                .await;
        assert!(authed.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(authed.contains("\"sessions\":[]"));
    }

    #[tokio::test]
    async fn prompt_api_accepts_post_body() {
        let authed = roundtrip(
            "POST /api/v1/prompt HTTP/1.1\r\nAuthorization: Bearer secret123\r\nContent-Length: 18\r\n\r\n{\"prompt\":\"hello\"}",
        )
        .await;
        assert!(authed.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(authed.contains("\"status\":\"accepted\""));
    }

    #[tokio::test]
    async fn cors_options_returns_no_content() {
        let response = roundtrip("OPTIONS /api/v1/sessions HTTP/1.1\r\n\r\n").await;
        assert!(response.starts_with("HTTP/1.1 204 No Content\r\n"));
        assert!(response.contains("Access-Control-Allow-Origin: *"));
    }

    #[tokio::test]
    async fn unknown_routes_are_not_found() {
        let response = roundtrip("GET /no/such/route?ws=x HTTP/1.1\r\n\r\n").await;
        assert!(response.starts_with("HTTP/1.1 404 Not Found\r\n"));
    }

    #[tokio::test]
    async fn traversal_is_not_a_route() {
        let response = roundtrip("GET /../../etc/passwd HTTP/1.1\r\n\r\n").await;
        assert!(response.starts_with("HTTP/1.1 404 Not Found\r\n"));
    }

    #[tokio::test]
    async fn head_omits_the_body() {
        let response = roundtrip("HEAD /healthz HTTP/1.1\r\n\r\n").await;
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        let body_start = response.find("\r\n\r\n").unwrap() + 4;
        assert_eq!(&response[body_start..], "");
    }
}
