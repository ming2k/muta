//! Runtime shadow against a live-shaped server.
//!
//! The hermetic comparison in `shadow.rs` proves the two transports agree on one
//! scripted stream. These tests prove the *runtime* wrapper behaves: it reports
//! agreement, it detects a divergence, and it never lets a shadow failure escape
//! into the caller.

#![cfg(feature = "net-shadow")]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::time::Duration;

use bytes::Bytes;
use http::Method;
use muta_llm_client::RequestParts;
use muta_llm_client::shadow::run;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Serve `connections` SSE responses, one per accepted connection. `divergent`
/// makes the second response carry different payloads.
async fn serve(listener: TcpListener, connections: usize, divergent: bool) {
    for index in 0..connections {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut request = Vec::new();
        let mut buffer = [0u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = socket.read(&mut buffer).await.expect("read");
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
        }
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n",
            )
            .await
            .expect("head");
        let label = if divergent && index == 1 {
            "different"
        } else {
            "same"
        };
        for frame in 0..6 {
            let payload = format!("data: {{\"i\":{frame},\"who\":\"{label}\"}}\n\n");
            socket
                .write_all(format!("{:x}\r\n{payload}\r\n", payload.len()).as_bytes())
                .await
                .expect("chunk");
            socket.flush().await.expect("flush");
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        socket.write_all(b"0\r\n\r\n").await.expect("end");
        socket.flush().await.expect("flush end");
    }
}

fn request_for(address: &str) -> RequestParts {
    RequestParts {
        label: "Shadow",
        url: format!("http://{address}/v1/chat/completions"),
        method: Method::POST,
        headers: http::HeaderMap::new(),
        body: Some(Bytes::from_static(
            b"{\"model\":\"shadow\",\"stream\":true}",
        )),
        timeout: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_runtime_shadow_reports_agreement() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("addr").to_string();
    let server = tokio::spawn(serve(listener, 2, false));

    let report = run(request_for(&address)).await;
    assert!(report.agrees(), "{report:?}");
    assert!(report.payloads_identical(), "{report:?}");
    assert_eq!(report.reqwest_payloads, 6);
    assert_eq!(report.net_payloads, 6);
    assert_eq!(report.net_status, Some(200));
    let trace = report.trace.expect("owned run produced a trace");
    let derived = netune_trace::derive(&trace);
    // The egress trace carries transport scopes. Output tokens are the protocol
    // adapter's to count, so `ttft_us` is legitimately absent here — asserting
    // it would push protocol knowledge into the transport.
    assert!(derived.ttfb_us.is_measured());
    assert!(derived.first_byte_us.is_measured());
    assert!(
        trace
            .log
            .first_of(netune_trace::EventKind::BodyEnd)
            .is_some()
    );
    let _ = server.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_runtime_shadow_detects_a_divergence() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("addr").to_string();
    let server = tokio::spawn(serve(listener, 2, true));

    let report = run(request_for(&address)).await;
    assert!(report.agrees(), "both transports completed: {report:?}");
    assert!(
        !report.payloads_identical(),
        "a differing payload must be reported: {report:?}"
    );
    assert_eq!(report.divergence, Some(0));
    assert_eq!(report.reqwest_payloads, report.net_payloads);
    let _ = server.await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shadow_against_a_dead_server_fails_quietly() {
    // Nothing is listening: the report must describe the failure instead of
    // propagating it.
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("addr").to_string();
    drop(listener);

    let report = run(request_for(&address)).await;
    assert!(!report.agrees());
    assert!(
        report.reqwest_error.is_some() || report.net_error.is_some(),
        "{report:?}"
    );
}
