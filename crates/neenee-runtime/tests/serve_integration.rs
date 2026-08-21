#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use neenee_contracts::{AgentRequest, AgentResponse, MonitorAction, MonitorEvent, RoundEvent};
use neenee_persistence::session::SessionStore;
use neenee_runtime::monitor::MonitorTracker;
use neenee_runtime::registry::{HostedSession, SessionRegistry};
use neenee_runtime::serve::{self, AttachAction, Wire};
use tokio::sync::{Mutex, broadcast, mpsc};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

fn idle_base(id: String) -> neenee_contracts::MonitoredSession {
    neenee_contracts::MonitoredSession {
        id,
        overview: String::new(),
        created_at: 0,
        updated_at: 0,
        message_count: 0,
        status: neenee_contracts::SessionStatus::Idle,
        hosting: neenee_contracts::SessionHosting::Hosted,
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
        parent_id: None,
        fork_kind: neenee_contracts::SessionForkKind::default(),
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
        neenee_contracts::SessionStatus::Idle,
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
            registry_for_tap.publish_for_test(MonitorEvent::SessionUpdated(row));
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
            created_at: std::time::Instant::now(),
            last_activity: tokio::sync::Mutex::new(std::time::Instant::now()),
            last_seen_tick: std::sync::atomic::AtomicU64::new(0),
            activity_tick: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            agent_for_session_end: None,
        })
        .await;
    (registry, req_rx, bc_tx)
}

/// Host a throwaway session rooted at `project`, returning its id. Unlike
/// [`prehosted`] this takes the project explicitly and skips the
/// broadcast-tap: project-scoping tests need several hosted sessions with
/// distinct roots in one registry, and only the registry's project index
/// matters for them.
async fn host_with_project(registry: &SessionRegistry, project: std::path::PathBuf) -> String {
    // `for_path` keeps every artifact under the given project dir instead of
    // minting files in the real XDG project bucket; the registry only needs
    // `project_root` for routing/indexing.
    let session = Arc::new(SessionStore::for_path(
        project.join("sessions").join("session.json"),
    ));
    let id = session.id().await;
    let (req_tx, _req_rx) = mpsc::unbounded_channel::<AgentRequest>();
    let (bc_tx, _) = broadcast::channel::<AgentResponse>(1024);
    let tracker = Arc::new(Mutex::new(MonitorTracker::bootstrap(
        idle_base(id.clone()),
        neenee_contracts::SessionStatus::Idle,
    )));
    registry
        .host(HostedSession {
            project_root: project,
            session,
            req_tx,
            events: bc_tx,
            cancel: tokio_util::sync::CancellationToken::new(),
            tracker,
            sync_buffer: Arc::new(Mutex::new(std::collections::VecDeque::new())),
            created_at: std::time::Instant::now(),
            last_activity: tokio::sync::Mutex::new(std::time::Instant::now()),
            last_seen_tick: std::sync::atomic::AtomicU64::new(0),
            activity_tick: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            agent_for_session_end: None,
        })
        .await;
    id
}

#[tokio::test]
async fn test_select_then_attach_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    // `for_path` keeps every artifact (session json/jsonl + blobs) inside the
    // tempdir; `load_for_project` would instead resolve the real XDG project
    // bucket and mint files under ~/.local/share/neenee.
    let session = Arc::new(SessionStore::for_path(tmp.path().join("session.json")));
    let session_id = session.id().await;
    let (registry, mut req_rx, bc_tx) = prehosted(session).await;

    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    let select = serde_json::to_string(&Wire::Select {
        version: None,
        action: AttachAction::Attach(None),
        project: None,
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

    // Followed by the attach-time HarnessState sync (ADR-0128).
    let state_msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match serde_json::from_str::<Wire>(state_msg.to_text().unwrap_or("")).unwrap() {
        Wire::Response {
            response:
                AgentResponse::Round {
                    event: RoundEvent::HarnessState(_),
                    ..
                },
        } => {}
        other => panic!("expected HarnessState sync, got {other:?}"),
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
    // `for_path` keeps every artifact (session json/jsonl + blobs) inside the
    // tempdir; `load_for_project` would instead resolve the real XDG project
    // bucket and mint files under ~/.local/share/neenee.
    let session = Arc::new(SessionStore::for_path(tmp.path().join("session.json")));
    let session_id = session.id().await;

    // Give the session content so its file persists, then a non-empty list.
    session
        .replace_messages(vec![neenee_contracts::Message::new(
            neenee_contracts::Role::User,
            "hello",
        )])
        .await
        .unwrap();
    let mut todos = neenee_contracts::TodoList::new();
    todos.items.push(neenee_contracts::TodoItem {
        id: neenee_contracts::TodoId(1),
        content: "restored task".to_string(),
        status: neenee_contracts::TodoStatus::InProgress,
        created_at: 1,
        updated_at: 1,
    });
    session.set_todos(todos.clone()).await.unwrap();

    let (registry, _req_rx, _bc_tx) = prehosted(session).await;
    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    let select = serde_json::to_string(&Wire::Select {
        version: None,
        action: AttachAction::Attach(None),
        project: None,
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
    // `for_path` keeps every artifact (session json/jsonl + blobs) inside the
    // tempdir; `load_for_project` would instead resolve the real XDG project
    // bucket and mint files under ~/.local/share/neenee.
    let session = Arc::new(SessionStore::for_path(tmp.path().join("session.json")));
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

    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    ws.send(WsMessage::Text(
        serde_json::to_string(&Wire::Select {
            version: None,
            action: AttachAction::Attach(None),
            project: None,
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
    // `for_path` keeps every artifact (session json/jsonl + blobs) inside the
    // tempdir; `load_for_project` would instead resolve the real XDG project
    // bucket and mint files under ~/.local/share/neenee.
    let session = Arc::new(SessionStore::for_path(tmp.path().join("session.json")));
    let (registry, _req_rx, _bc_tx) = prehosted(session).await;
    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    let select = serde_json::to_string(&Wire::Select {
        version: None,
        action: AttachAction::Attach(Some("nope".into())),
        project: None,
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
        Wire::Error { message, .. } => assert!(message.contains("nope")),
        other => panic!("expected Error, got {other:?}"),
    }
}

/// Project scoping (ADR-0096): the Select frame's optional `project` declares
/// the caller's working directory, and auto-attach must resolve inside THAT
/// project — not the daemon process's cwd, which is whatever the first client
/// that spawned the daemon happened to use. `New` creation and lazy resume
/// are scoped by the same value (`registry::SessionRegistry::resolve`).
#[tokio::test]
async fn select_project_scopes_auto_attach() {
    let registry = Arc::new(SessionRegistry::prehost_only());
    let project_a = std::env::temp_dir().join(format!("neenee-scope-a-{}", uuid::Uuid::new_v4()));
    let project_b = std::env::temp_dir().join(format!("neenee-scope-b-{}", uuid::Uuid::new_v4()));
    let id_a = host_with_project(&registry, project_a.clone()).await;
    let _id_b = host_with_project(&registry, project_b).await;

    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;

    // Two hosted sessions, neither rooted at the daemon's cwd: under the old
    // cwd-only behavior this attach could only yield a Pick frame. Declaring
    // project A must bind A's session directly.
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    let select = serde_json::to_string(&Wire::Select {
        version: None,
        action: AttachAction::Attach(None),
        project: Some(project_a),
    })
    .unwrap();
    ws.send(WsMessage::Text(select.into())).await.unwrap();

    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match serde_json::from_str::<Wire>(msg.to_text().unwrap_or("")).unwrap() {
        Wire::Welcome { session_id, .. } => assert_eq!(session_id, id_a),
        other => panic!("expected Welcome for project-a's session, got {other:?}"),
    }
}

/// Wire compatibility: a Select frame without `project` — what every client
/// sent before the field existed — still deserializes, and the daemon falls
/// back to its own process cwd as the caller's project scope.
#[tokio::test]
async fn select_without_project_falls_back_to_daemon_cwd() {
    let cwd = std::env::current_dir().unwrap();
    let registry = Arc::new(SessionRegistry::prehost_only());
    let cwd_session = host_with_project(&registry, cwd).await;
    let elsewhere = std::env::temp_dir().join(format!("neenee-scope-c-{}", uuid::Uuid::new_v4()));
    let _other = host_with_project(&registry, elsewhere).await;

    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;

    // Hand-written legacy frame: no `project` key at all. The fallback scope
    // is the process cwd, which the test process shares with the in-process
    // server — so the cwd-rooted session must win. Without the cwd fallback
    // the two hosted sessions could only produce a Pick frame.
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    ws.send(WsMessage::Text(
        r#"{"type":"Select","action":{"attach":null}}"#.into(),
    ))
    .await
    .unwrap();

    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match serde_json::from_str::<Wire>(msg.to_text().unwrap_or("")).unwrap() {
        Wire::Welcome { session_id, .. } => assert_eq!(session_id, cwd_session),
        other => panic!("expected Welcome for the cwd-rooted session, got {other:?}"),
    }
}

/// Regression (the "wrong workspace" bug): a client that *declared* its
/// project must never be silently auto-bound to the daemon's one hosted
/// session when that session belongs to a different project. Launching
/// `neenee resume` from project A with only project B's session live used to
/// attach straight into B's session — the model then read and edited B while
/// the header showed A. The declared-project client now gets the picker; the
/// cross-project session remains an explicit choice.
#[tokio::test]
async fn declared_project_is_never_auto_bound_to_a_foreign_session() {
    let registry = Arc::new(SessionRegistry::prehost_only());
    let project_a = std::env::temp_dir().join(format!("neenee-scope-d-{}", uuid::Uuid::new_v4()));
    let project_b = std::env::temp_dir().join(format!("neenee-scope-e-{}", uuid::Uuid::new_v4()));
    let id_b = host_with_project(&registry, project_b.clone()).await;

    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    let select = serde_json::to_string(&Wire::Select {
        version: None,
        action: AttachAction::Attach(None),
        project: Some(project_a),
    })
    .unwrap();
    ws.send(WsMessage::Text(select.into())).await.unwrap();

    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match serde_json::from_str::<Wire>(msg.to_text().unwrap_or("")).unwrap() {
        Wire::Pick { sessions } => {
            // The picker offers exactly the foreign session — an explicit
            // choice, never an automatic one.
            assert_eq!(sessions.len(), 1);
            assert_eq!(sessions[0].id, id_b);
        }
        other => panic!("expected Pick (foreign session must not auto-bind), got {other:?}"),
    }
}

/// ADR-0093: a monitor client receives a snapshot whose rows reflect the
/// registry's trackers — and, with `watch`, live diffs as sessions report.
#[tokio::test]
async fn monitor_handshake_yields_snapshot_then_diffs() {
    let tmp = tempfile::tempdir().unwrap();
    // `for_path` keeps every artifact (session json/jsonl + blobs) inside the
    // tempdir; `load_for_project` would instead resolve the real XDG project
    // bucket and mint files under ~/.local/share/neenee.
    let session = Arc::new(SessionStore::for_path(tmp.path().join("session.json")));
    let session_id = session.id().await;
    let (registry, _req_rx, bc_tx) = prehosted(session).await;
    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    let select = serde_json::to_string(&Wire::Select {
        version: None,
        action: AttachAction::Monitor(MonitorAction {
            watch: true,
            include_idle: true,
        }),
        project: None,
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
            assert_eq!(row.status, neenee_contracts::SessionStatus::Running);
            assert_eq!(row.round, 1);
            assert_eq!(row.turn, Some(0));
        }
        other => panic!("expected monitor SessionUpdated, got {other:?}"),
    }
}

/// A rename flows handler → broadcast-tap → monitor diff: the republished row
/// carries the new title because the tracker's base header is re-seeded from
/// the sessions-overview snapshot the rename handler pushes.
#[tokio::test]
async fn rename_live_session_republishes_monitor_row() {
    let tmp = tempfile::tempdir().unwrap();
    // `for_path` keeps every artifact under the tempdir; the session needs
    // real content so it persists and appears in `list()`.
    let session = Arc::new(SessionStore::for_path(tmp.path().join("session.json")));
    session
        .replace_messages(vec![neenee_contracts::Message::new(
            neenee_contracts::Role::User,
            "first prompt",
        )])
        .await
        .unwrap();
    let session_id = session.id().await;
    let (registry, _req_rx, bc_tx) = prehosted(session.clone()).await;

    // Subscribe before the rename so the diff is captured.
    let mut monitor_rx = registry.subscribe_monitor();

    // Drive the production handler; forward its replies onto the session
    // broadcast exactly like the driver's response channel does.
    let (resp_tx, mut resp_rx) = mpsc::unbounded_channel::<AgentResponse>();
    let forward = bc_tx.clone();
    tokio::spawn(async move {
        while let Some(response) = resp_rx.recv().await {
            let _ = forward.send(response);
        }
    });
    neenee_runtime::handlers_session::rename(
        &session,
        &resp_tx,
        session_id.clone(),
        Some("panel rename".to_string()),
    )
    .await;

    let row = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match monitor_rx.recv().await {
                Ok(MonitorEvent::SessionUpdated(row)) if row.id == session_id => break row,
                Ok(_) => continue,
                Err(error) => panic!("monitor stream ended before the rename diff: {error}"),
            }
        }
    })
    .await
    .expect("the rename must republish the session's monitor row");
    assert_eq!(row.overview, "panel rename");

    // A fresh monitor subscriber sees the renamed row in the snapshot too.
    let snapshot = registry
        .monitor_snapshot(MonitorAction {
            watch: false,
            include_idle: true,
        })
        .await;
    let row = snapshot
        .sessions
        .iter()
        .find(|row| row.id == session_id)
        .unwrap();
    assert_eq!(row.overview, "panel rename");
}

/// Without `watch` the daemon closes the connection after the snapshot.
#[tokio::test]
async fn monitor_one_shot_closes_after_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    // `for_path` keeps every artifact (session json/jsonl + blobs) inside the
    // tempdir; `load_for_project` would instead resolve the real XDG project
    // bucket and mint files under ~/.local/share/neenee.
    let session = Arc::new(SessionStore::for_path(tmp.path().join("session.json")));
    let (registry, _req_rx, _bc_tx) = prehosted(session).await;
    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    let select = serde_json::to_string(&Wire::Select {
        version: None,
        action: AttachAction::Monitor(MonitorAction {
            watch: false,
            include_idle: false,
        }),
        project: None,
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

/// ADR-0096: the control plane manages sessions without attaching — create,
/// observe in the monitor snapshot, kill.
/// ADR-0112: a client declares the session ended over its attach
/// connection; the daemon tears the session down (registry entry gone,
/// `SessionRemoved` published, terminal `Exit` flushed to the attach
/// client) and the connection closes. The request must never reach the
/// driver queue — the teardown races what it would cancel.
#[tokio::test]
async fn attach_end_session_tears_down_and_notifies() {
    let tmp = tempfile::tempdir().unwrap();
    let (_dir, session) = fresh_empty_store("end-session");
    let (registry, mut _req_rx, _bc) = prehosted(session.clone()).await;
    let id = session.id().await;

    // Watch the monitor plane so the SessionRemoved diff is observable.
    let registry_for_serve = registry.clone();
    let mut handle = serve::start_server(serve::ServeOptions::default(), registry_for_serve);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();

    // A dashboard watches the monitor plane; its row must disappear when
    // the attach client ends the session.
    let (mut monitor, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    let select_monitor = serde_json::to_string(&Wire::Select {
        version: None,
        action: AttachAction::Monitor(MonitorAction {
            watch: true,
            include_idle: true,
        }),
        project: None,
    })
    .unwrap();
    monitor
        .send(WsMessage::Text(select_monitor.into()))
        .await
        .unwrap();
    let first = tokio::time::timeout(Duration::from_secs(2), monitor.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame: Wire = serde_json::from_str(first.to_text().unwrap_or("")).unwrap();
    assert!(
        matches!(
            frame,
            Wire::Monitor {
                event: MonitorEvent::Snapshot(_)
            }
        ),
        "expected monitor Snapshot, got {frame:?}"
    );

    // Attach.
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    let select = serde_json::to_string(&Wire::Select {
        version: None,
        action: AttachAction::Attach(Some(id.clone())),
        project: None,
    })
    .unwrap();
    ws.send(WsMessage::Text(select.into())).await.unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame: Wire = serde_json::from_str(msg.to_text().unwrap_or("")).unwrap();
    assert!(
        matches!(frame, Wire::Welcome { .. }),
        "expected Welcome, got {frame:?}"
    );

    // Declare the session ended — raw frame shape the web panel sends.
    ws.send(WsMessage::Text(
        r#"{"type":"Request","EndSession":null}"#.into(),
    ))
    .await
    .unwrap();

    // The attach connection receives the terminal Exit before closing.
    let mut saw_exit = false;
    let deadline = Duration::from_secs(3);
    while let Ok(Some(Ok(msg))) = tokio::time::timeout(deadline, ws.next()).await {
        if let Ok(Wire::Response {
            response: AgentResponse::Exit,
        }) = serde_json::from_str::<Wire>(msg.to_text().unwrap_or(""))
        {
            saw_exit = true;
            break;
        }
    }
    assert!(saw_exit, "attach client must observe the terminal Exit");

    // The registry no longer hosts the session.
    assert!(!hosted_ids(&registry).await.contains(&id));

    // The driver queue never saw the request (it was intercepted at the
    // connection layer, not forwarded).
    assert!(
        _req_rx.try_recv().is_err(),
        "EndSession must not reach the driver queue"
    );

    // The dashboard's monitor stream sees the row disappear.
    let removed = tokio::time::timeout(Duration::from_secs(3), monitor.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let frame: Wire = serde_json::from_str(removed.to_text().unwrap_or("")).unwrap();
    match frame {
        Wire::Monitor {
            event: MonitorEvent::SessionRemoved { session_id },
        } => assert_eq!(session_id, id),
        other => panic!("expected monitor SessionRemoved, got {other:?}"),
    }

    let _ = handle;
    let _ = tmp;
}

/// ADR-0096: the control plane manages sessions without attaching — create,
/// observe in the monitor snapshot, kill.
#[tokio::test]
async fn control_create_observe_kill_roundtrip() {
    let registry = Arc::new(SessionRegistry::prehost_only());
    let probe = registry.clone();
    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();
    let _ = handle;

    // Create a session via the control verb. prehost_only cannot assemble, so
    // this must fail cleanly — proving the verb reaches the registry and the
    // reply shape is right.
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    let select = serde_json::to_string(&Wire::Select {
        version: None,
        action: AttachAction::Control(serve::ControlRequest::CreateSession {
            project: "/tmp/x".into(),
            prompt: None,
        }),
        project: None,
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
        version: None,
        action: AttachAction::Control(serve::ControlRequest::KillSession {
            session_id: "nope".into(),
        }),
        project: None,
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

/// ADR-0096/0130: the same control plane is served over native local IPC —
/// UDS on Unix and a per-user named pipe on Windows. No bearer token is
/// needed because the OS endpoint permissions are the authentication boundary.
#[tokio::test]
async fn native_local_ipc_serves_same_protocol_without_token() {
    let tmp = tempfile::tempdir().unwrap();
    let socket_path = tmp.path().join("daemon.sock");
    let endpoint = neenee_platform::ipc::endpoint_for_instance(
        socket_path.clone(),
        &format!("serve-integration-{}", std::process::id()),
    )
    .unwrap();
    let registry = Arc::new(SessionRegistry::prehost_only());
    let mut handle = serve::start_server(
        serve::ServeOptions {
            local_endpoint: Some(endpoint.clone()),
            ..serve::ServeOptions::default()
        },
        registry,
    );
    let bound = handle
        .startup
        .local_ready
        .take()
        .unwrap()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bound, Some(endpoint.clone()));
    // Socket file is 0600.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&socket_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "socket must be 0600, got {mode:o}");
    }

    // Full handshake over the native endpoint: monitor one-shot.
    let stream = neenee_platform::ipc::connect(&endpoint).await.unwrap();
    let request = "ws://localhost/".into_client_request().unwrap();
    let (mut ws, _) = tokio_tungstenite::client_async(request, stream)
        .await
        .unwrap();
    let select = serde_json::to_string(&Wire::Select {
        version: None,
        action: AttachAction::Monitor(MonitorAction {
            watch: false,
            include_idle: true,
        }),
        project: None,
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
        other => panic!("expected Snapshot over native local IPC, got {other:?}"),
    }

    // Cancel cleans up the socket file — deterministically: the removal runs
    // inside the supervised accept task, so joining the task book (bounded)
    // guarantees the file is gone before the assertion (ADR-0101; the old
    // 100ms sleep papered over exactly this race).
    handle.cancel.cancel();
    let hung = handle
        .tasks
        .join_all_with_budget(Duration::from_secs(2))
        .await;
    assert!(
        hung.is_empty(),
        "accept tasks must stop on cancel: {hung:?}"
    );
    let probe = neenee_platform::ipc::probe(&endpoint);
    assert!(
        !probe.exists,
        "local endpoint removed on shutdown: {endpoint}"
    );
}

#[tokio::test]
async fn native_local_ipc_bind_failure_is_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let endpoint = neenee_platform::ipc::endpoint_for_instance(
        tmp.path().join("occupied.sock"),
        &format!("occupied-{}", std::process::id()),
    )
    .unwrap();
    let _occupied = neenee_platform::ipc::LocalListener::bind(&endpoint).unwrap();
    let registry = Arc::new(SessionRegistry::prehost_only());
    let mut handle = serve::start_server(
        serve::ServeOptions {
            local_endpoint: Some(endpoint),
            ..serve::ServeOptions::default()
        },
        registry,
    );
    let result = handle.startup.local_ready.take().unwrap().await.unwrap();
    assert!(
        result.is_err(),
        "an occupied native endpoint must fail startup"
    );
    handle.cancel.cancel();
}

// ── Idle-empty session reaper ─────────────────────────────────────────────

/// Construct and host a bare session with no broadcast-tap subscriber, so the
/// only potential event receiver is one the test adds explicitly. Unlike
/// [`prehosted`] this leaves `events.receiver_count() == 0`, which is what the
/// reaper's "no attached client" probe keys on.
async fn host_bare(
    session: Arc<SessionStore>,
    created_at: std::time::Instant,
) -> (
    Arc<SessionRegistry>,
    broadcast::Sender<AgentResponse>,
    String,
) {
    let (req_tx, _req_rx) = mpsc::unbounded_channel::<AgentRequest>();
    let (bc_tx, _) = broadcast::channel::<AgentResponse>(1024);
    let registry = Arc::new(SessionRegistry::prehost_only());
    let base = idle_base(session.id().await);
    let tracker = Arc::new(Mutex::new(MonitorTracker::bootstrap(
        base,
        neenee_contracts::SessionStatus::Idle,
    )));
    let id = session.id().await;
    registry
        .host(HostedSession {
            project_root: std::env::temp_dir().join("neenee-reaper-project"),
            session,
            req_tx,
            events: bc_tx.clone(),
            cancel: tokio_util::sync::CancellationToken::new(),
            tracker,
            sync_buffer: Arc::new(Mutex::new(std::collections::VecDeque::new())),
            created_at,
            last_activity: tokio::sync::Mutex::new(created_at),
            last_seen_tick: std::sync::atomic::AtomicU64::new(0),
            activity_tick: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
            agent_for_session_end: None,
        })
        .await;
    (registry, bc_tx, id)
}

/// A fresh project dir + SessionStore that has never persisted (empty).
fn fresh_empty_store(tag: &str) -> (std::path::PathBuf, Arc<SessionStore>) {
    let dir = std::env::temp_dir().join(format!("neenee-reaper-{tag}-{}", uuid::Uuid::new_v4()));
    // `for_path` keeps every artifact under the throwaway dir; nothing lands
    // in the real XDG project bucket.
    let store = Arc::new(SessionStore::for_path(dir.join("session.json")));
    (dir, store)
}

/// Snapshot the set of currently hosted session ids.
async fn hosted_ids(registry: &SessionRegistry) -> std::collections::HashSet<String> {
    registry
        .monitor_snapshot(MonitorAction {
            watch: false,
            include_idle: true,
        })
        .await
        .sessions
        .into_iter()
        .map(|r| r.id)
        .collect()
}

#[tokio::test]
async fn reaper_removes_idle_never_persisted_session() {
    let (_dir, store) = fresh_empty_store("idle");
    // Old enough to exceed any TTL we pass; no client ever attaches.
    let old = std::time::Instant::now() - Duration::from_secs(3600);
    let (registry, _tx, id) = host_bare(store, old).await;
    assert!(hosted_ids(&registry).await.contains(&id));

    let reaped = registry
        .reap_idle_empty_sessions_with(Duration::from_secs(60))
        .await;
    assert_eq!(reaped, vec![id.clone()], "the idle empty session is reaped");
    assert!(
        !hosted_ids(&registry).await.contains(&id),
        "reaped session is gone from the registry"
    );
}

#[tokio::test]
async fn reaper_keeps_empty_session_within_ttl() {
    let (_dir, store) = fresh_empty_store("fresh");
    // Brand-new: created just now, so a 60s TTL must leave it alone.
    let (registry, _tx, id) = host_bare(store, std::time::Instant::now()).await;

    let reaped = registry
        .reap_idle_empty_sessions_with(Duration::from_secs(60))
        .await;
    assert!(reaped.is_empty(), "a fresh empty session is not yet idle");
    assert!(hosted_ids(&registry).await.contains(&id));
}

#[tokio::test]
async fn reaper_keeps_empty_session_with_attached_client() {
    let (_dir, store) = fresh_empty_store("watched");
    let old = std::time::Instant::now() - Duration::from_secs(3600);
    let (registry, tx, id) = host_bare(store, old).await;
    // An attached client holds an event subscription open.
    let _client_rx = tx.subscribe();

    let reaped = registry
        .reap_idle_empty_sessions_with(Duration::from_secs(60))
        .await;
    assert!(
        reaped.is_empty(),
        "an empty session someone is watching is never reaped"
    );
    assert!(hosted_ids(&registry).await.contains(&id));
}

#[tokio::test]
async fn reaper_keeps_session_once_it_has_content() {
    let (dir, store) = fresh_empty_store("content");
    // Give the session real content: this persists it, so it is user history
    // and must never be reaped no matter how idle.
    store
        .replace_messages(vec![neenee_contracts::Message::new(
            neenee_contracts::Role::User,
            "hello",
        )])
        .await
        .unwrap();
    let old = std::time::Instant::now() - Duration::from_secs(3600);
    let (registry, _tx, id) = host_bare(store, old).await;

    let reaped = registry
        .reap_idle_empty_sessions_with(Duration::from_secs(60))
        .await;
    assert!(reaped.is_empty(), "a persisted session is never reaped");
    assert!(hosted_ids(&registry).await.contains(&id));
    let _ = std::fs::remove_dir_all(dir);
}

// ── Idle-hosted suspension (memory bounding for real sessions) ────────────

/// A persisted idle session with no clients is suspended after the TTL: the
/// daemon's memory must be bounded by *active* work, not by session history.
#[tokio::test]
async fn suspension_removes_idle_persisted_session() {
    let (dir, store) = fresh_empty_store("suspend");
    store
        .replace_messages(vec![neenee_contracts::Message::new(
            neenee_contracts::Role::User,
            "hello",
        )])
        .await
        .unwrap();
    let old = std::time::Instant::now() - Duration::from_secs(3600);
    let (registry, _tx, id) = host_bare(store, old).await;

    let suspended = registry
        .suspend_idle_sessions_with(Duration::from_secs(60))
        .await;
    assert_eq!(
        suspended,
        vec![id.clone()],
        "idle persisted session suspends"
    );
    assert!(
        !hosted_ids(&registry).await.contains(&id),
        "suspended session leaves the hosted set"
    );
    let _ = std::fs::remove_dir_all(dir);
}

/// An attached client keeps its session resident — suspension must never
/// yank a session out from under a live observer.
#[tokio::test]
async fn suspension_keeps_session_with_attached_client() {
    let (dir, store) = fresh_empty_store("suspend-attached");
    store
        .replace_messages(vec![neenee_contracts::Message::new(
            neenee_contracts::Role::User,
            "hello",
        )])
        .await
        .unwrap();
    let old = std::time::Instant::now() - Duration::from_secs(3600);
    let (registry, bc_tx, id) = host_bare(store, old).await;
    // Attach: a live broadcast receiver counts as an attached client.
    let _rx = bc_tx.subscribe();

    let suspended = registry
        .suspend_idle_sessions_with(Duration::from_secs(60))
        .await;
    assert!(suspended.is_empty(), "attached session must not suspend");
    assert!(hosted_ids(&registry).await.contains(&id));
    drop(_rx);
    let _ = std::fs::remove_dir_all(dir);
}

/// Recent tap activity defers suspension: the idle clock refreshes when the
/// tap tick advances between sweeps.
#[tokio::test]
async fn suspension_deferred_by_recent_activity() {
    let (dir, store) = fresh_empty_store("suspend-active");
    store
        .replace_messages(vec![neenee_contracts::Message::new(
            neenee_contracts::Role::User,
            "hello",
        )])
        .await
        .unwrap();
    // Hosted "recently": the idle clock has not run out yet.
    let fresh = std::time::Instant::now();
    let (registry, _tx, id) = host_bare(store, fresh).await;

    let suspended = registry
        .suspend_idle_sessions_with(Duration::from_secs(3600))
        .await;
    assert!(suspended.is_empty(), "fresh session must not suspend");
    assert!(hosted_ids(&registry).await.contains(&id));
    let _ = std::fs::remove_dir_all(dir);
}

/// Never-persisted empty sessions stay with the tighter empty-reaper; the
/// suspension path must not race it.
#[tokio::test]
async fn suspension_skips_empty_unpersisted_sessions() {
    let (_dir, store) = fresh_empty_store("suspend-empty");
    let old = std::time::Instant::now() - Duration::from_secs(3600);
    let (registry, _tx, id) = host_bare(store, old).await;

    let suspended = registry
        .suspend_idle_sessions_with(Duration::from_secs(60))
        .await;
    assert!(
        suspended.is_empty(),
        "empty session is the reaper's, not suspension's"
    );
    assert!(hosted_ids(&registry).await.contains(&id));
}

// ── Daemon lifecycle (ADR-0100/0101) ──────────────────────────────────────

/// `ControlRequest::Shutdown` funnels into the serve gate: the reply is sent
/// *before* the drain cancels this very connection (ADR-0100), and the
/// accept loops stop — provable without signals, since the gate is the same
/// trigger source signals use.
#[tokio::test]
async fn shutdown_control_verb_replies_then_stops_accepting() {
    let registry = Arc::new(SessionRegistry::prehost_only());
    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();

    // Issue the verb; the ControlReply must land before the drain kills us.
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
        .await
        .unwrap();
    let select = serde_json::to_string(&Wire::Select {
        version: None,
        action: AttachAction::Control(serve::ControlRequest::Shutdown),
        project: None,
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
        Wire::ControlReply { ok, .. } => assert!(ok, "shutdown verb must be accepted"),
        other => panic!("expected ControlReply, got {other:?}"),
    }

    // The gate latched with the ControlVerb reason and the accept tasks stop
    // deterministically (joined through the task book — no sleeps).
    assert!(
        tokio::time::timeout(Duration::from_secs(2), handle.gate.triggered())
            .await
            .is_ok(),
        "the serve gate must fire"
    );
    let hung = handle
        .tasks
        .join_all_with_budget(Duration::from_secs(2))
        .await;
    assert!(hung.is_empty(), "accept tasks must stop: {hung:?}");
}

/// Version negotiation (ADR-0100 rule 4): a skewed `Select.version` is
/// refused with a both-versions error before any session work; a matching
/// version proceeds normally (and an absent version is served).
#[tokio::test]
async fn version_skew_is_refused_with_both_versions() {
    let tmp = tempfile::tempdir().unwrap();
    // `for_path` keeps every artifact (session json/jsonl + blobs) inside the
    // tempdir; `load_for_project` would instead resolve the real XDG project
    // bucket and mint files under ~/.local/share/neenee.
    let session = Arc::new(SessionStore::for_path(tmp.path().join("session.json")));
    let (registry, _req_rx, _tx) = prehosted(session).await;
    let mut handle = serve::start_server(serve::ServeOptions::default(), registry);
    let port = handle.startup.port.take().unwrap().await.unwrap().unwrap();

    async fn first_frame(port: u16, version: Option<&str>) -> Wire {
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{port}"))
            .await
            .unwrap();
        let select = serde_json::to_string(&Wire::Select {
            version: version.map(str::to_string),
            action: AttachAction::Attach(Some("definitely-not-a-session".into())),
            project: None,
        })
        .unwrap();
        ws.send(WsMessage::Text(select.into())).await.unwrap();
        let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        serde_json::from_str(msg.to_text().unwrap_or("")).unwrap()
    }

    // Skewed older client: refused with a message recommending client update.
    match first_frame(port, Some("0.0.1")).await {
        Wire::Error { message, .. } => {
            assert!(
                message.contains("0.0.1"),
                "names the client build: {message}"
            );
            assert!(
                message.contains(serve::daemon_version()),
                "names the daemon build: {message}"
            );
            assert!(
                message.contains("update your neenee client"),
                "names the client update recommendation: {message}"
            );
        }
        other => panic!("expected Error for a skewed version, got {other:?}"),
    }
    // Skewed newer client: refused with a message recommending daemon stop/restart.
    match first_frame(port, Some("99.0.0")).await {
        Wire::Error { message, .. } => {
            assert!(
                message.contains("99.0.0"),
                "names the client build: {message}"
            );
            assert!(
                message.contains(serve::daemon_version()),
                "names the daemon build: {message}"
            );
            assert!(
                message.contains("neenee stop"),
                "names the daemon restart fix: {message}"
            );
        }
        other => panic!("expected Error for a skewed version, got {other:?}"),
    }
    // Absent version: served (legacy-tolerant; the error is the normal
    // unknown-session one, not a version refusal).
    match first_frame(port, None).await {
        Wire::Error { message, .. } => {
            assert!(
                !message.contains("version mismatch"),
                "absent version must be served, got: {message}"
            );
        }
        other => panic!("expected the normal unknown-session error, got {other:?}"),
    }
}

/// The daemon's discovery record carries its build version (ADR-0100 rule
/// 4), so a client reading a stale record can refuse before speaking.
#[test]
fn global_record_carries_the_daemon_version() {
    let record = neenee_runtime::serve_discovery::Discovery {
        pid: 1,
        process_birth_token: None,
        port: 2,
        token: None,
        project_root: String::new(),
        started_at: 3,
        uds_path: None,
        local_endpoint: None,
        version: Some(serve::daemon_version().to_string()),
        grace_secs: None,
    };
    let json = serde_json::to_string(&record).unwrap();
    assert!(json.contains(serve::daemon_version()));
    let back: neenee_runtime::serve_discovery::Discovery = serde_json::from_str(&json).unwrap();
    assert_eq!(back.version.as_deref(), Some(serve::daemon_version()));
}
