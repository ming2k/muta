//! Cross-session usage-statistics store (ADR-0122).
//!
//! The durable sibling of the per-session token ledger. Terminal request
//! records are appended into **one JSON file per local day** under
//! `<data_dir>/usage/daily/` — a sibling of `projects/`, never inside a
//! project bucket — so the data survives every form of session cleanup
//! (deleting a session file, pruning empty sessions, wiping a whole project
//! bucket) and the `/usage` report reflects each day's real consumption.
//!
//! Correctness properties:
//! - **Append is idempotent per [`RequestUsageKey`]**: a replayed key is a
//!   no-op (crash-retry never double counts), and a replay carrying
//!   *reported* usage upgrades an earlier *estimated* record in place — the
//!   same monotonic rule the in-memory ledger applies.
//! - **Atomic**: each day file is rewritten via temp-file + rename
//!   ([`crate::fsutil::atomic_write_json`]); a crash mid-write never leaves
//!   a partial file.
//! - **Cross-process safe**: the read-modify-write window is serialised by a
//!   [`FileLock`] on a companion `.lock` file (the daemon and any
//!   `--no-daemon` standalone instance may write concurrently).
//! - **Unreadable days are non-fatal**: a corrupt/undecodable day file is
//!   skipped with a warning — usage telemetry must never take the app down.

use std::io::Read;
use std::path::{Path, PathBuf};

use muta_contracts::usage_stats::{
    UsageStatRecord, UsageStatsReport, aggregate_usage_records, day_key_from_epoch_ms,
};
use muta_contracts::{RequestUsageKey, RequestUsageRecord, RequestUsageSource};
use serde::{Deserialize, Serialize};

use crate::fsutil::{FileLock, atomic_write_json};
use crate::paths;

/// How many day files the report reads (newest first) before aggregation.
/// Bounds the work of a `/usage` query: 400 days ≈ 13 months of history.
const REPORT_DAY_WINDOW: usize = 400;

/// How many day files are **kept on disk**. Retention exceeds the report
/// window deliberately (data older than the window is never read, but the
/// user may lower `REPORT_DAY_WINDOW`-shaped settings before it is deleted);
/// anything past this age is telemetry about long-gone work and is pruned by
/// [`UsageStatsStore::prune_old_days`] so the store does not grow forever.
const RETAINED_DAYS: usize = 400;

/// On-disk shape of one day file: a plain list of records under a `records`
/// key (leaving room for future per-day metadata) with a `version` tag.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct DayFile {
    version: u32,
    #[serde(default)]
    records: Vec<UsageStatRecord>,
}

/// The append-only, day-partitioned usage store.
#[derive(Debug, Clone, Default)]
pub struct UsageStatsStore {
    /// Root override for tests. Production resolves via [`paths::get`].
    root: Option<PathBuf>,
}

impl UsageStatsStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind the store to an explicit root directory (tests / sandboxes).
    pub fn with_root(root: PathBuf) -> Self {
        Self {
            root: Some(root.join("usage")),
        }
    }

    fn root(&self) -> PathBuf {
        self.root
            .clone()
            .unwrap_or_else(|| paths::get().usage_stats_dir())
    }

    fn daily_dir(&self) -> PathBuf {
        self.root().join("daily")
    }

    fn day_file(&self, day: &str) -> PathBuf {
        if self.root.is_some() {
            self.root().join("daily").join(format!("{day}.json"))
        } else {
            paths::get().usage_stats_day_file(day)
        }
    }

    /// Delete day files older than `RETAINED_DAYS` newest days. Best-effort
    /// and cheap (a directory listing); called opportunistically from the
    /// settle path so telemetry cannot grow unboundedly. Returns the number
    /// of files removed.
    pub fn prune_old_days(&self) -> usize {
        let days = self.list_days();
        if days.len() <= RETAINED_DAYS {
            return 0;
        }
        let mut removed = 0;
        for day in days.into_iter().skip(RETAINED_DAYS) {
            if std::fs::remove_file(self.day_file(&day)).is_ok() {
                removed += 1;
            }
        }
        removed
    }

    /// Append one terminal request record to its day file. Idempotent per
    /// [`RequestUsageKey`]; a reported replay upgrades an estimated record.
    ///
    /// `recorded_at_ms` decides the day bucket (local timezone); `project`
    /// is the already-hashed project bucket name (empty = unknown).
    pub fn record(
        &self,
        recorded_at_ms: u64,
        project: &str,
        record: &RequestUsageRecord,
    ) -> Result<(), String> {
        if !record.status.is_terminal() {
            return Ok(());
        }
        let day = day_key_from_epoch_ms(recorded_at_ms);
        let path = self.day_file(&day);
        let mut day_file = read_day_file(&path);
        upsert_record(
            &mut day_file,
            UsageStatRecord {
                day,
                recorded_at_ms,
                project: project.to_string(),
                record: record.clone(),
            },
        );
        persist_day_file(&path, &day_file)
    }

    /// Append many records at once (batch flush). Each lands in its own day
    /// bucket; the whole batch shares one lock acquisition per distinct day
    /// file.
    pub fn record_batch(&self, entries: &[(u64, &str, RequestUsageRecord)]) -> Result<(), String> {
        // Group by day so each day file is loaded, mutated, and written once.
        let mut by_day: std::collections::BTreeMap<String, Vec<UsageStatRecord>> =
            std::collections::BTreeMap::new();
        for (recorded_at_ms, project, record) in entries {
            if !record.status.is_terminal() {
                continue;
            }
            let day = day_key_from_epoch_ms(*recorded_at_ms);
            by_day
                .entry(day.clone())
                .or_default()
                .push(UsageStatRecord {
                    day,
                    recorded_at_ms: *recorded_at_ms,
                    project: project.to_string(),
                    record: record.clone(),
                });
        }
        for (day, records) in by_day {
            let path = self.day_file(&day);
            let mut day_file = read_day_file(&path);
            for entry in records {
                upsert_record(&mut day_file, entry);
            }
            persist_day_file(&path, &day_file)?;
        }
        Ok(())
    }

    /// Every record across the report window, oldest day first. Corrupt day
    /// files are skipped with a warning.
    pub fn all_records(&self) -> Vec<UsageStatRecord> {
        let mut days = self.list_days();
        // Newest first from `list_days`; the report wants oldest first.
        days.reverse();
        // Bound the read window to the newest `REPORT_DAY_WINDOW` days so a
        // multi-year store cannot make a `/usage` query unbounded.
        if days.len() > REPORT_DAY_WINDOW {
            let start = days.len() - REPORT_DAY_WINDOW;
            days.drain(..start);
        }
        let mut out = Vec::new();
        for day in days {
            let path = self.day_file(&day);
            out.extend(read_day_file(&path).records);
        }
        out
    }

    /// The aggregated report over the whole window.
    pub fn report(&self, event_cap: usize) -> UsageStatsReport {
        aggregate_usage_records(&self.all_records(), event_cap)
    }

    /// Day keys present on disk, newest first. Files that do not match the
    /// `YYYY-MM-DD.json` naming are ignored.
    fn list_days(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(self.daily_dir()) else {
            return Vec::new();
        };
        let mut days: Vec<String> = entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().is_some_and(|e| e == "json"))
            .filter_map(|entry| {
                entry
                    .path()
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .map(|stem| stem.to_string())
            })
            .filter(|stem| {
                stem.len() == 10
                    && stem.as_bytes()[4] == b'-'
                    && stem.as_bytes()[7] == b'-'
                    && stem.bytes().all(|b| b.is_ascii_digit() || b == b'-')
            })
            .collect();
        days.sort();
        days.reverse();
        days
    }
}

/// Load a day file. Missing file → empty; corrupt file → empty (with a
/// warning) — usage telemetry must never crash the app.
fn read_day_file(path: &Path) -> DayFile {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return DayFile::default(),
    };
    let mut content = String::new();
    if file.read_to_string(&mut content).is_err() {
        return DayFile::default();
    }
    match serde_json::from_str(&content) {
        Ok(parsed) => parsed,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                %error,
                "skipping unreadable usage-stats day file"
            );
            DayFile::default()
        }
    }
}

/// Write a day file under its companion lock. Blocking lock acquisition:
/// contention is one other process flushing usage at the same moment, so
/// waiting beats losing the append.
fn persist_day_file(path: &Path, day_file: &DayFile) -> Result<(), String> {
    let _lock = FileLock::acquire(path).map_err(|e| e.to_string())?;
    atomic_write_json(path, &day_file)
}

/// Insert-or-upgrade one record. Same-key replay is idempotent; a reported
/// replay upgrades an estimated row (an estimate can never downgrade a
/// reported one — mirroring `TokenSourceLedger::settle_request`).
///
/// The record list stays sorted by `key` so `find` can binary-search: the
/// day file grows with the day's request count and the previous linear scan
/// per insert made each settle cost `O(n²)` over a heavy day.
fn upsert_record(day_file: &mut DayFile, entry: UsageStatRecord) {
    match day_file
        .records
        .binary_search_by(|existing| existing.record.key.cmp(&entry.record.key))
    {
        Ok(index) => {
            let existing = &mut day_file.records[index];
            let upgrade = existing.record.source != RequestUsageSource::Reported
                && entry.record.source == RequestUsageSource::Reported;
            if upgrade {
                *existing = entry;
            }
        }
        Err(index) => day_file.records.insert(index, entry),
    }
}

/// Convenience: the day bucket key for a wall-clock instant, exposed for
/// callers that pre-group records.
pub fn day_key(epoch_ms: u64) -> String {
    day_key_from_epoch_ms(epoch_ms)
}

/// Whether two records describe the same attempt (exposed for tests and
/// future importers).
pub fn same_attempt(a: &RequestUsageKey, b: &RequestUsageKey) -> bool {
    a == b
}

/// [`muta_contracts::UsageStatSink`] adapter over the store, safe to share
/// as an `Arc` into a [`muta_contracts::TokenSourceLedger`].
///
/// Writes happen synchronously on the settling thread. A terminal settle is
/// already an off-hot-path event (a provider response just completed), the
/// day file is small, and the write is atomic — so the simplicity of a
/// synchronous mirror beats a buffered channel that could lose the last
/// records on crash. Errors are logged and swallowed: usage telemetry must
/// never break request accounting.
impl muta_contracts::UsageStatSink for UsageStatsStore {
    fn record_usage(&self, recorded_at_ms: u64, project: &str, record: &RequestUsageRecord) {
        if let Err(error) = self.record(recorded_at_ms, project, record) {
            tracing::warn!(
                %error,
                session = %record.key.session_id,
                "could not persist usage-stat record"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muta_contracts::{RequestUsageKey, RequestUsageStatus};

    fn sample_record(session: &str, attempt: u32, total: i64) -> RequestUsageRecord {
        RequestUsageRecord {
            key: RequestUsageKey {
                session_id: session.to_string(),
                actor_id: "master".to_string(),
                round: 1,
                turn: 1,
                attempt,
            },
            provider: "openai".to_string(),
            model: "gpt-5".to_string(),
            status: RequestUsageStatus::Completed,
            source: RequestUsageSource::Reported,
            prompt_tokens: total - 50,
            completion_tokens: 50,
            total_tokens: total,
            ..Default::default()
        }
    }

    fn temp_root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "muta-usage-stats-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).expect("create temp root");
        dir
    }

    #[test]
    fn record_round_trips_through_disk() {
        let store = UsageStatsStore::with_root(temp_root());
        let record = sample_record("s1", 1, 1_000);
        store
            .record(1_700_000_000_000, "bucket-a", &record)
            .expect("record");
        let reloaded =
            UsageStatsStore::with_root(store.root.clone().unwrap().parent().unwrap().to_path_buf());
        let records = reloaded.all_records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].record.total_tokens, 1_000);
        assert_eq!(records[0].project, "bucket-a");
        assert_eq!(records[0].record.model, "gpt-5");
    }

    #[test]
    fn replay_is_idempotent_but_reported_upgrades_estimate() {
        let store = UsageStatsStore::with_root(temp_root());
        let mut estimated = sample_record("s1", 1, 900);
        estimated.source = RequestUsageSource::Estimated;
        estimated.total_tokens = 900;
        estimated.prompt_tokens = 850;
        store.record(1_700_000_000_000, "p", &estimated).unwrap();

        // Replay the same key → no duplicate.
        store.record(1_700_000_000_500, "p", &estimated).unwrap();
        assert_eq!(store.all_records().len(), 1);

        // A reported replay for the same key upgrades the estimate in place.
        let reported = sample_record("s1", 1, 1_200);
        store.record(1_700_000_001_000, "p", &reported).unwrap();
        let records = store.all_records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].record.source, RequestUsageSource::Reported);
        assert_eq!(records[0].record.total_tokens, 1_200);

        // A weaker replay after a reported record changes nothing.
        store.record(1_700_000_002_000, "p", &estimated).unwrap();
        assert_eq!(store.all_records()[0].record.total_tokens, 1_200);
    }

    #[test]
    fn in_flight_records_are_not_persisted() {
        let store = UsageStatsStore::with_root(temp_root());
        let mut in_flight = sample_record("s1", 1, 500);
        in_flight.status = RequestUsageStatus::InFlight;
        store.record(1_700_000_000_000, "p", &in_flight).unwrap();
        assert!(store.all_records().is_empty());
    }

    #[test]
    fn days_partition_and_aggregate() {
        let store = UsageStatsStore::with_root(temp_root());
        // Two different local days: 2023-11-14 22:13:20Z and (likely) the
        // next local day is far away, so pick two instants 48h apart.
        let t0 = 1_700_000_000_000u64;
        let t1 = t0 + 48 * 3600 * 1_000;
        store
            .record(t0, "p", &sample_record("s1", 1, 1_000))
            .unwrap();
        store
            .record(t1, "p", &sample_record("s1", 1, 2_000))
            .unwrap();
        let report = store.report(10);
        assert_eq!(report.days.len(), 2);
        assert_eq!(report.grand_total.total_tokens, 3_000);
        assert_ne!(report.days[0].day, report.days[1].day);
    }

    #[test]
    fn record_batch_groups_by_day() {
        let store = UsageStatsStore::with_root(temp_root());
        let t0 = 1_700_000_000_000u64;
        let t1 = t0 + 48 * 3600 * 1_000;
        let a = sample_record("s1", 1, 100);
        let b = sample_record("s1", 2, 200);
        let c = sample_record("s2", 1, 300);
        store
            .record_batch(&[(t0, "p", a), (t0, "p", b), (t1, "q", c)])
            .unwrap();
        let report = store.report(10);
        assert_eq!(report.days.len(), 2);
        assert_eq!(report.grand_total.requests, 3);
    }

    #[test]
    fn corrupt_day_file_is_skipped() {
        let root = temp_root();
        let store = UsageStatsStore::with_root(root.clone());
        store
            .record(1_700_000_000_000, "p", &sample_record("s1", 1, 10))
            .unwrap();
        // Corrupt one day file.
        let days = store.list_days();
        let day = days[0].clone();
        let path = store.day_file(&day);
        std::fs::write(&path, b"{ not json").unwrap();
        let fresh = UsageStatsStore::with_root(root);
        assert!(fresh.all_records().is_empty());
    }

    #[test]
    fn non_day_files_are_ignored() {
        let root = temp_root();
        let store = UsageStatsStore::with_root(root.clone());
        let daily = store.root().join("daily");
        std::fs::create_dir_all(&daily).unwrap();
        std::fs::write(daily.join("readme.txt"), b"ignore me").unwrap();
        std::fs::write(daily.join("junk.json"), b"{}").unwrap();
        assert!(store.list_days().is_empty());
        assert!(store.all_records().is_empty());
    }

    /// End-to-end: a `TokenSourceLedger` with this store installed as its
    /// `UsageStatSink` mirrors terminal settles into the day files, and the
    /// aggregate matches what the ledger itself would report — the same
    /// wiring the daemon bootstrap performs.
    #[test]
    fn ledger_sink_end_to_end_persists_and_aggregates() {
        use muta_contracts::TokenUsage;
        use std::sync::Arc;

        let store = Arc::new(UsageStatsStore::with_root(temp_root()));
        let ledger = muta_contracts::TokenSourceLedger::new();
        muta_contracts::TokenSourceLedger::install_usage_sink(
            &ledger,
            store.clone() as Arc<dyn muta_contracts::UsageStatSink>,
        );
        ledger.set_usage_project("proj-bucket");

        // One completed reported attempt.
        let first =
            ledger.begin_request_for_actor("s1", "master", "anthropic", "claude", 1, 1, 1_000);
        ledger.settle_request(
            &first,
            RequestUsageStatus::Completed,
            Some(TokenUsage {
                prompt_tokens: 1_200,
                completion_tokens: 300,
                total_tokens: 1_500,
                cache_creation_input_tokens: 200,
                cache_read_input_tokens: 500,
            }),
            0,
            4_000,
        );
        // One failed attempt (still consumes a request slot upstream).
        let retry =
            ledger.begin_request_for_actor("s1", "master", "anthropic", "claude", 1, 2, 900);
        ledger.settle_request(&retry, RequestUsageStatus::Failed, None, 20, 0);

        let report = store.report(10);
        assert_eq!(report.grand_total.requests, 2);
        assert_eq!(report.grand_total.completed, 1);
        assert_eq!(report.grand_total.total_tokens, 1_500);
        assert_eq!(report.grand_total.estimated_tokens, 920);
        assert_eq!(report.models.len(), 1);
        assert_eq!(report.models[0].provider, "anthropic");
        // Both records landed in today's day file under the stamped project.
        let records = store.all_records();
        assert!(records.iter().all(|r| r.project == "proj-bucket"));
        assert_eq!(records.len(), 2);
        // The store survives a fresh instance reading the same root (i.e.
        // session cleanup / restart cannot remove it). `root()` already
        // carries the `usage` segment, so re-wrap from its parent.
        let reread = UsageStatsStore::with_root(
            store
                .root()
                .parent()
                .expect("root has a parent")
                .to_path_buf(),
        );
        assert_eq!(reread.report(10).grand_total.requests, 2);
    }

    /// `upsert_record` keeps the day file sorted by key (the invariant the
    /// binary search in `upsert_record` relies on), stays idempotent on
    /// replays, and upgrades estimated rows to reported ones without ever
    /// letting a reported row regress.
    #[test]
    fn upsert_keeps_records_sorted_idempotent_and_upgrade_only() {
        fn entry(session: &str, attempt: u32, source: RequestUsageSource) -> UsageStatRecord {
            let mut record = sample_record(session, attempt, 100);
            record.source = source;
            UsageStatRecord {
                day: "2026-08-20".to_string(),
                recorded_at_ms: 1,
                project: "p".to_string(),
                record,
            }
        }

        let mut day = DayFile::default();
        // Insert out of key order: "s2" before "s1".
        upsert_record(&mut day, entry("s2", 1, RequestUsageSource::Reported));
        upsert_record(&mut day, entry("s1", 2, RequestUsageSource::Estimated));
        upsert_record(&mut day, entry("s1", 1, RequestUsageSource::Estimated));

        let keys: Vec<String> = day
            .records
            .iter()
            .map(|r| format!("{}#{}", r.record.key.session_id, r.record.key.attempt))
            .collect();
        assert_eq!(
            keys,
            vec!["s1#1", "s1#2", "s2#1"],
            "records stay key-sorted"
        );

        // Same-key replay is a no-op (idempotent, no duplicate row).
        let before = day.records.len();
        upsert_record(&mut day, entry("s1", 1, RequestUsageSource::Estimated));
        assert_eq!(day.records.len(), before);

        // An estimated row upgrades to reported…
        upsert_record(&mut day, entry("s1", 1, RequestUsageSource::Reported));
        assert_eq!(
            day.records[0].record.source,
            RequestUsageSource::Reported,
            "estimated upgrades to reported"
        );
        // …and a reported row never regresses to estimated.
        upsert_record(&mut day, entry("s1", 1, RequestUsageSource::Estimated));
        assert_eq!(
            day.records[0].record.source,
            RequestUsageSource::Reported,
            "reported must not downgrade"
        );
    }
}
