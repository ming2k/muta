//! `neenee status` (ADR-0093): the first control-plane client of the daemon
//! monitor protocol. One-shot by default (`neenee status`), a live table with
//! `--watch`, machine-readable frames with `--json`.
//!
//! Unlike `neenee attach`, status never spawns a daemon: observing is only
//! meaningful when a host is already running, so a missing/stale discovery
//! record is a clean "no daemon" report, not an excuse to start one.

use std::path::Path;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use neenee_core::{
    MonitorAction, MonitorEvent, MonitorSnapshot, MonitoredSession, SessionHosting, SessionStatus,
};
use neenee_transport::serve::Wire;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

use crate::remote::ServeInfo;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How `neenee status` renders its stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusOptions {
    pub watch: bool,
    pub json: bool,
    pub include_idle: bool,
}

pub async fn run(project_root: &Path, opts: StatusOptions) -> Result<(), String> {
    let Some(info) = crate::remote::discover(project_root) else {
        return Err(format!(
            "no neenee session host is running for {}. Start one with `neenee serve` \
             (or `neenee attach`, which spawns one on demand).",
            project_root.display()
        ));
    };
    let action = MonitorAction {
        watch: opts.watch,
        include_idle: opts.include_idle,
    };
    let mut rx = monitor_stream(&info, action).await?;
    // The first frame is always the snapshot; from then on the stream is
    // maintained client-side by folding diffs, so `--watch` renders one
    // coherent table instead of a raw event log.
    let mut state = match rx.recv().await {
        Some(MonitorEvent::Snapshot(snapshot)) => snapshot,
        Some(_) => return Err("monitor stream opened without a snapshot".to_string()),
        None => return Err("daemon closed the monitor stream".to_string()),
    };
    render(&state, opts);
    if !opts.watch {
        return Ok(());
    }
    while let Some(event) = rx.recv().await {
        match event {
            MonitorEvent::Snapshot(snapshot) => state = snapshot,
            MonitorEvent::SessionAdded(row) | MonitorEvent::SessionUpdated(row) => {
                upsert(&mut state.sessions, row);
            }
            MonitorEvent::SessionRemoved { session_id } => {
                state.sessions.retain(|row| row.id != session_id);
            }
        }
        render(&state, opts);
    }
    Ok(())
}

pub(crate) fn upsert(rows: &mut Vec<MonitoredSession>, row: MonitoredSession) {
    match rows.iter_mut().find(|existing| existing.id == row.id) {
        Some(existing) => *existing = row,
        None => rows.push(row),
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.updated_at));
}

fn render(snapshot: &MonitorSnapshot, opts: StatusOptions) {
    if opts.json {
        println!(
            "{}",
            serde_json::to_string(&MonitorEvent::Snapshot(snapshot.clone()))
                .unwrap_or_else(|_| "{}".to_string())
        );
        return;
    }
    if opts.watch {
        // Cheap in-place refresh: clear the screen and redraw. A full
        // alternate-screen TUI is overkill for a status table.
        print!("\x1b[2J\x1b[H");
    }
    println!("{}", table(snapshot));
}

/// The human-readable table. Extracted (and `pub(crate)`) so tests can pin
/// the layout without a daemon.
pub(crate) fn table(snapshot: &MonitorSnapshot) -> String {
    let mut out = String::new();
    let root = if snapshot.project_root.is_empty() {
        "all projects"
    } else {
        snapshot.project_root.as_str()
    };
    out.push_str(&format!(
        "neenee daemon — {} — {} session(s) needing attention\n",
        root,
        snapshot.sessions.len()
    ));
    if snapshot.sessions.is_empty() {
        out.push_str("  (all quiet — no running or blocked sessions)\n");
        return out;
    }
    out.push_str(&format!(
        "  {:<10} {:<14} {:<9} {:<7} {:>6} {:<9} {}\n",
        "SESSION", "STATUS", "HOSTING", "ROUND", "OUT", "ELAPSED", "DETAIL"
    ));
    for row in &snapshot.sessions {
        out.push_str(&format!(
            "  {:<10} {:<14} {:<9} {:<7} {:>6} {:<9} {}\n",
            short_id(&row.id),
            row.status.as_str(),
            hosting_cell(row),
            row.round_turn(),
            row.output_tokens,
            fmt_elapsed(row.elapsed_ms),
            detail(row),
        ));
    }
    out
}

/// How the row's session is driven. Since ADR-0096 every session is
/// daemon-held, so this is always `hosted`; the column stays so older
/// daemons' rows (which may omit `hosting`) still render.
fn hosting_cell(row: &MonitoredSession) -> String {
    match row.hosting {
        SessionHosting::Hosted => "hosted".to_string(),
    }
}

/// `round 3 › turn 2` while a round runs; `round 3` once it settled; `–`
/// before the first round.
trait RoundCell {
    fn round_turn(&self) -> String;
}
impl RoundCell for MonitoredSession {
    fn round_turn(&self) -> String {
        match (self.round, self.turn) {
            (0, _) => "–".to_string(),
            (round, Some(turn)) => format!("{round} › {turn}"),
            (round, None) => format!("{round}"),
        }
    }
}

fn detail(row: &MonitoredSession) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(tool) = &row.current_tool {
        parts.push(format!("tool {tool}"));
    }
    if let Some(note) = &row.note {
        parts.push(note.clone());
    } else if row.status == SessionStatus::Running
        && let Some(activity) = &row.activity
    {
        parts.push(activity.clone());
    }
    if let Some(tokens) = row.context_tokens {
        parts.push(format!("ctx {}", fmt_k(tokens)));
    }
    if parts.is_empty() {
        parts.push(truncate(&row.overview, 60));
    }
    parts.join(" · ")
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

fn fmt_elapsed(ms: u64) -> String {
    if ms == 0 {
        return "–".to_string();
    }
    let secs = ms / 1000;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

fn fmt_k(tokens: usize) -> String {
    if tokens >= 1000 {
        format!("{:.1}k", tokens as f64 / 1000.0)
    } else {
        tokens.to_string()
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Open the WebSocket, perform the monitor handshake, and return a channel of
/// stream frames. The WS pump runs on a background task; the channel closes
/// when the daemon hangs up.
pub(crate) async fn monitor_stream(
    info: &ServeInfo,
    action: MonitorAction,
) -> Result<tokio::sync::mpsc::UnboundedReceiver<MonitorEvent>, String> {
    // Prefer the Unix domain socket (the daemon's primary local channel,
    // ADR-0096); fall back to TCP for exposed/legacy deployments — the same
    // transport policy as `remote::connect`/`remote::control`, so the monitor
    // stream works against a UDS-only daemon.
    #[cfg(unix)]
    if let Some(uds) = &info.uds_path
        && let Ok(stream) = tokio::net::UnixStream::connect(uds).await
    {
        let request = "ws://localhost/"
            .into_client_request()
            .map_err(|e| format!("bad uds ws request: {e}"))?;
        let (ws, _) = tokio_tungstenite::client_async(request, stream)
            .await
            .map_err(|e| format!("ws handshake over uds: {e}"))?;
        return finish_monitor(ws.split(), action, "uds").await;
    }
    let url = format!("ws://127.0.0.1:{}/", info.port);
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|e| format!("bad ws url {url}: {e}"))?;
    if let Some(token) = &info.token {
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| format!("bad bearer token: {e}"))?;
        request.headers_mut().insert("Authorization", value);
    }
    let (ws, _response) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| format!("ws connect to {url}: {e}"))?;
    finish_monitor(ws.split(), action, &url).await
}

/// The stream-generic monitor handshake + framing, shared by the UDS and TCP
/// paths: send the `Select{Monitor}` handshake, await the opening snapshot
/// (bounded), then forward every diff frame into the returned channel.
async fn finish_monitor<S>(
    parts: (
        futures::stream::SplitSink<tokio_tungstenite::WebSocketStream<S>, WsMessage>,
        futures::stream::SplitStream<tokio_tungstenite::WebSocketStream<S>>,
    ),
    action: MonitorAction,
    target: &str,
) -> Result<tokio::sync::mpsc::UnboundedReceiver<MonitorEvent>, String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut ws_sink, mut ws_source) = parts;

    let select = serde_json::to_string(&Wire::Select {
        action: neenee_transport::serve::AttachAction::Monitor(action),
        // Monitor streams are host-wide; no project scope applies.
        project: None,
    })
    .map_err(|e| format!("serialize select: {e}"))?;
    ws_sink
        .send(WsMessage::Text(select.into()))
        .await
        .map_err(|e| format!("ws send select: {e}"))?;

    // Await the opening snapshot (or a handshake-level error) with a bound.
    let first = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
        loop {
            match ws_source.next().await {
                Some(Ok(WsMessage::Text(text))) => match serde_json::from_str::<Wire>(&text) {
                    Ok(Wire::Monitor { event }) => return Ok(event),
                    Ok(Wire::Error { message }) => return Err(message),
                    Ok(_) => tracing::warn!("status: unexpected frame during handshake, ignored"),
                    Err(error) => tracing::warn!(%error, "status: bad frame during handshake"),
                },
                Some(Ok(_)) => {}
                Some(Err(error)) => return Err(format!("ws recv during handshake: {error}")),
                None => return Err("server closed the connection".to_string()),
            }
        }
    })
    .await
    .map_err(|_| format!("timed out waiting for monitor snapshot from {target}"))??;

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let _ = tx.send(first);
    tokio::spawn(async move {
        while let Some(frame) = ws_source.next().await {
            match frame {
                Ok(WsMessage::Text(text)) => match serde_json::from_str::<Wire>(&text) {
                    Ok(Wire::Monitor { event }) => {
                        if tx.send(event).is_err() {
                            return;
                        }
                    }
                    Ok(_) => tracing::warn!("status: unexpected post-handshake frame, ignored"),
                    Err(error) => tracing::warn!(%error, "status: bad frame from daemon, ignored"),
                },
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(%error, "status: ws recv failed");
                    break;
                }
            }
        }
    });
    Ok(rx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, status: SessionStatus) -> MonitoredSession {
        MonitoredSession {
            id: id.into(),
            overview: "refactor the parser".into(),
            created_at: 1,
            updated_at: 100,
            message_count: 12,
            status,
            hosting: SessionHosting::Hosted,
            round: 3,
            turn: Some(1),
            output_tokens: 1_240,
            elapsed_ms: 83_000,
            current_tool: Some("bash".into()),
            activity: Some("waiting for model".into()),
            context_tokens: Some(48_200),
            note: None,
            project_root: "/tmp/project".into(),
            wip: None,
        }
    }

    fn snapshot(rows: Vec<MonitoredSession>) -> MonitorSnapshot {
        MonitorSnapshot {
            project_root: "/home/u/proj".into(),
            daemon_started_at: 50,
            sessions: rows,
        }
    }

    #[test]
    fn empty_snapshot_reports_all_quiet() {
        let text = table(&snapshot(Vec::new()));
        assert!(text.contains("all quiet"), "{text}");
        assert!(text.contains("/home/u/proj"), "{text}");
    }

    #[test]
    fn table_renders_status_round_and_detail() {
        let text = table(&snapshot(vec![row("abcdef123456", SessionStatus::Running)]));
        assert!(text.contains("abcdef12"), "{text}");
        assert!(text.contains("running"), "{text}");
        assert!(text.contains("3 › 1"), "{text}");
        assert!(text.contains("tool bash"), "{text}");
        assert!(text.contains("1m23s"), "{text}");
        assert!(text.contains("ctx 48.2k"), "{text}");
    }

    #[test]
    fn blocked_row_shows_its_note() {
        let mut blocked = row("zz", SessionStatus::NeedsApproval);
        blocked.current_tool = None;
        blocked.note = Some("permission: write_file".into());
        let text = table(&snapshot(vec![blocked]));
        assert!(text.contains("needs-approval"), "{text}");
        assert!(text.contains("permission: write_file"), "{text}");
        // The note wins over the raw activity string for blocked rows.
        assert!(!text.contains("waiting for model"), "{text}");
    }

    #[test]
    fn upsert_replaces_in_place_and_sorts_by_recency() {
        let mut rows = vec![row("a", SessionStatus::Running)];
        let mut newer = row("b", SessionStatus::Idle);
        newer.updated_at = 200;
        upsert(&mut rows, newer);
        assert_eq!(rows[0].id, "b");
        let mut updated = row("b", SessionStatus::Failed);
        updated.updated_at = 300;
        upsert(&mut rows, updated);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].status, SessionStatus::Failed);
    }

    #[test]
    fn elapsed_formats_progressively() {
        assert_eq!(fmt_elapsed(0), "–");
        assert_eq!(fmt_elapsed(9_000), "9s");
        assert_eq!(fmt_elapsed(83_000), "1m23s");
        assert_eq!(fmt_elapsed(3_900_000), "1h05m");
    }
}
