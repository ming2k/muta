#![cfg(feature = "reqwest-oracle")]
//! Shadow comparison: the owned transport and `reqwest` must agree.
//!
//! ADR-0200's P2 exit criterion is "byte-identical response corpus against the
//! `reqwest` path under shadow". This is that comparison, run hermetically: the
//! same scripted SSE server answers both clients, and both feed the *same* SSE
//! reassembly (`muta_llm_client::sse`). Any divergence in framing, chunking or
//! reassembly shows up as a diff here rather than in production.
//!
//! The owned path additionally produces a trace, so the test also asserts that a
//! realistic stream derives the timings the product will show.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use futures::StreamExt;
use http::Method;
use muta_contracts::{ProviderError, ProviderErrorKind};
use muta_llm_client::sse::{data_payloads, payloads_from_chunks};
use muta_net::{Client, ClientConfig, NetError, Pool, RequestHead, Target, TcpConnector};
use muta_trace::{
    AttemptRef, ConnectionInfo, EndpointRef, EventKind, Fidelity, FrameClass, Recorder,
    RequestTrace, TraceId, derive,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const FRAMES: usize = 24;
const FRAME_GAP: Duration = Duration::from_millis(3);

/// Serve `connections` sequential requests, one SSE stream each.
async fn serve_sse(listener: TcpListener, connections: usize) {
    for _ in 0..connections {
        let (mut socket, _) = listener.accept().await.expect("accept");
        serve_one(&mut socket).await;
    }
}

async fn serve_one(socket: &mut tokio::net::TcpStream) {
    let mut request = Vec::new();
    let mut buffer = [0u8; 1024];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = socket.read(&mut buffer).await.expect("read");
        if read == 0 {
            return;
        }
        request.extend_from_slice(&buffer[..read]);
    }
    socket
        .write_all(
            b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n",
        )
        .await
        .expect("head");
    socket.flush().await.expect("flush head");
    // Head first, then a prefill gap: the shape that keeps `server_ttft_us`
    // estimable.
    tokio::time::sleep(Duration::from_millis(20)).await;
    for index in 0..FRAMES {
        let frame = format!("data: {{\"i\":{index}}}\n\n");
        let chunk = format!("{:x}\r\n{frame}\r\n", frame.len());
        socket.write_all(chunk.as_bytes()).await.expect("chunk");
        socket.flush().await.expect("flush");
        tokio::time::sleep(FRAME_GAP).await;
    }
    let done = "data: [DONE]\n\n";
    socket
        .write_all(format!("{:x}\r\n{done}\r\n", done.len()).as_bytes())
        .await
        .expect("done");
    socket.write_all(b"0\r\n\r\n").await.expect("end");
    socket.flush().await.expect("flush end");
}

fn provider_error(error: NetError) -> ProviderError {
    ProviderError::new("Shadow", ProviderErrorKind::Transport, error.to_string())
}

#[tokio::test(flavor = "multi_thread")]
async fn both_transports_produce_identical_sse_payloads() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("addr").to_string();
    let server = tokio::spawn(serve_sse(listener, 2));

    let body = serde_json::json!({"model": "shadow", "stream": true}).to_string();

    // --- Path A: reqwest plus the production SSE reassembly ---
    let response = reqwest::Client::new()
        .post(format!("http://{address}/v1/chat/completions"))
        .header("content-type", "application/json")
        .body(body.clone())
        .send()
        .await
        .expect("reqwest send");
    assert_eq!(response.status(), 200);
    let reqwest_content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let status = response.status();
    let headers = response.headers().clone();
    let body_stream = response
        .bytes_stream()
        .map(|item| item.map_err(|error| muta_llm_client::transport_error("Shadow", error)))
        .boxed();
    let via_reqwest: Vec<String> = data_payloads(
        muta_llm_client::HttpResponse {
            status,
            headers,
            body: body_stream,
        },
        "Shadow",
    )
    .filter_map(|item| async move { item.ok() })
    .collect()
    .await;

    // --- Path B: the owned transport, feeding the same reassembly ---
    let recorder = Arc::new(Mutex::new(Recorder::start(4096)));
    let client = Client::new(
        TcpConnector::new(),
        Pool::default(),
        ClientConfig::default(),
    );
    let head = RequestHead::new(Method::POST, "/v1/chat/completions")
        .with_header("content-type", "application/json");
    let response = client
        .send(
            &Target::plain(&address),
            Arc::clone(&recorder),
            head,
            Some(Bytes::from(body)),
        )
        .await
        .expect("muta-net send");
    assert_eq!(response.head.status, http::StatusCode::OK);
    assert_eq!(
        response
            .head
            .headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        reqwest_content_type.as_deref(),
        "the two clients must agree on the head"
    );

    let token_recorder = Arc::clone(&recorder);
    let chunks = futures::stream::unfold(
        (response.body, token_recorder, false),
        |(mut body, recorder, failed)| async move {
            if failed {
                return None;
            }
            match body.next_chunk().await {
                Ok(Some(chunk)) => {
                    if chunk.windows(6).any(|window| window == b"data: ") {
                        recorder
                            .lock()
                            .expect("recorder")
                            .frame(FrameClass::Text, 1);
                    }
                    Some((Ok::<Bytes, NetError>(chunk), (body, recorder, false)))
                }
                Ok(None) => None,
                Err(error) => Some((Err(error), (body, recorder, true))),
            }
        },
    );
    let via_net: Vec<String> = payloads_from_chunks(chunks, "Shadow", provider_error)
        .filter_map(|item| async move { item.ok() })
        .collect()
        .await;

    assert_eq!(
        via_net, via_reqwest,
        "the owned transport must reassemble the same payloads as reqwest"
    );
    assert_eq!(via_net.len(), FRAMES, "every frame but [DONE] arrives");

    // The trace the product would show.
    let trace = RequestTrace {
        id: TraceId::new("shadow"),
        attempt: AttemptRef {
            round: 1,
            turn: 1,
            attempt: 1,
        },
        endpoint: EndpointRef {
            provider: "shadow".into(),
            model: "shadow".into(),
            authority: address.clone(),
        },
        connection: ConnectionInfo::default(),
        fidelity: Fidelity::l1(),
        log: recorder.lock().expect("recorder").log().clone(),
    };
    assert!(trace.log.first_of(EventKind::HeadComplete).is_some());
    let derived = derive(&trace);
    assert!(derived.dns_us.is_measured());
    assert!(derived.tcp_us.is_measured());
    assert!(derived.ttfb_us.is_measured());
    assert!(derived.first_byte_us.is_measured());
    assert!(derived.ttft_us.is_measured());
    assert!(
        derived.server_ttft_us.is_measured(),
        "{:?}",
        derived.server_ttft_us
    );
    assert!(derived.stream_us.is_measured());
    assert!(derived.tail_us.is_measured());
    assert!(
        derived.ttfb_us.value() < derived.ttft_us.value(),
        "head precedes token"
    );

    let _ = server.await;
}
