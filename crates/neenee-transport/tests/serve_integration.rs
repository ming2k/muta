#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use neenee_core::{AgentRequest, AgentResponse, RoundEvent};
use neenee_persistence::session::SessionStore;
use neenee_transport::registry::{HostedSession, SessionRegistry};
use neenee_transport::serve::{self, AttachAction, Wire};
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::tungstenite::Message as WsMessage;

async fn prehosted(
    session: Arc<SessionStore>,
) -> (
    Arc<SessionRegistry>,
    mpsc::UnboundedReceiver<AgentRequest>,
    broadcast::Sender<AgentResponse>,
) {
    let (req_tx, req_rx) = mpsc::unbounded_channel::<AgentRequest>();
    let (bc_tx, _) = broadcast::channel::<AgentResponse>(1024);
    let registry = Arc::new(SessionRegistry::prehost_only());
    registry
        .host(HostedSession {
            session,
            req_tx,
            events: bc_tx.clone(),
            cancel: tokio_util::sync::CancellationToken::new(),
        })
        .await;
    (registry, req_rx, bc_tx)
}

#[tokio::test]
async fn test_select_then_attach_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    let session = Arc::new(SessionStore::load_for_project(tmp.path().to_path_buf()));
    let session_id = session.id().await;
    let (registry, mut req_rx, bc_tx) = prehosted(session).await;

    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.port.await.unwrap();
    let _ = handle;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    let select = serde_json::to_string(&Wire::Select {
        action: AttachAction::Attach(None),
    })
    .unwrap();
    ws.send(WsMessage::Text(select.into())).await.unwrap();

    let welcome_msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let welcome: Wire = serde_json::from_str(welcome_msg.to_text().unwrap_or("")).unwrap();
    match welcome {
        Wire::Welcome { session_id: id, .. } => assert_eq!(id, session_id),
        other => panic!("expected Welcome, got {other:?}"),
    }

    ws.send(WsMessage::Text(
        serde_json::json!({"type":"Request","Chat":{"text":"hi","images":[]}})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let req = tokio::time::timeout(Duration::from_secs(2), req_rx.recv())
        .await
        .unwrap()
        .unwrap();
    match req {
        AgentRequest::Chat { text, .. } => assert_eq!(text, "hi"),
        other => panic!("{other:?}"),
    }

    let _ = bc_tx.send(AgentResponse::Round {
        session_id: session_id.clone(),
        event: RoundEvent::Text("back".into()),
    });
    let resp_msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let resp: Wire = serde_json::from_str(resp_msg.to_text().unwrap_or("")).unwrap();
    match resp {
        Wire::Response {
            response: AgentResponse::Round { event, .. },
        } => match event {
            RoundEvent::Text(t) => assert_eq!(t, "back"),
            o => panic!("{o:?}"),
        },
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn unknown_id_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let session = Arc::new(SessionStore::load_for_project(tmp.path().to_path_buf()));
    let (registry, _req_rx, _bc_tx) = prehosted(session).await;
    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.port.await.unwrap();
    let _ = handle;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    let select = serde_json::to_string(&Wire::Select {
        action: AttachAction::Attach(Some("nope".into())),
    })
    .unwrap();
    ws.send(WsMessage::Text(select.into())).await.unwrap();
    let frame = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let parsed: Wire = serde_json::from_str(frame.to_text().unwrap_or("")).unwrap();
    match parsed {
        Wire::Error { message } => assert!(message.contains("nope")),
        other => panic!("expected Error, got {other:?}"),
    }
}
