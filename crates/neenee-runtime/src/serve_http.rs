//! The plain-HTTP side of the daemon's single TCP port (ADR-0105).
//!
//! The daemon's TCP listener speaks two protocols, split by peeking at the
//! request head (`classify` in `serve.rs`): WebSocket upgrades go to the
//! control plane; everything else lands here — the embedded web panel's
//! static bundle plus a tiny `GET /healthz` probe the panel uses to tell
//! "daemon alive, auth required" apart from "nothing listening" (a browser
//! cannot distinguish WebSocket handshake failures).
//!
//! Deliberately minimal: GET/HEAD only, `Connection: close`, no chunked
//! encoding, no request bodies. Anything more belongs behind a real reverse
//! proxy.

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

/// Request-head size cap; a peer that never finishes its headers is cut off.
const HEAD_CAP: usize = 16 * 1024;

/// Content-Security-Policy for the panel's HTML. The bundle is self-hosted
/// and inline-style-free (Svelte scopes compile to the stylesheet); the only
/// runtime exception is `style` *attributes* (composer textarea autosize).
/// `connect-src` allows ws:/wss: beyond 'self' because the panel can be
/// pointed at a different daemon endpoint from the connection dialog.
const PANEL_CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self' ws: wss:; font-src 'self'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'";

/// Serve one plain-HTTP connection, then close it. `auth_required` is only
/// reported through `/healthz` (whether a token is needed is not a secret —
/// the token itself never crosses this path).
pub async fn serve<S>(stream: S, daemon_version: &str, auth_required: bool) -> std::io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);

    // Read the request head (request line + headers, terminated by an empty
    // line). Bodies are not read: the routes we serve have none.
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
    let head = String::from_utf8_lossy(&head);
    let mut parts = head.lines().next().unwrap_or("").split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/"));
    let path = target.split(['?', '#']).next().unwrap_or("/");

    let response = match (method, path) {
        ("GET" | "HEAD", "/healthz") => {
            let body = format!(
                "{{\"version\":\"{daemon_version}\",\"auth\":{auth_required},\"panel\":{}}}",
                neenee_web_assets::real_dist_embedded(),
            );
            build_response(
                "200 OK",
                "application/json",
                body.as_bytes(),
                method == "HEAD",
                &[],
            )
        }
        ("GET" | "HEAD", _) => {
            if path.split('/').any(|seg| seg == "..") {
                build_response("400 Bad Request", "text/plain", b"bad path", false, &[])
            } else {
                let asset = neenee_web_assets::lookup(path);
                let cache = if asset.immutable {
                    "public, max-age=31536000, immutable"
                } else {
                    "no-cache"
                };
                let mut extra: Vec<(&str, &str)> = vec![("Cache-Control", cache)];
                if asset.content_type.starts_with("text/html") {
                    extra.push(("Content-Security-Policy", PANEL_CSP));
                }
                build_response(
                    "200 OK",
                    asset.content_type,
                    asset.bytes,
                    method == "HEAD",
                    &extra,
                )
            }
        }
        _ => build_response(
            "405 Method Not Allowed",
            "text/plain",
            b"method not allowed",
            false,
            &[("Allow", "GET, HEAD")],
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
    use tokio::io::AsyncReadExt;

    /// Drive one HTTP request through `serve` over an in-memory duplex and
    /// collect the full response.
    async fn roundtrip(request: &str) -> String {
        let (client, server) = tokio::io::duplex(64 * 1024);
        let task = tokio::spawn(serve(server, "0.0.0-test", true));
        let (mut client_reader, mut client_writer) = tokio::io::split(client);
        client_writer.write_all(request.as_bytes()).await.unwrap();
        client_writer.shutdown().await.unwrap();
        let mut buf = Vec::new();
        client_reader.read_to_end(&mut buf).await.unwrap();
        task.await.unwrap().unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[tokio::test]
    async fn root_serves_the_panel_html_with_security_headers() {
        let response = roundtrip("GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").await;
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.contains("Content-Type: text/html; charset=utf-8\r\n"));
        assert!(response.contains("X-Content-Type-Options: nosniff\r\n"));
        assert!(response.contains("Content-Security-Policy: default-src 'self'"));
        assert!(response.contains("Cache-Control: no-cache\r\n"));
        assert!(response.contains("Connection: close\r\n"));
        assert!(response.contains("<html"));
    }

    #[tokio::test]
    async fn healthz_reports_version_auth_and_panel_flag() {
        let response = roundtrip("GET /healthz HTTP/1.1\r\n\r\n").await;
        assert!(response.contains("Content-Type: application/json\r\n"));
        assert!(response.contains("\"version\":\"0.0.0-test\""));
        assert!(response.contains("\"auth\":true"));
    }

    #[tokio::test]
    async fn spa_fallback_serves_index_for_unknown_routes() {
        let response = roundtrip("GET /no/such/route?ws=x HTTP/1.1\r\n\r\n").await;
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(response.contains("Content-Type: text/html; charset=utf-8\r\n"));
    }

    #[tokio::test]
    async fn traversal_is_rejected() {
        let response = roundtrip("GET /../../etc/passwd HTTP/1.1\r\n\r\n").await;
        assert!(response.starts_with("HTTP/1.1 400 Bad Request\r\n"));
    }

    #[tokio::test]
    async fn post_is_rejected() {
        let response = roundtrip("POST / HTTP/1.1\r\nContent-Length: 3\r\n\r\nabc").await;
        assert!(response.starts_with("HTTP/1.1 405 Method Not Allowed\r\n"));
        assert!(response.contains("Allow: GET, HEAD\r\n"));
    }

    #[tokio::test]
    async fn head_omits_the_body() {
        let response = roundtrip("HEAD / HTTP/1.1\r\n\r\n").await;
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        let body_start = response.find("\r\n\r\n").unwrap() + 4;
        assert_eq!(&response[body_start..], "");
    }
}
