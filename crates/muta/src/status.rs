//! `muta daemon status` (ADR-0093): the first control-plane client of the daemon
//! monitor protocol. One-shot by default (`muta daemon status`), a live table with
//! `--watch`, machine-readable frames with `--json`.
//!
//! Unlike `mutx attach`, status never spawns a daemon: observing is only
//! meaningful when a host is already running, so a missing/stale discovery
//! record is a clean "no daemon" report, not an excuse to start one.
//!
//! This module is presentation only: the monitor-protocol client
//! ([`muta_runtime::client::monitor_stream`]) and the stream-folding helper
//! ([`muta_runtime::client::upsert_session_row`]) live with the wire protocol
//! in `muta-runtime`; what remains here is the terminal rendering of the
//! snapshot.

use std::path::Path;

use muta_contracts::{
    MonitorAction, MonitorEvent, MonitorSnapshot, MonitoredSession, SessionHosting, SessionStatus,
};
use muta_runtime::client::{self, DaemonDiagnostics, upsert_session_row};

/// How `muta daemon status` renders its stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusOptions {
    pub watch: bool,
    pub json: bool,
    pub include_idle: bool,
    pub diagnostic: bool,
}

pub async fn run(project_root: &Path, opts: StatusOptions) -> Result<(), String> {
    if opts.diagnostic {
        let diag = client::diagnose_daemon();
        render_diagnostics(&diag, opts.json);
        if opts.watch {
            return Err("cannot watch static diagnostic output".to_string());
        }
        if client::discover(project_root).is_none() {
            return Ok(());
        }
        println!();
    }

    let Some(info) = client::discover(project_root) else {
        let diag = client::diagnose_daemon();
        render_diagnostics(&diag, opts.json);
        return Ok(());
    };
    if !client::versions_compatible(&info) {
        return Err(client::incompatibility_error(&info));
    }
    let action = MonitorAction {
        watch: opts.watch,
        include_idle: opts.include_idle,
    };
    let mut rx = client::monitor_stream(&info, action).await?;
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
                upsert_session_row(&mut state.sessions, row);
            }
            MonitorEvent::SessionRemoved { session_id } => {
                state.sessions.retain(|row| row.id != session_id);
            }
            // The daemon is draining (ADR-0101): the stream ends right
            // after this frame. Print a note and stop watching — the next
            // `muta daemon status` re-discovers (or reports none running).
            MonitorEvent::DaemonDraining => {
                if !opts.json {
                    eprintln!("muta: daemon is shutting down; watch ended.");
                }
                return Ok(());
            }
        }
        render(&state, opts);
    }
    Ok(())
}

fn render_diagnostics(diag: &DaemonDiagnostics, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string(diag).unwrap_or_else(|_| "{}".to_string())
        );
    } else {
        print!("{}", format_diagnostics(diag));
    }
}

/// The human-readable daemon diagnostics output.
pub(crate) fn format_diagnostics(diag: &DaemonDiagnostics) -> String {
    let mut out = String::new();
    out.push_str("muta daemon — system status & diagnostics:\n");

    // Instance scope first (ADR-0121): every path below reads differently
    // once the reader knows whether this client resolves the host instance
    // or an isolated `MUTA_HOME` sandbox.
    out.push_str(&format!(
        "  Instance:          {} (default port {})\n",
        diag.instance_dir.display(),
        diag.default_port
    ));

    // Discovery record
    out.push_str("  Discovery Record: ");
    match &diag.discovery_record {
        Some(rec) => {
            let ver = rec.version.as_deref().unwrap_or("unknown");
            let alive_tag = if client::is_process_alive(rec.pid) {
                "alive"
            } else {
                "dead/stale"
            };
            out.push_str(&format!(
                "present (PID {}, {}, v{}, port {})\n",
                rec.pid, alive_tag, ver, rec.port
            ));
            out.push_str(&format!("    • Path: {}\n", diag.discovery_path.display()));
        }
        None => {
            out.push_str(&format!("missing ({})\n", diag.discovery_path.display()));
        }
    }

    // Instance Lock
    out.push_str("  Instance Lock:    ");
    if diag.lock_held {
        if let Some(pid) = diag.lock_holder_pid {
            let alive_tag = if diag.lock_holder_alive {
                "alive"
            } else {
                "dead"
            };
            out.push_str(&format!("HELD by PID {pid} (process {alive_tag})\n"));
        } else {
            out.push_str("HELD by another process\n");
        }
        out.push_str(&format!("    • Path: {}\n", diag.lock_path.display()));
    } else {
        out.push_str(&format!("free ({})\n", diag.lock_path.display()));
    }

    // Endpoints
    out.push_str("  Control Endpoints:\n");
    if let Some(endpoint) = &diag.local_endpoint {
        let local_status = if diag.local_endpoint_connectable {
            "active (connectable)"
        } else if diag.local_endpoint_exists {
            "unresponsive"
        } else {
            "not created"
        };
        out.push_str(&format!("    • Local: {endpoint} ({local_status})\n"));
    } else {
        out.push_str("    • Local: unavailable (endpoint resolution failed)\n");
    }

    let tcp_status = if diag.tcp_listening {
        "listening"
    } else {
        "closed"
    };
    out.push_str(&format!(
        "    • TCP: ws://127.0.0.1:{} ({tcp_status})\n",
        diag.tcp_port
    ));

    // Startup Log
    if let Some(last_log) = &diag.last_startup_log {
        out.push_str("  Recent Startup Log:\n");
        for line in last_log.lines().take(5) {
            out.push_str(&format!("    | {line}\n"));
        }
    }

    // High level diagnosis
    out.push_str("  Diagnosis:        ");
    if diag.discovery_valid && diag.tcp_listening {
        out.push_str("Daemon is running and healthy. (Observe with `muta status --watch`)\n");
    } else if diag.lock_held && diag.discovery_record.is_none() {
        out.push_str(
            "Ghost daemon detected: Instance lock is held but discovery record is missing.\n",
        );
        out.push_str("                    Run `muta stop` or kill the locking PID, then start with `muta start`.\n");
    } else if !diag.lock_held && diag.discovery_record.is_some() {
        out.push_str("Stale discovery record: Process is gone but discovery record remains.\n");
        out.push_str("                    Start a new daemon with `muta start`.\n");
    } else if !diag.lock_held {
        out.push_str("No session daemon is running.\n");
        out.push_str("                    Start one with `muta start`.\n");
    } else {
        out.push_str("Daemon state is transitioning or unresponsive.\n");
    }

    out
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
        "muta daemon — {} — {} session(s) needing attention\n",
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

#[cfg(test)]
mod tests {
    use super::*;
    use muta_contracts::SessionForkKind;

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
            current_tool: Some("execute_command".into()),
            activity: Some("waiting for model".into()),
            context_tokens: Some(48_200),
            note: None,
            project_root: "/tmp/project".into(),
            parent_id: None,
            fork_kind: SessionForkKind::default(),
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
        assert!(text.contains("tool execute_command"), "{text}");
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
        upsert_session_row(&mut rows, newer);
        assert_eq!(rows[0].id, "b");
        let mut updated = row("b", SessionStatus::Failed);
        updated.updated_at = 300;
        upsert_session_row(&mut rows, updated);
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

    fn base_diag() -> DaemonDiagnostics {
        DaemonDiagnostics {
            instance_dir: std::path::PathBuf::from("/run/user/1000/muta"),
            default_port: 9800,
            discovery_path: std::path::PathBuf::from("/run/user/1000/muta/daemon.json"),
            discovery_record: None,
            discovery_valid: true,
            lock_path: std::path::PathBuf::from("/run/user/1000/muta/daemon.lock"),
            lock_held: false,
            lock_holder_pid: None,
            lock_holder_alive: false,
            local_endpoint: Some(muta_platform::ipc::LocalEndpoint::UnixSocket(
                std::path::PathBuf::from("/run/user/1000/muta/daemon.sock"),
            )),
            local_endpoint_exists: false,
            local_endpoint_connectable: false,
            tcp_port: 9800,
            tcp_listening: false,
            startup_log_path: std::path::PathBuf::from("/tmp/startup.log"),
            last_startup_log: None,
        }
    }

    #[test]
    fn diagnostics_formatter_renders_healthy_state() {
        let diag = DaemonDiagnostics {
            discovery_record: Some(muta_runtime::client::DaemonInfo {
                pid: 12345,
                process_birth_token: None,
                port: 9800,
                token: None,
                project_root: String::new(),
                started_at: 1000,
                uds_path: Some(std::path::PathBuf::from("/run/user/1000/muta/daemon.sock")),
                local_endpoint: None,
                version: Some("0.25.1".to_string()),
                grace_secs: Some(10),
                protocol: None,
            }),
            discovery_valid: true,
            lock_held: true,
            lock_holder_pid: Some(12345),
            lock_holder_alive: true,
            local_endpoint_exists: true,
            local_endpoint_connectable: true,
            tcp_listening: true,
            ..base_diag()
        };
        let text = format_diagnostics(&diag);
        assert!(text.contains("PID 12345"), "{text}");
        assert!(text.contains("HELD by PID 12345"), "{text}");
        assert!(text.contains("Daemon is running and healthy"), "{text}");
        // The instance scope line leads the report (ADR-0121).
        assert!(text.contains("Instance:"), "{text}");
        assert!(text.contains("default port 9800"), "{text}");
    }

    #[test]
    fn diagnostics_formatter_detects_ghost_daemon() {
        let diag = DaemonDiagnostics {
            discovery_valid: false,
            lock_held: true,
            lock_holder_pid: Some(9999),
            lock_holder_alive: true,
            local_endpoint_exists: true,
            local_endpoint_connectable: false,
            tcp_listening: false,
            last_startup_log: Some("panic: something went wrong".to_string()),
            ..base_diag()
        };
        let text = format_diagnostics(&diag);
        assert!(text.contains("Ghost daemon detected"), "{text}");
        assert!(text.contains("HELD by PID 9999"), "{text}");
        assert!(text.contains("panic: something went wrong"), "{text}");
    }

    #[test]
    fn diagnostics_formatter_names_the_sandbox_instance() {
        // A sandboxed client (ADR-0121) must be identifiable at a glance:
        // the report's first data line names the instance dir and the
        // client-resolved default port, so "two daemons, one discovered"
        // becomes a one-command diagnosis.
        let mut diag = base_diag();
        diag.instance_dir = std::path::PathBuf::from("/tmp/muta-dev/muta/instance");
        diag.default_port = 9801;
        let text = format_diagnostics(&diag);
        assert!(
            text.contains("/tmp/muta-dev/muta/instance (default port 9801)"),
            "{text}"
        );
    }
}
