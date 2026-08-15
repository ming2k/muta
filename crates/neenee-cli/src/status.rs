//! `neenee status` (ADR-0093): the first control-plane client of the daemon
//! monitor protocol. One-shot by default (`neenee status`), a live table with
//! `--watch`, machine-readable frames with `--json`.
//!
//! Unlike `neenee attach`, status never spawns a daemon: observing is only
//! meaningful when a host is already running, so a missing/stale discovery
//! record is a clean "no daemon" report, not an excuse to start one.
//!
//! This module is presentation only: the monitor-protocol client
//! ([`neenee_runtime::client::monitor_stream`]) and the stream-folding helper
//! ([`neenee_runtime::client::upsert_session_row`]) live with the wire protocol
//! in `neenee-runtime`; what remains here is the terminal rendering of the
//! snapshot.

use std::path::Path;

use neenee_contracts::{
    MonitorAction, MonitorEvent, MonitorSnapshot, MonitoredSession, SessionHosting, SessionStatus,
};
use neenee_runtime::client::{self, upsert_session_row};

/// How `neenee status` renders its stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusOptions {
    pub watch: bool,
    pub json: bool,
    pub include_idle: bool,
}

pub async fn run(project_root: &Path, opts: StatusOptions) -> Result<(), String> {
    let Some(info) = client::discover(project_root) else {
        return Err(format!(
            "no neenee session daemon is running for {}. Start one with `neenee serve` \
             (or `neenee attach`, which spawns one on demand).",
            project_root.display()
        ));
    };
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
            // `neenee status` re-discovers (or reports none running).
            MonitorEvent::DaemonDraining => {
                if !opts.json {
                    eprintln!("neenee: daemon is shutting down; watch ended.");
                }
                return Ok(());
            }
        }
        render(&state, opts);
    }
    Ok(())
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
}
