//! Event-sourced session persistence (C11 foundation).
//!
//! The event log is the authoritative history for each project. `SessionStore`
//! replays the log on load to rebuild the in-memory `SessionData` snapshot,
//! and appends a new event for every mutation. The snapshot file is kept as a
//! cache so readers that do not need the full replay path can still open it,
//! but on a conflict the log wins.
//!
//! Events are stored as JSON Lines. Each line is an [`EventEnvelope`] carrying
//! a monotonic sequence number, a wall-clock timestamp, and the event payload.

use crate::fsutil;
use crate::session::ContextProjectionCheckpoint;
use muta_contracts::Message;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// A single change to a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    /// Session was created or opened from a prior snapshot.
    Started {
        id: String,
        parent_id: Option<String>,
        created_at: u64,
        project_root: PathBuf,
        schema_version: u32,
    },
    /// The active message list was replaced (e.g. after a round, on open, or
    /// after tool-result pruning).
    MessagesReplaced { messages: Vec<Message> },
    /// Messages were appended to the active list without rewriting the whole
    /// window. Emitted at ReAct-turn boundaries mid-round so a crash after a
    /// side-effecting tool call still leaves the transcript in sync with the
    /// filesystem. Replayed by appending to `data.messages`; a `MessagesReplaced`
    /// later in the log supersedes it (snapshot semantics). See ADR-0048.
    MessagesAppended { messages: Vec<Message> },
    /// A model-context projection (tool-result pruning or summarizing compaction)
    /// archived originals and replaced the model-visible window.
    ContextProjectionCommitted {
        archived_originals: Vec<Message>,
        model_window: Vec<Message>,
        checkpoint: ContextProjectionCheckpoint,
    },
    /// Messages were moved into the archived list without a compaction.
    Archived { messages: Vec<Message> },
    /// The active session was reset to a fresh empty session.
    Reset { id: String },
    /// The current session was forked: the active id changed and a parent link
    /// was recorded. Any archived messages are preserved by a preceding
    /// `Archived` event.
    Forked { id: String, parent_id: String },
    /// The unified task list changed (`todo` / `todo_update`). Mirrored from
    /// `Agent::todos` so resume restores the task panel. The full list is
    /// stored on every change (snapshot semantics); history of individual
    /// items is reconstructable from the log itself.
    TodosSet { todos: muta_contracts::TodoList },
    /// The scheduled-prompt list changed (`/schedule` add / cancel / fire, and
    /// the legacy `/repeat`). Snapshot semantics: the full list is stored on
    /// every change so resume restores the same schedule. The session that
    /// created a job owns it; fork and resume carry it along just like the
    /// todos.
    ScheduledJobsSet {
        jobs: Vec<muta_contracts::ScheduledJob>,
    },
    /// The durable command ledger changed (ADR-0091). Snapshot semantics: the
    /// full list is stored on every change so resume restores every command
    /// invocation and its typed result.
    CommandsReplaced {
        commands: Vec<muta_contracts::CommandRecord>,
    },
    /// The session title changed (ADR-0022). `title = None` clears it. `manual`
    /// marks a user-set title (`/title <text>`) that AI generation must not
    /// overwrite; automatic and on-demand generation always set `manual = false`.
    TitleSet { title: Option<String>, manual: bool },
    /// The session-level disabled-tool mask changed (ADR-0048 Phase 2).
    /// Snapshot semantics: the full set is stored on every change. Mirrors
    /// `Agent::disabled_tools` so a user toggle survives restart.
    DisabledToolsSet {
        tools: std::collections::HashSet<String>,
    },
    /// The harness round counter advanced (ADR-0048 Phase 2). Snapshot
    /// semantics. Mirrors `Agent::round_counter` so a resumed session's todo
    /// stale-detector comparisons stay valid.
    RoundCounterSet { counter: u64 },
    /// Insert or replace one lifecycle-aware request attempt. The key makes
    /// replay idempotent and avoids rewriting the full ledger on every stream
    /// boundary.
    RequestUsageUpsert {
        record: muta_contracts::RequestUsageRecord,
    },
    /// The session-scoped provider + model pin changed (C6). `selection = None`
    /// means "follow the global default". Snapshot semantics. Set by the
    /// `/models` switch handler so the session reopens on its own provider
    /// instead of the global default; read on resume to restore it. The global
    /// `config.toml` selection is left untouched, so one session switching
    /// provider/model never affects another.
    ProviderSelectionSet {
        selection: Option<crate::session::ProviderSelection>,
    },
    /// One round-interrupt record was appended (C11). The record carries its
    /// own `at_ms` because envelope timestamps are destroyed by log
    /// compaction. Replayed by appending to
    /// `data.round_interrupts`; `clear_round_interrupts` removes them.
    RoundInterruptRecorded {
        record: muta_contracts::RoundInterrupt,
    },
    /// All round-interrupt records were cleared (C11) — the interrupted
    /// round either completed on retry or the user resumed and moved on.
    /// Snapshot semantics: the list becomes empty.
    RoundInterruptsCleared {},
    /// The durable `/retry` resume point (C12) — the exact history watermark,
    /// turn ordinal, and paused accumulator of a round that stopped before
    /// completing, recorded so a later `/retry` can resume *that* round
    /// instead of minting a new one. Snapshot semantics: the single optional
    /// slot is replaced.
    RetryPendingRecorded { point: muta_contracts::RetryPoint },
    /// The `/retry` resume point was cleared (C12) — the stopped round
    /// completed (naturally or via retry), or the session moved on and the
    /// point went stale. Snapshot semantics: the slot becomes `None`.
    RetryPendingCleared {},
    /// The session's autopilot posture changed (ADR-0132). Snapshot
    /// semantics. Mirrors `Agent::get_autopilot` so a daemon restart
    /// (crash, kill, upgrade, reboot) restores the session in the same
    /// attended/unattended posture it died in instead of silently
    /// de-escalating to attended. Written by the `/autopilot` handler, the
    /// `--autopilot` startup path, and `/principal` role switches; read on
    /// every resume/rehost.
    AutopilotSet { enabled: bool },
}

/// Wrapper around a [`SessionEvent`] that adds metadata for ordering and
/// debugging. Stored as one JSON object per line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub seq: u64,
    pub timestamp: u64,
    #[serde(flatten)]
    pub event: SessionEvent,
}

/// Append-only event log for one project.
pub struct EventLog {
    path: PathBuf,
    /// Cached next sequence number. Populated lazily from the highest existing
    /// `seq` on the first append, then bumped monotonically so `append` is O(1)
    /// instead of re-reading and re-parsing the entire log on every event (the
    /// old behaviour was O(n) per append — quadratic over a long session).
    next_seq: std::sync::atomic::AtomicU64,
    /// Cached high-water `seq`, encoded: `0` = not yet resolved;
    /// `Some(seq)` is stored as `seq + 1`; `None` (a scanned-empty log) is
    /// stored as `1`. This makes every real seq and both resolved states
    /// representable without a second flag word — `high_seq()` is on every
    /// snapshot persist, and re-parsing the whole log each time was the same
    /// quadratic trap the `next_seq` cache fixed for `append`.
    high_seq_cache: std::sync::atomic::AtomicU64,
}

impl EventLog {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            next_seq: std::sync::atomic::AtomicU64::new(u64::MAX),
            high_seq_cache: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Read all events in log order.
    pub fn load(&self) -> Result<Vec<EventEnvelope>, String> {
        self.load_since(None)
    }

    /// Read only events with `seq > watermark` (or all events when `watermark`
    /// is `None`), in ascending `seq` order. Used by the snapshot fast path:
    /// a session's snapshot records the highest seq it already folded in
    /// `applied_seq`, so resuming replays only the post-snapshot tail instead
    /// of the whole history. Every line is still read (JSONL is not
    /// seekable by seq), but envelopes at or below the watermark are parsed
    /// and discarded without being stored, so a fresh snapshot with an empty
    /// tail pays only the sequential read cost, not the per-event allocation.
    pub fn load_since(&self, watermark: Option<u64>) -> Result<Vec<EventEnvelope>, String> {
        let file = match File::open(&self.path) {
            Ok(f) => f,
            Err(_) => return Ok(Vec::new()),
        };
        let reader = BufReader::new(file);
        let mut events = Vec::new();
        for (line_number, line) in reader.lines().enumerate() {
            let line = line.map_err(|e| format!("could not read event line: {e}"))?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<EventEnvelope>(&line) {
                Ok(envelope) => {
                    if watermark.is_none_or(|w| envelope.seq > w) {
                        events.push(envelope);
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        path = %self.path.display(),
                        line = line_number + 1,
                        error = %error,
                        "skipping malformed event line"
                    );
                }
            }
        }
        events.sort_by_key(|e| e.seq);
        Ok(events)
    }

    /// Whether the log file is empty (missing or zero-length). A metadata stat,
    /// not a parse: `ensure_event_log_started` is called on every mutation, so
    /// the old behaviour of re-parsing the entire log just to test emptiness
    /// was an O(n) cost per write. A file that exists with non-zero length is
    /// treated as seeded; the load path already tolerates blank/malformed
    /// trailing lines.
    pub fn is_empty(&self) -> bool {
        !self.path.exists()
            || std::fs::metadata(&self.path)
                .map(|m| m.len() == 0)
                .unwrap_or(true)
    }

    /// The highest `seq` present in the log, or `None` when the log is empty.
    /// Used to decide whether a snapshot's watermark is already at the
    /// high-water mark (tail empty) without replaying.
    ///
    /// O(1) after the first call: the high-water mark is cached and advanced
    /// by `append`/`rewrite`, so a snapshot persist on a long session no
    /// longer re-reads and re-parses the whole log every time (the previous
    /// per-persist full scan made persistence cost grow linearly with log
    /// size — quadratic over a session's lifetime).
    pub fn high_seq(&self) -> Option<u64> {
        // Encoding: 0 = not yet resolved; 1 = resolved "no events";
        // seq + 2 = resolved high-water mark. The +2 keeps `Some(0)` (a
        // one-event log) distinct from both sentinels — an earlier +1 draft
        // collided `Some(0)` with "no events" and re-broke compaction.
        let cached = self
            .high_seq_cache
            .load(std::sync::atomic::Ordering::Acquire);
        if cached >= 2 {
            return Some(cached - 2);
        }
        // `1` is the resolved "no events" state.
        if cached == 1 {
            return None;
        }
        let scanned = Self::scan_high_seq(&self.path);
        self.high_seq_cache.store(
            scanned.map_or(1, |seq| seq + 2),
            std::sync::atomic::Ordering::Release,
        );
        scanned
    }

    fn scan_high_seq(path: &Path) -> Option<u64> {
        let file = File::open(path).ok()?;
        let reader = BufReader::new(file);
        let mut max: Option<u64> = None;
        for line in reader.lines().map_while(Result::ok) {
            if line.trim().is_empty() {
                continue;
            }
            // Parse only the leading `seq` rather than the full envelope: a
            // tiny serde_json::Value read of the first field avoids
            // deserializing every message payload just to learn the high-water
            // mark.
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line)
                && let Some(seq) = value.get("seq").and_then(|v| v.as_u64())
            {
                max = Some(max.map_or(seq, |m| m.max(seq)));
            }
        }
        max
    }

    /// Append a single event atomically-ish: the line is written with
    /// `O_APPEND` and fsynced. A crash between write and fsync may leave a
    /// partial line; readers skip malformed lines. Returns the reserved `seq`,
    /// so callers can record it as the watermark of the data they just mutated.
    pub fn append(&self, event: SessionEvent) -> Result<u64, String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        // Lazily seed the cached seq counter from the log on first use, then
        // bump it in place. The sentinel `u64::MAX` means "not yet seeded".
        // After the first append this is an O(1) fetch_add instead of the
        // previous O(n) full-log re-read.
        //
        // Invariant: after this block the counter holds the next seq *after*
        // `next_seq`, i.e. ready for a future append to reserve.
        let next_seq = match self.next_seq.compare_exchange(
            u64::MAX,
            u64::MAX, // never observably used as a real seq
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        ) {
            // First append: seed from the log's high-water mark, then reserve
            // `base` and leave the counter at `base + 1`.
            Ok(_) => {
                let base = self.load()?.last().map(|e| e.seq + 1).unwrap_or(0);
                self.next_seq
                    .store(base + 1, std::sync::atomic::Ordering::Release);
                base
            }
            // Already seeded: atomically reserve the next id (counter advances).
            Err(_) => self
                .next_seq
                .fetch_add(1, std::sync::atomic::Ordering::AcqRel),
        };
        let envelope = EventEnvelope {
            seq: next_seq,
            timestamp: crate::session::unix_timestamp(),
            event,
        };
        let line = serde_json::to_vec(&envelope).map_err(|e| e.to_string())?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| format!("could not open event log: {e}"))?;
        file.write_all(&line)
            .and_then(|_| file.write_all(b"\n"))
            .and_then(|_| file.sync_all())
            .map_err(|e| format!("could not append event: {e}"))?;
        // Keep the high-water cache coherent. Encoding (seq + 2) preserves
        // order across the unresolved (0) and resolved-None (1) states, so a
        // conditional max is correct — including the first append (seq 0,
        // encoded 2) escaping an unresolved or empty cache.
        let encoded = next_seq + 2;
        let prior = self
            .high_seq_cache
            .load(std::sync::atomic::Ordering::Acquire);
        if encoded > prior {
            self.high_seq_cache
                .store(encoded, std::sync::atomic::Ordering::Release);
        }
        Ok(next_seq)
    }

    /// Replace the entire log with the given events. Used when compacting the
    /// log into a seed snapshot or when pruning old events.
    pub fn rewrite(&self, events: Vec<EventEnvelope>) -> Result<(), String> {
        let mut lines = Vec::new();
        let mut max_seq: Option<u64> = None;
        for envelope in events {
            max_seq = Some(max_seq.map_or(envelope.seq, |m| m.max(envelope.seq)));
            let mut line = serde_json::to_vec(&envelope).map_err(|e| e.to_string())?;
            line.push(b'\n');
            lines.extend(line);
        }
        fsutil::atomic_write_bytes(&self.path, &lines)
            .map_err(|e| format!("could not rewrite event log: {e}"))?;
        // The rewrite replaced the log wholesale; re-anchor the cache to the
        // new content (compaction's single seed typically *lowers* the mark,
        // so a max() here would be wrong).
        self.high_seq_cache.store(
            max_seq.map_or(1, |seq| seq + 2),
            std::sync::atomic::Ordering::Release,
        );
        Ok(())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::ContextProjectionKind;

    #[test]
    fn event_log_round_trips() {
        let dir = std::env::temp_dir().join(format!("muta-events-test-{}", uuid::Uuid::new_v4()));
        let log = EventLog::new(dir.join("events.jsonl"));

        log.append(SessionEvent::Reset {
            id: "a".to_string(),
        })
        .unwrap();
        log.append(SessionEvent::MessagesReplaced {
            messages: vec![muta_contracts::Message::new(
                muta_contracts::Role::User,
                "hi",
            )],
        })
        .unwrap();

        let loaded = log.load().unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].seq, 0);
        assert_eq!(loaded[1].seq, 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn event_log_seq_increments_monotonically() {
        let dir = std::env::temp_dir().join(format!("muta-events-seq-{}", uuid::Uuid::new_v4()));
        let log = EventLog::new(dir.join("events.jsonl"));

        // Three appends must yield seq 0, 1, 2 — the cached counter, not a
        // per-append full-log re-read (regression: O(n) re-read still produced
        // correct ids, this just locks the contract in).
        for _ in 0..3 {
            log.append(SessionEvent::Reset {
                id: "x".to_string(),
            })
            .unwrap();
        }
        let loaded = log.load().unwrap();
        let seqs: Vec<u64> = loaded.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![0, 1, 2]);

        // A fresh EventLog over the *same* file must seed from the existing
        // high-water mark, not restart at 0.
        let log2 = EventLog::new(dir.join("events.jsonl"));
        log2.append(SessionEvent::Reset {
            id: "y".to_string(),
        })
        .unwrap();
        let loaded2 = log2.load().unwrap();
        assert_eq!(loaded2.last().unwrap().seq, 3);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn pre_projection_event_tags_are_skipped_not_replayed() {
        // ADR-0120 policy: no serde aliases for renamed event shapes. An
        // event line written before the projection rename
        // (`compaction_committed`, `archived`/`active` fields) fails to
        // parse and the loader skips it with a warn — the accepted cost of
        // carrying no compat layer. This pins that behavior so a future
        // alias does not creep back in unnoticed.
        let dir = std::env::temp_dir().join(format!("muta-events-legacy-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("events.jsonl");
        std::fs::write(
            &path,
            "{\"seq\":0,\"timestamp\":1,\"type\":\"compaction_committed\",\
             \"archived\":[],\"active\":[],\
             \"checkpoint\":{\"archived_messages\":2,\"active_messages\":3,\
             \"window_tokens_before\":100,\"window_tokens_after\":40}}\n",
        )
        .unwrap();
        let loaded = EventLog::new(path).load().unwrap();
        assert!(
            loaded.is_empty(),
            "legacy line must be skipped, got {loaded:?}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn context_projection_committed_writes_new_projection_shape() {
        let dir =
            std::env::temp_dir().join(format!("muta-events-projection-{}", uuid::Uuid::new_v4()));
        let log = EventLog::new(dir.join("events.jsonl"));

        log.append(SessionEvent::ContextProjectionCommitted {
            archived_originals: vec![muta_contracts::Message::new(
                muta_contracts::Role::Tool,
                "old",
            )],
            model_window: vec![muta_contracts::Message::new(
                muta_contracts::Role::User,
                "live",
            )],
            checkpoint: crate::session::ContextProjectionCheckpoint {
                operation: ContextProjectionKind::Prune,
                archived_messages: 1,
                active_messages: 1,
                window_tokens_before: 100,
                window_tokens_after: 20,
            },
        })
        .unwrap();

        let raw = std::fs::read_to_string(log.path()).unwrap();
        assert!(raw.contains("\"type\":\"context_projection_committed\""));
        assert!(raw.contains("\"archived_originals\""));
        assert!(raw.contains("\"model_window\""));
        assert!(raw.contains("\"operation\":\"prune\""));

        let loaded = log.load().unwrap();
        match &loaded[0].event {
            SessionEvent::ContextProjectionCommitted {
                archived_originals,
                model_window,
                checkpoint,
            } => {
                assert_eq!(archived_originals[0].content, "old");
                assert_eq!(model_window[0].content, "live");
                assert_eq!(checkpoint.operation, ContextProjectionKind::Prune);
            }
            other => panic!("projection event did not round-trip: {other:?}"),
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn event_log_skips_malformed_lines() {
        let dir =
            std::env::temp_dir().join(format!("muta-events-corrupt-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("events.jsonl");
        std::fs::write(
            &path,
            "{\"seq\":0,\"timestamp\":1,\"type\":\"reset\",\"id\":\"x\"}\nnot-json\n",
        )
        .unwrap();
        let log = EventLog::new(path);
        let loaded = log.load().unwrap();
        assert_eq!(loaded.len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn messages_appended_round_trips_and_replays_after_replace() {
        // The incremental event (ADR-0048) must serialize under its snake_case
        // tag and round-trip through the log. A `MessagesReplaced` seeds the
        // window, then a `MessagesAppended` extends it — the exact interleave
        // `append_turn` produces at a turn boundary inside a real round.
        let dir = std::env::temp_dir().join(format!("muta-events-append-{}", uuid::Uuid::new_v4()));
        let log = EventLog::new(dir.join("events.jsonl"));

        log.append(SessionEvent::MessagesReplaced {
            messages: vec![muta_contracts::Message::new(
                muta_contracts::Role::User,
                "seed",
            )],
        })
        .unwrap();
        log.append(SessionEvent::MessagesAppended {
            messages: vec![
                muta_contracts::Message::new(muta_contracts::Role::Assistant, "turn 1"),
                muta_contracts::Message::new(muta_contracts::Role::Tool, "result 1"),
            ],
        })
        .unwrap();

        let loaded = log.load().unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(
            matches!(&loaded[1].event, SessionEvent::MessagesAppended { messages } if messages.len() == 2),
            "appended event must deserialize back to MessagesAppended"
        );
        // The on-disk tag is snake_case "messages_appended".
        let raw = std::fs::read_to_string(log.path()).unwrap();
        assert!(
            raw.contains("\"type\":\"messages_appended\""),
            "incremental event must serialize under messages_appended: {raw}"
        );

        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn round_counter_event_writes_canonical_tag() {
        // ADR-0120 policy: the pre-rename `turn_counter_set` tag is not aliased
        // and must fail to deserialize.
        let legacy =
            serde_json::from_str::<SessionEvent>(r#"{"type":"turn_counter_set","counter":7}"#);
        assert!(legacy.is_err(), "legacy tag must not parse");

        let serialized =
            serde_json::to_string(&SessionEvent::RoundCounterSet { counter: 7 }).unwrap();
        assert!(serialized.contains("\"type\":\"round_counter_set\""));
    }
    /// `high_seq` is cached and stays coherent across appends, rewrites (the
    /// compaction seed *lowers* the mark), and empty logs — the per-persist
    /// full-log scan it replaced made each snapshot write cost O(log size).
    #[test]
    fn high_seq_cache_tracks_append_and_rewrite() {
        let dir = std::env::temp_dir().join(format!("muta-events-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        let log = EventLog::new(path.clone());

        // Empty log: None, cached.
        assert_eq!(log.high_seq(), None);
        assert_eq!(log.high_seq(), None);

        let mk = || SessionEvent::Started {
            id: "s".into(),
            parent_id: None,
            created_at: 0,
            project_root: PathBuf::from("/tmp"),
            schema_version: 1,
        };
        let first = log.append(mk()).unwrap();
        let second = log.append(mk()).unwrap();
        assert_eq!(log.high_seq(), Some(second));
        assert!(second > first, "seq must advance");

        // A rewrite to a single seed with a LOWER seq must lower the mark
        // (compaction does exactly this).
        log.rewrite(vec![EventEnvelope {
            seq: 0,
            timestamp: 0,
            event: mk(),
        }])
        .unwrap();
        assert_eq!(log.high_seq(), Some(0), "rewrite re-anchors the mark");

        // A fresh handle over the same file must still scan correctly.
        let fresh = EventLog::new(path);
        assert_eq!(fresh.high_seq(), Some(0));
    }
}
