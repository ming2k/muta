//! Cross-process attach smoke: a minimal WebSocket client that exercises a
//! RUNNING `neenee-server` process end-to-end — discovery record, handshake
//! history replay, and one request/response round-trip — without the
//! interactive TUI.
//!
//! Usage: `attach_smoke <project_root>`
//!
//! Expects the server for `<project_root>` to be already running (its
//! discovery record must exist; XDG env must match the server's). Exits 0 on
//! success, 1 on any failure. This duplicates the control flow of the cli's
//! `remote::connect` on purpose: it verifies the real wire protocol against
//! the real server process, independent of the client implementation.

use std::path::PathBuf;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use neenee_transport::serve::Wire;
use neenee_transport::serve_discovery::{self, Discovery};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message as WsMessage;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let project_root = PathBuf::from(match std::env::args().nth(1) {
        Some(arg) => arg,
        None => {
            eprintln!("usage: attach_smoke <project_root>");
            std::process::exit(2);
        }
    });
    if let Err(error) = run(&project_root).await {
        eprintln!("attach_smoke: FAIL: {error}");
        std::process::exit(1);
    }
    println!("attach_smoke: OK");
}

async fn run(project_root: &std::path::Path) -> Result<(), String> {
    // 1. Discovery: the record the running server wrote at startup.
    let path = serve_discovery::discovery_path(project_root);
    let bytes = std::fs::read(&path)
        .map_err(|e| format!("no discovery record at {}: {e}", path.display()))?;
    let info: Discovery = serde_json::from_slice(&bytes)
        .map_err(|e| format!("corrupt discovery record at {}: {e}", path.display()))?;
    println!(
        "attach_smoke: discovered pid={} port={} session={} token={}",
        info.pid,
        info.port,
        info.session_id,
        info.token.is_some()
    );

    // 2. Handshake (bearer when the record carries one), then the one-shot
    //    History frame must be the first thing we read.
    let url = format!("ws://127.0.0.1:{}/", info.port);
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|e| format!("bad ws url: {e}"))?;
    if let Some(token) = &info.token {
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| format!("bad bearer token: {e}"))?;
        request.headers_mut().insert("Authorization", value);
    }
    let (ws, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| format!("ws connect to {url}: {e}"))?;
    let (mut sink, mut source) = ws.split();

    let (session_id, history_len) = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(frame) = source.next().await {
            if let Ok(WsMessage::Text(text)) = frame
                && let Ok(Wire::History {
                    session_id,
                    messages,
                    ..
                }) = serde_json::from_str::<Wire>(&text)
            {
                return Ok((session_id, messages.len()));
            }
        }
        Err("connection closed before history".to_string())
    })
    .await
    .map_err(|_| "timed out waiting for history".to_string())??;

    if session_id != info.session_id {
        return Err(format!(
            "handshake session {session_id} != discovery session {}",
            info.session_id
        ));
    }
    println!("attach_smoke: history replay: session={session_id} messages={history_len}");

    // 3. One request round-trip: the hosted driver has no provider configured
    //    in the isolated smoke env, but it must still answer with SOME
    //    response (an up-front refusal notice) — proving the request reached
    //    the live session driver and its events flow back.
    let chat = serde_json::json!({
        "type": "Request",
        "Chat": { "text": "ping from attach_smoke", "images": [] }
    });
    sink.send(WsMessage::Text(chat.to_string().into()))
        .await
        .map_err(|e| format!("ws send: {e}"))?;

    tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(frame) = source.next().await {
            if let Ok(WsMessage::Text(text)) = frame
                && let Ok(Wire::Response { response }) = serde_json::from_str::<Wire>(&text)
            {
                let preview = serde_json::to_string(&response).unwrap_or_default();
                let preview: String = preview.chars().take(120).collect();
                println!("attach_smoke: response received: {preview}");
                return Ok(());
            }
        }
        Err("connection closed before any response".to_string())
    })
    .await
    .map_err(|_| "timed out waiting for a response".to_string())?
}
