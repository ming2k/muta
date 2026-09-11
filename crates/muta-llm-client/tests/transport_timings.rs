//! Attempt telemetry belongs to the attempt that caused it (ADR-0232).
//!
//! The transport records a full trace for every request; these tests hold down
//! the property that the timings derived from that trace reach the *attempt*
//! they describe — and only that attempt. They replaced a per-transport slot
//! whose ownership was unrepresentable, so the concurrency test here is the
//! regression test for the defect that motivated the change: two attempts in
//! flight on one shared provider must each get their own numbers.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use futures::StreamExt;

use muta_contracts::{
    InstructionBundle, Message, ModelRequest, Provider, ProviderErrorKind, ProviderStreamEvent,
    Role, TransportObservation, TransportTelemetry, TransportTimings,
};
use muta_llm_client::{Client, MutaNetEgress, OpenAiChatCompletionsProvider};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const FRAMES: usize = 6;

/// Ack one request head off `socket`, returning false at EOF.
async fn read_head(socket: &mut TcpStream) -> bool {
    let mut head = Vec::new();
    let mut buffer = [0u8; 1024];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        match socket.read(&mut buffer).await {
            Ok(0) | Err(_) => return false,
            Ok(read) => head.extend_from_slice(&buffer[..read]),
        }
    }
    true
}

/// Write one chunked SSE response.
async fn write_response(socket: &mut TcpStream) {
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

/// Serve `connections` accepted connections, each answering
/// `requests_per_connection` requests.
///
/// Every connection gets its own task, so accepting is never blocked behind
/// writing a response — without that, a test that keeps one attempt in flight
/// while opening a second on the same provider would deadlock here rather than
/// at the transport. `requests_per_connection` > 1 is what exercises keep-alive
/// reuse; the socket is dropped once its planned responses are written, so the
/// server never waits on a client that is still holding it.
async fn serve(listener: TcpListener, connections: usize, requests_per_connection: usize) {
    let mut tasks = Vec::with_capacity(connections);
    for _ in 0..connections {
        let (mut socket, _) = listener.accept().await.expect("accept");
        tasks.push(tokio::spawn(async move {
            for _ in 0..requests_per_connection {
                if !read_head(&mut socket).await {
                    break;
                }
                write_response(&mut socket).await;
            }
        }));
    }
    for task in tasks {
        let _ = task.await;
    }
}

async fn listener_url() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("addr").to_string();
    (listener, format!("http://{address}/v1/chat/completions"))
}

/// A request carrying `telemetry`, exactly as the attempt's owner stamps it.
fn request_with(telemetry: TransportTelemetry) -> ModelRequest {
    let mut request = ModelRequest::with_instructions_and_tools(
        InstructionBundle::default(),
        vec![Message::new(Role::User, "hello")],
        &[],
    );
    request.transport_telemetry = telemetry;
    request
}

fn provider_for(url: &str) -> OpenAiChatCompletionsProvider {
    let mut provider =
        OpenAiChatCompletionsProvider::with_base_url("test-key".into(), "gpt-4o-mini".into(), url);
    provider.client = Client::with_egress(Arc::new(MutaNetEgress::plain()));
    provider
}

async fn open(
    provider: &OpenAiChatCompletionsProvider,
    telemetry: TransportTelemetry,
) -> muta_contracts::ProviderEventStream {
    provider
        .stream_chat_events(request_with(telemetry))
        .await
        .expect("stream opens")
}

async fn drain(provider: &OpenAiChatCompletionsProvider, telemetry: TransportTelemetry) {
    let events: Vec<_> = open(provider, telemetry)
        .await
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .map(|item| item.expect("event"))
        .collect();
    assert_eq!(events.len(), FRAMES + 1, "{events:?}");
}

/// A cold attempt reports every phase it actually paid, plus an anchor a caller
/// can re-anchor against its own clock.
#[tokio::test(flavor = "multi_thread")]
async fn a_cold_attempt_reports_its_connection_phases() {
    let (listener, url) = listener_url().await;
    let server = tokio::spawn(serve(listener, 1, 1));

    let provider = provider_for(&url);
    let telemetry = TransportTelemetry::new();
    assert!(
        telemetry.read().is_none(),
        "a fresh handle holds nothing until the transport fills it"
    );

    drain(&provider, telemetry.clone()).await;

    let timings = telemetry
        .read()
        .expect("the attempt's own handle must hold the attempt's timings");
    assert_eq!(
        timings.observation,
        TransportObservation::ColdConnection,
        "a fresh socket paid a connection: {timings:?}"
    );
    assert!(
        timings.dispatch_at.is_some(),
        "the anchor is what makes every offset in here interpretable"
    );
    // A connection to a literal address skips resolution, so DNS may legitimately
    // be absent; the TCP phase cannot be.
    assert!(timings.tcp_us.is_some(), "no TCP phase: {timings:?}");
    assert!(timings.connected_us.is_some(), "no connect instant: {timings:?}");
    assert!(
        timings.request_sent_us.is_some(),
        "no upload anchor: {timings:?}"
    );
    assert!(
        timings.stream_ready_us.is_some(),
        "no response head: {timings:?}"
    );

    // The ordering the latency timeline depends on: the connection is ready
    // before the request is written, which completes before the head returns.
    let connected = timings.connected_us.unwrap();
    let sent = timings.request_sent_us.unwrap();
    let ready = timings.stream_ready_us.unwrap();
    assert!(
        connected <= sent && sent <= ready,
        "connect {connected} → sent {sent} → ready {ready} must be monotonic"
    );

    let _ = server.await;
}

/// A second attempt on the same transport reuses the pooled socket, and the
/// regime is read from the trace's own reuse event rather than guessed from the
/// fields that happen to be absent.
#[tokio::test(flavor = "multi_thread")]
async fn a_pooled_attempt_is_reported_as_pooled_not_as_silence() {
    let (listener, url) = listener_url().await;
    let server = tokio::spawn(serve(listener, 1, 2));

    let provider = provider_for(&url);
    let cold = TransportTelemetry::new();
    let warm = TransportTelemetry::new();

    drain(&provider, cold.clone()).await;
    drain(&provider, warm.clone()).await;

    let cold = cold.read().expect("cold attempt");
    assert_eq!(cold.observation, TransportObservation::ColdConnection);

    let warm = warm.read().expect("second attempt");
    assert_eq!(
        warm.observation,
        TransportObservation::PooledConnection,
        "the second attempt reused the pooled socket: {warm:?}"
    );
    // Pooled means the phases genuinely did not happen — not that they went
    // unrecorded. `observation` is what tells those two apart.
    assert!(warm.dns_us.is_none() && warm.tcp_us.is_none() && warm.tls_us.is_none());
    assert!(
        warm.connected_us.is_some(),
        "the pool handing the socket over is itself an event: {warm:?}"
    );
    assert!(
        warm.stream_ready_us.is_some(),
        "a reused socket still received a response head: {warm:?}"
    );

    let _ = server.await;
}

/// Two attempts in flight on one shared provider, each carrying its own handle.
///
/// This is the regression test for the defect ADR-0232 removes. A per-transport
/// slot cannot say whose numbers it holds: with one slot, the attempt that seals
/// second overwrites the first's timings, and at most one of these two reads
/// could come back populated. With attempt-owned handles both must, each with
/// its own dispatch anchor.
#[tokio::test(flavor = "multi_thread")]
async fn concurrent_attempts_on_one_provider_never_share_telemetry() {
    let (listener, url) = listener_url().await;
    // Two connections: the first attempt holds one open, so the second attempt
    // must open its own. Accepting concurrently is what lets both be in flight.
    let server = tokio::spawn(serve(listener, 2, 1));

    // One provider, shared by both attempts — as a parent and its subagent do.
    let provider = provider_for(&url);
    let first = TransportTelemetry::new();
    let second = TransportTelemetry::new();

    // Both attempts are opened before either is finished.
    let mut first_stream = open(&provider, first.clone()).await;
    let mut second_stream = open(&provider, second.clone()).await;

    // Interleave the reads so neither attempt is the "last" one to seal.
    let first_event = first_stream.next().await.expect("first").expect("event");
    let second_event = second_stream.next().await.expect("second").expect("event");
    assert!(matches!(first_event, ProviderStreamEvent::TextDelta(_)));
    assert!(matches!(second_event, ProviderStreamEvent::TextDelta(_)));

    drop(first_stream);
    drop(second_stream);

    let first = first.read().expect("the first attempt's handle");
    let second = second.read().expect("the second attempt's handle");

    // Each attempt reports the work it did, and the two are distinguishable
    // attempts rather than one attempt's numbers read twice.
    assert!(
        first.stream_ready_us.is_some() && second.stream_ready_us.is_some(),
        "both attempts observed a response head: {first:?} / {second:?}"
    );
    assert!(
        first.dispatch_at.is_some() && second.dispatch_at.is_some(),
        "each attempt carries its own dispatch anchor: {first:?} / {second:?}"
    );
    assert_ne!(
        first.dispatch_at, second.dispatch_at,
        "two attempts dispatched at two instants, so two anchors"
    );

    let _ = server.await;
}

/// The production path: a protocol adapter stops reading at the provider's
/// completion marker, so the body is never driven to EOF. The timings must
/// still be published, or every streamed turn would report nothing.
#[tokio::test(flavor = "multi_thread")]
async fn an_abandoned_body_still_publishes_its_timings() {
    let (listener, url) = listener_url().await;
    let server = tokio::spawn(serve(listener, 1, 1));

    let provider = provider_for(&url);
    let telemetry = TransportTelemetry::new();
    {
        let mut stream = open(&provider, telemetry.clone()).await;
        // One event, then walk away — no EOF, no `[DONE]`.
        let first = stream.next().await.expect("first").expect("event");
        assert!(matches!(first, ProviderStreamEvent::TextDelta(_)));
    }

    let timings = telemetry
        .read()
        .expect("an abandoned body must still report what was observed");
    assert_eq!(timings.observation, TransportObservation::ColdConnection);
    assert!(timings.tcp_us.is_some());
    assert!(timings.stream_ready_us.is_some());

    let _ = server.await;
}

/// A connection that never completes must still publish what it did observe:
/// the phases that were paid before the failure are the only record of how far
/// it got, and they are what a user diagnoses a refused or timing-out endpoint
/// with.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_connection_still_publishes_what_it_observed() {
    // Bind, then drop, so the port is (almost certainly) refused rather than
    // merely slow.
    let address = {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        listener.local_addr().expect("addr").to_string()
    };
    let url = format!("http://{address}/v1/chat/completions");

    let provider = provider_for(&url);
    let telemetry = TransportTelemetry::new();
    let error = match provider.stream_chat_events(request_with(telemetry.clone())).await {
        Ok(_) => panic!("nothing is listening on {address}, yet the stream opened"),
        Err(error) => error,
    };
    assert_eq!(
        error.kind(),
        ProviderErrorKind::Transport,
        "a refused connection is a transport failure"
    );

    let timings = telemetry.read().expect(
        "the transport watched this attempt fail, and must say so rather than fall silent",
    );
    assert!(
        timings.dispatch_at.is_some(),
        "the anchor is present even when the attempt failed: {timings:?}"
    );
    // Resolution was genuinely attempted, so the regime is `ColdConnection` —
    // and the phase that completed is reported. A refused TCP connect leaves no
    // completion, so `connected_us` stays absent rather than pointing at an
    // instant that never arrived.
    assert_eq!(
        timings.observation,
        TransportObservation::ColdConnection,
        "a cold connection was attempted: {timings:?}"
    );
    assert!(
        timings.connected_us.is_none(),
        "no connection completed: {timings:?}"
    );
    assert!(timings.tcp_us.is_none(), "TCP never completed: {timings:?}");
    assert!(timings.request_sent_us.is_none() && timings.stream_ready_us.is_none());
}

/// A handle nothing filled reads as unobserved — never as reuse, and never as a
/// measured zero.
#[test]
fn an_unfilled_handle_reports_nothing_rather_than_a_regime() {
    let unfilled = TransportTelemetry::new();
    assert!(unfilled.read().is_none());

    // The same fact on the ledger's side of the seam: silence is never a claim
    // of reuse, and an unsampled socket never yields a retransmit count.
    let performance = muta_contracts::RequestPerformance::default();
    assert_eq!(performance.observation, TransportObservation::Unreported);
    assert!(!performance.transport_observed());
    assert_eq!(performance.pooled_connection(), None);
    assert!(!performance.tcp_info_sampled());
    assert_eq!(performance.connected_us, None);

    assert_eq!(
        TransportTimings::default().observation,
        TransportObservation::Unreported
    );
}
