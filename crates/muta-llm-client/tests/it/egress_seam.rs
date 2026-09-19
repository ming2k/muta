//! One provider, two transports.
//!
//! The point of the [`muta_llm_client::Egress`] seam is that switching the
//! transport does not touch protocol code. This test proves it: the *same*
//! `OpenAiChatCompletionsProvider` — same request building, same auth, same
//! stream parsing — runs once on `reqwest` and once on the owned transport, and
//! emits identical events.

#![cfg(feature = "reqwest-oracle")]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use futures::StreamExt;

use muta_contracts::{
    InstructionBundle, Message, ModelRequest, Provider, ProviderStreamEvent, Role,
};
use muta_llm_client::{
    Client, Egress, MutaNetEgress, OpenAiChatCompletionsProvider, ReqwestEgress,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const FRAMES: usize = 6;

/// Serve `connections` SSE responses, one per accepted connection.
async fn serve(listener: TcpListener, connections: usize) {
    for _ in 0..connections {
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
        for index in 0..FRAMES {
            let payload = format!(
                "data: {{\"id\":\"x\",\"choices\":[{{\"delta\":{{\"content\":\"tok{index}\"}},\"index\":0}}]}}\n\n"
            );
            socket
                .write_all(format!("{:x}\r\n{payload}\r\n", payload.len()).as_bytes())
                .await
                .expect("chunk");
            socket.flush().await.expect("flush");
        }
        let done = "data: [DONE]\n\n";
        socket
            .write_all(format!("{:x}\r\n{done}\r\n", done.len()).as_bytes())
            .await
            .expect("done");
        socket.write_all(b"0\r\n\r\n").await.expect("end");
        socket.flush().await.expect("flush end");
    }
}

fn request() -> ModelRequest {
    ModelRequest::with_instructions_and_tools(
        InstructionBundle::default(),
        vec![Message::new(Role::User, "hello")],
        &[],
    )
}

async fn events_with(egress: Arc<dyn Egress>, url: &str) -> Vec<ProviderStreamEvent> {
    let mut provider =
        OpenAiChatCompletionsProvider::with_base_url("test-key".into(), "gpt-4o-mini".into(), url);
    provider.client = Client::with_egress(egress);
    let stream = provider
        .stream_chat_events(request())
        .await
        .expect("stream opens");
    stream
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(|item| item.expect("event"))
        .collect()
}

/// Text deltas, in order — the part of the event stream a user sees.
fn texts(events: &[ProviderStreamEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            ProviderStreamEvent::TextDelta(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_same_provider_emits_the_same_events_on_both_transports() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("addr").to_string();
    let server = tokio::spawn(serve(listener, 2));
    let url = format!("http://{address}/v1/chat/completions");

    let reqwest_egress: Arc<dyn Egress> = Arc::new(ReqwestEgress::new(reqwest::Client::new()));
    let via_reqwest = events_with(reqwest_egress, &url).await;
    let via_owned = events_with(Arc::new(MutaNetEgress::plain()), &url).await;

    assert_eq!(
        texts(&via_owned),
        texts(&via_reqwest),
        "the owned transport must emit the same deltas as reqwest"
    );
    assert_eq!(texts(&via_reqwest).len(), FRAMES);
    assert_eq!(via_owned, via_reqwest, "the full event streams must match");

    let _ = server.await;
}
