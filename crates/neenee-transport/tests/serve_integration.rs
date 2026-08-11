#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use neenee_core::{
    AgentRequest, AgentResponse, MirrorHello, MonitorAction, MonitorEvent, MonitoredSession,
    RoundEvent, SessionHosting, SessionStatus,
};
use neenee_persistence::session::SessionStore;
use neenee_transport::monitor::MonitorTracker;
use neenee_transport::registry::{HostedSession, SessionRegistry};
use neenee_transport::serve::{self, AttachAction, Wire};
use tokio::sync::{Mutex, broadcast, mpsc};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

fn idle_base(id: String) -> neenee_core::MonitoredSession {
    neenee_core::MonitoredSession {
        id,
        overview: String::new(),
        created_at: 0,
        updated_at: 0,
        message_count: 0,
        status: neenee_core::SessionStatus::Idle,
        hosting: neenee_core::SessionHosting::Hosted,
        round: 0,
        turn: None,
        output_tokens: 0,
        elapsed_ms: 0,
        current_tool: None,
        activity: None,
        context_tokens: None,
        note: None,
        project_root: String::new(),
        wip: None,
    }
}

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
    let base = idle_base(session.id().await);
    let tracker = Arc::new(Mutex::new(MonitorTracker::bootstrap(
        base,
        neenee_core::SessionStatus::Idle,
    )));
    // Mimic the registry's broadcast-tap: fold every emitted response into the
    // tracker, publish a monitor diff, and buffer the attach-sync events, so
    // attach tests exercise the same path a real hosted session would take.
    let tap_tracker = tracker.clone();
    let mut tap_rx = bc_tx.subscribe();
    let registry_for_tap = registry.clone();
    let sync_buffer = Arc::new(Mutex::new(
        std::collections::VecDeque::<AgentResponse>::new(),
    ));
    let sync_buffer_for_tap = sync_buffer.clone();
    tokio::spawn(async move {
        while let Ok(response) = tap_rx.recv().await {
            let row = {
                let mut guard = tap_tracker.lock().await;
                guard.observe(&response);
                guard.row()
            };
            let _ = registry_for_tap.publish_for_test(MonitorEvent::SessionUpdated(row));
            if matches!(
                response,
                AgentResponse::ProviderSwitched { .. }
                    | AgentResponse::ProviderPicker(_)
                    | AgentResponse::ProviderKeys(_)
            ) {
                sync_buffer_for_tap.lock().await.push_back(response);
            }
        }
    });
    registry
        .host(HostedSession {
            project_root: std::path::PathBuf::from("/tmp/neenee-test-project"),
            session,
            req_tx,
            events: bc_tx.clone(),
            cancel: tokio_util::sync::CancellationToken::new(),
            tracker,
            sync_buffer,
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

    let handle = serve::start_server(serve::ServeOptions::default(), registry);
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

    // The frame right after the welcome is the attach-time task-list sync
    // (empty here: this session has no todos).
    let sync_msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match serde_json::from_str::<Wire>(sync_msg.to_text().unwrap_or("")).unwrap() {
        Wire::Response {
            response:
                AgentResponse::Round {
                    event: RoundEvent::TodosUpdated(list),
                    ..
                },
        } => assert!(list.items.is_empty()),
        other => panic!("expected TodosUpdated sync, got {other:?}"),
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

/// Attach-time state sync: a client that attaches to a session with a
/// persisted task list receives it as a `TodosUpdated` round event right
/// after the welcome — otherwise its todo panel would stay empty until the
/// model next touched the list (resume loses the restored todos).
#[tokio::test]
async fn attach_receives_restored_todos_after_welcome() {
    let tmp = tempfile::tempdir().unwrap();
    let session = Arc::new(SessionStore::load_for_project(tmp.path().to_path_buf()));
    let session_id = session.id().await;

    // Give the session content so its file persists, then a non-empty list.
    session
        .replace_messages(vec![neenee_core::Message::new(
            neenee_core::Role::User,
            "hello",
        )])
        .await
        .unwrap();
    let mut todos = neenee_core::TodoList::new();
    todos.items.push(neenee_core::TodoItem {
        id: neenee_core::TodoId(1),
        content: "restored task".to_string(),
        status: neenee_core::TodoStatus::InProgress,
        created_at: 1,
        updated_at: 1,
    });
    session.set_todos(todos.clone()).await.unwrap();

    let (registry, _req_rx, _bc_tx) = prehosted(session).await;
    let handle = serve::start_server(serve::ServeOptions::default(), registry);
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
    match serde_json::from_str::<Wire>(welcome_msg.to_text().unwrap_or("")).unwrap() {
        Wire::Welcome { session_id: id, .. } => assert_eq!(id, session_id),
        other => panic!("expected Welcome, got {other:?}"),
    }

    // The very next frame must be the restored task list.
    let todos_msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match serde_json::from_str::<Wire>(todos_msg.to_text().unwrap_or("")).unwrap() {
        Wire::Response {
            response:
                AgentResponse::Round {
                    session_id: id,
                    event: RoundEvent::TodosUpdated(list),
                },
        } => {
            assert_eq!(id, session_id);
            assert_eq!(list, todos);
        }
        other => panic!("expected TodosUpdated restore, got {other:?}"),
    }
}

/// Attach-time state sync (hint bar): a provider/model switch emitted while no
/// client is attached is buffered and replayed to the next client that
/// attaches, so the TUI's hint bar (model name, reasoning effort, `@instance`)
/// hydrates immediately instead of staying blank until the next mutation.
#[tokio::test]
async fn attach_receives_buffered_provider_state_after_welcome() {
    let tmp = tempfile::tempdir().unwrap();
    let session = Arc::new(SessionStore::load_for_project(tmp.path().to_path_buf()));
    let (registry, _req_rx, bc_tx) = prehosted(session).await;

    // Emit the startup provider sync BEFORE any client subscribes — this is
    // the production scenario: the driver broadcasts it at session start, when
    // the only subscriber is the registry tap, so a later attacher would
    // otherwise never see it.
    bc_tx
        .send(AgentResponse::ProviderSwitched {
            provider: "111xianyu".to_string(),
            model: "k3".to_string(),
        })
        .unwrap();
    // Give the tap task a moment to fold the event into the sync buffer before
    // the client attaches (the tap runs on its own task).
    tokio::time::sleep(Duration::from_millis(50)).await;

    let handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.port.await.unwrap();
    let _ = handle;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    ws.send(WsMessage::Text(
        serde_json::to_string(&Wire::Select {
            action: AttachAction::Attach(None),
        })
        .unwrap()
        .into(),
    ))
    .await
    .unwrap();

    // Skip the welcome, then collect the sync frames that follow.
    let _welcome = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    let mut saw_provider_switched = false;
    // The attach sync replays buffered provider events; the todos sync rides
    // the same prefix. Read a few frames and confirm the switch is among them.
    for _ in 0..4 {
        let msg = match tokio::time::timeout(Duration::from_secs(2), ws.next()).await {
            Ok(Some(Ok(m))) => m,
            _ => break,
        };
        if let Ok(Wire::Response { response }) =
            serde_json::from_str::<Wire>(msg.to_text().unwrap_or(""))
            && let AgentResponse::ProviderSwitched { provider, model } = response
        {
            assert_eq!(provider, "111xianyu");
            assert_eq!(model, "k3");
            saw_provider_switched = true;
            break;
        }
    }
    assert!(
        saw_provider_switched,
        "attach must replay the buffered ProviderSwitched so the hint bar hydrates"
    );
}

#[tokio::test]
async fn unknown_id_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let session = Arc::new(SessionStore::load_for_project(tmp.path().to_path_buf()));
    let (registry, _req_rx, _bc_tx) = prehosted(session).await;
    let handle = serve::start_server(serve::ServeOptions::default(), registry);
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

/// ADR-0093: a monitor client receives a snapshot whose rows reflect the
/// registry's trackers — and, with `watch`, live diffs as sessions report.
#[tokio::test]
async fn monitor_handshake_yields_snapshot_then_diffs() {
    let tmp = tempfile::tempdir().unwrap();
    let session = Arc::new(SessionStore::load_for_project(tmp.path().to_path_buf()));
    let session_id = session.id().await;
    let (registry, _req_rx, bc_tx) = prehosted(session).await;
    let handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.port.await.unwrap();
    let _ = handle;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    let select = serde_json::to_string(&Wire::Select {
        action: AttachAction::Monitor(MonitorAction {
            watch: true,
            include_idle: true,
        }),
    })
    .unwrap();
    ws.send(WsMessage::Text(select.into())).await.unwrap();

    let first = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame: Wire = serde_json::from_str(first.to_text().unwrap_or("")).unwrap();
    match frame {
        Wire::Monitor {
            event: MonitorEvent::Snapshot(snapshot),
        } => {
            assert_eq!(snapshot.sessions.len(), 1);
            assert_eq!(snapshot.sessions[0].id, session_id);
        }
        other => panic!("expected monitor Snapshot, got {other:?}"),
    }

    // A broadcast response flows through the tracker into a diff. A
    // TurnStarted flips the row to Running, which the watch stream reports.
    let _ = bc_tx.send(AgentResponse::Round {
        session_id: session_id.clone(),
        event: RoundEvent::TurnStarted { round: 1, turn: 0 },
    });
    let diff = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame: Wire = serde_json::from_str(diff.to_text().unwrap_or("")).unwrap();
    match frame {
        Wire::Monitor {
            event: MonitorEvent::SessionUpdated(row),
        } => {
            assert_eq!(row.id, session_id);
            assert_eq!(row.status, neenee_core::SessionStatus::Running);
            assert_eq!(row.round, 1);
            assert_eq!(row.turn, Some(0));
        }
        other => panic!("expected monitor SessionUpdated, got {other:?}"),
    }
}

/// Without `watch` the daemon closes the connection after the snapshot.
#[tokio::test]
async fn monitor_one_shot_closes_after_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let session = Arc::new(SessionStore::load_for_project(tmp.path().to_path_buf()));
    let (registry, _req_rx, _bc_tx) = prehosted(session).await;
    let handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.port.await.unwrap();
    let _ = handle;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    let select = serde_json::to_string(&Wire::Select {
        action: AttachAction::Monitor(MonitorAction {
            watch: false,
            include_idle: false,
        }),
    })
    .unwrap();
    ws.send(WsMessage::Text(select.into())).await.unwrap();

    // include_idle=false with an idle session: the snapshot is empty.
    let first = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame: Wire = serde_json::from_str(first.to_text().unwrap_or("")).unwrap();
    match frame {
        Wire::Monitor {
            event: MonitorEvent::Snapshot(snapshot),
        } => assert!(snapshot.sessions.is_empty()),
        other => panic!("expected monitor Snapshot, got {other:?}"),
    }
    // Then the stream ends.
    let end = tokio::time::timeout(Duration::from_secs(2), ws.next()).await;
    match end {
        Ok(None) | Ok(Some(Ok(WsMessage::Close(_)))) => {}
        other => panic!("expected connection close, got {other:?}"),
    }
}

/// ADR-0095: a standalone session mirrors its status into the host; monitor
/// clients see it as a `mirrored` row; disconnecting removes the row.
#[tokio::test]
async fn mirror_reports_row_and_disconnect_removes_it() {
    let tmp = tempfile::tempdir().unwrap();
    let session = Arc::new(SessionStore::load_for_project(tmp.path().to_path_buf()));
    let (registry, _req_rx, _bc_tx) = prehosted(session).await;
    let probe = registry.clone();
    let handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.port.await.unwrap();
    let _ = handle;

    // 1. A mirror client connects and adopts its session identity.
    let (mut mirror, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    let select = serde_json::to_string(&Wire::Select {
        action: AttachAction::Mirror,
    })
    .unwrap();
    mirror.send(WsMessage::Text(select.into())).await.unwrap();
    let hello = serde_json::to_string(&Wire::Mirror {
        hello: MirrorHello {
            session_id: "standalone-1".into(),
            overview: "local TUI work".into(),
            created_at: 1,
            message_count: 4,
        },
    })
    .unwrap();
    mirror.send(WsMessage::Text(hello.into())).await.unwrap();

    // 2. A mirror update flows in: running, round 1. Wait until the registry
    //    has folded it before snapshotting (WS delivery is async).
    let mut row = MonitoredSession::empty("standalone-1".into());
    row.status = SessionStatus::Running;
    row.round = 1;
    row.turn = Some(0);
    row.output_tokens = 42;
    let update = serde_json::to_string(&Wire::MirrorUpdate { row }).unwrap();
    mirror.send(WsMessage::Text(update.into())).await.unwrap();

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let snap = probe
            .monitor_snapshot(MonitorAction {
                watch: false,
                include_idle: true,
            })
            .await;
        if snap
            .sessions
            .iter()
            .any(|r| r.id == "standalone-1" && r.status == SessionStatus::Running)
        {
            break;
        }
        if std::time::Instant::now() > deadline {
            panic!("mirror row never reached Running: {snap:?}");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // 3. A one-shot monitor snapshot contains the mirrored row, forced to
    //    `mirrored` hosting, with identity pinned to the hello.
    let (mut mon, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    let select = serde_json::to_string(&Wire::Select {
        action: AttachAction::Monitor(MonitorAction {
            watch: true,
            include_idle: false,
        }),
    })
    .unwrap();
    mon.send(WsMessage::Text(select.into())).await.unwrap();
    let first = tokio::time::timeout(Duration::from_secs(2), mon.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame: Wire = serde_json::from_str(first.to_text().unwrap_or("")).unwrap();
    match frame {
        Wire::Monitor {
            event: MonitorEvent::Snapshot(snapshot),
        } => {
            let mirrored = snapshot
                .sessions
                .iter()
                .find(|r| r.id == "standalone-1")
                .expect("mirrored row should be in the snapshot");
            assert_eq!(mirrored.hosting, SessionHosting::Mirrored);
            assert_eq!(mirrored.status, SessionStatus::Running);
            assert_eq!(mirrored.output_tokens, 42);
            assert_eq!(mirrored.overview, "local TUI work");
        }
        other => panic!("expected Snapshot, got {other:?}"),
    }

    // 4. The mirror disconnects: the watch stream reports SessionRemoved.
    drop(mirror);
    let mut removed = false;
    for _ in 0..4 {
        let msg = tokio::time::timeout(Duration::from_secs(2), mon.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let frame: Wire = serde_json::from_str(msg.to_text().unwrap_or("")).unwrap();
        if let Wire::Monitor {
            event: MonitorEvent::SessionRemoved { session_id },
        } = frame
        {
            assert_eq!(session_id, "standalone-1");
            removed = true;
            break;
        }
    }
    assert!(removed, "expected SessionRemoved after mirror disconnect");
}

/// ADR-0095: a mirrored row never shadows a hosted session with the same id.
#[tokio::test]
async fn hosted_session_wins_over_mirror_with_same_id() {
    let tmp = tempfile::tempdir().unwrap();
    let session = Arc::new(SessionStore::load_for_project(tmp.path().to_path_buf()));
    let session_id = session.id().await;
    let (registry, _req_rx, _bc_tx) = prehosted(session).await;
    let snapshot_registry = registry.clone();
    let handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.port.await.unwrap();
    let _ = handle;

    // A mirror claims the SAME id the registry already hosts.
    let (mut mirror, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    let select = serde_json::to_string(&Wire::Select {
        action: AttachAction::Mirror,
    })
    .unwrap();
    mirror.send(WsMessage::Text(select.into())).await.unwrap();
    let hello = serde_json::to_string(&Wire::Mirror {
        hello: MirrorHello {
            session_id: session_id.clone(),
            overview: "impostor".into(),
            created_at: 1,
            message_count: 1,
        },
    })
    .unwrap();
    mirror.send(WsMessage::Text(hello.into())).await.unwrap();

    let snapshot = snapshot_registry
        .monitor_snapshot(MonitorAction {
            watch: false,
            include_idle: true,
        })
        .await;
    let rows: Vec<_> = snapshot
        .sessions
        .iter()
        .filter(|r| r.id == session_id)
        .collect();
    assert_eq!(rows.len(), 1, "one row per session id, hosted wins");
    assert_eq!(rows[0].hosting, SessionHosting::Hosted);
}

/// ADR-0096: the control plane manages sessions without attaching — create,
/// observe in the monitor snapshot, kill.
#[tokio::test]
async fn control_create_observe_kill_roundtrip() {
    let registry = Arc::new(SessionRegistry::prehost_only());
    let probe = registry.clone();
    let handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.port.await.unwrap();
    let _ = handle;

    // Create a session via the control verb. prehost_only cannot assemble, so
    // this must fail cleanly — proving the verb reaches the registry and the
    // reply shape is right.
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    let select = serde_json::to_string(&Wire::Select {
        action: AttachAction::Control(serve::ControlRequest::CreateSession {
            project: "/tmp/x".into(),
            prompt: None,
        }),
    })
    .unwrap();
    ws.send(WsMessage::Text(select.into())).await.unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame: Wire = serde_json::from_str(msg.to_text().unwrap_or("")).unwrap();
    match frame {
        Wire::ControlReply { ok, error, .. } => {
            assert!(!ok, "prehost registry cannot create sessions");
            assert!(error.unwrap().contains("cannot create"));
        }
        other => panic!("expected ControlReply, got {other:?}"),
    }

    // Kill on a missing session is a clean error too.
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    let select = serde_json::to_string(&Wire::Select {
        action: AttachAction::Control(serve::ControlRequest::KillSession {
            session_id: "nope".into(),
        }),
    })
    .unwrap();
    ws.send(WsMessage::Text(select.into())).await.unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame: Wire = serde_json::from_str(msg.to_text().unwrap_or("")).unwrap();
    match frame {
        Wire::ControlReply { ok, error, .. } => {
            assert!(!ok);
            assert!(error.unwrap().contains("nope"));
        }
        other => panic!("expected ControlReply, got {other:?}"),
    }
    let _ = probe;
}

/// ADR-0096: the same control plane is served over the Unix domain socket —
/// no bearer token (filesystem permissions are the auth boundary).
#[cfg(unix)]
#[tokio::test]
async fn uds_serves_same_protocol_without_token() {
    let tmp = tempfile::tempdir().unwrap();
    let uds = tmp.path().join("daemon.sock");
    let registry = Arc::new(SessionRegistry::prehost_only());
    let handle = serve::start_server(
        serve::ServeOptions {
            uds_path: Some(uds.clone()),
            ..serve::ServeOptions::default()
        },
        registry,
    );
    let bound = handle.uds_ready.await.unwrap();
    assert_eq!(bound.as_deref(), Some(uds.as_path()));
    // Socket file is 0600.
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&uds).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "socket must be 0600, got {mode:o}");

    // Full handshake over UDS: monitor one-shot.
    let stream = tokio::net::UnixStream::connect(&uds).await.unwrap();
    let request = "ws://localhost/".into_client_request().unwrap();
    let (mut ws, _) = tokio_tungstenite::client_async(request, stream)
        .await
        .unwrap();
    let select = serde_json::to_string(&Wire::Select {
        action: AttachAction::Monitor(MonitorAction {
            watch: false,
            include_idle: true,
        }),
    })
    .unwrap();
    ws.send(WsMessage::Text(select.into())).await.unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame: Wire = serde_json::from_str(msg.to_text().unwrap_or("")).unwrap();
    match frame {
        Wire::Monitor {
            event: MonitorEvent::Snapshot(_),
        } => {}
        other => panic!("expected Snapshot over UDS, got {other:?}"),
    }

    // Cancel cleans up the socket file.
    handle.cancel.cancel();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!uds.exists(), "socket file removed on shutdown");
}
