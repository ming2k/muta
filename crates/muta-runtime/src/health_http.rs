//! The generic health-probe side of the daemon's TCP port.
//!
//! The daemon's TCP listener speaks two protocols, split by peeking at the
//! request head (`classify` in `serve.rs`): WebSocket upgrades go to the
//! control plane; plain HTTP lands here. The only route is `GET /healthz`.
//! Frontend assets are deployed by their applications, never embedded in the
//! daemon.
//!
//! Deliberately minimal: GET/HEAD only, `Connection: close`, no chunked
//! encoding, no request bodies. Anything more belongs behind a real reverse
//! proxy.

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

/// Request-head size cap; a peer that never finishes its headers is cut off.
const HEAD_CAP: usize = 16 * 1024;

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
            let body = format!("{{\"version\":\"{daemon_version}\",\"auth\":{auth_required}}}");
            build_response(
                "200 OK",
                "application/json",
                body.as_bytes(),
                method == "HEAD",
                &[],
            )
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
    async fn post_is_rejected() {
        let response = roundtrip("POST / HTTP/1.1\r\nContent-Length: 3\r\n\r\nabc").await;
        assert!(response.starts_with("HTTP/1.1 405 Method Not Allowed\r\n"));
        assert!(response.contains("Allow: GET, HEAD\r\n"));
    }

    #[tokio::test]
    async fn head_omits_the_body() {
        let response = roundtrip("HEAD /healthz HTTP/1.1\r\n\r\n").await;
        assert!(response.starts_with("HTTP/1.1 200 OK\r\n"));
        let body_start = response.find("\r\n\r\n").unwrap() + 4;
        assert_eq!(&response[body_start..], "");
    }
}
