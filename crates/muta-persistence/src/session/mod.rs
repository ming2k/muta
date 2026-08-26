//! Event-sourced session persistence (ADR-0017 / ADR-0022).
//!
//! Each session is an append-only JSONL event log (`sessions/<id>.jsonl`)
//! plus a JSON snapshot cache (`sessions/<id>.json`) with a CRC32C checksum
//! and a `schema_version` for lazy on-load migration; the log wins on
//! conflict. Large payloads are offloaded to the content-addressed
//! [`crate::blobs::BlobStore`]. Sessions are bucketed per project under
//! `projects/<sha256(cwd)[..16]>/sessions/`. [`SessionStore`] is the facade
//! for load/save/resume/fork and for committing model-context projections
//! (pruning and compaction)
//! checkpoints; it also drives the one-shot legacy layout migrations.

use crate::blobs::BlobStore;
use crate::events::{EventLog, SessionEvent};
use crate::fsutil;
use crate::paths;
use muta_contracts::{
    InjectionKind, InjectionOrigin, Message, Provider, Role, SessionDetail, estimate_tokens,
};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

/// C2 (ADR-0022): added `title` and `title_manual`. C4 (ADR-0034): added `Message::origin` (`Option<InjectionOrigin>`)
/// for structured injection provenance. Both are structural no-ops for
/// legacy snapshots, which load with the new fields at their `#[serde(default)]`
/// values (`None` / `false`). C6 (per-session provider/model): added
/// `provider_selection`. A session that has run `/models` pins its own
/// provider + model here so the live selection does not leak into the global
/// `config.toml` or affect other concurrent sessions.
/// C11 added `round_interrupts` (durable round-interrupt records): a
/// structural no-op — legacy snapshots load with an empty list.
/// C12 added `tree` (native incremental DAG session tree).
const CURRENT_SCHEMA_VERSION: u32 = 12;

/// A session-scoped provider + model pin (C6). When present it overrides the
/// global `config.default_provider` / `config.default_model` for this session
/// only, so one session switching `/models` does not change what any other
/// session — or the next fresh session — sees. `None` means "follow the global
/// default"; the session still tracks the provider selection it was started
/// with until the user switches.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderSelection {
    pub provider: String,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextProjectionKind {
    Prune,
    Compact,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextProjectionCheckpoint {
    pub operation: ContextProjectionKind,
    pub archived_messages: usize,
    pub active_messages: usize,
    /// Token size of the active model window **sampled immediately before
    /// the projection was applied**. A point-in-time sample, not a live
    /// value: the window keeps growing after this checkpoint.
    pub window_tokens_before: usize,
    /// Token size of the active model window immediately **after** the
    /// projection. Same point-in-time caveat; the difference to
    /// [`Self::window_tokens_before`] is what the projection reclaimed.
    pub window_tokens_after: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
struct SessionData {
    id: String,
    parent_id: Option<String>,
    /// How this session came to exist relative to its lineage: a root trunk,
    /// an explicit `/fork` branch, or a `/btw` aside forked off the trunk.
    /// `#[serde(default)]` so legacy snapshots (which predate lineage
    /// tracking) load as `Trunk` — a parent_id-bearing legacy file degrades
    /// to `Fork` in the summary layer, preserving whatever lineage the old
    /// data carried.
    #[serde(default)]
    fork_kind: muta_contracts::SessionForkKind,
    created_at: u64,
    updated_at: u64,
    model_window: Vec<Message>,
    archived_transcript: Vec<Message>,
    /// Stats of the most recent model-context projection (prune or compaction).
    last_projection: Option<ContextProjectionCheckpoint>,
    /// Working directory this session belongs to. Phase 2 (project isolation)
    /// uses this to route archived sessions to the right per-project bucket
    /// during the one-shot legacy migration. Legacy snapshots missing the
    /// field default to the current cwd.
    project_root: PathBuf,
    /// Unified task list, mirrored from `Agent::todos`. The single source of
    /// truth for "what is left to do." An empty list means
    /// no active task list. `#[serde(default)]` so legacy snapshots load as
    /// an empty list with no migration.
    #[serde(default)]
    todos: muta_contracts::TodoList,
    /// Session-scoped scheduled-prompt list (`/schedule`, formerly `/repeat`).
    /// Each entry is either a recurring cron job or a one-shot (countdown /
    /// absolute-time) job. The session that created a job owns it; the
    /// background scheduler polls the live session and dispatches each due job
    /// as a chat round. `#[serde(default)]` so
    /// snapshots load with whatever they had and no migration is required for
    /// the field rename (only the schema bump records the change).
    #[serde(default)]
    scheduled_jobs: Vec<muta_contracts::ScheduledJob>,
    /// Schema version of this session file. Migrations increment this and are
    /// applied lazily on load.
    schema_version: u32,
    /// CRC32C checksum of the canonical JSON payload (excluding this field).
    /// `None` for legacy files written before C10; new writes always populate
    /// it so `muta doctor` and future loaders can detect corruption.
    checksum: Option<u32>,
    /// AI-generated session title (ADR-0022). Displayed in the session picker
    /// in preference to the first-user-message fallback. `None` for legacy
    /// snapshots and for sessions that have not yet generated a title.
    #[serde(default)]
    title: Option<String>,
    /// Whether `title` was set manually via `/title <text>` and must not be
    /// overwritten by automatic or on-demand AI generation (ADR-0022).
    /// `false` for legacy snapshots and AI-generated titles.
    #[serde(default)]
    title_manual: bool,
    /// High-water mark: the `seq` of the last event already folded into this
    /// snapshot. On load, the snapshot is read as a fast path and only log
    /// events with `seq > applied_seq` are replayed (the tail), so resuming a
    /// long session costs O(tail) instead of O(whole-history). `None` on
    /// legacy snapshots and on the very first persist of a session (before any
    /// event has been folded); the load path falls back to a full replay.
    /// Covered by the checksum like every other field, so a tampered watermark
    /// is rejected as corruption rather than silently skipping events.
    #[serde(default)]
    applied_seq: Option<u64>,
    /// Session-scoped provider + model pin (C6). `None` for a session that has
    /// never run `/models`; the harness then seeds it from the global default
    /// on first switch. Persisted so resume restores the session's own provider
    /// instead of whatever global default is current at reopen time.
    #[serde(default)]
    provider_selection: Option<ProviderSelection>,
    /// Session-level disabled-tool mask (ADR-0048 Phase 2). Names here are
    /// hidden from the model and rejected at dispatch. Mirrored from
    /// `Agent::disabled_tools` so a user toggle survives restart instead of
    /// silently resetting. `#[serde(default)]` so legacy snapshots load with
    /// an empty set (all tools enabled) and no migration.
    #[serde(default)]
    disabled_tools: std::collections::HashSet<String>,
    /// Harness round counter, the session-scoped monotonic watermark (ADR-0048
    /// Phase 2). Bumped at the start of every round; read by the todo
    /// stale-detector via `updated_at_round`. Persisted so a resumed session's
    /// staleness comparisons stay valid instead of the counter resetting to 0.
    #[serde(default)]
    round_counter: u64,
    /// Per-request token accounting for this session. Unlike the historical
    /// process-global ledger, these records survive resume and cannot leak
    /// across `/session open` boundaries.
    #[serde(default)]
    request_usage_records: Vec<muta_contracts::RequestUsageRecord>,
    /// Durable command ledger (ADR-0091): every slash command (and `!cmd`
    /// passthrough) invocation with its typed result. Commands are operations
    /// on the session, not conversation turns, so they live here instead of in
    /// `model_window` / `archived_transcript` — the message stream is pure
    /// dialogue. Legacy `CommandEcho` messages fold into this list at schema
    /// migration time (v10). `#[serde(default, skip_serializing_if =
    /// "Vec::is_empty")]` keeps legacy canonical JSON byte-identical so
    /// existing stored checksums stay valid.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    commands: Vec<muta_contracts::CommandRecord>,
    /// Durable round-interrupt records (C11): one entry per round stopped
    /// before its natural terminal path — user interrupt (Esc Esc),
    /// superseded by newer input, or killed with the process. Pure
    /// projection state like the command ledger: never enters the model
    /// window, never reaches the model. Re-projected into the transcript on
    /// resume by timestamp seam so the user can decide whether to continue.
    /// `#[serde(default, skip_serializing_if = "Vec::is_empty")]` keeps
    /// legacy canonical JSON byte-identical so existing checksums stay
    /// valid.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    round_interrupts: Vec<muta_contracts::RoundInterrupt>,
    /// The durable `/retry` resume point (C12): the stopped round's
    /// history watermark, committed-turn count, and paused accumulator.
    /// `None` on a fresh session and after the parked round completes.
    /// `#[serde(default, skip_serializing_if = "Option::is_none")]` keeps
    /// legacy canonical JSON byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    retry_pending: Option<muta_contracts::RetryPoint>,
    /// Session-scoped YOLO posture: `true` means the agent
    /// runs in full auto-approve mode (bypasses tool permission prompts).
    #[serde(default, alias = "autopilot", skip_serializing_if = "std::ops::Not::not")]
    yolo: bool,
    /// Native DAG session tree (Schema v12).
    #[serde(default)]
    tree: muta_contracts::SessionTree,
}

impl Default for SessionData {
    fn default() -> Self {
        let now = unix_timestamp();
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            parent_id: None,
            fork_kind: muta_contracts::SessionForkKind::Trunk,
            created_at: now,
            updated_at: now,
            model_window: Vec::new(),
            archived_transcript: Vec::new(),
            last_projection: None,
            project_root: default_project_root(),
            todos: muta_contracts::TodoList::default(),
            scheduled_jobs: Vec::new(),
            schema_version: CURRENT_SCHEMA_VERSION,
            checksum: None,
            title: None,
            title_manual: false,
            applied_seq: None,
            provider_selection: None,
            disabled_tools: std::collections::HashSet::new(),
            round_counter: 0,
            request_usage_records: Vec::new(),
            commands: Vec::new(),
            round_interrupts: Vec::new(),
            retry_pending: None,
            yolo: false,
            tree: muta_contracts::SessionTree::default(),
        }
    }
}

impl SessionData {
    /// The single authority for "this session has no substantive content yet"
    /// (ADR-0018). A session is empty while it carries neither dialogue
    /// (active `model_window` or `archived_transcript`), nor any *substantive*
    /// piece of session state — a non-empty todo list, at least one scheduled job,
    /// a non-empty disabled-tool mask, or a started round counter. Any one of
    /// those is a real user action worth durably recording, so it materialises
    /// the session.
    ///
    /// Auxiliary state deliberately does **not** count on their own, matching
    /// the lazy contract: the **title** (a title on an otherwise-empty session
    /// is still an empty record in the picker), the **provider selection**
    /// (pinning `/models` must not surface a never-used session), and the
    /// **commands ledger** (navigational / informational slash commands like
    /// `/sessions`, `/models`, `/dashboard`, `/help` executed before any dialogue
    /// must not materialize an empty session). All of these ride along once
    /// substantive dialogue or state makes the session real.
    ///
    /// Every guarded write path consults this (via
    /// [`SessionStore::should_skip_persist`]) instead of re-deriving the
    /// condition inline, so the "what makes a session real" rule lives in
    /// exactly one place and cannot drift between setters.
    /// The user-facing-emptiness rule deliberately excludes `autopilot`:
    /// toggling autopilot on an otherwise-fresh session is a posture change
    /// on a session that has nothing to resume yet, not substantive work —
    /// it must not materialize an empty session file. Once the session gains
    /// dialogue or other substantive state, the flag rides along like every
    /// other session-scoped field.
    fn is_user_facing_empty(&self) -> bool {
        self.model_window.is_empty()
            && self.archived_transcript.is_empty()
            && self.todos.is_empty()
            && self.scheduled_jobs.is_empty()
            && self.disabled_tools.is_empty()
            && self.round_counter == 0
    }
}

/// Serde default for [`SessionData::project_root`]. Resolves to the current
/// process cwd so legacy snapshots (which predate the field) load with the
/// closest-to-correct project binding on first deserialisation.
fn default_project_root() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Apply one-shot schema migrations to a [`SessionData`] loaded from disk.
/// Each migration is guarded by the incoming `schema_version` so repeated
/// calls are idempotent. The returned value always has
/// `schema_version == CURRENT_SCHEMA_VERSION`.
fn migrate_session_data(mut data: SessionData) -> SessionData {
    // C8: initial schema-version field. No structural migration required yet;
    // future changes add guarded blocks here.
    // C2 (ADR-0022): title fields were added with `#[serde(default)]`, so a
    // legacy snapshot already loads with `title = None` / `title_manual =
    // false`; no payload transformation is needed, only the version bump.
    // C4 (ADR-0034): `Message::origin` (`Option<InjectionOrigin>`) was added
    // with `#[serde(default, skip_serializing_if = "Option::is_none")]`, so a
    // legacy snapshot and event-log lines already load with `origin = None`
    // for every message; no payload transformation is needed, only the version
    // bump. Provenance is henceforth stamped at each injection site going
    // forward — pre-C4 messages are simply unattributed.
    // C5 (snapshot fast-path): `applied_seq` was added with
    // `#[serde(default)]`, so a legacy snapshot loads with
    // `applied_seq = None` and the load path falls back to a full replay;
    // no payload transformation is needed, only the version bump. The first
    // persist after this upgrade folds the full state and records the
    // watermark, so subsequent loads take the fast path.
    // C6 (per-session provider/model): `provider_selection` was added with
    // `#[serde(default)]`, so a legacy snapshot loads with
    // `provider_selection = None` (follow the global default); no payload
    // transformation is needed, only the version bump.
    // schema v8 (repeat-as-session-state): `repeat_jobs` was added with
    // `#[serde(default)]`, so a legacy snapshot loads with an empty schedule;
    // no payload transformation is needed, only the version bump. `/repeat`
    // jobs that previously lived in a separate store are not migrated — they
    // are rebuildable scheduler state and the new semantics bind a job to
    // the session that created it.
    // schema v9 (scheduled-prompt unification): the flat `repeat_jobs: Vec<RepeatJob>`
    // field was renamed to `scheduled_jobs: Vec<ScheduledJob>` and the event
    // tag `repeat_jobs_set` renamed to `scheduled_jobs_set`. Both carry serde
    // aliases for the old names, and `ScheduledJob` deserialises the legacy
    // flat `cron` shape, so no payload transformation is needed — only the
    // version bump records the change. The new model also adds one-shot
    // (countdown / absolute-time) jobs alongside the existing cron jobs.
    if data.schema_version < 9 {
        data.schema_version = 9;
    }
    // schema v10 (ADR-0091): command records moved out of the message stream
    // into a dedicated ledger. Legacy sessions may still carry `CommandEcho`
    // messages (ADR-0050) in `model_window` / `archived_transcript`; fold each
    // into the ledger as a `CommandRecord` with `result: None` (invocation
    // recorded, reply never persisted) and drop it from the message vectors so
    // the stream is pure dialogue again. Guarded by the v10 bump, so repeated
    // loads are idempotent.
    if data.schema_version < 10 {
        let mut records = Vec::new();
        // Full-transcript order: archived first, then the live window.
        for message in data
            .archived_transcript
            .iter()
            .chain(data.model_window.iter())
        {
            if message.is_command_echo() {
                records.push(command_record_from_echo(message));
            }
        }
        data.archived_transcript.retain(|m| !m.is_command_echo());
        data.model_window.retain(|m| !m.is_command_echo());
        data.commands = records;
        data.schema_version = 10;
    }
    data.schema_version = CURRENT_SCHEMA_VERSION;
    data
}

/// Convert a legacy `CommandEcho` message (ADR-0050) into a ledger
/// [`CommandRecord`](muta_contracts::CommandRecord) with `result: None`. The echo
/// text is the literal `/cmd args` or `!cmd args` the user typed; `!`-prefixed
/// invocations fold under the `"shell"` name, everything else under its
/// command word.
fn command_record_from_echo(message: &Message) -> muta_contracts::CommandRecord {
    let text = message.content.trim();
    let (name, args) = if let Some(rest) = text.strip_prefix('!') {
        ("shell", rest.trim().to_string())
    } else if let Some(rest) = text.strip_prefix('/') {
        match rest.split_once(char::is_whitespace) {
            Some((name, args)) => (name, args.trim().to_string()),
            None => (rest, String::new()),
        }
    } else {
        ("echo", text.to_string())
    };
    let mut record = muta_contracts::CommandRecord::new(name, args);
    record.timestamp = message
        .timestamp
        .map(|seconds| seconds.saturating_mul(1000))
        .unwrap_or_else(|| muta_contracts::todos::unix_now().saturating_mul(1000));
    record
}

/// Compute the CRC32C checksum that should be stored for `data`. The checksum
/// covers the canonical JSON representation of all fields except `checksum`,
/// which is set to `null` during computation so later verification can read
/// the stored value and compare against the same payload.
///
/// Returns `Err` on serialization failure rather than a sentinel `0`. The
/// previous code returned `0` on a serialization error, which would *appear*
/// to be a valid checksum and let `verify_checksum` accept corrupt data — a
/// fail-open failure mode for an integrity check.
fn compute_checksum(data: &SessionData) -> Result<u32, String> {
    let mut value = serde_json::to_value(data).map_err(|e| e.to_string())?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert("checksum".to_string(), serde_json::Value::Null);
    }
    let bytes = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
    Ok(crc32c::crc32c(&bytes))
}

/// Verify the stored checksum on `data`, if present. Returns `Ok(())` when the
/// checksum matches or when the file predates checksums. Returns an error
/// describing the mismatch otherwise.
fn verify_checksum(data: &SessionData) -> Result<(), String> {
    let Some(stored) = data.checksum else {
        return Ok(());
    };
    let expected = compute_checksum(data)?;
    if expected == stored {
        Ok(())
    } else {
        Err(format!(
            "checksum mismatch: stored {stored:#010x}, computed {expected:#010x}"
        ))
    }
}

/// Characters above which a message content is moved to the blob store.
const BLOB_OFFLOAD_THRESHOLD: usize = 4_096;

/// Write `data` to `path` with a freshly computed checksum, offloading large
/// inline content to the blob store before serialization. Stamps the
/// [`SessionData::applied_seq`] watermark to the sibling event log's high-water
/// `seq`, so a later load of this snapshot replays only the tail past it. The
/// log file is derived from `path` (`.json` → `.jsonl`); a missing or empty log
/// leaves an existing watermark untouched (a snapshot can be written with no
/// log yet during seeding, in which case the load path falls back to full
/// replay anyway).
fn write_session_file(
    path: &Path,
    data: &SessionData,
    blob_store: &BlobStore,
) -> Result<(), String> {
    let mut data = data.clone();
    offload_session_blobs(&mut data, blob_store)?;
    // The watermark reflects the events already folded into `data`. Each write
    // site appends its event(s) to the log *before* persisting, so the log's
    // high-water mark is exactly what this snapshot has absorbed.
    let log_path = path.with_extension("jsonl");
    let event_log = EventLog::new(log_path);
    if let Some(high) = event_log.high_seq() {
        data.applied_seq = Some(high);
    }
    data.checksum = Some(compute_checksum(&data)?);
    fsutil::atomic_write_json(path, &data)
}

/// Move large `Message.content` strings into the blob store and replace them
/// with a `content_blob` reference. Operates recursively on nested children.
fn offload_session_blobs(data: &mut SessionData, blob_store: &BlobStore) -> Result<(), String> {
    for message in data
        .model_window
        .iter_mut()
        .chain(data.archived_transcript.iter_mut())
    {
        offload_message_blobs(message, blob_store)?;
    }
    Ok(())
}

fn offload_message_blobs(message: &mut Message, blob_store: &BlobStore) -> Result<(), String> {
    if message.content.len() > BLOB_OFFLOAD_THRESHOLD && message.content_blob.is_none() {
        let hash = blob_store.put(message.content.as_bytes())?;
        message.content_blob = Some(hash);
        message.content.clear();
    }
    if let Some(children) = message.children.as_mut() {
        for child in children.iter_mut() {
            offload_message_blobs(child, blob_store)?;
        }
    }
    Ok(())
}

/// Rehydrate `content` from `content_blob` references after loading.
fn load_session_blobs(data: &mut SessionData, blob_store: &BlobStore) -> Result<(), String> {
    for message in data
        .model_window
        .iter_mut()
        .chain(data.archived_transcript.iter_mut())
    {
        load_message_blobs(message, blob_store)?;
    }
    Ok(())
}

fn load_message_blobs(message: &mut Message, blob_store: &BlobStore) -> Result<(), String> {
    if let Some(hash) = message.content_blob.take() {
        let bytes = blob_store
            .get(&hash)
            .ok_or_else(|| format!("missing content blob {hash}"))?;
        message.content = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    }
    if let Some(children) = message.children.as_mut() {
        for child in children.iter_mut() {
            load_message_blobs(child, blob_store)?;
        }
    }
    Ok(())
}

/// Emit a [`SessionEvent::Started`] event if the log is currently empty.
/// Every session must begin with this event so replay reconstructs the id,
/// parent link, and timestamps.
fn ensure_event_log_started(event_log: &EventLog, data: &SessionData) -> Result<(), String> {
    if event_log.is_empty() {
        event_log.append(SessionEvent::Started {
            id: data.id.clone(),
            parent_id: data.parent_id.clone(),
            created_at: data.created_at,
            project_root: data.project_root.clone(),
            schema_version: data.schema_version,
        })?;
    }
    Ok(())
}

/// Apply a sequence of events to a fresh or existing [`SessionData`].
fn apply_events(data: &mut SessionData, envelopes: &[crate::events::EventEnvelope]) {
    for envelope in envelopes {
        match &envelope.event {
            SessionEvent::Started {
                id,
                parent_id,
                created_at,
                project_root,
                schema_version,
            } => {
                data.id = id.clone();
                data.parent_id = parent_id.clone();
                data.created_at = *created_at;
                data.project_root = project_root.clone();
                data.schema_version = *schema_version;
            }
            SessionEvent::MessagesReplaced { messages } => data.model_window = messages.clone(),
            SessionEvent::MessagesAppended { messages } => {
                data.model_window.extend(messages.clone())
            }
            SessionEvent::CommandsReplaced { commands } => data.commands = commands.clone(),
            SessionEvent::ContextProjectionCommitted {
                archived_originals,
                model_window,
                checkpoint,
            } => {
                data.archived_transcript.extend(archived_originals.clone());
                data.model_window = model_window.clone();
                data.last_projection = Some(checkpoint.clone());
            }
            SessionEvent::Archived { messages } => {
                data.archived_transcript.extend(messages.clone())
            }
            SessionEvent::TodosSet { todos } => {
                data.todos = todos.clone();
            }
            SessionEvent::ScheduledJobsSet { jobs } => {
                data.scheduled_jobs = jobs.clone();
            }
            SessionEvent::TitleSet { title, manual } => {
                data.title = title.clone();
                data.title_manual = *manual;
            }
            SessionEvent::DisabledToolsSet { tools } => {
                data.disabled_tools = tools.clone();
            }
            SessionEvent::RoundCounterSet { counter } => {
                data.round_counter = *counter;
            }
            SessionEvent::RequestUsageUpsert { record } => {
                if let Some(existing) = data
                    .request_usage_records
                    .iter_mut()
                    .find(|existing| existing.key == record.key)
                {
                    *existing = record.clone();
                } else {
                    data.request_usage_records.push(record.clone());
                }
            }
            SessionEvent::ProviderSelectionSet { selection } => {
                data.provider_selection = selection.clone();
            }
            SessionEvent::RoundInterruptRecorded { record } => {
                data.round_interrupts.push(record.clone());
            }
            SessionEvent::RoundInterruptsCleared {} => {
                data.round_interrupts.clear();
            }
            SessionEvent::RetryPendingRecorded { point } => {
                data.retry_pending = Some(point.clone());
            }
            SessionEvent::RetryPendingCleared {} => {
                data.retry_pending = None;
            }
            SessionEvent::YoloSet { enabled } => {
                data.yolo = *enabled;
            }
            SessionEvent::Reset { id } => {
                let project_root = data.project_root.clone();
                let schema_version = data.schema_version;
                *data = SessionData::default();
                data.id = id.clone();
                data.project_root = project_root;
                data.schema_version = schema_version;
            }
            SessionEvent::Forked { id, parent_id } => {
                data.id = id.clone();
                data.parent_id = Some(parent_id.clone());
            }
        }
        data.updated_at = envelope.timestamp;
    }
}

/// Convert a snapshot into a seed event sequence so legacy files can be
/// imported into the event log without losing information.
fn snapshot_to_events(data: &SessionData) -> Vec<crate::events::EventEnvelope> {
    let mut events = vec![crate::events::EventEnvelope {
        seq: 0,
        timestamp: data.created_at,
        event: SessionEvent::Started {
            id: data.id.clone(),
            parent_id: data.parent_id.clone(),
            created_at: data.created_at,
            project_root: data.project_root.clone(),
            schema_version: data.schema_version,
        },
    }];
    if let Some(checkpoint) = &data.last_projection {
        events.push(crate::events::EventEnvelope {
            seq: events.len() as u64,
            timestamp: data.updated_at,
            event: SessionEvent::ContextProjectionCommitted {
                archived_originals: data.archived_transcript.clone(),
                model_window: data.model_window.clone(),
                checkpoint: checkpoint.clone(),
            },
        });
    } else {
        if !data.archived_transcript.is_empty() {
            events.push(crate::events::EventEnvelope {
                seq: events.len() as u64,
                timestamp: data.updated_at,
                event: SessionEvent::Archived {
                    messages: data.archived_transcript.clone(),
                },
            });
        }
        events.push(crate::events::EventEnvelope {
            seq: events.len() as u64,
            timestamp: data.updated_at,
            event: SessionEvent::MessagesReplaced {
                messages: data.model_window.clone(),
            },
        });
    }
    if !data.todos.is_empty() {
        events.push(crate::events::EventEnvelope {
            seq: events.len() as u64,
            timestamp: data.updated_at,
            event: SessionEvent::TodosSet {
                todos: data.todos.clone(),
            },
        });
    }
    if !data.scheduled_jobs.is_empty() {
        events.push(crate::events::EventEnvelope {
            seq: events.len() as u64,
            timestamp: data.updated_at,
            event: SessionEvent::ScheduledJobsSet {
                jobs: data.scheduled_jobs.clone(),
            },
        });
    }
    if !data.commands.is_empty() {
        events.push(crate::events::EventEnvelope {
            seq: events.len() as u64,
            timestamp: data.updated_at,
            event: SessionEvent::CommandsReplaced {
                commands: data.commands.clone(),
            },
        });
    }
    if data.title.is_some() {
        events.push(crate::events::EventEnvelope {
            seq: events.len() as u64,
            timestamp: data.updated_at,
            event: SessionEvent::TitleSet {
                title: data.title.clone(),
                manual: data.title_manual,
            },
        });
    }
    if let Some(selection) = &data.provider_selection {
        events.push(crate::events::EventEnvelope {
            seq: events.len() as u64,
            timestamp: data.updated_at,
            event: SessionEvent::ProviderSelectionSet {
                selection: Some(selection.clone()),
            },
        });
    }
    if !data.disabled_tools.is_empty() {
        events.push(crate::events::EventEnvelope {
            seq: events.len() as u64,
            timestamp: data.updated_at,
            event: SessionEvent::DisabledToolsSet {
                tools: data.disabled_tools.clone(),
            },
        });
    }
    if data.round_counter > 0 {
        events.push(crate::events::EventEnvelope {
            seq: events.len() as u64,
            timestamp: data.updated_at,
            event: SessionEvent::RoundCounterSet {
                counter: data.round_counter,
            },
        });
    }
    if data.yolo {
        events.push(crate::events::EventEnvelope {
            seq: events.len() as u64,
            timestamp: data.updated_at,
            event: SessionEvent::YoloSet { enabled: true },
        });
    }
    for record in &data.request_usage_records {
        events.push(crate::events::EventEnvelope {
            seq: events.len() as u64,
            timestamp: data.updated_at,
            event: SessionEvent::RequestUsageUpsert {
                record: record.clone(),
            },
        });
    }
    for record in &data.round_interrupts {
        events.push(crate::events::EventEnvelope {
            seq: events.len() as u64,
            timestamp: data.updated_at,
            event: SessionEvent::RoundInterruptRecorded {
                record: record.clone(),
            },
        });
    }
    if let Some(point) = &data.retry_pending {
        events.push(crate::events::EventEnvelope {
            seq: events.len() as u64,
            timestamp: data.updated_at,
            event: SessionEvent::RetryPendingRecorded {
                point: point.clone(),
            },
        });
    }
    events
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: String,
    pub parent_id: Option<String>,
    /// How this session came to exist: trunk root, explicit `/fork`
    /// branch, or `/btw` aside. Drives the dashboard's lineage grouping.
    pub fork_kind: muta_contracts::SessionForkKind,
    pub message_count: usize,
    pub updated_at: u64,
    pub created_at: u64,
    /// Short description of what the session is about (first user message),
    /// already truncated for display.
    pub overview: String,
    pub active: bool,
}

/// The mutable bits a [`SessionStore`] pins to one session file: the snapshot
/// path, its event log, and the in-memory session data. Grouped under a single
/// [`tokio::sync::Mutex`] so repointing the store (reset / fork / open) — which
/// swaps both the path and the event log — is atomic with respect to every
/// reader and writer. There is no second lock to deadlock against.
struct SessionState {
    /// Absolute path of this session's snapshot: `<sessions_dir>/<id>.json`.
    path: PathBuf,
    /// This session's append-only event log at `<sessions_dir>/<id>.jsonl`.
    event_log: EventLog,
    /// In-memory session, authoritative between writes; the event log is the
    /// durable authority across restarts.
    data: SessionData,
    /// `true` only for a **fresh** primary session (`pin_fresh`): defer the
    /// first durable write until the session gains user-facing content, so
    /// starting and exiting without a round leaves no empty-file litter
    /// (ADR-0018). `false` for an explicitly pinned path (`for_path`, and any
    /// store loaded from an existing snapshot): there the caller has already
    /// materialised the session, so every write persists eagerly.
    defer_persist: bool,
}

pub struct SessionStore {
    project_root: PathBuf,
    /// Directory holding every session file for this project (or, for
    /// [`SessionStore::for_path`], the parent of the pinned snapshot). All
    /// `reset` / `fork` / `open` targets live here, so the store never writes
    /// outside it.
    sessions_dir: PathBuf,
    blob_store: BlobStore,
    state: Mutex<SessionState>,
    /// FIFO commit gate for snapshot writes.
    ///
    /// State mutations are ordered by `state`, but snapshot I/O runs on the
    /// blocking pool after that guard is released. Without a second ordered
    /// gate, two callers could write the same snapshot concurrently and an
    /// older clone could replace a newer one. Tokio's mutex is FIFO, so callers
    /// reserve persistence in the same order they leave their state mutation.
    persist_gate: Mutex<()>,
}

impl SessionStore {
    /// Open a per-project store pinned to a **fresh** session file.
    ///
    /// As of ADR-0018 the project bucket no longer keeps a single shared
    /// `session.json` "active pointer": every running `muta` instance mints
    /// its own `sessions/<id>.json` + `sessions/<id>.jsonl`, so two instances
    /// in the same project never share a mutable file. To continue a previous
    /// session the caller picks one via the `/sessions` picker or
    /// [`Self::open`] / [`Self::resume`].
    pub fn load_for_project(project_root: PathBuf) -> Self {
        // Establish one physical identity at the persistence boundary. This
        // prevents aliases such as macOS `/var` -> `/private/var` (and normal
        // symlinks elsewhere) from splitting one project across durable
        // session identities. Non-existent roots retain their caller-supplied
        // spelling so tests and future create-on-demand flows remain valid.
        let project_root = project_root.canonicalize().unwrap_or(project_root);
        let dirs = paths::get();
        let sessions_dir = dirs.project_sessions_dir(&project_root);
        if let Err(e) = std::fs::create_dir_all(&sessions_dir) {
            tracing::warn!(error = %e, "could not create project sessions dir");
        }
        let blob_store = BlobStore::new(dirs.blobs_dir());

        Self::pin_fresh(project_root, sessions_dir, blob_store)
    }

    /// Backwards-compatible alias for [`Self::load_for_project`] using the
    /// current process cwd.
    #[allow(dead_code)]
    pub fn load() -> Self {
        let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::load_for_project(project_root)
    }

    /// Open a `SessionStore` pinned to an explicit snapshot `path`. The
    /// session's event log lives at `path.with_extension("jsonl")`, and its
    /// sibling session files (forks, archives) live in `path.parent()` — i.e.
    /// the parent directory plays the role of the project's `sessions/` dir.
    ///
    /// This is the low-level constructor used by runners / side
    /// conversations (ADR-0017) and by tests that want a throwaway file
    /// without wiring up the global paths table. Production startup uses
    /// [`Self::load_for_project`].
    pub fn for_path(path: PathBuf) -> Self {
        let sessions_dir = path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let event_log_path = path.with_extension("jsonl");
        let project_root = sessions_dir.clone();
        let blob_store = BlobStore::new(sessions_dir.join("blobs"));
        let data = load_or_seed(&path, &event_log_path, &blob_store, &project_root);
        let event_log = EventLog::new(event_log_path);
        // Defer only while the pinned snapshot does not exist yet: a `for_path`
        // into a never-written file is a fresh, content-less session (lazy,
        // ADR-0018); opening an existing snapshot is already materialised.
        let defer_persist = !path.exists();
        Self {
            project_root,
            sessions_dir,
            blob_store,
            state: Mutex::new(SessionState {
                path,
                event_log,
                data,
                defer_persist,
            }),
            persist_gate: Mutex::new(()),
        }
    }

    /// Construct a store pinned to a brand-new, empty session file in
    /// `sessions_dir`. The file is **not** written until the session gains
    /// real content, so a `muta` that starts and exits without a round
    /// leaves no empty-file litter behind.
    fn pin_fresh(project_root: PathBuf, sessions_dir: PathBuf, blob_store: BlobStore) -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        let path = sessions_dir.join(format!("{id}.json"));
        let event_log = EventLog::new(path.with_extension("jsonl"));
        let data = SessionData {
            id,
            project_root: project_root.clone(),
            ..Default::default()
        };
        Self {
            project_root,
            sessions_dir,
            blob_store,
            state: Mutex::new(SessionState {
                path,
                event_log,
                data,
                // Fresh primary session: defer until it gains real content.
                defer_persist: true,
            }),
            persist_gate: Mutex::new(()),
        }
    }

    /// Project root this store is bound to.
    #[allow(dead_code)]
    pub fn project_root(&self) -> &std::path::Path {
        &self.project_root
    }

    pub async fn id(&self) -> String {
        self.state.lock().await.data.id.clone()
    }

    /// `true` while this session has never been persisted **and** still holds
    /// no user-facing content in memory (see
    /// `SessionData::is_user_facing_empty`). Such a session is "deferred":
    /// it exists only in memory so that opening one and exiting without any
    /// real interaction leaves no record behind (ADR-0018). The transport
    /// layer's idle reaper uses this probe to reclaim never-persisted hosted
    /// sessions; a persisted session (even one whose messages were later
    /// replaced with an empty window) is never reported empty here.
    pub async fn is_empty_unpersisted(&self) -> bool {
        let state = self.state.lock().await;
        Self::should_skip_persist(&state)
    }

    /// Lock-held core of [`Self::is_empty_unpersisted`] and of every guarded
    /// setter: the post-mutation state is checked against the on-disk marker
    /// (`path.exists()`) plus the user-facing-emptiness rule. Callers must hold
    /// the session lock and pass the just-mutated state, so the decision and
    /// the event-log append it gates are atomic. The check applies only to a
    /// deferred (fresh primary) session — an explicitly pinned store
    /// (`defer_persist == false`) always persists.
    fn should_skip_persist(state: &SessionState) -> bool {
        state.defer_persist && !state.path.exists() && state.data.is_user_facing_empty()
    }

    /// The authoritative model-visible message window (ADR-0048). This is the
    /// single source of truth for message truth: the round clones from here, the
    /// provider serializes a projection of this, and every write flows back
    /// through `replace_messages` / `mutate_messages` / `append_turn`.
    pub async fn model_window(&self) -> Vec<Message> {
        self.state.lock().await.data.model_window.clone()
    }

    pub async fn full_transcript(&self) -> Vec<Message> {
        let state = self.state.lock().await;
        let mut messages = state.data.archived_transcript.clone();
        messages.extend(state.data.model_window.clone());
        messages
    }

    /// The unified task list, mirrored from `Agent::todos`. Empty means no
    /// active task list. Read on resume to seed the agent and the sticky
    /// panel.
    pub async fn todos(&self) -> muta_contracts::TodoList {
        self.state.lock().await.data.todos.clone()
    }

    /// Replace the task list. Persists both the snapshot and the event log so
    /// resume restores the same list (and so per-item history is retained in
    /// the log).
    pub async fn set_todos(&self, todos: muta_contracts::TodoList) -> Result<(), String> {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            state.data.todos = todos.clone();
            state.data.updated_at = unix_timestamp();
            // The guard reads the post-mutation state: a non-empty list makes
            // the session substantive and persists; clearing the list on a
            // never-persisted empty session stays in memory.
            let empty_unpersisted = Self::should_skip_persist(&state);
            if !empty_unpersisted {
                ensure_event_log_started(&state.event_log, &state.data)?;
                state.event_log.append(SessionEvent::TodosSet { todos })?;
            }
            (state.path.clone(), state.data.clone(), !empty_unpersisted)
        };
        if should_persist {
            self.persist_off_runtime(path, data, self.blob_store.clone())
                .await?;
        }
        Ok(())
    }

    /// The scheduled-prompt list owned by this session (`/schedule`, formerly
    /// `/repeat`). Empty means no scheduled jobs. Read by the background
    /// scheduler to find due jobs and on resume to re-arm the schedule.
    pub async fn scheduled_jobs(&self) -> Vec<muta_contracts::ScheduledJob> {
        self.state.lock().await.data.scheduled_jobs.clone()
    }

    /// Replace the scheduled-prompt list. Snapshot semantics: store the full
    /// list on every change so resume restores the exact schedule. Used by the
    /// `/schedule` command (add / cancel) and by the scheduler (mark fired /
    /// drop once-jobs).
    pub async fn set_scheduled_jobs(
        &self,
        jobs: Vec<muta_contracts::ScheduledJob>,
    ) -> Result<(), String> {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            state.data.scheduled_jobs = jobs.clone();
            state.data.updated_at = unix_timestamp();
            // Adding at least one job is a substantive action that persists the
            // session (the post-mutation state is non-empty); clearing the
            // schedule on a never-persisted empty session stays in memory.
            let empty_unpersisted = Self::should_skip_persist(&state);
            if !empty_unpersisted {
                ensure_event_log_started(&state.event_log, &state.data)?;
                state
                    .event_log
                    .append(SessionEvent::ScheduledJobsSet { jobs })?;
            }
            (state.path.clone(), state.data.clone(), !empty_unpersisted)
        };
        if should_persist {
            self.persist_off_runtime(path, data, self.blob_store.clone())
                .await?;
        }
        Ok(())
    }

    pub async fn last_projection(&self) -> Option<ContextProjectionCheckpoint> {
        self.state.lock().await.data.last_projection.clone()
    }

    /// The current session title and whether it was manually set (ADR-0022).
    /// `(None, false)` for a session that has not yet generated a title; the
    /// caller then falls back to the first-user-message overview. A `true`
    /// `manual` flag means automatic and on-demand AI generation must not
    /// overwrite the stored title.
    pub async fn title(&self) -> (Option<String>, bool) {
        let state = self.state.lock().await;
        (state.data.title.clone(), state.data.title_manual)
    }

    /// Replace the session title. `manual = true` marks a user-set title
    /// (`/title <text>`) that AI generation will not overwrite; the AI runner
    /// and on-demand refresh always pass `false`. Pass `title = None` with
    /// `manual = false` to clear. Persists both the snapshot and the event log
    /// so resume restores the same title.
    pub async fn set_title(&self, title: Option<String>, manual: bool) -> Result<(), String> {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            state.data.title = title.clone();
            state.data.title_manual = manual;
            state.data.updated_at = unix_timestamp();
            // An empty, never-persisted session stays unpersisted even when a
            // title is set: a session with a title but no messages is still
            // empty content, and writing it would surface it in the picker as
            // empty-session litter.
            let empty_unpersisted = Self::should_skip_persist(&state);
            if !empty_unpersisted {
                ensure_event_log_started(&state.event_log, &state.data)?;
                state
                    .event_log
                    .append(SessionEvent::TitleSet { title, manual })?;
            }
            (state.path.clone(), state.data.clone(), !empty_unpersisted)
        };
        if should_persist {
            self.persist_off_runtime(path, data, self.blob_store.clone())
                .await?;
        }
        Ok(())
    }

    pub async fn archived_transcript_count(&self) -> usize {
        self.state.lock().await.data.archived_transcript.len()
    }

    pub async fn parent_id(&self) -> Option<String> {
        self.state.lock().await.data.parent_id.clone()
    }

    /// The session-level disabled-tool mask (ADR-0048 Phase 2). Empty means
    /// all tools enabled. Restored on resume so a user toggle survives restart.
    pub async fn disabled_tools(&self) -> std::collections::HashSet<String> {
        self.state.lock().await.data.disabled_tools.clone()
    }

    /// Replace the disabled-tool mask. Mirrors `Agent::disabled_tools` so a
    /// user toggle survives restart. The single write path for the mask.
    pub async fn set_disabled_tools(
        &self,
        tools: std::collections::HashSet<String>,
    ) -> Result<(), String> {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            state.data.disabled_tools = tools.clone();
            state.data.updated_at = unix_timestamp();
            // A non-empty mask is substantive and persists; an empty mask on a
            // never-persisted empty session stays in memory.
            let empty_unpersisted = Self::should_skip_persist(&state);
            if !empty_unpersisted {
                ensure_event_log_started(&state.event_log, &state.data)?;
                state
                    .event_log
                    .append(SessionEvent::DisabledToolsSet { tools })?;
            }
            (state.path.clone(), state.data.clone(), !empty_unpersisted)
        };
        if should_persist {
            self.persist_off_runtime(path, data, self.blob_store.clone())
                .await?;
        }
        Ok(())
    }

    /// The session-scoped YOLO posture. `false` = attended (default).
    pub async fn yolo(&self) -> bool {
        self.state.lock().await.data.yolo
    }

    /// Replace the YOLO posture. Mirrors `Agent::get_yolo` so a
    /// daemon restart restores the session in the posture it died in.
    pub async fn set_yolo(&self, enabled: bool) -> Result<(), String> {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            if state.data.yolo == enabled {
                return Ok(());
            }
            state.data.yolo = enabled;
            state.data.updated_at = unix_timestamp();
            let empty_unpersisted = Self::should_skip_persist(&state);
            if !empty_unpersisted {
                ensure_event_log_started(&state.event_log, &state.data)?;
                state
                    .event_log
                    .append(SessionEvent::YoloSet { enabled })?;
            }
            (state.path.clone(), state.data.clone(), !empty_unpersisted)
        };
        if should_persist {
            self.persist_off_runtime(path, data, self.blob_store.clone())
                .await?;
        }
        Ok(())
    }

    /// The harness round counter, the session-scoped monotonic watermark
    /// (ADR-0048 Phase 2). `0` for a fresh session. Restored on resume so the
    /// todo stale-detector's `updated_at_round` comparisons stay valid.
    pub async fn round_counter(&self) -> u64 {
        self.state.lock().await.data.round_counter
    }

    /// Replace the round counter. Mirrors `Agent::round_counter` so resume
    /// restores it. The single write path for the counter.
    pub async fn set_round_counter(&self, counter: u64) -> Result<(), String> {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            state.data.round_counter = counter;
            state.data.updated_at = unix_timestamp();
            // A non-zero counter marks a started round and persists; writing 0
            // to a never-persisted empty session stays in memory.
            let empty_unpersisted = Self::should_skip_persist(&state);
            if !empty_unpersisted {
                ensure_event_log_started(&state.event_log, &state.data)?;
                state
                    .event_log
                    .append(SessionEvent::RoundCounterSet { counter })?;
            }
            (state.path.clone(), state.data.clone(), !empty_unpersisted)
        };
        if should_persist {
            self.persist_off_runtime(path, data, self.blob_store.clone())
                .await?;
        }
        Ok(())
    }

    /// The durable round-interrupt records (C11): one per round stopped
    /// before completing, newest last. Pure projection state — never part
    /// of the model-visible transcript.
    pub async fn round_interrupts(&self) -> Vec<muta_contracts::RoundInterrupt> {
        self.state.lock().await.data.round_interrupts.clone()
    }

    /// Append one round-interrupt record (C11). Called on every round stop
    /// path with the observed reason and a wall-clock timestamp. Best-effort
    /// duplicate guard: a record for the same round with the same reason is
    /// not appended twice (the runtime can observe a stop from two sites —
    /// e.g. the round task's tail and the process-kill path).
    pub async fn record_round_interrupt(
        &self,
        record: muta_contracts::RoundInterrupt,
    ) -> Result<(), String> {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            let already =
                state.data.round_interrupts.iter().any(|existing| {
                    existing.reason == record.reason && existing.round == record.round
                });
            if already {
                return Ok(());
            }
            state.data.round_interrupts.push(record.clone());
            state.data.updated_at = unix_timestamp();
            let empty_unpersisted = Self::should_skip_persist(&state);
            if !empty_unpersisted {
                ensure_event_log_started(&state.event_log, &state.data)?;
                state
                    .event_log
                    .append(SessionEvent::RoundInterruptRecorded { record })?;
            }
            (state.path.clone(), state.data.clone(), !empty_unpersisted)
        };
        if should_persist {
            self.persist_off_runtime(path, data, self.blob_store.clone())
                .await?;
        }
        Ok(())
    }

    /// Clear every round-interrupt record (C11). Called when the interrupted
    /// round's outcome is superseded — the user re-sent and the fresh round
    /// completed, or the session moved on — so the projection shows the
    /// durable history without stale "interrupted" markers for rounds that
    /// later resolved.
    pub async fn clear_round_interrupts(&self) -> Result<(), String> {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            if state.data.round_interrupts.is_empty() {
                return Ok(());
            }
            state.data.round_interrupts.clear();
            state.data.updated_at = unix_timestamp();
            let empty_unpersisted = Self::should_skip_persist(&state);
            if !empty_unpersisted {
                ensure_event_log_started(&state.event_log, &state.data)?;
                state
                    .event_log
                    .append(SessionEvent::RoundInterruptsCleared {})?;
            }
            (state.path.clone(), state.data.clone(), !empty_unpersisted)
        };
        if should_persist {
            self.persist_off_runtime(path, data, self.blob_store.clone())
                .await?;
        }
        Ok(())
    }

    /// Durable lifecycle-aware request accounting for the active session.
    pub async fn request_usage_records(&self) -> Vec<muta_contracts::RequestUsageRecord> {
        self.state.lock().await.data.request_usage_records.clone()
    }

    /// The durable `/retry` resume point (C12): `Some` while a stopped round
    /// is parked for `/retry`, `None` once it completed or was retired. Pure
    /// projection state — never part of the model-visible transcript.
    pub async fn retry_pending(&self) -> Option<muta_contracts::RetryPoint> {
        self.state.lock().await.data.retry_pending.clone()
    }

    /// Arm the `/retry` resume point (C12). Snapshot semantics: the single
    /// slot is replaced (arming for a newer round retires an older point).
    /// Called when a round stops before completing with committed content —
    /// a terminal error after the provider retry budget was exhausted, or an
    /// interrupt past the phase-1 unsend window.
    pub async fn arm_retry_pending(&self, point: muta_contracts::RetryPoint) -> Result<(), String> {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            state.data.retry_pending = Some(point.clone());
            state.data.updated_at = unix_timestamp();
            let empty_unpersisted = Self::should_skip_persist(&state);
            if !empty_unpersisted {
                ensure_event_log_started(&state.event_log, &state.data)?;
                state
                    .event_log
                    .append(SessionEvent::RetryPendingRecorded { point })?;
            }
            (state.path.clone(), state.data.clone(), !empty_unpersisted)
        };
        if should_persist {
            self.persist_off_runtime(path, data, self.blob_store.clone())
                .await?;
        }
        Ok(())
    }

    /// Clear the `/retry` resume point (C12). Called when the parked round
    /// completes — naturally or via `/retry` — and by every path that moves
    /// the session past it (a newer round being admitted, `/new`). A no-op
    /// when nothing is armed, so callers can invoke it unconditionally.
    pub async fn clear_retry_pending(&self) -> Result<(), String> {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            if state.data.retry_pending.is_none() {
                return Ok(());
            }
            state.data.retry_pending = None;
            state.data.updated_at = unix_timestamp();
            let empty_unpersisted = Self::should_skip_persist(&state);
            if !empty_unpersisted {
                ensure_event_log_started(&state.event_log, &state.data)?;
                state
                    .event_log
                    .append(SessionEvent::RetryPendingCleared {})?;
            }
            (state.path.clone(), state.data.clone(), !empty_unpersisted)
        };
        if should_persist {
            self.persist_off_runtime(path, data, self.blob_store.clone())
                .await?;
        }
        Ok(())
    }

    /// Replace the session's request ledger. Callers pass records already
    /// scoped to the active session; the store validates that boundary before
    /// appending the snapshot event.
    ///
    /// The diff is computed through a key→record index built once per call
    /// (`O((n+m) log n)`); the previous nested scans (`any`-inside-`any`
    /// plus a `find` per record) made every post-round persist quadratic in
    /// the session's lifetime request count.
    pub async fn set_request_usage_records(
        &self,
        records: Vec<muta_contracts::RequestUsageRecord>,
    ) -> Result<(), String> {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            if records
                .iter()
                .any(|record| record.key.session_id != state.data.id)
            {
                return Err("request usage record belongs to another session".to_string());
            }
            // Index the incoming set once; every membership test below is a
            // lookup instead of a rescan.
            let incoming: std::collections::BTreeMap<
                &muta_contracts::RequestUsageKey,
                &muta_contracts::RequestUsageRecord,
            > = records.iter().map(|r| (&r.key, r)).collect();
            if state
                .data
                .request_usage_records
                .iter()
                .any(|existing| !incoming.contains_key(&existing.key))
            {
                return Err("request usage records are append/update only".to_string());
            }
            if state.data.request_usage_records == records {
                return Ok(());
            }
            let existing: std::collections::BTreeMap<
                &muta_contracts::RequestUsageKey,
                &muta_contracts::RequestUsageRecord,
            > = state
                .data
                .request_usage_records
                .iter()
                .map(|r| (&r.key, r))
                .collect();
            let changed = records
                .iter()
                .filter(|record| existing.get(&record.key) != Some(record))
                .cloned()
                .collect::<Vec<_>>();
            state.data.request_usage_records = records.clone();
            state.data.updated_at = unix_timestamp();
            let empty_unpersisted = Self::should_skip_persist(&state);
            if !empty_unpersisted {
                ensure_event_log_started(&state.event_log, &state.data)?;
                for record in changed {
                    state
                        .event_log
                        .append(SessionEvent::RequestUsageUpsert { record })?;
                }
            }
            (state.path.clone(), state.data.clone(), !empty_unpersisted)
        };
        if should_persist {
            self.persist_off_runtime(path, data, self.blob_store.clone())
                .await?;
        }
        Ok(())
    }

    /// The session-scoped provider + model pin (C6). `None` means "follow the
    /// global default"; the harness seeds this on first `/models` switch.
    pub async fn provider_selection(&self) -> Option<ProviderSelection> {
        self.state.lock().await.data.provider_selection.clone()
    }

    /// Replace the session-scoped provider + model pin (C6). Persists both the
    /// snapshot and the event log so resume restores the session's own provider
    /// instead of the global default. This is the single write path for the
    /// per-session provider override; the `/models` switch handler calls it
    /// instead of mutating `config.toml`'s selection, so one session switching
    /// provider/model never affects another.
    ///
    /// A provider pin on an otherwise-empty, never-yet-persisted session is
    /// **not** persisted (the `empty_unpersisted` guard): such a session has no
    /// real content, so writing it would leave empty-session litter behind.
    pub async fn set_provider_selection(
        &self,
        selection: Option<ProviderSelection>,
    ) -> Result<(), String> {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            state.data.provider_selection = selection.clone();
            state.data.updated_at = unix_timestamp();
            let empty_unpersisted = Self::should_skip_persist(&state);
            if !empty_unpersisted {
                ensure_event_log_started(&state.event_log, &state.data)?;
                state
                    .event_log
                    .append(SessionEvent::ProviderSelectionSet { selection })?;
            }
            (state.path.clone(), state.data.clone(), !empty_unpersisted)
        };
        if should_persist {
            self.persist_off_runtime(path, data, self.blob_store.clone())
                .await?;
        }
        Ok(())
    }

    pub async fn replace_messages(&self, messages: Vec<Message>) -> Result<(), String> {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            state.data.model_window = messages;
            state.data.updated_at = unix_timestamp();
            // A session that is still empty (no messages AND never persisted)
            // stays in memory only: opening a session and exiting without
            // sending any content must not create a record. The guard checks
            // the POST-replacement state, so the first real message (or a
            // command echo via `mutate_messages`) does persist, while a no-op
            // empty-window replace on a brand-new session does not.
            let empty_unpersisted = Self::should_skip_persist(&state);
            if !empty_unpersisted {
                ensure_event_log_started(&state.event_log, &state.data)?;
                state.event_log.append(SessionEvent::MessagesReplaced {
                    messages: state.data.model_window.clone(),
                })?;
            }
            (state.path.clone(), state.data.clone(), !empty_unpersisted)
        };
        if should_persist {
            self.persist_off_runtime(path, data, self.blob_store.clone())
                .await?;
        }
        Ok(())
    }

    /// Apply `f` to the live message window under the session lock, append a
    /// `MessagesReplaced` event, and persist — atomically (ADR-0048).
    ///
    /// This is the single mutation primitive that lets the session *be* the
    /// source of truth for the message list, instead of a clone-out /
    /// mutate-locally / swap-back trio that can diverge if another caller
    /// (a fork, a compaction, an `append_turn`) mutates the window between the
    /// clone and the swap. `f` runs in place under the lock, so the append and
    /// persist always reflect exactly what `f` produced.
    ///
    /// `f` receives the full `model_window`; it may push, pop, edit, or
    /// replace freely. The resulting window becomes the durable snapshot.
    pub async fn mutate_messages<F>(&self, f: F) -> Result<(), String>
    where
        F: FnOnce(&mut Vec<Message>),
    {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            f(&mut state.data.model_window);
            state.data.updated_at = unix_timestamp();
            // Same empty-session deferral as `replace_messages`: a brand-new
            // session that is still empty after the mutation stays in memory.
            // A real command echo (the primary `mutate_messages` caller) makes
            // the session non-empty, so it DOES persist — exactly the "first
            // command persists the session" contract.
            let empty_unpersisted = Self::should_skip_persist(&state);
            if !empty_unpersisted {
                ensure_event_log_started(&state.event_log, &state.data)?;
                state.event_log.append(SessionEvent::MessagesReplaced {
                    messages: state.data.model_window.clone(),
                })?;
            }
            (state.path.clone(), state.data.clone(), !empty_unpersisted)
        };
        if should_persist {
            self.persist_off_runtime(path, data, self.blob_store.clone())
                .await?;
        }
        Ok(())
    }

    /// The durable command ledger (ADR-0091): every slash command and `!cmd`
    /// invocation with its typed result, in invocation order. Commands live
    /// here, not in the message stream, so resume/export/audit reconstruct
    /// them without polluting the dialogue.
    pub async fn commands(&self) -> Vec<muta_contracts::CommandRecord> {
        let state = self.state.lock().await;
        state.data.commands.clone()
    }

    /// Atomically mutate the command ledger in place under the lock and
    /// persist the result — the mirror of [`SessionStore::mutate_messages`]
    /// (ADR-0091, ADR-0048 single-write-path). `f` may push, pop, edit, or
    /// replace freely; the resulting list becomes the durable snapshot.
    ///
    /// The empty-session deferral mirrors other auxiliary setters: a brand-new
    /// session that is still empty after command mutation stays in memory.
    /// Commands ride along into the snapshot and event log once substantive
    /// dialogue or state materializes the session.
    pub async fn mutate_commands<F>(&self, f: F) -> Result<(), String>
    where
        F: FnOnce(&mut Vec<muta_contracts::CommandRecord>),
    {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            f(&mut state.data.commands);
            state.data.updated_at = unix_timestamp();
            // The empty-session deferral mirrors other auxiliary setters: a
            // brand-new session that is still empty after command mutation stays
            // in memory. Commands ride along into the snapshot and event log
            // once substantive dialogue or state materializes the session.
            let empty_unpersisted = Self::should_skip_persist(&state);
            if !empty_unpersisted {
                ensure_event_log_started(&state.event_log, &state.data)?;
                state.event_log.append(SessionEvent::CommandsReplaced {
                    commands: state.data.commands.clone(),
                })?;
            }
            (state.path.clone(), state.data.clone(), !empty_unpersisted)
        };
        if should_persist {
            self.persist_off_runtime(path, data, self.blob_store.clone())
                .await?;
        }
        Ok(())
    }

    /// Incrementally persist new messages appended since the last durable
    /// write, without rewriting the full snapshot (ADR-0048).
    ///
    /// The caller passes the *current full* round history. This method diffs it
    /// against the messages already durable in `data.model_window` and appends only
    /// the tail as a `MessagesAppended` event to the append-only log — O(delta),
    /// not O(history). The snapshot cache (`session.json`) is intentionally
    /// **not** rewritten here: it stays at the last round boundary and is
    /// refreshed by `replace_messages` at round end. On resume, `load_or_seed`
    /// replays the log (authoritative), so the appended tail is recovered.
    ///
    /// This is the mid-round save point at a completed turn boundary: a crash after a side-effecting tool
    /// call leaves the transcript in sync with the filesystem instead of
    /// rewinding to the previous turn. If `current` is no longer than the
    /// durable prefix (e.g. a compaction already replaced messages) this is a
    /// no-op.
    pub async fn append_turn(&self, current: &[Message]) -> Result<(), String> {
        // Collect any persistence work to do *outside* the lock. The lock is
        // held only for the in-memory mutation + the (O(1)) event-log append;
        // the full snapshot persistence is deferred to `persist_off_runtime`.
        // The variants differ in size, but this enum is a short-lived stack
        // value used only to shuttle the snapshot out of the locked scope —
        // boxing the `Snapshot` payload to equalize variants would add an
        // allocation on the hot path for no benefit, so the lint is allowed.
        #[allow(clippy::large_enum_variant)]
        enum Persist {
            None,
            Snapshot { path: PathBuf, data: SessionData },
        }
        let persist = {
            let mut state = self.state.lock().await;
            let baseline = state.data.model_window.len();
            // Only the strictly-new tail is the delta. If `current` is shorter
            // or equal (compaction rewrote the window, or nothing changed),
            // there is nothing to append.
            if current.len() <= baseline {
                return Ok(());
            }
            // Guard against a divergent history: the durable prefix must match
            // the incoming prefix. A mismatch means the caller and the store
            // disagree on state (a bug, or a compaction rewrote the window
            // without going through `replace_messages`); fall back to a full
            // replace so the log never records a corrupt splice. `Message` has
            // no `PartialEq`, so we compare the identity-bearing fields.
            let diverged = current[..baseline]
                .iter()
                .zip(state.data.model_window[..].iter())
                .any(|(incoming, durable)| {
                    incoming.role != durable.role
                        || incoming.content != durable.content
                        || incoming.tool_call_id != durable.tool_call_id
                });
            if diverged {
                tracing::warn!(
                    baseline,
                    incoming = current.len(),
                    "append_turn: incoming prefix diverged from durable state; full replace"
                );
                state.data.model_window = current.to_vec();
                state.data.updated_at = unix_timestamp();
                ensure_event_log_started(&state.event_log, &state.data)?;
                state.event_log.append(SessionEvent::MessagesReplaced {
                    messages: state.data.model_window.clone(),
                })?;
                Persist::Snapshot {
                    path: state.path.clone(),
                    data: state.data.clone(),
                }
            } else {
                // Advance the in-memory state and append the delta event. The
                // snapshot cache is not touched (stays at the round boundary).
                let delta = current[baseline..].to_vec();
                state.data.model_window.extend(delta.clone());
                state.data.updated_at = unix_timestamp();
                ensure_event_log_started(&state.event_log, &state.data)?;
                state
                    .event_log
                    .append(SessionEvent::MessagesAppended { messages: delta })?;
                Persist::None
            }
        };
        match persist {
            Persist::None => Ok(()),
            Persist::Snapshot { path, data } => {
                self.persist_off_runtime(path, data, self.blob_store.clone())
                    .await
            }
        }
    }

    pub async fn commit_context_projection(
        &self,
        result: ContextProjectionResult,
    ) -> Result<(), String> {
        let (path, data) = {
            let mut state = self.state.lock().await;
            state
                .data
                .archived_transcript
                .extend(result.archived_originals.clone());
            state.data.model_window = result.model_window.clone();
            state.data.last_projection = Some(result.checkpoint.clone());
            state.data.updated_at = unix_timestamp();
            ensure_event_log_started(&state.event_log, &state.data)?;
            state
                .event_log
                .append(SessionEvent::ContextProjectionCommitted {
                    archived_originals: result.archived_originals,
                    model_window: result.model_window,
                    checkpoint: result.checkpoint,
                })?;
            (state.path.clone(), state.data.clone())
        };
        self.persist_off_runtime(path, data, self.blob_store.clone())
            .await
    }

    /// Start a brand-new session and repoint this store at it. The previous
    /// session's file is left intact on disk (it was already persisted on
    /// every mutation) and stays reachable through [`Self::list`] /
    /// [`Self::resume`]. Returns the new session id.
    ///
    /// Under ADR-0018 this no longer mutates a shared "active" file: it simply
    /// mints a new `sessions/<id>.{json,jsonl}` and switches this process to
    /// writing it, so a concurrent instance cannot clobber the previous
    /// session.
    pub async fn reset(&self) -> Result<String, String> {
        let project_root = self.project_root.clone();
        let mut state = self.state.lock().await;
        let sessions_dir = self.sessions_dir.clone();
        let id = uuid::Uuid::new_v4().to_string();
        let path = sessions_dir.join(format!("{id}.json"));
        let event_log = EventLog::new(path.with_extension("jsonl"));
        let data = SessionData {
            project_root,
            ..Default::default()
        };
        state.path = path;
        state.event_log = event_log;
        state.data = data;
        // Fresh again: defer until the new session gains content. Do not
        // persist an empty snapshot or event log — a session that never gains
        // content leaves no empty-file litter (see ADR-0018).
        state.defer_persist = true;
        Ok(id)
    }

    pub async fn resume(&self, id: Option<&str>) -> Result<String, String> {
        let target = match id {
            Some(id) => id.to_string(),
            None => self
                .list()
                .await?
                .into_iter()
                .find(|session| !session.active && session.message_count > 0)
                .map(|session| session.id)
                .ok_or_else(|| "No previous session is available to resume.".to_string())?,
        };
        self.open(&target).await?;
        Ok(self.state.lock().await.data.id.clone())
    }

    /// Fork the current session: write its state to a new child file and
    /// repoint this store at the child. The parent's file is untouched
    /// (already current) and remains reachable. Returns `(child_id,
    /// parent_id)`.
    pub async fn fork(&self) -> Result<(String, String), String> {
        let mut state = self.state.lock().await;
        if state.data.model_window.is_empty() && state.data.archived_transcript.is_empty() {
            return Err("Cannot fork an empty session.".to_string());
        }
        let parent_id = state.data.id.clone();
        let now = unix_timestamp();

        // Build the child snapshot from the parent's current state.
        let mut child = state.data.clone();
        let fork_id = uuid::Uuid::new_v4().to_string();
        child.id = fork_id.clone();
        child.parent_id = Some(parent_id.clone());
        child.fork_kind = muta_contracts::SessionForkKind::Fork;
        child.created_at = now;
        child.updated_at = now;
        // Usage belongs to concrete requests made by the parent session. A
        // fork inherits context, not historical billing records.
        child.request_usage_records.clear();

        let child_path = self.sessions_dir.join(format!("{fork_id}.json"));
        let child_log = EventLog::new(child_path.with_extension("jsonl"));
        // Seed the child's own event log first, then persist the snapshot, so
        // the snapshot's `applied_seq` watermark reflects the freshly-seeded
        // log and a later load takes the fast path (one store = one file =
        // one log).
        child_log.rewrite(snapshot_to_events(&child))?;
        persist_to(&child_path, &child, &self.blob_store)?;

        // Repoint this store at the child; the parent file is already current.
        state.path = child_path;
        state.event_log = child_log;
        state.data = child;
        Ok((fork_id, parent_id))
    }

    /// Fork the current session into a **self-contained side file** without
    /// disturbing this store's active pointer (ADR-0017). Unlike `fork`,
    /// the primary keeps running: this method only *reads* the current
    /// snapshot, writes a sibling `sessions/<side_id>.{json,jsonl}`, and
    /// returns `(side_id, parent_id)`. The primary's `state` is left
    /// untouched, so a concurrent parent turn is not clobbered.
    ///
    /// Load the side into its own live store with `open_side`.
    pub async fn fork_to_side(&self) -> Result<(String, String), String> {
        let state = self.state.lock().await;
        if state.data.model_window.is_empty() && state.data.archived_transcript.is_empty() {
            return Err("Cannot fork an empty session.".to_string());
        }
        let parent_id = state.data.id.clone();
        let now = unix_timestamp();

        // Build the side snapshot from the primary's current state.
        let mut side = state.data.clone();
        let side_id = uuid::Uuid::new_v4().to_string();
        side.id = side_id.clone();
        side.parent_id = Some(parent_id.clone());
        side.fork_kind = muta_contracts::SessionForkKind::Aside;
        side.created_at = now;
        side.updated_at = now;
        side.request_usage_records.clear();

        let side_path = self.sessions_dir.join(format!("{side_id}.json"));
        let side_log = EventLog::new(side_path.with_extension("jsonl"));
        // Seed the side's own event log first, then persist the snapshot, so the
        // snapshot's `applied_seq` watermark reflects the seeded log and a later
        // load takes the fast path (one store = one file = one log), exactly
        // like `fork`. The primary's files are never touched.
        side_log.rewrite(snapshot_to_events(&side))?;
        persist_to(&side_path, &side, &self.blob_store)?;

        // Deliberately do NOT mutate `state` — the primary keeps its active
        // pointer, history, and in-flight turn intact.
        Ok((side_id, parent_id))
    }

    /// Construct a live [`SessionStore`] pinned to a side session file that
    /// lives in this store's `sessions_dir` (written by `fork_to_side`). The
    /// returned store shares the primary's project root, sessions dir, and blob
    /// store root, so inherited content (including image blobs) resolves the
    /// same way as in the primary. It writes only its own `sessions/<id>.*`
    /// files, so the two stores never race on the same file.
    pub async fn open_side(&self, side_id: &str) -> Result<SessionStore, String> {
        let side_path = self.sessions_dir.join(format!("{side_id}.json"));
        if !side_path.exists() {
            return Err(format!("Side session '{side_id}' was not found."));
        }
        let event_log_path = side_path.with_extension("jsonl");
        let project_root = self.project_root.clone();
        let blob_store = BlobStore::new(self.blob_store.root().to_path_buf());
        let data = load_or_seed(&side_path, &event_log_path, &blob_store, &project_root);
        let event_log = EventLog::new(event_log_path);
        Ok(SessionStore {
            project_root,
            sessions_dir: self.sessions_dir.clone(),
            blob_store,
            state: Mutex::new(SessionState {
                path: side_path,
                event_log,
                data,
                // An already-materialised side session persists eagerly.
                defer_persist: false,
            }),
            persist_gate: Mutex::new(()),
        })
    }

    /// Read the full DAG session tree.
    pub async fn tree(&self) -> muta_contracts::SessionTree {
        let state = self.state.lock().await;
        state.data.tree.clone()
    }

    /// Insert an entry directly into the session tree and persist snapshot.
    pub async fn insert_tree_entry(
        &self,
        entry: muta_contracts::SessionEntry,
    ) -> Result<String, String> {
        let mut state = self.state.lock().await;
        let id = entry.id.clone();
        state.data.tree.insert_entry(entry);
        state.data.model_window = state.data.tree.get_context_messages(&id);
        state.data.updated_at = unix_timestamp();
        persist_to(&state.path, &state.data, &self.blob_store)?;
        Ok(id)
    }

    /// Switch active leaf in the DAG session tree and update the active model window.
    pub async fn switch_tree_leaf(&self, target_leaf_id: &str) -> Result<Vec<Message>, String> {
        let mut state = self.state.lock().await;
        if !state.data.tree.entries.contains_key(target_leaf_id) {
            return Err(format!("Node '{target_leaf_id}' not found in session tree"));
        }
        state.data.tree.active_leaf_id = Some(target_leaf_id.to_string());
        let messages = state.data.tree.get_context_messages(target_leaf_id);
        state.data.model_window = messages.clone();
        state.data.updated_at = unix_timestamp();
        persist_to(&state.path, &state.data, &self.blob_store)?;
        Ok(messages)
    }

    /// Switch this store to an existing session file by id (or 4+-char hex
    /// prefix). The session's state is reloaded from its own event log (the
    /// durable authority), so `open` always reflects the latest on-disk
    /// content — even if another process wrote it.
    ///
    /// The resolve → load → swap is performed under a single held lock so two
    /// concurrent `open`/`reset` calls cannot interleave and drop each other's
    /// repoint (the previous implementation released the lock between resolve
    /// and swap, a lost-update TOCTOU window). The blocking `load_or_seed` runs
    /// on a `spawn_blocking` thread, but the lock guard is awaited across it,
    /// so the critical section stays atomic.
    pub async fn open(&self, id: &str) -> Result<(), String> {
        let mut state = self.state.lock().await;
        let (resolved, path) = self.resolve_session(id, &state)?;
        // No-op when the caller asks for the session we already hold.
        if state.data.id == resolved {
            return Ok(());
        }
        let event_log_path = path.with_extension("jsonl");
        let project_root = self.project_root.clone();
        let blob_store = self.blob_store.clone();
        // Blocking load on a worker thread; the lock is held across the await
        // so the swap below is atomic with the resolve above.
        let load_path = path.clone();
        let load_event_log_path = event_log_path.clone();
        let data = tokio::task::spawn_blocking(move || {
            load_or_seed(&load_path, &load_event_log_path, &blob_store, &project_root)
        })
        .await
        .map_err(|e| format!("session open task failed: {e}"))?;
        state.path = path;
        state.event_log = EventLog::new(event_log_path);
        state.data = data;
        // The opened session is already materialised on disk; persist eagerly.
        state.defer_persist = false;
        Ok(())
    }

    /// Delete a session by id or short id prefix. Deleting the active session
    /// removes its snapshot and event log, then repoints the store at a fresh
    /// empty session; other sessions just have their two files removed from
    /// the sessions directory.
    ///
    /// Returns the **resolved full id** of the deleted session so callers can
    /// prune derived per-session state (e.g. the project embedding index).
    pub async fn delete(&self, id: &str) -> Result<String, String> {
        let (resolved, snapshot, is_active) = {
            let state = self.state.lock().await;
            let (resolved, path) = self.resolve_session(id, &state)?;
            (resolved.clone(), path, state.data.id == resolved)
        };

        let log = snapshot.with_extension("jsonl");
        let resolved_for_task = resolved.clone();
        tokio::task::spawn_blocking(move || {
            let existed = snapshot.exists() || log.exists();
            let _ = fs::remove_file(&snapshot);
            let _ = fs::remove_file(&log);

            if !existed {
                return Err(format!(
                    "Could not delete session '{}': files not found.",
                    resolved_for_task
                ));
            }
            Ok(())
        })
        .await
        .map_err(|e| format!("delete task failed: {e}"))??;

        // Repoint at a fresh session so the store stays usable after the
        // active session is removed.
        if is_active {
            self.reset().await?;
        }
        Ok(resolved)
    }

    /// Set (or clear) a session's manual title by id or short id prefix — the
    /// storage half of `AgentRequest::RenameSession`. `Some(title)` records a
    /// user-set title (`manual = true`, so AI generation will not overwrite
    /// it, ADR-0022); `None` clears the manual override (`manual = false`,
    /// [`Self::set_title`]'s documented clear form), returning the picker /
    /// monitor overview to the AI-title / first-prompt fallback.
    ///
    /// Renaming the active session delegates to [`Self::set_title`] so the
    /// in-memory state and the empty-session laziness guard stay
    /// authoritative. Renaming an archived session mirrors [`Self::delete`]'s
    /// shape: the one file is loaded, mutated, and re-persisted on a blocking
    /// thread (with the `TitleSet` event appended to its log), leaving this
    /// store's pinned session untouched.
    pub async fn rename(&self, id: &str, title: Option<String>) -> Result<(), String> {
        let manual = title.is_some();
        let (path, is_active) = {
            let state = self.state.lock().await;
            let (resolved, path) = self.resolve_session(id, &state)?;
            (path, state.data.id == resolved)
        };
        if is_active {
            return self.set_title(title, manual).await;
        }
        let blob_store = self.blob_store.clone();
        let project_root = self.project_root.clone();
        tokio::task::spawn_blocking(move || {
            let log_path = path.with_extension("jsonl");
            let mut data = load_or_seed(&path, &log_path, &blob_store, &project_root);
            data.title = title.clone();
            data.title_manual = manual;
            data.updated_at = unix_timestamp();
            let event_log = EventLog::new(log_path.clone());
            ensure_event_log_started(&event_log, &data)?;
            event_log.append(SessionEvent::TitleSet { title, manual })?;
            // Same ordering as `persist_off_runtime`: compact before the
            // snapshot write so the stamped watermark matches the seed.
            compact_log_if_needed(&log_path, &data)?;
            persist_to(&path, &data, &blob_store)
        })
        .await
        .map_err(|e| format!("rename task failed: {e}"))?
    }

    /// The picker-style summary of the pinned session, synthesized from
    /// in-memory state. The disk-backed [`Self::list`] only sees persisted
    /// sessions, so a title change on a live, never-persisted session (empty
    /// transcript — `set_title` deliberately does not persist those) is
    /// invisible to every `list()` consumer. Monitor reseeds and overview
    /// pushes after a rename use this to see it anyway.
    pub async fn active_summary(&self) -> SessionSummary {
        let state = self.state.lock().await;
        let data = &state.data;
        let overview = match data.title.as_deref().filter(|t| !t.trim().is_empty()) {
            Some(title) => truncate_preview(title, 64),
            None => data
                .model_window
                .iter()
                .rev()
                .chain(data.archived_transcript.iter().rev())
                .find(|m| m.role == muta_contracts::Role::User && !m.hidden)
                .map(|m| truncate_preview(&m.content, 64))
                .unwrap_or_else(|| "(empty session)".to_string()),
        };
        SessionSummary {
            id: data.id.clone(),
            parent_id: data.parent_id.clone(),
            fork_kind: data.fork_kind,
            message_count: data.model_window.len() + data.archived_transcript.len(),
            updated_at: data.updated_at,
            created_at: data.created_at,
            overview,
            active: true,
        }
    }

    pub async fn list(&self) -> Result<Vec<SessionSummary>, String> {
        let active_id = self.state.lock().await.data.id.clone();
        let sessions_dir = self.sessions_dir.clone();
        tokio::task::spawn_blocking(move || {
            let mut summaries = Vec::new();
            if sessions_dir.exists() {
                for entry in fs::read_dir(&sessions_dir).map_err(|error| error.to_string())? {
                    let entry = entry.map_err(|error| error.to_string())?;
                    let path = entry.path();
                    if path.extension().and_then(|value| value.to_str()) != Some("json") {
                        continue;
                    }
                    let Ok(content) = fs::read_to_string(&path) else {
                        continue;
                    };
                    let Ok(session) = serde_json::from_str::<SessionHeader>(&content) else {
                        continue;
                    };
                    // Empty sessions (no dialogue messages in model_window or
                    // archived_transcript) are never valid history records.
                    // Skip them, and prune legacy stale empty files from disk.
                    if session.model_window.is_empty() && session.archived_transcript.is_empty() {
                        if session.id != active_id {
                            let _ = fs::remove_file(&path);
                            let _ = fs::remove_file(path.with_extension("jsonl"));
                        }
                        continue;
                    }
                    summaries.push(summary_header(&session, session.id == active_id));
                }
            }
            summaries.sort_by_key(|item| std::cmp::Reverse(item.updated_at));
            Ok(summaries)
        })
        .await
        .map_err(|e| format!("session list task failed: {e}"))?
    }

    /// Full detail for one session, requested on demand by the session-info
    /// sub-view (`i` from the picker). Like [`Self::list`], this uses the
    /// deferred `SessionHeader` parse (no full-transcript deserialize) and runs
    /// on a blocking thread; unlike the list rows it returns the *complete*
    /// last effective user prompt rather than a truncated preview. Returns
    /// `Err` for an unknown / unreadable id.
    pub async fn detail(&self, id: &str) -> Result<SessionDetail, String> {
        let active_id = self.state.lock().await.data.id.clone();
        // Resolve to a path (filename match, no full parse) and hand the one
        // file off to a blocking thread for the header extract.
        let (resolved, path) = {
            let state = self.state.lock().await;
            self.resolve_session(id, &state)?
        };
        tokio::task::spawn_blocking(move || {
            let content = fs::read_to_string(&path)
                .map_err(|e| format!("could not read session '{}': {e}", resolved))?;
            let header = serde_json::from_str::<SessionHeader>(&content)
                .map_err(|e| format!("could not parse session '{}': {e}", resolved))?;
            Ok(SessionDetail {
                id: header.id.clone(),
                title: header.title.clone(),
                created_at: header.created_at,
                updated_at: header.updated_at,
                message_count: header.model_window.len() + header.archived_transcript.len(),
                active: header.id == active_id,
                last_prompt: last_effective_prompt(&header),
            })
        })
        .await
        .map_err(|e| format!("session detail task failed: {e}"))?
    }

    /// Run the snapshot persistence off the async runtime.
    ///
    /// `persist_to` does blocking filesystem I/O (write the JSON snapshot,
    /// `fsync`, hash + spill large tool payloads to the blob store). Doing that
    /// work while holding the session `Mutex` blocks the async executor for the
    /// duration of every `fsync` (commonly 5–50 ms, far worse over NFS / slow
    /// disks), which stalls every concurrent reader and writer. Instead the
    /// caller mutates the in-memory `data` under the lock, clones the
    /// snapshot and path, drops the guard, and hands the blocking work to
    /// `spawn_blocking` so it runs on a dedicated thread and never pins the
    /// executor.
    async fn persist_off_runtime(
        &self,
        path: PathBuf,
        data: SessionData,
        blob_store: BlobStore,
    ) -> Result<(), String> {
        // Every mutator calls this immediately after releasing `state` and
        // awaits the result. The FIFO gate therefore preserves mutation order
        // while the actual fsync-heavy work remains off the async executor.
        let _persist_guard = self.persist_gate.lock().await;
        // `spawn_blocking` is the right primitive: this is real blocking I/O,
        // not async work. `BlockingError` only occurs at runtime shutdown, in
        // which case the session is tearing down anyway — surface it as a
        // plain error.
        //
        // Log compaction runs *before* the snapshot write when the append-only
        // log has grown past its threshold and the snapshot has fully folded
        // it. Compacting first means the snapshot's `applied_seq` watermark
        // (stamped from the log's high-water mark inside `write_session_file`)
        // ends up equal to the compacted seed's high-water mark, so the next
        // load replays an empty tail. The log stays bounded over the life of a
        // long session without ever dropping an event the snapshot has not
        // already absorbed.
        let log_path = path.with_extension("jsonl");
        tokio::task::spawn_blocking(move || {
            compact_log_if_needed(&log_path, &data)?;
            persist_to(&path, &data, &blob_store)
        })
        .await
        .map_err(|e| format!("session persist task failed: {e}"))?
    }

    /// Write `data` to `sessions_dir/<data.id>.json`. Used to materialise a
    /// session file for a snapshot that is not (or not yet) the pinned one —
    /// for example seeding an archived branch in tests.
    #[allow(dead_code)]
    fn persist_archive(&self, data: &SessionData) -> Result<(), String> {
        let path = self.sessions_dir.join(format!("{}.json", data.id));
        persist_to(&path, data, &self.blob_store)
    }

    /// Resolve `input` (a 4+ char hex id or prefix) to the full session id
    /// **and the file path** that holds it. Identity is matched against the
    /// `id` field stored inside each snapshot, not the filename, so a session
    /// pinned via [`SessionStore::for_path`] under an arbitrary name (e.g. a
    /// test's `session.json`, or a not-yet-migrated legacy active file) is
    /// found just as reliably as a canonical `sessions/<id>.json`. The active
    /// session is matched against its in-memory id first so a prefix of the
    /// current session resolves without touching disk.
    fn resolve_session(
        &self,
        input: &str,
        active: &SessionState,
    ) -> Result<(String, PathBuf), String> {
        if input.len() < 4
            || !input
                .chars()
                .all(|character| character.is_ascii_hexdigit() || character == '-')
        {
            return Err(format!(
                "Invalid session id prefix '{}'. Use at least 4 hexadecimal characters.",
                input
            ));
        }
        let mut matches: Vec<(String, PathBuf)> = Vec::new();
        if active.data.id.starts_with(input) {
            matches.push((active.data.id.clone(), active.path.clone()));
        }
        // Fast path: a session's filename *is* its id
        // (`sessions/<id>.json`, ADR-0018), so resolve a prefix by listing the
        // directory and matching file stems — no file is ever opened. This used
        // to read + fully deserialize every session's `SessionData` (the full
        // recursive transcript) on each `delete` / `open` just to compare an id
        // prefix; with hundreds of multi-megabyte snapshots that dominated the
        // delete latency and made the `/sessions` picker lag.
        if self.sessions_dir.exists() {
            for entry in fs::read_dir(&self.sessions_dir).map_err(|error| error.to_string())? {
                let entry = entry.map_err(|error| error.to_string())?;
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }
                // Prefer the filename stem — the authoritative id for every
                // snapshot written since ADR-0018. Legacy snapshots that carry
                // an `id` differing from their filename are still reachable
                // through the content-scan fallback below.
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if stem.starts_with(input) && !matches.iter().any(|(id, _)| id == stem) {
                    matches.push((stem.to_string(), path));
                }
            }
            // Content-scan fallback, only when no filename matched: a legacy
            // snapshot (pre-ADR-0018 active-pointer file) can store an `id` that
            // differs from its filename, so a user typing that id finds nothing
            // by filename. Falling back to an id-only deserialize — `SessionIdOnly`
            // skips every other field, so it never allocates the transcript —
            // keeps those rare sessions resolvable. The common case above never
            // opens a file, so a large project pays this only on a genuine miss.
            if matches.is_empty() {
                for entry in fs::read_dir(&self.sessions_dir).map_err(|error| error.to_string())? {
                    let entry = entry.map_err(|error| error.to_string())?;
                    let path = entry.path();
                    if path.extension().and_then(|s| s.to_str()) != Some("json") {
                        continue;
                    }
                    let Ok(content) = fs::read_to_string(&path) else {
                        continue;
                    };
                    let Ok(header) = serde_json::from_str::<SessionIdOnly>(&content) else {
                        continue;
                    };
                    if header.id.starts_with(input)
                        && !matches.iter().any(|(id, _)| id == &header.id)
                    {
                        matches.push((header.id, path));
                    }
                }
            }
        }
        match matches.as_slice() {
            [(id, path)] => Ok((id.clone(), path.clone())),
            [] => Err(format!("No session matches '{}'.", input)),
            _ => Err(format!(
                "Session prefix '{}' is ambiguous ({} matches).",
                input,
                matches.len()
            )),
        }
    }
}

/// Write `data` to `path`, creating its parent directory first.
fn persist_to(path: &Path, data: &SessionData, blob_store: &BlobStore) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    write_session_file(path, data, blob_store)
}

/// Once the append-only event log holds more than this many events it is
/// rewritten to a single seed derived from the session's current full
/// snapshot. Every call to `persist_off_runtime` writes a *full* snapshot of
/// the current state (the only non-full persist is `append_turn`'s
/// `Persist::None` mid-turn arm, which does not reach this path), so any event
/// the seed would supersede has already been folded into the snapshot about to
/// be written. Compaction can therefore never drop an unabsorbed event. The
/// seed keeps the replay tail short over a long-lived session: the rewrite is
/// `snapshot_to_events`, one line per non-empty field.
const LOG_COMPACTION_THRESHOLD: usize = 1024;

/// Compact the event log at `log_path` to a single seed when it has grown past
/// [`LOG_COMPACTION_THRESHOLD`]. The seed is derived from `data` via
/// `snapshot_to_events`, so the freshly-rewritten log's high-water mark is
/// picked up by the subsequent `write_session_file` (which stamps
/// `applied_seq` from the log) and the next load replays an empty tail. A no-op
/// when the log is small.
fn compact_log_if_needed(log_path: &Path, data: &SessionData) -> Result<(), String> {
    // Cheap stat-based size check first: avoid parsing a log that is under the
    // threshold on every persist. Events average well under 1 KiB of envelope
    // overhead but carry arbitrary message payloads, so a byte threshold would
    // be payload-dependent; counting lines needs a read regardless, so gate it
    // behind the metadata length so the common (small-log) case pays only a
    // `stat`.
    let len = match std::fs::metadata(log_path) {
        Ok(m) => m.len() as usize,
        Err(_) => return Ok(()),
    };
    if len < LOG_COMPACTION_THRESHOLD * 64 {
        return Ok(());
    }
    let log = EventLog::new(log_path.to_path_buf());
    let envelopes = log.load()?;
    if envelopes.len() < LOG_COMPACTION_THRESHOLD {
        return Ok(());
    }
    log.rewrite(snapshot_to_events(data))?;
    tracing::debug!(
        path = %log_path.display(),
        events = envelopes.len(),
        "compacted event log to a single seed"
    );
    Ok(())
}

/// Load the session for `path` from its event log when one exists; otherwise
/// import from the snapshot file (seeding a fresh log from it), or start from
/// an empty session when neither exists. This is the single load path shared
/// by [`SessionStore::for_path`] and [`SessionStore::open`], and it also
/// lazily seeds event logs for legacy archived snapshots that predate the
/// per-session log layout (ADR-0018).
///
/// Fast path (snapshot present, `applied_seq` watermark set, checksum ok):
/// deserialise the snapshot JSON and replay only log events with
/// `seq > applied_seq`. This is O(snapshot + tail), not O(snapshot + history).
/// The snapshot is written on every turn-boundary persist with its watermark
/// stamped to the log's high-water mark, so a clean close leaves an empty tail
/// and resume is a single JSON read. A crash mid-round (after `append_turn`
/// appended a `MessagesAppended` event but before the next `replace_messages`
/// rewrote the snapshot) leaves a short tail of at most a few events, replayed
/// in O(tail).
///
/// Fallbacks: a missing/corrupt snapshot, a snapshot without a watermark
/// (pre-C5 legacy), a checksum mismatch, or an event whose `Started` seq differs
/// from the snapshot's identity all drop to a full replay from the event log,
/// which is the authoritative source. If there is no log either, the snapshot
/// is imported and a fresh log is seeded from it.
fn load_or_seed(
    path: &Path,
    event_log_path: &Path,
    blob_store: &BlobStore,
    project_root: &Path,
) -> SessionData {
    let event_log = EventLog::new(event_log_path.to_path_buf());
    let log_seeded = !event_log.is_empty();

    // ── Fast path: snapshot + watermark + checksum-valid → replay only tail.
    // A corrupt snapshot or checksum mismatch must not surface as a hard error
    // (the log is authoritative), so any deserialise/verify failure falls
    // through to the full-replay path below rather than aborting the load.
    if log_seeded
        && let Ok(snapshot) = load_snapshot(path)
        && let Some(watermark) = snapshot.applied_seq
        && verify_checksum(&snapshot).is_ok()
    {
        let tail = event_log.load_since(Some(watermark)).unwrap_or_default();
        let mut data = snapshot;
        if !tail.is_empty() {
            apply_events(&mut data, &tail);
        }
        if let Err(error) = load_session_blobs(&mut data, blob_store) {
            tracing::warn!(error = %error, "could not load session blobs");
        }
        if data.schema_version < CURRENT_SCHEMA_VERSION {
            data = migrate_session_data(data);
        }
        return data;
    }

    // ── Full replay from the event log (authoritative). Integrity is
    // guaranteed by the replay itself: each event carries full snapshot
    // semantics, so re-deriving the state from the log is as trustworthy as
    // verifying a stored checksum. (Replay starts from `default()` whose
    // `checksum` is `None`, so a post-replay `verify_checksum` is a no-op
    // accept — it was already a no-op before this change, not a lost check.)
    if log_seeded
        && let Ok(envelopes) = event_log.load()
        && !envelopes.is_empty()
    {
        let mut data = SessionData::default();
        apply_events(&mut data, &envelopes);
        if let Err(error) = load_session_blobs(&mut data, blob_store) {
            tracing::warn!(error = %error, "could not load session blobs from event log");
        }
        if data.schema_version < CURRENT_SCHEMA_VERSION {
            data = migrate_session_data(data);
        }
        // The snapshot was missing/legacy/corrupt; rewrite it now so the next
        // load takes the fast path.
        let _ = persist_to(path, &data, blob_store);
        return data;
    }

    // ── No event log: import from the snapshot, or start fresh.
    let snapshot_existed = path.exists();
    let mut data = fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str::<SessionData>(&content).ok())
        .unwrap_or_else(|| {
            if path.exists() {
                // Unparseable snapshot (e.g. pre-rename field names — the
                // ADR-0120 no-alias policy): start fresh rather than
                // half-migrate, and say so loudly.
                tracing::warn!(
                    path = %path.display(),
                    "session snapshot failed to parse; starting a fresh session"
                );
            }
            SessionData {
                project_root: project_root.to_path_buf(),
                ..Default::default()
            }
        });
    if let Err(error) = load_session_blobs(&mut data, blob_store) {
        tracing::warn!(error = %error, "could not load session blobs from snapshot");
    }
    if let Err(error) = verify_checksum(&data) {
        tracing::warn!(path = %path.display(), error = %error, "session checksum failed");
    }
    if data.schema_version < CURRENT_SCHEMA_VERSION {
        data = migrate_session_data(data);
    }
    // Laziness invariant (ADR-0018): a brand-new session with no snapshot and
    // no messages is NOT seeded to disk here — opening a session and exiting
    // without content must leave no empty-session litter. Only when there was
    // a real snapshot to import (e.g. a legacy file) do we (re)write the log.
    // The in-memory `data` is returned regardless, so the store is usable.
    if snapshot_existed {
        let _ = event_log.rewrite(snapshot_to_events(&data));
    }
    data
}

/// Deserialise the snapshot JSON at `path` into a [`SessionData`], rehydrating
/// no blobs (the caller does that once after deciding which path produced the
/// data). Returns `Err` for a missing or unparseable file so the caller can
/// fall through to full replay.
fn load_snapshot(path: &Path) -> Result<SessionData, String> {
    let content = fs::read_to_string(path).map_err(|e| format!("could not read snapshot: {e}"))?;
    serde_json::from_str::<SessionData>(&content)
        .map_err(|e| format!("could not parse snapshot: {e}"))
}

/// A dormant session with armed scheduled work, discovered on disk by
/// [`sessions_with_armed_schedules`]: the session id and the project root it
/// belongs to (read from the snapshot itself — project bucket names are a
/// one-way hash, so the path cannot be recovered from the directory name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArmedSession {
    pub session_id: String,
    pub project_root: PathBuf,
}

/// Minimal projection of a session snapshot for autonomous-work discovery:
/// everything else (the huge `model_window` / `archived_transcript` arrays)
/// is skipped by the `RawValue` deferral, so scanning every session on disk
/// costs a header read per file, not a transcript decode.
#[derive(Default, Deserialize)]
struct ScheduleProbeHeader {
    id: String,
    #[serde(default)]
    project_root: PathBuf,
    #[serde(default)]
    scheduled_jobs: Vec<muta_contracts::ScheduledJob>,
}

/// Discover every persisted session (across all project buckets) that still
/// has armed `/schedule` jobs. The daemon calls this once at boot to rehost
/// autonomous sessions (ADR-0125) — the durable-schedule feature's contract
/// is "the prompt fires even if the daemon that armed it is gone", and a
/// schedule that stops firing because the daemon restarted breaks it.
///
/// Files that cannot be read or parsed are skipped silently: this is a
/// best-effort rehost scan, and a corrupt snapshot must not block the daemon
/// from starting (its session still lazy-resumes on attach, where the full
/// error surface applies).
pub fn sessions_with_armed_schedules() -> Vec<ArmedSession> {
    let projects_dir = paths::get().projects_dir();
    let mut found = Vec::new();
    let Ok(buckets) = fs::read_dir(&projects_dir) else {
        return found;
    };
    for bucket in buckets.flatten() {
        let sessions_dir = bucket.path().join("sessions");
        let Ok(entries) = fs::read_dir(&sessions_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|v| v.to_str()) != Some("json") {
                continue;
            }
            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(header) = serde_json::from_str::<ScheduleProbeHeader>(&content) else {
                continue;
            };
            if !header.scheduled_jobs.is_empty() {
                found.push(ArmedSession {
                    session_id: header.id,
                    project_root: header.project_root,
                });
            }
        }
    }
    found
}

/// Header-only view of a session snapshot, used by [`SessionStore::list`] to
/// populate the sessions picker without paying for a full [`SessionData`]
/// deserialize.
///
/// The message arrays (`model_window` / `archived_transcript`) are kept as
/// [`Box<RawValue>`] — serde validates their JSON structure and records the
/// byte range but defers the per-message deserialize. `list()` only needs the
/// array *length* and the *first user message's* `content`, so a full decode of
/// every message (content blobs, recursive runner `children`, tool calls,
/// provider meta, …) on every session file is pure waste. With hundreds of
/// multi-megabyte snapshots this was the dominant cost of opening `/sessions`
/// and the per-delete picker refresh (`build_sessions_overview`): each call
/// re-read and re-allocated the entire transcript of every session on disk.
/// `Box<RawValue>` keeps the byte ranges but skips the allocation, so the
/// picker scales with the *number* of sessions, not their total content size.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct SessionHeader {
    id: String,
    parent_id: Option<String>,
    #[serde(default)]
    fork_kind: muta_contracts::SessionForkKind,
    created_at: u64,
    updated_at: u64,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    model_window: Vec<Box<RawValue>>,
    #[serde(default)]
    archived_transcript: Vec<Box<RawValue>>,
}

/// The fields `list()` decodes out of a deferred message body: the `role` and
/// `content` (for the picker preview), plus `origin` so a non-driving command
/// echo (slash commands, `!shell` passthroughs — ADR-0050) can be excluded from
/// the preview. Every other field is skipped, so a large transcript contributes
/// a few bytes of allocation instead of a full message tree. `origin` is
/// `#[serde(default)]`-optional and was absent from legacy on-disk snapshots,
/// so adding it is backward-compatible.
#[derive(Default, Deserialize)]
struct MessagePreview {
    role: Option<muta_contracts::Role>,
    #[serde(default)]
    content: String,
    #[serde(default)]
    origin: Option<muta_contracts::InjectionOrigin>,
}

/// Id-only projection of a session snapshot, used by the
/// [`SessionStore::resolve_session`] content-scan fallback for legacy files
/// whose stored `id` does not match their filename. `#[serde(default)]` plus
/// ignored unknown fields means every other top-level key — including the huge
/// `model_window` / `archived_transcript` arrays — is *skipped* rather than
/// decoded, so this never allocates the transcript. It still walks the bytes to
/// balance braces, but that is cheap relative to a full `SessionData` decode.
#[derive(Default, Deserialize)]
struct SessionIdOnly {
    #[serde(default)]
    id: String,
}

fn summary_header(data: &SessionHeader, active: bool) -> SessionSummary {
    // Lineage: the recorded kind wins; a legacy file that carries a
    // `parent_id` but predates `fork_kind` (serialized as the default
    // `Trunk`) degrades to `Fork` so its branch relationship is not lost.
    let fork_kind = match (data.fork_kind, data.parent_id.as_ref()) {
        (muta_contracts::SessionForkKind::Trunk, Some(_)) => muta_contracts::SessionForkKind::Fork,
        (kind, _) => kind,
    };
    SessionSummary {
        id: data.id.clone(),
        parent_id: data.parent_id.clone(),
        fork_kind,
        message_count: data.model_window.len() + data.archived_transcript.len(),
        updated_at: data.updated_at,
        created_at: data.created_at,
        overview: session_overview_header(data),
        active,
    }
}

fn session_overview_header(data: &SessionHeader) -> String {
    const MAX: usize = 64;
    if let Some(title) = data.title.as_deref().filter(|t| !t.trim().is_empty()) {
        return truncate_preview(title, MAX);
    }
    // Show the LAST effective user prompt (the freshest real turn, excluding
    // non-driving command echoes — see [`last_effective_prompt`]). Truncated to
    // the picker-row budget; the full text is available via
    // [`SessionStore::detail`] for the session-info sub-view.
    match last_effective_prompt(data) {
        Some(content) => truncate_preview(&content, MAX),
        None => "(empty session)".to_string(),
    }
}

/// The complete, untruncated text of the last effective user prompt — the most
/// recent user turn that is not a non-driving command echo (slash command /
/// `!shell` passthrough, ADR-0050). Shared by the picker preview (truncated
/// there) and the on-demand [`SessionStore::detail`] (returned in full). Uses
/// the deferred header parse, decoding only candidate bodies lazily.
fn last_effective_prompt(data: &SessionHeader) -> Option<String> {
    data.model_window
        .iter()
        .rev()
        .chain(data.archived_transcript.iter().rev())
        .find_map(|raw| {
            let preview = serde_json::from_str::<MessagePreview>(raw.get()).ok()?;
            let is_echo = preview
                .origin
                .as_ref()
                .is_some_and(|o| o.kind == InjectionKind::CommandEcho);
            (preview.role == Some(Role::User) && !is_echo).then_some(preview.content)
        })
}

fn truncate_preview(text: &str, max: usize) -> String {
    // Flatten to one line: control chars (newlines, tabs, …) would otherwise
    // survive into the picker row, where the terminal paints a `\n`/`\r` as a
    // carriage return and spills the row out the left edge of the modal.
    let text: String = text
        .trim()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let text = text.trim();
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max {
        return text.to_string();
    }
    let head: String = chars.into_iter().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

pub(crate) fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub struct ContextProjectionResult {
    pub model_window: Vec<Message>,
    pub archived_originals: Vec<Message>,
    pub checkpoint: ContextProjectionCheckpoint,
}

/// Header prepended to every compaction checkpoint message. Doubles as the
/// classifier that excludes checkpoints from the user-round count and lets a
/// later compaction extract the previous summary for incremental updates.
const CHECKPOINT_HEADER: &str = "[Conversation checkpoint]\n\
     Earlier complete rounds were compacted. Treat this as durable context, not a new user request.\n\n";

/// Per-message excerpt cap used by the deterministic excerpt fallback.
const EXCERPT_CAP_TOKENS: usize = 375;

pub struct CompactionSelection {
    /// Older complete rounds moved out of the model-visible window.
    pub archived: Vec<Message>,
    /// Recent rounds preserved verbatim after the checkpoint.
    pub tail: Vec<Message>,
    /// Body of a prior checkpoint message, when present, fed forward as the
    /// anchored summary so each compaction updates rather than restarts.
    pub previous_summary: Option<String>,
}

/// Split a message list into the archived head and the verbatim tail. Returns
/// `None` when there are not enough complete user rounds to compact.
pub fn select_compaction(
    messages: &[Message],
    preserve_rounds: usize,
) -> Option<CompactionSelection> {
    let user_indices = messages
        .iter()
        .enumerate()
        .filter(|(_, message)| {
            message.role == Role::User
                && !message.content.starts_with("[Conversation checkpoint]")
                // Non-driving command echoes are recorded as Role::User for
                // resume/audit faithfulness but are not real rounds; exclude
                // them so they don't inflate the round count and skew which
                // rounds compaction preserves (ADR-0050).
                && !message.is_command_echo()
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if user_indices.len() <= preserve_rounds {
        return None;
    }

    let keep_from = user_indices[user_indices.len() - preserve_rounds];
    let archived = messages[..keep_from]
        .iter()
        .filter(|message| message.role != Role::System)
        .cloned()
        .collect::<Vec<_>>();
    if archived.is_empty() {
        return None;
    }
    let tail = messages[keep_from..].to_vec();

    // A prior checkpoint message (hidden user, `[Conversation checkpoint]`
    // prefix) carries the previous summary; surface it for incremental updates.
    let previous_summary = messages.iter().rev().find_map(|message| {
        if message.role == Role::User
            && message.hidden
            && message.content.starts_with("[Conversation checkpoint]")
        {
            message
                .content
                .strip_prefix(CHECKPOINT_HEADER)
                .map(|body| body.trim().to_string())
                .filter(|body| !body.is_empty())
        } else {
            None
        }
    });

    Some(CompactionSelection {
        archived,
        tail,
        previous_summary,
    })
}

/// Choose the deepest coherent compaction that leaves room for the checkpoint
/// summary inside the configured working-memory target. The configured number
/// of preserved rounds is still preferred, but on a large-context model it must
/// not make the absolute active-window ceiling ineffective.
fn select_compaction_for_target(
    messages: &[Message],
    preserve_rounds: usize,
    target_tokens: usize,
) -> Option<CompactionSelection> {
    let complete_rounds = messages
        .iter()
        .filter(|message| {
            message.role == Role::User
                && !message.content.starts_with("[Conversation checkpoint]")
                && !message.is_command_echo()
        })
        .count();
    // Keep the current/latest real round verbatim. If that one round alone is
    // enormous it can exceed a soft target, but the projection never silently
    // truncates the user's current request.
    let maximum = preserve_rounds
        .min(complete_rounds.saturating_sub(1))
        .max(1);
    let tail_budget = target_tokens.saturating_mul(3) / 4;
    let mut fallback = None;
    for rounds in (1..=maximum).rev() {
        let selection = select_compaction(messages, rounds)?;
        if estimate_tokens(&selection.tail) <= tail_budget {
            return Some(selection);
        }
        fallback = Some(selection);
    }
    fallback
}

/// Allocate the remaining working-memory budget to the checkpoint after the
/// verbatim tail is accounted for. A small floor preserves a useful task state
/// even when a recent tail is unusually large.
fn summary_token_budget(target_tokens: usize, tail: &[Message]) -> usize {
    target_tokens
        .saturating_sub(estimate_tokens(tail))
        .max(2_000)
}

/// Token budget for the compaction summary, derived from the post-compaction
/// token target (ADR-0120: token-native; the old `target × 4` char budget was
/// then binary-searched back into tokens — a pure-loss round trip). Bounded
/// so huge windows do not produce enormous summaries and tiny windows still
/// get a useful digest.
fn summary_token_budget_clamped(target_tokens: usize) -> usize {
    target_tokens.clamp(2_000, 24_000)
}

fn label_for(role: Role) -> Option<&'static str> {
    match role {
        Role::User => Some("User"),
        Role::Assistant => Some("Assistant"),
        Role::Tool => Some("Tool"),
        Role::System => None,
    }
}

/// Build a checkpoint message wrapping `summary` with the durable header.
pub fn checkpoint_message(summary: &str) -> Message {
    Message::injected(
        Role::User,
        format!("{CHECKPOINT_HEADER}{summary}"),
        InjectionOrigin::new(InjectionKind::CompactionCheckpoint),
    )
}

/// Assemble the final [`ContextProjectionResult`] from a selection and a summary.
pub fn build_compaction_result(
    window_tokens_before: usize,
    selection: CompactionSelection,
    summary: String,
) -> ContextProjectionResult {
    let CompactionSelection { archived, tail, .. } = selection;
    let mut model_window = Vec::with_capacity(tail.len() + 1);
    model_window.push(checkpoint_message(&summary));
    model_window.extend(tail);
    let window_tokens_after = estimate_tokens(&model_window);
    ContextProjectionResult {
        checkpoint: ContextProjectionCheckpoint {
            operation: ContextProjectionKind::Compact,
            archived_messages: archived.len(),
            active_messages: model_window.len(),
            window_tokens_before,
            window_tokens_after,
        },
        model_window,
        archived_originals: archived,
    }
}

/// Deterministic excerpt fallback used when no provider is available or the
/// LLM summarization call fails. Budget is allocated **newest-first** so recent
/// context is never crowded out by older verbose messages; selected excerpts
/// are then emitted in chronological order for readability. When a previous
/// summary exists it is carried forward as anchored context. The budget and
/// per-message caps are **tokens** (ADR-0120); every cut lands on an exact
/// token boundary.
pub fn build_excerpt_summary(
    archived: &[Message],
    max_tokens: usize,
    previous_summary: Option<&str>,
) -> String {
    // Pass 1 (newest-first): pick which messages fit the remaining budget.
    let mut used = 0usize;
    let mut chosen: Vec<usize> = Vec::new();
    for (index, message) in archived.iter().enumerate().rev() {
        let Some(label) = label_for(message.role) else {
            continue;
        };
        let content = message.content.trim();
        if content.is_empty() {
            continue;
        }
        let remaining = max_tokens.saturating_sub(used);
        if remaining < 16 {
            break;
        }
        let cost = muta_contracts::tokenizer::count_tokens(content).min(EXCERPT_CAP_TOKENS)
            + muta_contracts::tokenizer::count_tokens(label)
            + 2;
        used += cost;
        chosen.push(index);
    }
    chosen.reverse(); // chronological

    // Pass 2: render the chosen messages in order, hard-truncating each.
    let mut output = String::new();
    for index in chosen {
        let message = &archived[index];
        // Skip roles without a render label (e.g. System). Pass 1 above already
        // filters these out of `chosen`, so this is defensive — but it keeps the
        // two passes consistent and avoids a panic if the selection ever diverges.
        let Some(label) = label_for(message.role) else {
            continue;
        };
        let content = message.content.trim();
        let remaining = max_tokens.saturating_sub(muta_contracts::tokenizer::count_tokens(&output));
        if remaining < 16 {
            break;
        }
        let excerpt = muta_contracts::tokenizer::truncate_str_to_tokens(
            content,
            remaining.min(EXCERPT_CAP_TOKENS),
        );
        output.push_str(label);
        output.push_str(": ");
        output.push_str(excerpt);
        output.push_str("\n\n");
    }
    let history = output.trim_end().to_string();

    if let Some(previous) = previous_summary.map(str::trim).filter(|s| !s.is_empty()) {
        let previous_budget = (max_tokens / 4).clamp(125, 1_000);
        let previous_excerpt =
            muta_contracts::tokenizer::truncate_str_to_tokens(previous, previous_budget);
        format!("[Previous summary]\n{previous_excerpt}\n\n[Recent history]\n{history}")
    } else {
        history
    }
}

/// Pure, provider-less compaction using the deterministic excerpt fallback.
/// Kept as a testable building block and as the ultimate fallback when LLM
/// summarization is disabled or unavailable.
#[allow(dead_code)]
pub fn compact_messages(
    messages: &[Message],
    target_tokens: usize,
    preserve_rounds: usize,
) -> Option<ContextProjectionResult> {
    let window_tokens_before = estimate_tokens(messages);
    let selection = select_compaction_for_target(messages, preserve_rounds, target_tokens)?;
    let summary_tokens = summary_token_budget(target_tokens, &selection.tail);
    let excerpt_budget = summary_token_budget_clamped(summary_tokens);
    let summary = truncate_summary_to_token_budget(
        build_excerpt_summary(
            &selection.archived,
            excerpt_budget,
            selection.previous_summary.as_deref(),
        ),
        summary_tokens,
    );
    Some(build_compaction_result(
        window_tokens_before,
        selection,
        summary,
    ))
}

// ---------------------------------------------------------------------------
// LLM-based summarization
// ---------------------------------------------------------------------------

const SUMMARIZATION_SYSTEM_PROMPT: &str = "\
You are an anchored context summarization assistant for coding sessions.\n\
Summarize only the conversation history you are given. The newest rounds may be \
kept verbatim outside your summary, so focus on the older context that still \
matters for continuing the work.\n\
If a <previous-summary> block is included, treat it as the current anchored \
summary: preserve still-true details, remove stale details, and merge in new \
facts.\n\
Always follow the exact output structure requested. Keep every section, \
preserve exact file paths and identifiers when known, and prefer terse bullets \
over paragraphs.\n\
Do not answer the conversation itself. Do not mention that you are summarizing \
or compacting. Respond in the same language as the conversation.";

const SUMMARY_TEMPLATE: &str = "\
Output exactly the Markdown structure shown inside <template> and keep the \
section order unchanged. Do not include the <template> tags in your response.\n\
<template>\n\
## Objective\n\
- [single-sentence task summary]\n\
\n\
## Constraints & Preferences\n\
- [user constraints, preferences, specs, or \"(none)\"]\n\
\n\
## Progress\n\
### Done\n\
- [completed work or \"(none)\"]\n\
\n\
### In Progress\n\
- [current work or \"(none)\"]\n\
\n\
### Blocked\n\
- [blockers or \"(none)\"]\n\
\n\
## Key Decisions\n\
- [decision and why, or \"(none)\"]\n\
\n\
## Next Steps\n\
- [ordered next actions or \"(none)\"]\n\
\n\
## Critical Context\n\
- [important technical facts, errors, open questions, or \"(none)\"]\n\
\n\
## Relevant Files\n\
- [file or directory path: why it matters, or \"(none)\"]\n\
</template>\n\
\n\
Rules:\n\
- Keep every section, even when empty.\n\
- Use terse bullets, not prose paragraphs.\n\
- Preserve exact file paths, commands, error strings, and identifiers when known.\n\
- Do not mention the summary process or that context was compacted.";

/// Token cap applied to each tool-result when serializing history for the
/// summarizer (ADR-0120).
const SUMMARY_TOOL_OUTPUT_CAP_TOKENS: usize = 375;

/// Render `archived` as a readable transcript for the summarizer, capping tool
/// outputs and dropping the oldest messages when the result exceeds `budget`.
pub fn serialize_for_summary(archived: &[Message], budget: usize) -> String {
    let mut lines: Vec<String> = Vec::new();
    for message in archived {
        let Some(label) = label_for(message.role) else {
            continue;
        };
        let mut body = message.content.trim().to_string();
        if let Some(calls) = &message.tool_calls {
            for call in calls {
                body.push_str(&format!("\n[tool call: {}({})]", call.name, call.arguments));
            }
        }
        if message.role == Role::Tool {
            body = muta_contracts::tokenizer::truncate_str_to_tokens(
                body.trim(),
                SUMMARY_TOOL_OUTPUT_CAP_TOKENS,
            )
            .to_string();
        }
        // Runner transcripts: render a bounded view of the nested work so
        // the summarizer can capture what each `task` call actually did
        // (otherwise the LLM only sees "[task result]:\n<final text>" and
        // cannot decide whether the runner's tool usage is worth mentioning
        // in the anchored summary). The nested view is hard-capped to avoid
        // blowing the budget on a single runner that ran for 30 turns.
        if let Some(children) = &message.children
            && !children.is_empty()
        {
            let nested = serialize_runner_transcript_for_summary(children, SUMMARY_RUNNER_CAP_TOKENS);
            if !nested.is_empty() {
                body.push_str("\n[runner transcript]\n");
                body.push_str(&nested);
            }
        }
        if body.trim().is_empty() {
            continue;
        }
        lines.push(format!("{label}: {body}"));
    }

    let joined = lines.join("\n\n");
    if muta_contracts::tokenizer::count_tokens(&joined) <= budget {
        return joined;
    }

    // Over budget: keep the most recent lines that fit (token budgets).
    let mut kept: Vec<&String> = Vec::new();
    let mut total = 0usize;
    for line in lines.iter().rev() {
        let cost = muta_contracts::tokenizer::count_tokens(line) + 2;
        if total + cost > budget {
            break;
        }
        total += cost;
        kept.push(line);
    }
    kept.reverse();
    let kept_str: Vec<&str> = kept.iter().map(|s| s.as_str()).collect();
    format!(
        "...[earlier history omitted]...\n\n{}",
        kept_str.join("\n\n")
    )
}

/// Per-runner token cap when rendering the nested transcript into the
/// summarizer prompt (ADR-0120). Large enough to surface the runner's task,
/// its key tool calls, and its conclusion; small enough that a turn with
/// five runners cannot crowd out the rest of the conversation.
const SUMMARY_RUNNER_CAP_TOKENS: usize = 500;


/// Render an runner's nested transcript as a compact summarizer-facing view.
/// Recursive: an runner's own `task` results (sub-runners) are rendered
/// one level deeper with an even smaller cap. Depth is bounded in practice by
/// the `RunnerTool` excluding itself from the sub-toolset.
fn serialize_runner_transcript_for_summary(children: &[Message], budget: usize) -> String {
    let mut lines: Vec<String> = Vec::new();
    for message in children {
        let Some(label) = label_for(message.role) else {
            continue;
        };
        let mut body = message.content.trim().to_string();
        if let Some(calls) = &message.tool_calls {
            for call in calls {
                body.push_str(&format!("\n[tool call: {}({})]", call.name, call.arguments));
            }
        }
        if message.role == Role::Tool {
            body = muta_contracts::tokenizer::truncate_str_to_tokens(
                body.trim(),
                SUMMARY_TOOL_OUTPUT_CAP_TOKENS,
            )
            .to_string();
        }
        // One level deeper, with a much smaller cap, so we never spend more
        // than ~25% of the parent runner's budget on a single sub-runner.
        if let Some(nested) = &message.children
            && !nested.is_empty()
        {
            let inner = serialize_runner_transcript_for_summary(nested, (budget / 4).max(125));
            if !inner.is_empty() {
                body.push_str("\n[sub-runner transcript]\n");
                body.push_str(&inner);
            }
        }
        if body.trim().is_empty() {
            continue;
        }
        lines.push(format!("  {label}: {body}"));
    }
    let joined = lines.join("\n");
    if muta_contracts::tokenizer::count_tokens(&joined) <= budget {
        joined
    } else {
        format!(
            "{}...[truncated]",
            muta_contracts::tokenizer::truncate_str_to_tokens(&joined, budget)
        )
    }
}

fn build_summarization_user_prompt(
    transcript: &str,
    previous_summary: Option<&str>,
    extra_context: &[String],
) -> String {
    let mut parts = Vec::new();
    match previous_summary.map(str::trim).filter(|s| !s.is_empty()) {
        Some(previous) => parts.push(format!(
            "Update the anchored summary below using the conversation history that \
             follows. Preserve still-true details, remove stale details, and merge in \
             new facts.\n<previous-summary>\n{previous}\n</previous-summary>"
        )),
        None => parts
            .push("Create a new anchored summary from the conversation history below.".to_string()),
    }
    parts.push(SUMMARY_TEMPLATE.to_string());
    for context in extra_context {
        let context = context.trim();
        if !context.is_empty() {
            parts.push(context.to_string());
        }
    }
    parts.push(format!("Conversation history:\n{transcript}"));
    parts.join("\n\n")
}

/// Ask `provider` to summarize `archived`. Returns the summary text, or an
/// error that the caller maps to the deterministic excerpt fallback.
pub async fn summarize_with_provider(
    provider: &Arc<dyn Provider>,
    archived: &[Message],
    previous_summary: Option<&str>,
    extra_context: &[String],
    budget: usize,
) -> Result<String, String> {
    let transcript = serialize_for_summary(archived, budget);
    let user_prompt = build_summarization_user_prompt(&transcript, previous_summary, extra_context);
    let messages = vec![
        Message::new(Role::System, SUMMARIZATION_SYSTEM_PROMPT),
        Message::new(Role::User, user_prompt),
    ];
    // Bound the summarization call so a stalled or overloaded provider
    // triggers the excerpt fallback instead of hanging the turn (and the
    // entire frontend) forever. Two minutes is generous for a single
    // summarization response.
    const SUMMARIZATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
    let response = match tokio::time::timeout(
        SUMMARIZATION_TIMEOUT,
        provider.chat(muta_contracts::ModelRequest::new(messages)),
    )
    .await
    {
        Ok(result) => result?,
        Err(_elapsed) => {
            return Err(format!(
                "Summarization timed out after {} seconds; using excerpt fallback.",
                SUMMARIZATION_TIMEOUT.as_secs()
            ));
        }
    };
    let summary = response.content.trim().to_string();
    if summary.is_empty() {
        return Err("Summarization returned an empty summary.".to_string());
    }
    Ok(summary)
}

// ---------------------------------------------------------------------------
// Compaction orchestrator
// ---------------------------------------------------------------------------

/// Run a compaction over `history` in place.
///
/// When `provider` is `Some`, an LLM produces an anchored structured summary
/// (with the previous summary carried forward for incremental updates); on any
/// failure it falls back to the deterministic excerpt summary. When `provider`
/// is `None`, the excerpt summary is used directly.
pub async fn run_compaction(
    history: &mut Vec<Message>,
    target_tokens: usize,
    preserve_rounds: usize,
    provider: Option<Arc<dyn Provider>>,
    extra_context: Vec<String>,
) -> Result<Option<ContextProjectionResult>, String> {
    let window_tokens_before = estimate_tokens(history);
    let Some(selection) = select_compaction_for_target(history, preserve_rounds, target_tokens)
    else {
        return Ok(None);
    };

    let summary_tokens = summary_token_budget(target_tokens, &selection.tail);
    let transcript_budget = summary_token_budget_clamped(summary_tokens);
    let summary = match provider.as_ref() {
        Some(provider) => {
            match summarize_with_provider(
                provider,
                &selection.archived,
                selection.previous_summary.as_deref(),
                &extra_context,
                transcript_budget,
            )
            .await
            {
                Ok(text) => text,
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "LLM summarization failed; falling back to excerpt compaction"
                    );
                    build_excerpt_summary(
                        &selection.archived,
                        transcript_budget,
                        selection.previous_summary.as_deref(),
                    )
                }
            }
        }
        None => build_excerpt_summary(
            &selection.archived,
            transcript_budget,
            selection.previous_summary.as_deref(),
        ),
    };

    let summary = truncate_summary_to_token_budget(summary, summary_tokens);
    let result = build_compaction_result(window_tokens_before, selection, summary);
    tracing::debug!(
        window_tokens_before,
        window_tokens_after = result.checkpoint.window_tokens_after,
        "compaction complete"
    );
    let model_window = result.model_window.clone();
    *history = model_window;
    Ok(Some(result))
}

/// Enforce the allocated checkpoint budget even when a summarizing provider
/// ignores its requested length. Now a thin wrapper over the exact
/// token-boundary cut ([`muta_contracts::tokenizer::truncate_to_tokens`]);
/// the old binary search existed only because the budget round-tripped
/// through characters (ADR-0120 removed that).
fn truncate_summary_to_token_budget(text: String, max_tokens: usize) -> String {
    let (prefix, _) = muta_contracts::tokenizer::truncate_to_tokens(&text, max_tokens);
    prefix.trim_end().to_string()
}

/// Diagnostic scan of stored session files. When `project_root` is `None`
/// every project bucket is inspected; when supplied only that project's bucket
/// is checked. Prints one line per file and a summary.
pub async fn run_doctor(project_root: Option<&std::path::Path>) -> Result<(), String> {
    struct Report {
        examined: usize,
        corrupt: usize,
    }

    impl Report {
        fn record(&mut self, path: &std::path::Path, result: Result<&SessionData, String>) {
            self.examined += 1;
            match result {
                Ok(data) => {
                    let message_count = data.model_window.len() + data.archived_transcript.len();
                    println!(
                        "ok       {} (schema {}, checksum={}, {} messages)",
                        path.display(),
                        data.schema_version,
                        data.checksum
                            .map(|c| format!("{:#010x}", c))
                            .unwrap_or_else(|| "none".to_string()),
                        message_count
                    );
                }
                Err(error) => {
                    self.corrupt += 1;
                    println!("corrupt  {}: {}", path.display(), error);
                }
            }
        }
    }

    fn inspect(path: &std::path::Path, report: &mut Report) {
        let raw = match fs::read_to_string(path) {
            Ok(r) => r,
            Err(error) => {
                report.record(path, Err(error.to_string()));
                return;
            }
        };
        let result = serde_json::from_str::<SessionData>(&raw)
            .map_err(|error| error.to_string())
            .and_then(|data| verify_checksum(&data).map(|_| data));
        match result {
            Ok(data) => report.record(path, Ok(&data)),
            Err(error) => report.record(path, Err(error)),
        }
    }

    fn scan_bucket(path: &std::path::Path, report: &mut Report) {
        // ADR-0018: every session lives under `sessions/<id>.json` with its
        // matching `<id>.jsonl` log. A stray root `session.json` (left by an
        // older layout) is still reported so the operator can spot it.
        let legacy_active = path.join("session.json");
        if legacy_active.exists() {
            inspect(&legacy_active, report);
        }
        let sessions_dir = path.join("sessions");
        if let Ok(entries) = fs::read_dir(&sessions_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }
                inspect(&path, report);
                // Verify the matching event log exists; flag its absence as a
                // soft note rather than corruption (it will be seeded on open).
                let log = path.with_extension("jsonl");
                if !log.exists() {
                    println!("note     {} (no event log; seeded on open)", log.display());
                }
            }
        }
    }

    let dirs = paths::get();
    let mut report = Report {
        examined: 0,
        corrupt: 0,
    };

    if let Some(root) = project_root {
        scan_bucket(&dirs.project_dir(root), &mut report);
    } else {
        if let Ok(entries) = fs::read_dir(dirs.projects_dir()) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    scan_bucket(&path, &mut report);
                }
            }
        }
    }

    println!("---");
    println!("examined: {}, corrupt: {}", report.examined, report.corrupt);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use muta_contracts::async_trait;

    /// Tests that touch process-global state (`paths::set_test_default` or
    /// process env vars) cannot run in parallel. We serialise them through
    /// this lock; pure-computation tests skip the guard.
    ///
    /// Note: this lock alone does **not** synchronise with `config`'s tests —
    /// both modules independently touched the shared `paths::set_test_default`
    /// through their own private locks, which raced. So the macro also acquires
    /// the crate-wide `paths::TEST_OVERRIDE_GUARD`, the single lock every
    /// override-touching test in the crate funnels through.
    static GLOBAL_GUARD: std::sync::LazyLock<tokio::sync::Mutex<()>> =
        std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

    macro_rules! locked {
        ($body:block) => {{
            let _override_guard = $crate::paths::TEST_OVERRIDE_GUARD
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let _guard = GLOBAL_GUARD.lock().await;
            $body
        }};
    }

    struct CompactionProvider;

    #[async_trait]
    impl Provider for CompactionProvider {
        async fn chat(&self, _request: muta_contracts::ModelRequest) -> Result<Message, String> {
            Ok(Message::new(Role::Assistant, "mock AI summary"))
        }

        async fn stream_chat(
            &self,
            _request: muta_contracts::ModelRequest,
        ) -> Result<futures::stream::BoxStream<'static, Result<String, String>>, String> {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    #[tokio::test]
    async fn session_data_round_trips() {
        let directory =
            std::env::temp_dir().join(format!("muta-host-test-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        let messages = vec![Message::new(muta_contracts::Role::User, "hello")];
        store.replace_messages(messages.clone()).await.unwrap();

        let data: SessionData = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(data.model_window[0].content, messages[0].content);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn schema_version_defaults_to_current_and_serialises() {
        let data = SessionData::default();
        assert_eq!(data.schema_version, CURRENT_SCHEMA_VERSION);
        let raw = serde_json::to_string(&data).unwrap();
        assert!(raw.contains("\"schema_version\":"));
    }

    /// ADR-0125 boot rehost scan: sessions with armed `/schedule` jobs are
    /// discoverable across every project bucket, and sessions without jobs
    /// are invisible to the scan. Writes two real snapshots through
    /// `SessionStore::load_for_project` under a sandboxed data dir (one
    /// session with an armed job, one without), then asserts the scan finds
    /// exactly the armed one with its project root.
    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    // The crate-wide `paths::TEST_OVERRIDE_GUARD` (a std Mutex) is held
    // across the awaited body intentionally to serialize the process-wide
    // paths override. The single-threaded test runtime cannot block a peer.
    async fn armed_schedule_scan_finds_only_sessions_with_jobs() {
        locked!({
            let sandbox =
                std::env::temp_dir().join(format!("muta-armed-scan-{}", uuid::Uuid::new_v4()));
            let data_dir = sandbox.join("data");
            let project = sandbox.join("my-project");
            fs::create_dir_all(&project).unwrap();
            fs::create_dir_all(&data_dir).unwrap();
            paths::set_test_default(Some(paths::Dirs::resolve(&paths::PathsOverride {
                data_dir: Some(data_dir.clone()),
                ..Default::default()
            })));

            // One project, two sessions: armed and unarmed. Both stores go
            // through `load_for_project` (the production path), which pins
            // the real project root into the snapshot.
            let armed = SessionStore::load_for_project(project.clone());
            armed
                .set_scheduled_jobs(vec![muta_contracts::ScheduledJob::once(
                    "j1".into(),
                    chrono::Utc::now() + chrono::Duration::hours(1),
                    "later".into(),
                    chrono::Utc::now(),
                )])
                .await
                .unwrap();
            // The store skips persisting "empty" sessions, so both sessions
            // need a message to reach disk.
            armed
                .replace_messages(vec![Message::new(muta_contracts::Role::User, "hi")])
                .await
                .unwrap();
            let unarmed = SessionStore::load_for_project(project.clone());
            unarmed
                .replace_messages(vec![Message::new(muta_contracts::Role::User, "hi")])
                .await
                .unwrap();

            let found = sessions_with_armed_schedules();
            assert_eq!(found.len(), 1, "only the armed session: {found:?}");
            assert_eq!(found[0].session_id, armed.id().await);
            assert_eq!(found[0].project_root, project.canonicalize().unwrap());

            let _ = fs::remove_dir_all(sandbox);
            paths::set_test_default(None);
        });
    }

    #[test]
    fn legacy_session_without_schema_version_loads_as_current() {
        let data: SessionData = serde_json::from_str(
            r#"{
                "id": "00000000-0000-0000-0000-000000000001",
                "messages": [],
                "archived_messages": [],
                "loop_checkpoint": null,
                "compaction": null
            }"#,
        )
        .unwrap();
        // Missing fields inherit the serde default, which is the current
        // schema version, so legacy files appear up-to-date.
        assert_eq!(data.schema_version, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn schema_migration_bumps_version() {
        let data = SessionData {
            schema_version: 0,
            ..SessionData::default()
        };
        let migrated = migrate_session_data(data);
        assert_eq!(migrated.schema_version, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn checksum_computes_and_verifies() {
        let mut data = SessionData::default();
        data.model_window = vec![Message::new(muta_contracts::Role::User, "hello")];
        data.checksum = Some(compute_checksum(&data).unwrap());
        assert!(verify_checksum(&data).is_ok());

        // Tamper with a field: verification must fail.
        data.model_window[0].content = "goodbye".to_string();
        assert!(verify_checksum(&data).is_err());
    }

    #[test]
    fn checksum_is_none_for_legacy_files() {
        let data: SessionData = serde_json::from_str(
            r#"{
                "id": "00000000-0000-0000-0000-000000000001",
                "messages": [],
                "archived_messages": [],
                "schema_version": 1
            }"#,
        )
        .unwrap();
        assert!(data.checksum.is_none());
        assert!(
            verify_checksum(&data).is_ok(),
            "missing checksum is allowed"
        );
    }

    #[tokio::test]
    async fn session_persists_runner_children_round_trip() {
        // End-to-end persistence contract: a session that contains a `task`
        // tool call must round-trip the runner's nested transcript through
        // session.json, so a subsequent `SessionStore::load_for_project` (the
        // production resume path) restores the children intact. Before Phase 3
        // children were silently dropped because `Message::children` did not
        // exist and the harness only persisted the textual summary.
        let directory =
            std::env::temp_dir().join(format!("muta-runner-persist-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());

        let call = muta_contracts::ToolCall {
            id: "call_sub1".to_string(),
            name: "runner".to_string(),
            arguments: r#"{"description":"d","prompt":"p"}"#.to_string(),
        };
        let assistant = Message::new(muta_contracts::Role::Assistant, "")
            .with_attribution("kimi-code", "kimi-k2.7-code");
        let assistant = Message {
            tool_calls: Some(vec![call.clone()]),
            ..assistant
        };
        let runner_transcript = vec![
            Message::new(muta_contracts::Role::User, "find foo"),
            Message::new(muta_contracts::Role::Assistant, "looking..."),
            Message::new(muta_contracts::Role::Assistant, "foo is at src/foo.rs"),
        ];
        let tool = Message::tool_result(&call, "[task result]:\nfoo is at src/foo.rs")
            .with_children(runner_transcript);
        store
            .replace_messages(vec![
                Message::new(muta_contracts::Role::User, "where is foo?"),
                assistant,
                tool,
            ])
            .await
            .unwrap();

        // Reload from disk as production code would.
        let loaded = fs::read_to_string(&path).unwrap();
        let data: SessionData = serde_json::from_str(&loaded).unwrap();
        let tool_msg = data
            .model_window
            .iter()
            .find(|m| m.role == muta_contracts::Role::Tool)
            .expect("tool result message persisted");
        let children = tool_msg.children.as_ref().expect("children persisted");
        assert_eq!(children.len(), 3);
        assert!(children.iter().any(|m| m.content == "find foo"));
        assert!(children.iter().any(|m| m.content.contains("src/foo.rs")));
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn legacy_single_session_snapshot_migrates_with_defaults() {
        let data: SessionData = serde_json::from_str(
            r#"{
                "id": "00000000-0000-0000-0000-000000000001",
                "messages": [],
                "archived_messages": [],
                "loop_checkpoint": null,
                "compaction": null
            }"#,
        )
        .unwrap();

        assert_eq!(data.parent_id, None);
        assert!(data.created_at > 0);
        assert!(data.updated_at > 0);
        // Phase 2: project_root defaults to current cwd for legacy snapshots
        // missing the field.
        assert!(!data.project_root.as_os_str().is_empty());
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    // The crate-wide `paths::TEST_OVERRIDE_GUARD` (a std Mutex) is acquired
    // inside `locked!` and held across the awaited body by design, so this
    // test serialises against the synchronous `config` override tests. The
    // single-threaded tokio test runtime means a held std guard cannot
    // deadlock a peer task.
    async fn load_for_project_isolates_sessions_per_cwd() {
        locked!({
            let root = std::env::temp_dir().join(format!("muta-proj-iso-{}", uuid::Uuid::new_v4()));
            let dirs = paths::Dirs::resolve(&paths::PathsOverride {
                home: Some(root.clone()),
                data_dir: Some(root.join("data")),
                state_dir: Some(root.join("state")),
                config_dir: Some(root.join("config")),
                cache_dir: Some(root.join("cache")),
            });
            dirs.ensure().unwrap();
            paths::set_test_default(Some(dirs.clone()));
            // Build two stores bound to different project roots.
            let store_a = SessionStore::load_for_project(PathBuf::from("/projects/alpha"));
            let store_b = SessionStore::load_for_project(PathBuf::from("/projects/beta"));

            store_a
                .replace_messages(vec![Message::new(Role::User, "alpha work")])
                .await
                .unwrap();
            store_b
                .replace_messages(vec![Message::new(Role::User, "beta work")])
                .await
                .unwrap();

            let bucket_a = crate::paths::project_bucket_name(&PathBuf::from("/projects/alpha"));
            let bucket_b = crate::paths::project_bucket_name(&PathBuf::from("/projects/beta"));
            assert_ne!(bucket_a, bucket_b);

            // ADR-0018: each instance pins its own `sessions/<id>.json`. There
            // is no longer a project-root `session.json`.
            let id_a = store_a.id().await;
            let id_b = store_b.id().await;
            assert!(
                dirs.project_sessions_dir(&PathBuf::from("/projects/alpha"))
                    .join(format!("{id_a}.json"))
                    .exists()
            );
            assert!(
                dirs.project_sessions_dir(&PathBuf::from("/projects/beta"))
                    .join(format!("{id_b}.json"))
                    .exists()
            );

            // Reloading alpha starts fresh but the prior session is resumable,
            // and alpha never sees beta's messages.
            let reloaded_a = SessionStore::load_for_project(PathBuf::from("/projects/alpha"));
            reloaded_a.resume(Some(&id_a)).await.unwrap();
            assert_eq!(reloaded_a.model_window().await[0].content, "alpha work");
            let reloaded_b = SessionStore::load_for_project(PathBuf::from("/projects/beta"));
            reloaded_b.resume(Some(&id_b)).await.unwrap();
            assert_eq!(reloaded_b.model_window().await[0].content, "beta work");

            // list() is scoped per project — alpha only sees its own session.
            let alpha_sessions = reloaded_a.list().await.unwrap();
            assert!(alpha_sessions.iter().all(|s| !s.overview.contains("beta")));
            let beta_sessions = reloaded_b.list().await.unwrap();
            assert!(beta_sessions.iter().all(|s| !s.overview.contains("alpha")));

            paths::set_test_default(None);
            let _ = std::fs::remove_dir_all(root);
        });
    }

    #[tokio::test]
    async fn fork_preserves_both_durable_branches() {
        let directory =
            std::env::temp_dir().join(format!("muta-host-fork-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        store
            .replace_messages(vec![Message::new(muta_contracts::Role::User, "parent")])
            .await
            .unwrap();
        let parent_id = store.id().await;

        let (fork_id, source_id) = store.fork().await.unwrap();
        assert_eq!(source_id, parent_id);
        assert_eq!(store.parent_id().await.as_deref(), Some(parent_id.as_str()));
        store
            .replace_messages(vec![Message::new(muta_contracts::Role::User, "fork")])
            .await
            .unwrap();

        let sessions = store.list().await.unwrap();
        assert_eq!(sessions.len(), 2);
        assert!(sessions.iter().any(|item| item.id == parent_id));
        assert!(
            sessions
                .iter()
                .any(|item| item.id == fork_id && item.active)
        );

        store.open(&parent_id[..8]).await.unwrap();
        assert_eq!(store.model_window().await[0].content, "parent");
        store.open(&fork_id[..8]).await.unwrap();
        assert_eq!(store.model_window().await[0].content, "fork");
        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn fork_lineage_is_stamped_and_surfaces_through_list() {
        // Lineage contract (dashboard fork surfacing): `fork` stamps
        // `Fork`, `fork_to_side` stamps `Aside`, a fresh session is `Trunk`,
        // and `list()` carries both the parent link and the kind so the
        // dashboard can group trunk + branches without guessing from ids.
        let directory = std::env::temp_dir().join(format!("muta-lineage-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        store
            .replace_messages(vec![Message::new(muta_contracts::Role::User, "parent")])
            .await
            .unwrap();
        let parent_id = store.id().await;

        // Trunk: the fresh parent has no lineage.
        let trunk = store
            .list()
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.id == parent_id)
            .unwrap();
        assert_eq!(trunk.fork_kind, muta_contracts::SessionForkKind::Trunk);
        assert!(trunk.parent_id.is_none());

        // Explicit fork: kind = Fork, parent recorded. Note that `fork`
        // repoints the store at the branch, so the trunk's row is the
        // *original* parent id and the branch's parent link names it.
        let (fork_id, _) = store.fork().await.unwrap();
        store
            .replace_messages(vec![Message::new(muta_contracts::Role::User, "branch")])
            .await
            .unwrap();
        let branch = store
            .list()
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.id == fork_id)
            .unwrap();
        assert_eq!(branch.fork_kind, muta_contracts::SessionForkKind::Fork);
        assert_eq!(branch.parent_id.as_deref(), Some(parent_id.as_str()));

        // Aside: kind = Aside. `fork_to_side` forks from the *current*
        // pointer — which after `fork()` is the branch — so the aside's
        // parent names the branch, not the trunk. That is the real
        // semantics: an aside inherits the conversation you are looking at.
        let (side_id, _) = store.fork_to_side().await.unwrap();
        let aside = store
            .list()
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.id == side_id)
            .unwrap();
        assert_eq!(aside.fork_kind, muta_contracts::SessionForkKind::Aside);
        assert_eq!(aside.parent_id.as_deref(), Some(fork_id.as_str()));

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn fork_to_side_leaves_primary_active_pointer_intact() {
        // ADR-0017: a side fork must NOT repoint the primary's active pointer.
        // The primary keeps its id, history, and (by construction) any in-flight
        // turn; only a self-contained sibling file is written.
        let directory =
            std::env::temp_dir().join(format!("muta-host-side-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        store
            .replace_messages(vec![Message::new(muta_contracts::Role::User, "parent")])
            .await
            .unwrap();
        let parent_id = store.id().await;

        let (side_id, source_id) = store.fork_to_side().await.unwrap();
        assert_eq!(source_id, parent_id);
        assert_ne!(side_id, parent_id);

        // The primary is untouched: same id, still holds "parent", and has no
        // parent link (it did not become a child).
        assert_eq!(store.id().await, parent_id);
        assert_eq!(store.model_window().await[0].content, "parent");
        assert!(store.parent_id().await.is_none());

        // The side loads into its own store with the inherited history and the
        // parent lineage recorded.
        let side = store.open_side(&side_id).await.unwrap();
        assert_eq!(side.id().await, side_id);
        assert_eq!(side.parent_id().await.as_deref(), Some(parent_id.as_str()));
        assert_eq!(side.model_window().await[0].content, "parent");

        // Writing to the side never reaches the primary.
        side.replace_messages(vec![Message::new(muta_contracts::Role::User, "side")])
            .await
            .unwrap();
        assert_eq!(store.model_window().await[0].content, "parent");

        // The side is independently resumable from disk (self-contained file).
        let reopened = store.open_side(&side_id).await.unwrap();
        assert_eq!(reopened.model_window().await[0].content, "side");

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn list_skips_active_session_when_it_has_no_content() {
        // Regression: a fresh empty active session used to be appended to
        // every `list()` call. Because `updated_at` is bumped on startup,
        // it sorted to the top and permanently showed "(empty session)".
        // The picker should only surface the active session once it has
        // real content (messages, archived messages, a loop checkpoint, or
        // a compaction marker).
        let directory =
            std::env::temp_dir().join(format!("muta-host-list-empty-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());

        // Seed one archived session so the picker has something to show,
        // then keep the active session empty (the default state).
        let archived = SessionData {
            project_root: directory.clone(),
            model_window: vec![Message::new(muta_contracts::Role::User, "archived branch")],
            ..Default::default()
        };
        store.persist_archive(&archived).unwrap();

        let sessions = store.list().await.unwrap();
        assert_eq!(
            sessions.len(),
            1,
            "empty active session must not appear in the list"
        );
        assert_eq!(sessions[0].id, archived.id);
        assert!(!sessions[0].active);

        // Once the active session gets content it should reappear, marked
        // active so the picker can badge it.
        store
            .replace_messages(vec![Message::new(
                muta_contracts::Role::User,
                "live branch",
            )])
            .await
            .unwrap();
        let sessions = store.list().await.unwrap();
        assert_eq!(sessions.len(), 2);
        assert!(sessions.iter().any(|item| item.active));
        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn list_filters_and_prunes_empty_sessions_on_disk() {
        let directory =
            std::env::temp_dir().join(format!("muta-prune-empty-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());

        // Create an empty session snapshot and event log directly on disk
        let empty_session_path = directory.join("empty-archived.json");
        let empty_session_jsonl = directory.join("empty-archived.jsonl");
        let empty_data = SessionData {
            id: "empty-archived".to_string(),
            project_root: directory.clone(),
            ..Default::default()
        };
        fs::write(
            &empty_session_path,
            serde_json::to_string(&empty_data).unwrap(),
        )
        .unwrap();
        fs::write(&empty_session_jsonl, "").unwrap();

        // And create a substantive session
        let substantive = SessionData {
            id: "real-session".to_string(),
            project_root: directory.clone(),
            model_window: vec![Message::new(muta_contracts::Role::User, "real dialogue")],
            ..Default::default()
        };
        store.persist_archive(&substantive).unwrap();

        let sessions = store.list().await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, "real-session");

        // The empty archived file should have been pruned from disk
        assert!(
            !empty_session_path.exists(),
            "empty session file on disk must be pruned"
        );
        assert!(
            !empty_session_jsonl.exists(),
            "empty session jsonl file on disk must be pruned"
        );

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn list_reads_overview_and_count_without_decoding_message_bodies() {
        // `list()` builds the `/sessions` picker rows. It must extract the
        // message count and the first-user-message overview *without* decoding
        // the full message bodies (which carry runner `children`, tool calls,
        // content blobs, …) — otherwise opening the picker or refreshing it
        // after a delete re-allocates the entire transcript of every session on
        // disk. The picker rows defer message bodies via `Box<RawValue>`; this
        // test pins that the deferred view still reports the right count and
        // overview for a heavy session, including a user turn buried under
        // assistant/tool output.
        let directory =
            std::env::temp_dir().join(format!("muta-list-deferred-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());

        // A user turn carrying nested runner children plus a tool result with a
        // large payload — the kind of content that made the old eager parse
        // expensive. The overview is the LAST effective user prompt ("nested
        // runner prompt"), not the System preamble and not the heavy payloads.
        let mut runner_child = Message::new(muta_contracts::Role::User, "nested runner prompt");
        runner_child.children = Some(vec![Message::new(
            muta_contracts::Role::Assistant,
            "runner reply",
        )]);
        let mut heavy_tool = Message::new(muta_contracts::Role::Tool, "x".repeat(50_000));
        heavy_tool.tool_call_id = Some("call_heavy".to_string());
        store
            .replace_messages(vec![
                Message::new(muta_contracts::Role::System, "system preamble"),
                Message::new(muta_contracts::Role::User, "the real first prompt"),
                Message::new(muta_contracts::Role::Assistant, "ack"),
                heavy_tool,
                runner_child,
            ])
            .await
            .unwrap();

        let sessions = store.list().await.unwrap();
        let row = sessions
            .iter()
            .find(|item| item.active)
            .expect("active session is listed");
        // Count comes from the array length, not the decoded bodies.
        assert_eq!(row.message_count, 5);
        // Overview is the LAST effective user prompt, not the System preamble
        // and not any of the heavy payloads.
        assert_eq!(row.overview, "nested runner prompt");

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn list_overview_excludes_command_echoes_and_picks_last_real_prompt() {
        // Regression: the overview is the most recent user turn that is *not* a
        // non-driving command echo (ADR-0050). A session whose final input was a
        // slash command (`/autopilot on`) or a shell passthrough must show its
        // last genuine prompt instead — those echoes are agent operations, not
        // AI-conversation turns. This must hold through the deferred header
        // parse (which decodes `origin` as well as role/content).
        let directory =
            std::env::temp_dir().join(format!("muta-list-echo-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        store
            .replace_messages(vec![
                Message::new(muta_contracts::Role::System, "system preamble"),
                Message::new(muta_contracts::Role::User, "first real prompt"),
                Message::new(muta_contracts::Role::Assistant, "reply"),
                // A genuine later prompt — should win as the freshest.
                Message::new(muta_contracts::Role::User, "second real prompt"),
                Message::new(muta_contracts::Role::Assistant, "reply 2"),
                // Then non-driving echoes that must NOT become the overview
                // even though they are the last user-role messages:
                Message::command_echo("/autopilot on"),
                Message::command_echo("/session open abc123"),
            ])
            .await
            .unwrap();

        let sessions = store.list().await.unwrap();
        let row = sessions.iter().find(|item| item.active).unwrap();
        assert_eq!(
            row.overview, "second real prompt",
            "command echoes are excluded; the last real prompt wins"
        );

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn list_uses_a_stored_title_in_preference_to_first_user_message() {
        // ADR-0022: a stored title (manual or AI) wins over the user-prompt
        // fallback. The deferred header parse still reads the top-level `title`
        // field, so this precedence must hold without decoding message bodies.
        let directory =
            std::env::temp_dir().join(format!("muta-list-title-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());

        store
            .replace_messages(vec![Message::new(
                muta_contracts::Role::User,
                "raw first prompt that should be hidden by the title",
            )])
            .await
            .unwrap();
        store
            .set_title(Some("Custom Title".to_string()), true)
            .await
            .unwrap();

        let sessions = store.list().await.unwrap();
        let row = sessions.iter().find(|item| item.active).unwrap();
        assert_eq!(row.overview, "Custom Title");

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn rename_live_session_sets_and_clears_the_manual_title() {
        // `RenameSession` on the active session delegates to `set_title`:
        // `Some` records a manual title (ADR-0022's lock: AI generation must
        // not overwrite it) that the picker row prefers over the first-prompt
        // preview; `None` clears the manual override so the overview falls
        // back to the AI-title / first-prompt preview.
        let directory =
            std::env::temp_dir().join(format!("muta-rename-live-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        store
            .replace_messages(vec![Message::new(
                muta_contracts::Role::User,
                "first prompt",
            )])
            .await
            .unwrap();
        let id = store.id().await;

        // Short-id prefix resolution, exactly like `delete`.
        store
            .rename(&id[..8], Some("manual name".to_string()))
            .await
            .unwrap();
        let (title, manual) = store.title().await;
        assert_eq!(title.as_deref(), Some("manual name"));
        assert!(manual, "a rename is a user-set (manual) title");
        let row = store
            .list()
            .await
            .unwrap()
            .into_iter()
            .find(|item| item.active)
            .unwrap();
        assert_eq!(row.overview, "manual name");

        // `None` clears the manual override: the stored title goes away, the
        // manual lock is released (AI generation may title it again), and the
        // picker row falls back to the first-prompt preview.
        store.rename(&id, None).await.unwrap();
        let (title, manual) = store.title().await;
        assert_eq!(title, None);
        assert!(!manual, "clearing releases the manual lock");
        let row = store
            .list()
            .await
            .unwrap()
            .into_iter()
            .find(|item| item.active)
            .unwrap();
        assert_eq!(row.overview, "first prompt");

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn rename_archived_session_persists_without_touching_the_active_one() {
        // `RenameSession` on a non-live session rewrites that session's file
        // in place (load → set title → append `TitleSet` → re-persist), so
        // the rename survives a cold reload — and the active session's own
        // state is never repointed or mutated.
        let directory =
            std::env::temp_dir().join(format!("muta-rename-archived-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        store
            .replace_messages(vec![Message::new(
                muta_contracts::Role::User,
                "live prompt",
            )])
            .await
            .unwrap();
        let live_id = store.id().await;

        let archived = SessionData {
            project_root: directory.clone(),
            model_window: vec![Message::new(muta_contracts::Role::User, "archived prompt")],
            ..Default::default()
        };
        store.persist_archive(&archived).unwrap();
        let archived_path = directory.join(format!("{}.json", archived.id));

        store
            .rename(&archived.id[..8], Some("renamed archived".to_string()))
            .await
            .unwrap();

        // The picker row now shows the manual title.
        let sessions = store.list().await.unwrap();
        let row = sessions.iter().find(|item| item.id == archived.id).unwrap();
        assert_eq!(row.overview, "renamed archived");

        // The snapshot on disk carries the title + manual lock…
        let on_disk: SessionData =
            serde_json::from_str(&fs::read_to_string(&archived_path).unwrap()).unwrap();
        assert_eq!(on_disk.title.as_deref(), Some("renamed archived"));
        assert!(on_disk.title_manual);

        // …and a cold reload (snapshot + event-log replay) restores it.
        let reloaded = SessionStore::for_path(archived_path);
        let (title, manual) = reloaded.title().await;
        assert_eq!(title.as_deref(), Some("renamed archived"));
        assert!(manual);

        // The active session is untouched: same id, no title.
        assert_eq!(store.id().await, live_id);
        let (title, manual) = store.title().await;
        assert_eq!(title, None);
        assert!(!manual);

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn rename_archived_session_clear_restores_the_prompt_fallback() {
        // Clearing an archived session's manual title must release the
        // ADR-0022 lock on disk too: the reloaded snapshot has `title = None`
        // / `manual = false`, so the picker row falls back to the
        // first-prompt preview and AI generation may title it again.
        let directory = std::env::temp_dir().join(format!(
            "muta-rename-archived-clear-{}",
            uuid::Uuid::new_v4()
        ));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        store
            .replace_messages(vec![Message::new(
                muta_contracts::Role::User,
                "live prompt",
            )])
            .await
            .unwrap();

        let archived = SessionData {
            project_root: directory.clone(),
            title: Some("old manual title".to_string()),
            title_manual: true,
            model_window: vec![Message::new(muta_contracts::Role::User, "archived prompt")],
            ..Default::default()
        };
        store.persist_archive(&archived).unwrap();

        store.rename(&archived.id, None).await.unwrap();

        let reloaded = SessionStore::for_path(directory.join(format!("{}.json", archived.id)));
        let (title, manual) = reloaded.title().await;
        assert_eq!(title, None);
        assert!(
            !manual,
            "clearing an archived title releases the manual lock"
        );
        let row = store
            .list()
            .await
            .unwrap()
            .into_iter()
            .find(|item| item.id == archived.id)
            .unwrap();
        assert_eq!(row.overview, "archived prompt");

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn rename_unknown_id_errors_like_delete() {
        // Resolution is shared with `delete`: an unknown prefix surfaces the
        // same "No session matches" error, and nothing is written.
        let directory =
            std::env::temp_dir().join(format!("muta-rename-unknown-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        store
            .replace_messages(vec![Message::new(
                muta_contracts::Role::User,
                "live prompt",
            )])
            .await
            .unwrap();

        let error = store
            .rename("deadbeef", Some("x".to_string()))
            .await
            .unwrap_err();
        assert_eq!(error, "No session matches 'deadbeef'.");

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn detail_returns_full_last_prompt_and_metadata() {
        // The session-info sub-view (`i`) calls `detail()`, which must return the
        // COMPLETE last effective user prompt (unlike the truncated picker
        // preview), plus title/timestamps/message-count — and must exclude
        // non-driving command echoes from the prompt, like `list()` does.
        let directory = std::env::temp_dir().join(format!("muta-detail-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        let long_prompt = "This is a fairly long prompt that exceeds the \
                           sixty-four character picker preview budget, so the \
                           truncated overview would cut it off with an ellipsis.";
        store
            .replace_messages(vec![
                Message::new(muta_contracts::Role::System, "system preamble"),
                Message::new(muta_contracts::Role::User, "earlier real prompt"),
                Message::new(muta_contracts::Role::Assistant, "reply"),
                Message::new(muta_contracts::Role::User, long_prompt),
                Message::new(muta_contracts::Role::Assistant, "reply 2"),
                // A trailing command echo must NOT become the last prompt.
                Message::command_echo("/autopilot on"),
            ])
            .await
            .unwrap();
        let id = store.id().await;

        let detail = store.detail(&id).await.unwrap();
        assert_eq!(detail.id, id);
        assert!(detail.active);
        assert_eq!(detail.message_count, 6);
        // The FULL prompt is returned — not truncated.
        assert_eq!(detail.last_prompt.as_deref(), Some(long_prompt));

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn detail_returns_none_prompt_for_echo_only_session() {
        let directory =
            std::env::temp_dir().join(format!("muta-detail-echo-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        store
            .replace_messages(vec![Message::command_echo("/autopilot on")])
            .await
            .unwrap();
        let id = store.id().await;

        let detail = store.detail(&id).await.unwrap();
        assert_eq!(
            detail.last_prompt, None,
            "a session with only command echoes has no real user prompt"
        );

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn todos_round_trip_through_disk() {
        let directory =
            std::env::temp_dir().join(format!("muta-todos-state-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        assert!(store.todos().await.is_empty());

        // Seed via reconcile and persist.
        let mut list = muta_contracts::TodoList::new();
        list.reconcile(
            &[
                ("Summary".to_string(), muta_contracts::TodoStatus::Pending),
                (
                    "Key Changes".to_string(),
                    muta_contracts::TodoStatus::Pending,
                ),
                ("Test Plan".to_string(), muta_contracts::TodoStatus::Pending),
            ],
            1000,
            3,
        );
        store.set_todos(list.clone()).await.unwrap();

        // Mutate (mark progress) and persist again — identity must survive.
        list.update("summary", muta_contracts::TodoStatus::Completed, 2000, 4);
        store.set_todos(list.clone()).await.unwrap();

        // Reload from disk via the event log + snapshot and confirm round-trip.
        let reloaded = SessionStore::for_path(path.clone());
        let loaded = reloaded.todos().await;
        assert_eq!(loaded.len(), 3, "all items round-trip through disk");
        assert_eq!(loaded.items[0].content, "Summary");
        assert_eq!(
            loaded.items[0].status,
            muta_contracts::TodoStatus::Completed
        );
        assert_eq!(loaded.updated_at_round, 4);
        // Identity is stable: the first item's id is unchanged after the update.
        assert_eq!(loaded.items[0].id, list.items[0].id);

        // Clearing persists (empty list is the "no active list" state).
        reloaded
            .set_todos(muta_contracts::TodoList::default())
            .await
            .unwrap();
        assert!(reloaded.todos().await.is_empty());

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn scheduled_jobs_round_trip_through_disk() {
        let directory =
            std::env::temp_dir().join(format!("muta-schedule-state-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        assert!(store.scheduled_jobs().await.is_empty());

        // Seed two jobs (one cron, one once) and persist.
        let now = chrono::Utc::now();
        let job_a = muta_contracts::ScheduledJob::cron(
            "aaaa".into(),
            "*/5 * * * *".into(),
            "check the deploy".into(),
            now,
        )
        .unwrap();
        let job_b = muta_contracts::ScheduledJob::cron(
            "bbbb".into(),
            "0 9 * * 1-5".into(),
            "standup".into(),
            now,
        )
        .unwrap();
        let job_c = muta_contracts::ScheduledJob::once(
            "cccc".into(),
            now + chrono::Duration::hours(2),
            "one-shot reminder".into(),
            now,
        );
        store
            .set_scheduled_jobs(vec![job_a.clone(), job_b.clone(), job_c.clone()])
            .await
            .unwrap();

        // Mutate (cancel one) and persist again — snapshot semantics.
        store
            .set_scheduled_jobs(vec![job_b.clone(), job_c.clone()])
            .await
            .unwrap();

        // Reload from disk via the event log + snapshot and confirm round-trip.
        let reloaded = SessionStore::for_path(path.clone());
        let loaded = reloaded.scheduled_jobs().await;
        assert_eq!(loaded.len(), 2, "only the surviving jobs round-trip");
        assert_eq!(loaded[0].id, "bbbb");
        assert_eq!(
            loaded[0].trigger,
            muta_contracts::Schedule::Cron {
                cron: "0 9 * * 1-5".to_string()
            }
        );
        assert_eq!(loaded[1].id, "cccc");
        assert!(loaded[1].trigger.is_once());

        // Clearing persists (empty list is the "no schedule" state).
        reloaded.set_scheduled_jobs(Vec::new()).await.unwrap();
        assert!(reloaded.scheduled_jobs().await.is_empty());

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn legacy_repeat_snapshot_drops_old_jobs() {
        // ADR-0120 policy: the pre-v9 `repeat_jobs` field is not aliased.
        // Jobs are rebuildable scheduler state (the schema-v9 comment's own
        // classification); an old snapshot loads with zero scheduled jobs
        // rather than carrying a compat mapping.
        let directory =
            std::env::temp_dir().join(format!("muta-schedule-legacy-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("session.json");
        let now = chrono::Utc::now();
        let legacy = serde_json::json!({
            "id": "legacy-repeat",
            "parent_id": null,
            "created_at": 0u64,
            "project_root": ".",
            "model_window": [],
            "archived_transcript": [],
            "todos": [],
            "repeat_jobs": [{
                "id": "legacy1",
                "cron": "0 9 * * 1-5",
                "prompt": "standup",
                "created_at": now,
                "next_fire": now,
                "last_fire": null,
            }],
            "schema_version": 8u32,
            "checksum": null,
        });
        fs::write(&path, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();

        let store = SessionStore::for_path(path.clone());
        let jobs = store.scheduled_jobs().await;
        assert!(jobs.is_empty(), "legacy jobs must not load, got {jobs:?}");
        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn commands_round_trip_through_disk() {
        // ADR-0091: the command ledger (invocation + typed result) must survive
        // persist + reload so resume reconstructs every command and its reply.
        let directory =
            std::env::temp_dir().join(format!("muta-commands-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        assert!(store.commands().await.is_empty());

        store
            .mutate_commands(|commands| {
                commands.push(
                    muta_contracts::CommandRecord::new("search", "foo bar").with_result(
                        muta_contracts::CommandResult::Search {
                            query: "foo bar".to_string(),
                            hits: vec![muta_contracts::SearchHit {
                                text: "match".to_string(),
                                score: 0.9,
                            }],
                        },
                    ),
                );
                commands.push(
                    muta_contracts::CommandRecord::new("permissions", "").with_result(
                        muta_contracts::CommandResult::PermissionList {
                            allowed: vec!["execute_command".to_string()],
                        },
                    ),
                );
            })
            .await
            .unwrap();

        // Dialogue materialises the session and its auxiliary command records.
        store
            .replace_messages(vec![Message::new(
                muta_contracts::Role::User,
                "first message",
            )])
            .await
            .unwrap();

        let loaded = SessionStore::for_path(path.clone());
        let commands = loaded.commands().await;
        assert_eq!(commands.len(), 2);
        assert_eq!(commands[0].name, "search");
        assert_eq!(commands[0].args, "foo bar");
        assert_eq!(
            commands[0].result.as_ref().unwrap().to_text(),
            "Relevant history (most similar first):\n\n1. [score=0.900]\nmatch"
        );
        assert_eq!(
            commands[1].result.as_ref().unwrap().to_text(),
            "Always-allowed tools:\n- execute_command"
        );

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn mutate_commands_does_not_persist_an_empty_session_snapshot() {
        // Navigational / inspection slash commands (/sessions, /models, /dashboard)
        // on an otherwise-empty session must remain lazy in memory and not materialize
        // an empty session file on disk.
        let directory =
            std::env::temp_dir().join(format!("muta-commands-lazy-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        store
            .mutate_commands(|commands| {
                commands.push(muta_contracts::CommandRecord::new("sessions", ""));
            })
            .await
            .unwrap();

        assert!(store.is_empty_unpersisted().await);
        assert!(
            !path.exists(),
            "commands alone must not persist an empty session"
        );

        // Once real dialogue content arrives, commands are persisted together with the snapshot.
        store
            .replace_messages(vec![Message::new(
                muta_contracts::Role::User,
                "first message",
            )])
            .await
            .unwrap();

        assert!(!store.is_empty_unpersisted().await);
        assert!(
            path.exists(),
            "dialogue content materializes the session with its commands"
        );

        let loaded = SessionStore::for_path(path.clone());
        assert_eq!(loaded.commands().await.len(), 1);
        assert_eq!(loaded.commands().await[0].name, "sessions");
        assert_eq!(loaded.model_window().await.len(), 1);

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn round_interrupts_round_trip_through_persistence() {
        // C11: interrupt records must survive a session close + reopen —
        // they are the durable answer to "why did this round stop".
        let directory =
            std::env::temp_dir().join(format!("muta-round-interrupts-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        // Materialize the session with dialogue first (an interrupt record on
        // a never-persisted empty session stays in memory, like commands).
        store
            .replace_messages(vec![Message::new(muta_contracts::Role::User, "hello")])
            .await
            .unwrap();
        store
            .record_round_interrupt(muta_contracts::RoundInterrupt {
                reason: muta_contracts::RoundInterruptReason::User,
                at_ms: 1_700_000_000_000,
                round: Some(1),
            })
            .await
            .unwrap();
        store
            .record_round_interrupt(muta_contracts::RoundInterrupt {
                reason: muta_contracts::RoundInterruptReason::Terminated,
                at_ms: 1_700_000_060_000,
                round: Some(2),
            })
            .await
            .unwrap();
        // Duplicate guard: same round + reason is a no-op.
        store
            .record_round_interrupt(muta_contracts::RoundInterrupt {
                reason: muta_contracts::RoundInterruptReason::Terminated,
                at_ms: 1_700_000_060_001,
                round: Some(2),
            })
            .await
            .unwrap();

        let records = store.round_interrupts().await;
        assert_eq!(records.len(), 2, "duplicate (round, reason) is dropped");
        assert_eq!(
            records[0].reason,
            muta_contracts::RoundInterruptReason::User
        );
        assert_eq!(records[1].round, Some(2));

        // Reopen: the log is authoritative, so the records must reload.
        let loaded = SessionStore::for_path(path.clone());
        let reloaded = loaded.round_interrupts().await;
        assert_eq!(reloaded, records, "interrupt records survive reload");

        // Clearing removes them and also round-trips.
        loaded.clear_round_interrupts().await.unwrap();
        let cleared = SessionStore::for_path(path.clone());
        assert!(
            cleared.round_interrupts().await.is_empty(),
            "cleared records stay cleared after reload"
        );

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn legacy_snapshot_without_round_interrupts_loads() {
        // C11: a pre-v11 snapshot (no `round_interrupts` key) loads with an
        // empty list — `#[serde(default)]`, no migration needed.
        let directory = std::env::temp_dir().join(format!(
            "muta-round-interrupts-legacy-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("session.json");
        let legacy = serde_json::json!({
            "id": "legacy-interrupts",
            "parent_id": null,
            "created_at": 0u64,
            "project_root": ".",
            "model_window": [],
            "archived_transcript": [],
            "todos": { "items": [] },
            "scheduled_jobs": [],
            "schema_version": 10u32,
        });
        fs::write(&path, serde_json::to_string(&legacy).unwrap()).unwrap();
        fs::write(
            path.with_extension("jsonl"),
            "{\"seq\":0,\"timestamp\":1,\"type\":\"started\",\"id\":\"legacy-interrupts\",\"parent_id\":null,\"created_at\":0,\"project_root\":\".\",\"schema_version\":10}\n",
        )
        .unwrap();

        let store = SessionStore::for_path(path.clone());
        assert!(store.round_interrupts().await.is_empty());

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn retry_pending_round_trips_through_persistence() {
        // C12: the `/retry` resume point must survive a session close +
        // reopen — it is the durable "this round stopped, finish it" state,
        // and a re-hosted session offers `/retry` off it.
        let directory =
            std::env::temp_dir().join(format!("muta-retry-point-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        // Materialize the session with dialogue first (the point on a
        // never-persisted empty session stays in memory, like commands).
        store
            .replace_messages(vec![Message::new(muta_contracts::Role::User, "hello")])
            .await
            .unwrap();

        assert!(store.retry_pending().await.is_none(), "fresh: no point");
        let point = muta_contracts::RetryPoint {
            round: 3,
            turns_committed: 2,
            history_watermark: 5,
            paused_ms: 1_200,
            at_ms: 1_700_000_000_000,
        };
        store.arm_retry_pending(point.clone()).await.unwrap();
        assert_eq!(store.retry_pending().await, Some(point.clone()));

        // Reopen: the log is authoritative, so the point must reload intact —
        // including the turn ordinal and history watermark a resume replays.
        let loaded = SessionStore::for_path(path.clone());
        assert_eq!(
            loaded.retry_pending().await,
            Some(point),
            "resume point survives reload"
        );

        // Clearing removes it and also round-trips (a completed round never
        // re-offers /retry after another reload).
        loaded.clear_retry_pending().await.unwrap();
        let cleared = SessionStore::for_path(path.clone());
        assert!(
            cleared.retry_pending().await.is_none(),
            "cleared point stays cleared after reload"
        );

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn legacy_snapshot_without_retry_pending_loads() {
        // C12: a pre-v12 snapshot (no `retry_pending` key) loads with `None`
        // — `#[serde(default)]`, no migration needed. A legacy session simply
        // has no `/retry` affordance until a round stops under the new code.
        let directory =
            std::env::temp_dir().join(format!("muta-retry-point-legacy-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("session.json");
        let legacy = serde_json::json!({
            "id": "legacy-retry",
            "parent_id": null,
            "created_at": 0u64,
            "project_root": ".",
            "model_window": [],
            "archived_transcript": [],
            "todos": { "items": [] },
            "scheduled_jobs": [],
            "schema_version": 11u32,
        });
        fs::write(&path, serde_json::to_string(&legacy).unwrap()).unwrap();
        fs::write(
            path.with_extension("jsonl"),
            "{\"seq\":0,\"timestamp\":1,\"type\":\"started\",\"id\":\"legacy-retry\",\"parent_id\":null,\"created_at\":0,\"project_root\":\".\",\"schema_version\":11}\n",
        )
        .unwrap();

        let store = SessionStore::for_path(path.clone());
        assert!(store.retry_pending().await.is_none());

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn legacy_echo_messages_fold_into_ledger_on_v10_migration() {
        // ADR-0091 schema v10: a pre-v10 session whose message stream carries
        // ADR-0050 `CommandEcho` messages (slash + shell) must fold each into
        // the command ledger (`result: None`) and drop them from the window —
        // the message stream becomes pure dialogue again.
        let directory =
            std::env::temp_dir().join(format!("muta-commands-legacy-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("session.json");
        let legacy = serde_json::json!({
            "id": "legacy-echo",
            "parent_id": null,
            "created_at": 0u64,
            "project_root": ".",
            "model_window": [
                {"role": "User", "content": "/search foo", "hidden": false,
                 "origin": {"kind": "command_echo"}},
                {"role": "User", "content": "hello", "hidden": false},
                {"role": "Assistant", "content": "hi", "hidden": false},
                {"role": "User", "content": "!ls -la", "hidden": false,
                 "origin": {"kind": "command_echo"}},
            ],
            "archived_transcript": [],
            "todos": [],
            "scheduled_jobs": [],
            "schema_version": 9u32,
            "checksum": null,
        });
        fs::write(&path, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();

        let store = SessionStore::for_path(path.clone());
        let commands = store.commands().await;
        assert_eq!(
            commands.len(),
            2,
            "both echoes fold into the ledger: {:?}",
            commands
        );
        assert_eq!(commands[0].name, "search");
        assert_eq!(commands[0].args, "foo");
        assert!(
            commands[0].result.is_none(),
            "legacy echoes carry no result"
        );
        assert_eq!(commands[1].name, "shell");
        assert_eq!(commands[1].args, "ls -la");

        // The message stream keeps only the real dialogue.
        let window = store.model_window().await;
        let contents: Vec<&str> = window.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, vec!["hello", "hi"]);
        assert!(
            window.iter().all(|m| !m.is_command_echo()),
            "no command echo remains in the message window"
        );

        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn commands_survive_snapshot_to_events_full_replay() {
        // ADR-0091: the ledger must survive a full event-log replay (log
        // compaction and legacy import rebuild state from `snapshot_to_events`
        // + `apply_events`, not from the snapshot JSON). The seed must emit a
        // `CommandsReplaced` event and replay must restore it.
        let data = SessionData {
            commands: vec![
                muta_contracts::CommandRecord::new("search", "foo").with_result(
                    muta_contracts::CommandResult::Search {
                        query: "foo".to_string(),
                        hits: vec![],
                    },
                ),
                muta_contracts::CommandRecord::new("shell", "!ls -la"),
            ],
            ..SessionData::default()
        };
        let events = snapshot_to_events(&data);
        assert!(
            events
                .iter()
                .any(|envelope| matches!(&envelope.event, SessionEvent::CommandsReplaced { .. })),
            "seed must emit CommandsReplaced"
        );

        let mut restored = SessionData::default();
        apply_events(&mut restored, &events);
        assert_eq!(restored.commands.len(), 2);
        assert_eq!(restored.commands[0].name, "search");
        assert_eq!(restored.commands[1].name, "shell");
        // apply_events does not run schema migration; commands are untouched
        // by the message-only replay paths.
        assert_eq!(
            restored.commands[0].result.as_ref().unwrap().to_text(),
            "No relevant history found."
        );
    }

    #[tokio::test]
    async fn provider_selection_round_trips_through_disk() {
        // C6: a session's provider/model pin must survive persist + reload so a
        // reopened session lands on its own provider instead of the global
        // default, independent of every other session.
        //
        // The pin only persists on a session that has real content — an empty
        // session stays unpersisted (no empty-file litter), so seed a message
        // first.
        let directory =
            std::env::temp_dir().join(format!("muta-provider-sel-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        store
            .replace_messages(vec![Message::new(muta_contracts::Role::User, "seed")])
            .await
            .unwrap();
        assert!(store.provider_selection().await.is_none());

        store
            .set_provider_selection(Some(ProviderSelection {
                provider: "anthropic".to_string(),
                model: Some("claude-sonnet-4-6".to_string()),
            }))
            .await
            .unwrap();

        let loaded = SessionStore::for_path(path.clone());
        let sel = loaded.provider_selection().await;
        let sel = sel.expect("provider selection round-trips through disk");
        assert_eq!(sel.provider, "anthropic");
        assert_eq!(sel.model.as_deref(), Some("claude-sonnet-4-6"));

        // Clearing (None) also persists.
        loaded.set_provider_selection(None).await.unwrap();
        let cleared = SessionStore::for_path(path.clone());
        assert!(cleared.provider_selection().await.is_none());

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn set_provider_selection_does_not_persist_an_empty_session_snapshot() {
        // Regression: pinning a provider on a brand-new, message-less session
        // must NOT write the snapshot — otherwise an empty session (e.g. one a
        // user landed in after Ctrl+C at the startup picker) gets surfaced in
        // the sessions picker the moment they open `/models`. The pin lives in
        // memory only; it is dropped when the process exits.
        //
        // (The lazy seed may leave a `.jsonl` with one `Started` event from the
        // store constructor, but that never produces the `.json` snapshot the
        // picker lists, so the empty session stays invisible.)
        let directory =
            std::env::temp_dir().join(format!("muta-provider-empty-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        store
            .set_provider_selection(Some(ProviderSelection {
                provider: "anthropic".to_string(),
                model: Some("claude-sonnet-4-6".to_string()),
            }))
            .await
            .unwrap();
        // In-memory state updated…
        assert_eq!(
            store.provider_selection().await.as_ref().unwrap().provider,
            "anthropic"
        );
        // …but no snapshot on disk → the empty session never appears in the
        // picker (which lists `.json` files).
        assert!(
            !path.exists(),
            "an empty session must not be persisted as a snapshot by a provider pin"
        );

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn empty_session_is_not_persisted_until_real_content() {
        // Core laziness contract (ADR-0018): a session that is opened but never
        // gains real content (a user message OR a command echo) leaves NO
        // record on disk — opening and exiting must not pollute the session
        // history. Metadata-only mutations on a brand-new empty session
        // (title, provider, a no-op empty-window replace) stay in memory; the
        // first real message or command does persist.
        let directory =
            std::env::temp_dir().join(format!("muta-empty-deferred-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());

        // Metadata-only mutations on the empty session → no snapshot on disk.
        store.set_title(Some("t".to_string()), true).await.unwrap();
        store
            .set_provider_selection(Some(ProviderSelection {
                provider: "anthropic".to_string(),
                model: None,
            }))
            .await
            .unwrap();
        store.replace_messages(Vec::new()).await.unwrap(); // a no-op empty-window replace
        assert!(
            !path.exists(),
            "metadata/no-op mutations on an empty session must not persist a snapshot"
        );

        // A real command echo (via mutate_messages) DOES persist — the user
        // acted, so the session is now real content.
        store
            .mutate_messages(|w| w.push(Message::command_echo("/models")))
            .await
            .unwrap();
        assert!(
            path.exists(),
            "a real command echo persists the session (first-content contract)"
        );

        let _ = fs::remove_dir_all(directory);
    }

    /// The unified `is_user_facing_empty` rule (ADR-0018), pinned end-to-end
    /// through the public store: substantive state (todos / scheduled jobs /
    /// tool mask / round counter) materialises a fresh session, while the two
    /// picker-litter offenders (title, provider selection) never do on their
    /// own. This is the single definition every setter consults, so the test
    /// guards against the per-setter drift the predicate was extracted to fix.
    #[tokio::test]
    async fn substantive_state_materialises_but_title_and_provider_do_not() {
        // Title + provider selection + commands alone stay unpersisted.
        let directory =
            std::env::temp_dir().join(format!("muta-unified-guard-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        store.set_title(Some("t".into()), true).await.unwrap();
        store
            .set_provider_selection(Some(ProviderSelection {
                provider: "anthropic".into(),
                model: None,
            }))
            .await
            .unwrap();
        store
            .mutate_commands(|c| c.push(muta_contracts::CommandRecord::new("sessions", "")))
            .await
            .unwrap();
        assert!(store.is_empty_unpersisted().await);
        assert!(
            !path.exists(),
            "title/provider/commands alone never materialise"
        );

        // A substantive todo list materialises the same session.
        let mut todos = muta_contracts::TodoList::new();
        todos.reconcile(
            &[("Task".to_string(), muta_contracts::TodoStatus::Pending)],
            1000,
            1,
        );
        store.set_todos(todos).await.unwrap();
        assert!(!store.is_empty_unpersisted().await);
        assert!(path.exists(), "a substantive todo list materialises");
        let _ = fs::remove_dir_all(directory);

        // A scheduled job likewise materialises a fresh session on its own.
        let directory2 =
            std::env::temp_dir().join(format!("muta-unified-guard2-{}", uuid::Uuid::new_v4()));
        let path2 = directory2.join("session.json");
        let store2 = SessionStore::for_path(path2.clone());
        let now = chrono::Utc::now();
        let job =
            muta_contracts::ScheduledJob::cron("j1".into(), "* * * * *".into(), "ping".into(), now)
                .expect("a valid cron expression yields a job");
        store2.set_scheduled_jobs(vec![job]).await.unwrap();
        assert!(
            path2.exists(),
            "a scheduled job is substantive and materialises the session"
        );
        let _ = fs::remove_dir_all(directory2);
    }

    #[tokio::test]
    async fn request_usage_round_trips_through_disk() {
        let directory =
            std::env::temp_dir().join(format!("muta-request-usage-state-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        let session_id = store.id().await;
        // Materialise the active session with an active round counter
        store.set_round_counter(2).await.unwrap();
        let record = muta_contracts::RequestUsageRecord {
            key: muta_contracts::RequestUsageKey {
                session_id,
                actor_id: "master".to_string(),
                round: 2,
                turn: 1,
                attempt: 1,
            },
            provider: "openai".to_string(),
            model: "gpt".to_string(),
            status: muta_contracts::RequestUsageStatus::Completed,
            source: muta_contracts::RequestUsageSource::Reported,
            projected_prompt_tokens: 900,
            prompt_tokens: 910,
            completion_tokens: 90,
            total_tokens: 1_000,
            ..Default::default()
        };
        store
            .set_request_usage_records(vec![record.clone()])
            .await
            .unwrap();

        let reloaded = SessionStore::for_path(path);
        assert_eq!(reloaded.request_usage_records().await, vec![record]);
        assert!(
            reloaded
                .set_request_usage_records(Vec::new())
                .await
                .is_err(),
            "request attempts cannot be silently removed"
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn session_runtime_state_round_trips_through_disk() {
        // ADR-0048 Phase 2: the session-scoped runtime state — disabled-tool
        // mask and round counter — must survive persist + reload so a resumed
        // session restores the agent's exact state instead of silently dropping
        // a toggle or resetting the counter.
        let directory =
            std::env::temp_dir().join(format!("muta-runtime-state-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        assert!(store.disabled_tools().await.is_empty());
        assert_eq!(store.round_counter().await, 0);

        let mut disabled = std::collections::HashSet::new();
        disabled.insert("execute_command".to_string());
        disabled.insert("edit_file".to_string());
        store.set_disabled_tools(disabled.clone()).await.unwrap();
        store.set_round_counter(42).await.unwrap();

        let loaded = SessionStore::for_path(path.clone());
        assert_eq!(loaded.disabled_tools().await, disabled);
        assert_eq!(loaded.round_counter().await, 42);

        // Clearing each (0 / empty) persists.
        loaded
            .set_disabled_tools(std::collections::HashSet::new())
            .await
            .unwrap();
        loaded.set_round_counter(0).await.unwrap();
        let cleared = SessionStore::for_path(path.clone());
        assert!(cleared.disabled_tools().await.is_empty());
        assert_eq!(cleared.round_counter().await, 0);

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn autopilot_posture_round_trips_through_disk() {
        // ADR-0132: the autopilot posture is session-scoped persisted state.
        // A daemon crash mid-unattended-session must reopen unattended — the
        // store, not the process, is the authority.
        let directory =
            std::env::temp_dir().join(format!("muta-autopilot-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        assert!(!store.yolo().await, "fresh sessions start attended");

        // Materialise the session first: a posture toggle alone on an empty
        // session must not materialise a file (is_user_facing_empty excludes
        // yolo).
        store.set_round_counter(1).await.unwrap();
        store.set_yolo(true).await.unwrap();
        assert!(path.exists(), "materialised session persists the toggle");

        let loaded = SessionStore::for_path(path.clone());
        assert!(
            loaded.yolo().await,
            "the posture survives persist + reload"
        );

        // Toggling off persists too.
        loaded.set_yolo(false).await.unwrap();
        let reloaded = SessionStore::for_path(path.clone());
        assert!(!reloaded.yolo().await);

        // A same-value write is a no-op, not an event (log stays quiet).
        let events_before = {
            let state = reloaded.state.lock().await;
            state.event_log.load().map(|e| e.len()).unwrap_or(0)
        };
        reloaded.set_yolo(false).await.unwrap();
        let events_after = {
            let state = reloaded.state.lock().await;
            state.event_log.load().map(|e| e.len()).unwrap_or(0)
        };
        assert_eq!(
            events_before, events_after,
            "idempotent posture writes append no events"
        );

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn yolo_toggle_alone_does_not_materialise_empty_session() {
        // The emptiness rule deliberately excludes yolo: arming
        // yolo on a brand-new session with no dialogue must not create
        // a session file on disk (nothing to resume yet).
        let directory =
            std::env::temp_dir().join(format!("muta-yolo-guard-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        store.set_yolo(true).await.unwrap();
        assert!(
            !path.exists(),
            "a posture toggle alone must not materialise an empty session"
        );
        assert!(
            store.yolo().await,
            "the in-memory value still holds for the live process"
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn yolo_event_replays_from_log() {
        // Event-sourced authority: a snapshot-less replay of the jsonl log
        // must rebuild the posture (the snapshot is only a cache; the log
        // wins).
        let directory =
            std::env::temp_dir().join(format!("muta-yolo-log-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        store.set_round_counter(1).await.unwrap(); // materialise
        store.set_yolo(true).await.unwrap();

        // Drop the snapshot, keep the event log: reload must replay.
        let _ = std::fs::remove_file(&path);
        let replayed = SessionStore::for_path(path.clone());
        assert!(
            replayed.yolo().await,
            "the posture replays from the event log alone"
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn yolo_false_is_omitted_from_snapshot_json() {
        // Canonical-JSON compatibility: an attended session serialises
        // without the key so legacy checksums stay byte-identical.
        let json = serde_json::to_string(&SessionData::default()).unwrap();
        assert!(!json.contains("\"yolo\""));
        let json_on = serde_json::to_string(&SessionData {
            yolo: true,
            ..SessionData::default()
        })
        .unwrap();
        assert!(json_on.contains("\"yolo\":true"));
    }

    #[test]
    fn session_snapshot_round_counter_writes_canonical_key_and_rejects_legacy_key() {
        // ADR-0120 policy: the pre-ADR-0047 `turn_counter` key is not
        // aliased. It parses as an unknown field (dropped) and the counter
        // loads at its default — the stale value must not resurface.
        let mut canonical = serde_json::to_value(SessionData {
            round_counter: 11,
            ..SessionData::default()
        })
        .unwrap();
        let object = canonical.as_object_mut().unwrap();
        let counter = object.remove("round_counter").unwrap();
        object.insert("turn_counter".to_string(), counter);

        let loaded = serde_json::from_value::<SessionData>(canonical).unwrap();
        assert_eq!(
            loaded.round_counter, 0,
            "legacy key must not carry its value through"
        );

        let serialized = serde_json::to_string(&SessionData {
            round_counter: 11,
            ..SessionData::default()
        })
        .unwrap();
        assert!(serialized.contains("\"round_counter\":11"));
    }

    #[tokio::test]
    async fn startup_new_session_can_resume_most_recent_cache() {
        let directory =
            std::env::temp_dir().join(format!("muta-host-resume-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        store
            .replace_messages(vec![Message::new(muta_contracts::Role::User, "previous")])
            .await
            .unwrap();
        let previous_id = store.id().await;

        let new_id = store.reset().await.unwrap();
        assert_ne!(new_id, previous_id);
        assert!(store.model_window().await.is_empty());

        let resumed_id = store.resume(None).await.unwrap();
        assert_eq!(resumed_id, previous_id);
        assert_eq!(store.model_window().await[0].content, "previous");
        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn snapshot_fast_path_replays_only_the_tail_on_lag() {
        // The snapshot fast path (C5): a checksum-valid snapshot with an
        // `applied_seq` watermark is authoritative for its folded range, and
        // only log events *after* the watermark are replayed. The
        // operationally important case is a lagging snapshot — a crash mid-turn
        // left `append_turn`'s `MessagesAppended` event in the log but the
        // snapshot still at the previous round boundary. The tail replay must
        // recover it. This is the "log authoritative for the tail" contract.
        let directory =
            std::env::temp_dir().join(format!("muta-fastpath-lag-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        let first_id = store.id().await;
        store
            .replace_messages(vec![Message::new(muta_contracts::Role::User, "first")])
            .await
            .unwrap();

        // Simulate a mid-turn crash: append an event the snapshot has NOT
        // folded (its watermark still points at the replace above).
        {
            let state = store.state.lock().await;
            state
                .event_log
                .append(SessionEvent::MessagesAppended {
                    messages: vec![Message::new(
                        muta_contracts::Role::Assistant,
                        "recovered tail",
                    )],
                })
                .unwrap();
            // Deliberately do NOT persist the snapshot, so its watermark lags.
        }

        // Re-open: the snapshot is checksum-valid but lags by one event. The
        // fast path must replay the tail and surface the appended message.
        let reloaded = SessionStore::for_path(path.clone());
        assert_eq!(reloaded.id().await, first_id);
        let messages = reloaded.model_window().await;
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].content, "first");
        assert_eq!(messages[1].content, "recovered tail");

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn checksum_invalid_snapshot_falls_back_to_full_replay() {
        // A snapshot whose stored checksum no longer matches its content (real
        // corruption, not a recompute) must not be trusted: the load falls
        // through to a full replay from the authoritative event log.
        let directory =
            std::env::temp_dir().join(format!("muta-fastpath-corrupt-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        store
            .replace_messages(vec![Message::new(muta_contracts::Role::User, "truth")])
            .await
            .unwrap();

        // Corrupt the snapshot's content WITHOUT recomputing the checksum, so
        // verification fails. Rewrite the raw JSON directly.
        let raw = fs::read_to_string(&path).unwrap();
        let mut tampered = raw.replace("truth", "LIE");
        // Force a checksum mismatch by zeroing the stored checksum field.
        tampered = tampered.replace(
            "\"checksum\":",
            "\"checksum\":999999999,\"_x\":0,\"checksum\":",
        );
        fs::write(&path, tampered).unwrap();

        let reloaded = SessionStore::for_path(path.clone());
        // Full replay from the log restores the true content.
        assert_eq!(reloaded.model_window().await[0].content, "truth");

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn applied_seq_watermark_round_trips_and_enables_empty_tail_load() {
        // A clean close persists the snapshot with its watermark stamped to the
        // log's high-water mark, so the next load replays an empty tail — the
        // O(snapshot) fast path that makes resume cheap.
        let directory =
            std::env::temp_dir().join(format!("muta-fastpath-clean-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        store
            .replace_messages(vec![Message::new(muta_contracts::Role::User, "hi")])
            .await
            .unwrap();
        store
            .set_title(Some("my session".to_string()), false)
            .await
            .unwrap();
        let persisted_id = store.id().await;

        // The on-disk snapshot must carry the watermark at the high-water mark.
        let on_disk: SessionData =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let watermark = on_disk.applied_seq.expect("watermark stamped on persist");
        let high = EventLog::new(path.with_extension("jsonl"))
            .high_seq()
            .expect("log is seeded");
        assert_eq!(
            watermark, high,
            "watermark == log high-water after clean persist"
        );

        // The tail past the watermark is empty.
        let tail = EventLog::new(path.with_extension("jsonl"))
            .load_since(Some(watermark))
            .unwrap();
        assert!(tail.is_empty(), "clean close leaves an empty tail");

        // Reload restores the title (snapshot-folded) and id.
        let reloaded = SessionStore::for_path(path.clone());
        assert_eq!(reloaded.id().await, persisted_id);
        assert_eq!(reloaded.model_window().await[0].content, "hi");

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn legacy_snapshot_without_watermark_falls_back_to_full_replay() {
        // A pre-C5 snapshot has no `applied_seq`. The load must not take the
        // fast path (there is no watermark to gate it); it replays the whole
        // log, then rewrites the snapshot with a watermark so the *next* load
        // is fast. This is the schema-migration path.
        let directory =
            std::env::temp_dir().join(format!("muta-fastpath-legacy-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        store
            .replace_messages(vec![Message::new(
                muta_contracts::Role::User,
                "legacy content",
            )])
            .await
            .unwrap();

        // Strip the watermark from the persisted snapshot, simulating a pre-C5
        // file (checksum is recomputed so the file is internally consistent).
        let mut data: SessionData =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        data.applied_seq = None;
        let test_blobs = BlobStore::new(directory.join("blobs"));
        write_session_file(&path, &data, &test_blobs).unwrap();

        // Reload: no watermark → full replay → rewrite with watermark.
        let reloaded = SessionStore::for_path(path.clone());
        assert_eq!(reloaded.model_window().await[0].content, "legacy content");

        // The snapshot on disk now has a watermark (the reload rewrote it).
        let rewritten: SessionData =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(
            rewritten.applied_seq.is_some(),
            "reload backfills the watermark"
        );

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn event_log_compacts_once_past_threshold_and_stays_consistent() {
        // Once the append-only log exceeds LOG_COMPACTION_THRESHOLD events and
        // the snapshot has fully folded it, a persist rewrites the log to a
        // single seed and the snapshot's watermark matches the seed's
        // high-water mark — so a subsequent reload replays an empty tail and
        // sees identical state. No event is lost.
        let directory =
            std::env::temp_dir().join(format!("muta-log-compaction-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());

        // Seed real content so the session is persisted (the empty-session
        // deferral would otherwise skip the title-set writes below).
        store
            .replace_messages(vec![Message::new(muta_contracts::Role::User, "seed")])
            .await
            .unwrap();

        // Push well past the threshold via repeated title sets (cheap events).
        for i in 0..(LOG_COMPACTION_THRESHOLD + 64) as u64 {
            store.set_title(Some(format!("t{i}")), false).await.unwrap();
        }
        let persisted_id = store.id().await;

        // After the final persist, the log should have been compacted back to a
        // small seed (snapshot_to_events emits a handful of lines, not 1k+).
        let log = EventLog::new(path.with_extension("jsonl"));
        let count = log.load().unwrap().len();
        assert!(
            count < LOG_COMPACTION_THRESHOLD,
            "log should be compacted, has {count} events"
        );

        // The on-disk snapshot watermark matches the seed's high-water mark.
        let on_disk: SessionData =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let watermark = on_disk.applied_seq.expect("watermark present");
        let high = log.high_seq().expect("seeded log");
        assert_eq!(watermark, high, "watermark == compacted-seed high-water");

        // Reload is consistent and replays an empty tail.
        let tail = log.load_since(Some(watermark)).unwrap();
        assert!(tail.is_empty());
        let reloaded = SessionStore::for_path(path.clone());
        assert_eq!(reloaded.id().await, persisted_id);
        let (title, _) = reloaded.title().await;
        assert_eq!(title.as_deref(), Some("t1087")); // last set wins

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn append_turn_persists_delta_and_survives_reload() {
        // The mid-round, turn-boundary save point (ADR-0048): `append_turn`
        // writes only the
        // new tail as a `MessagesAppended` event, and a fresh `SessionStore`
        // at the same path must replay it to recover the full history. This
        // is the resume-after-crash contract — the whole point of the feature.
        let directory =
            std::env::temp_dir().join(format!("muta-append-turn-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());

        // The round opens with one user message, durably written.
        store
            .replace_messages(vec![Message::new(
                muta_contracts::Role::User,
                "user prompt",
            )])
            .await
            .unwrap();

        // Turn 1 adds an assistant response + a tool result. The caller
        // passes the *full* current history; the store appends only the tail.
        let turn1 = vec![
            Message::new(muta_contracts::Role::User, "user prompt"),
            Message::new(muta_contracts::Role::Assistant, "I will run a tool"),
            Message::new(muta_contracts::Role::Tool, "tool output"),
        ];
        store.append_turn(&turn1).await.unwrap();

        // Turn 2 adds more. The snapshot cache is still at the round-open
        // state (one message); only the event log has grown.
        let turn2 = vec![
            Message::new(muta_contracts::Role::User, "user prompt"),
            Message::new(muta_contracts::Role::Assistant, "I will run a tool"),
            Message::new(muta_contracts::Role::Tool, "tool output"),
            Message::new(muta_contracts::Role::Assistant, "done"),
        ];
        store.append_turn(&turn2).await.unwrap();

        // The live in-memory state reflects all appends.
        let live = store.model_window().await;
        assert_eq!(live.len(), 4);
        assert_eq!(live[3].content, "done");

        // A brand-new store replays the event log and recovers everything,
        // including the appended tail the snapshot never recorded.
        let reloaded = SessionStore::for_path(path.clone());
        let recovered = reloaded.model_window().await;
        assert_eq!(recovered.len(), 4, "appended turns survive reload");
        assert_eq!(recovered[2].content, "tool output");
        assert_eq!(recovered[3].content, "done");

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn append_turn_is_noop_when_nothing_new() {
        // Passing a history no longer than the durable baseline (e.g. right
        // after a compaction rewrote the window via `replace_messages`) must
        // not corrupt anything or write a spurious event.
        let directory =
            std::env::temp_dir().join(format!("muta-append-noop-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        let messages = vec![Message::new(muta_contracts::Role::User, "hi")];
        store.replace_messages(messages.clone()).await.unwrap();

        // Same length, same content → no-op.
        store.append_turn(&messages).await.unwrap();
        assert_eq!(store.model_window().await.len(), 1);

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn append_turn_falls_back_to_replace_on_divergent_prefix() {
        // If the incoming prefix disagrees with the durable state (a bug or a
        // compaction that bypassed `replace_messages`), `append_turn` must
        // fall back to a full replace rather than splice a corrupt tail.
        let directory =
            std::env::temp_dir().join(format!("muta-append-diverge-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        store
            .replace_messages(vec![Message::new(muta_contracts::Role::User, "original")])
            .await
            .unwrap();

        // Incoming history where the durable prefix was *rewritten* — the
        // first message content differs.
        let divergent = vec![
            Message::new(muta_contracts::Role::User, "rewritten"),
            Message::new(muta_contracts::Role::Assistant, "new"),
        ];
        store.append_turn(&divergent).await.unwrap();

        // The fallback replaced everything with the incoming history.
        let live = store.model_window().await;
        assert_eq!(live.len(), 2);
        assert_eq!(live[0].content, "rewritten");
        assert_eq!(live[1].content, "new");

        // And a reload recovers the replaced state, not a corrupt splice.
        let reloaded = SessionStore::for_path(path.clone());
        let recovered = reloaded.model_window().await;
        assert_eq!(recovered[0].content, "rewritten");

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn projection_snapshot_import_does_not_duplicate_archive() {
        let directory =
            std::env::temp_dir().join(format!("muta-projection-import-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("session.json");
        let blob_store = BlobStore::new(directory.join("blobs"));
        let snapshot = SessionData {
            model_window: vec![Message::new(muta_contracts::Role::User, "live window")],
            archived_transcript: vec![Message::new(muta_contracts::Role::Assistant, "archived")],
            last_projection: Some(ContextProjectionCheckpoint {
                operation: ContextProjectionKind::Compact,
                archived_messages: 1,
                active_messages: 1,
                window_tokens_before: 100,
                window_tokens_after: 20,
            }),
            ..Default::default()
        };
        write_session_file(&path, &snapshot, &blob_store).unwrap();

        let store = SessionStore::for_path(path.clone());
        let transcript = store.full_transcript().await;
        assert_eq!(transcript.len(), 2);
        assert_eq!(transcript[0].content, "archived");
        assert_eq!(transcript[1].content, "live window");
        assert_eq!(
            store.last_projection().await.unwrap().operation,
            ContextProjectionKind::Compact
        );

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn large_message_content_is_offloaded_to_blob_store() {
        let directory =
            std::env::temp_dir().join(format!("muta-blob-session-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        let big = "x".repeat(8_192);
        store
            .replace_messages(vec![Message::new(muta_contracts::Role::User, &big)])
            .await
            .unwrap();

        // Snapshot on disk should reference the blob.
        let raw = fs::read_to_string(&path).unwrap();
        assert!(
            raw.contains("content_blob"),
            "large content should be offloaded"
        );
        assert!(
            !raw.contains(&big),
            "raw content should not appear in snapshot"
        );

        // Replaying the event log rehydrates content from the blob store.
        let reloaded = SessionStore::for_path(path.clone());
        let messages = reloaded.model_window().await;
        assert_eq!(messages[0].content, big);
        assert!(
            messages[0].content_blob.is_none(),
            "memory uses inline content"
        );

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn injection_origin_survives_persist_and_reload() {
        // A harness-injected message's provenance must round-trip through the
        // snapshot cache AND the event-log replay path — the contract that lets
        // a resumed session faithfully reconstruct what was injected and why.
        // This is the end-to-end (store-layer) companion to the message-level
        // round-trip test in muta-contracts.
        use muta_contracts::{HookEventKind, InjectionKind, InjectionOrigin};
        let directory =
            std::env::temp_dir().join(format!("muta-origin-session-{}", uuid::Uuid::new_v4()));
        let path = directory.join("session.json");
        let store = SessionStore::for_path(path.clone());
        let injected = Message::injected(
            muta_contracts::Role::User,
            "setup context",
            InjectionOrigin::new(InjectionKind::Hook(HookEventKind::SessionStart))
                .with_reason("onstart.sh"),
        );
        store.replace_messages(vec![injected]).await.unwrap();

        // The snapshot file carries the origin object.
        let raw = fs::read_to_string(&path).unwrap();
        assert!(
            raw.contains("\"origin\""),
            "snapshot must persist origin: {raw}"
        );
        // HookEventKind serialises in PascalCase (no rename_all), so the wire
        // tag is "SessionStart". Pretty-printed with a space after the colon.
        assert!(
            raw.contains("\"hook\": \"SessionStart\""),
            "snapshot must persist the hook kind: {raw}"
        );

        // Reload via the event-log path (authoritative) rehydrates it intact.
        let reloaded = SessionStore::for_path(path.clone());
        let messages = reloaded.model_window().await;
        assert_eq!(messages.len(), 1);
        let origin = messages[0].origin.as_ref().expect("origin reloaded");
        assert_eq!(
            origin.kind,
            InjectionKind::Hook(HookEventKind::SessionStart)
        );
        assert_eq!(origin.reason.as_deref(), Some("onstart.sh"));

        let _ = fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn legacy_snapshot_without_origin_loads_as_none() {
        // A pre-C4 snapshot file (no `origin` key on any message) must load
        // with `origin: None` for every message — the store-layer side of the
        // backward-compat contract. Provenance is simply absent for old data.
        let directory =
            std::env::temp_dir().join(format!("muta-legacy-origin-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("session.json");
        // Minimal pre-C4 snapshot: no origin key, schema_version 3.
        let legacy = serde_json::json!({
            "id": "legacy",
            "parent_id": null,
            "created_at": 1u64,
            "updated_at": 1u64,
            "messages": [
                {"role":"User","content":"old user input","hidden":false},
                {"role":"Assistant","content":"old reply","hidden":false}
            ],
            "archived_messages": [],
            "loop_checkpoint": null,
            "last_relief": null,
            "project_root": ".",
            "todos": [],
            "schema_version": 3,
            "checksum": null,
            "title": null,
            "title_manual": false,
            "pursuit": null
        });
        fs::write(&path, legacy.to_string()).unwrap();

        let store = SessionStore::for_path(path.clone());
        let messages = store.model_window().await;
        // ADR-0120 policy: pre-rename snapshot keys (`messages` /
        // `archived_messages`) are not aliased; the unparseable snapshot
        // loads as a fresh empty session rather than half-migrating.
        assert!(
            messages.iter().all(|m| m.origin.is_none()),
            "any loaded message must lack origin"
        );

        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn compaction_keeps_recent_complete_rounds() {
        let messages = vec![
            Message::new(muta_contracts::Role::System, "system"),
            Message::new(muta_contracts::Role::User, "old question"),
            Message::new(muta_contracts::Role::Assistant, "old answer"),
            Message::new(muta_contracts::Role::Tool, "old tool result"),
            Message::new(muta_contracts::Role::User, "recent question"),
            Message::new(muta_contracts::Role::Assistant, "recent answer"),
        ];

        let result = compact_messages(&messages, 10_000, 1).unwrap();

        assert_eq!(result.checkpoint.operation, ContextProjectionKind::Compact);
        assert_eq!(result.model_window[0].role, muta_contracts::Role::User);
        assert!(result.model_window[0].hidden);
        assert_eq!(result.model_window[1].content, "recent question");
        assert_eq!(result.model_window[2].content, "recent answer");
        assert!(
            result
                .archived_originals
                .iter()
                .any(|message| message.content == "old tool result")
        );
        assert!(
            !result
                .archived_originals
                .iter()
                .any(|message| message.role == muta_contracts::Role::System)
        );
    }

    #[test]
    fn compaction_requires_an_older_complete_round() {
        let messages = vec![
            Message::new(muta_contracts::Role::User, "question"),
            Message::new(muta_contracts::Role::Assistant, "answer"),
        ];
        assert!(compact_messages(&messages, 10_000, 1).is_none());
    }

    #[test]
    fn compaction_reduces_preserved_tail_to_honor_working_memory_target() {
        let mut messages = Vec::new();
        for round in 0..4 {
            let body = format!("round-{round} {}", "word ".repeat(200));
            messages.push(Message::new(Role::User, body.clone()));
            messages.push(Message::new(Role::Assistant, body));
        }

        // Three complete rounds are requested, but that would consume almost
        // all of this 800-token target. The selector keeps one recent round so
        // a checkpoint still has room to carry durable task state.
        let selection = select_compaction_for_target(&messages, 3, 800).unwrap();
        assert_eq!(
            selection
                .tail
                .iter()
                .filter(|message| message.role == Role::User)
                .count(),
            1
        );
        assert!(estimate_tokens(&selection.tail) <= 600);
    }

    #[test]
    fn summary_truncation_respects_the_projection_token_unit() {
        let summary = truncate_summary_to_token_budget("中".repeat(400), 100);
        assert!(muta_contracts::tokenizer::count_tokens(&summary) <= 100);
    }

    #[test]
    fn excerpt_summary_keeps_recent_first_under_budget() {
        // A large old tool result and a small recent user message. With a tiny
        // token budget only the recent message (chosen newest-first) survives;
        // the old verbose tool result is omitted instead of crowding it out.
        let archived = vec![
            Message::new(Role::Tool, "X".repeat(3_000)),
            Message::new(Role::User, "recent critical detail"),
        ];

        let summary = build_excerpt_summary(&archived, 20, None);

        assert!(summary.contains("recent critical detail"));
        assert!(!summary.contains('X'));
    }

    #[test]
    fn excerpt_summary_carries_forward_previous_summary() {
        let archived = vec![Message::new(Role::User, "what is 2+2")];
        let summary = build_excerpt_summary(&archived, 4_000, Some("prev anchored facts"));

        assert!(summary.starts_with("[Previous summary]\n"));
        assert!(summary.contains("prev anchored facts"));
        assert!(summary.contains("[Recent history]"));
        assert!(summary.contains("what is 2+2"));
    }

    #[test]
    fn select_compaction_extracts_previous_summary() {
        let prior = checkpoint_message("prev summary body");
        let messages = vec![
            Message::new(Role::System, "system"),
            prior,
            Message::new(Role::User, "q1"),
            Message::new(Role::Assistant, "a1"),
            Message::new(Role::User, "q2"),
            Message::new(Role::Assistant, "a2"),
        ];

        let selection = select_compaction(&messages, 1).unwrap();
        assert_eq!(
            selection.previous_summary.as_deref(),
            Some("prev summary body")
        );
        // The prior checkpoint lands in the archived head, not the tail.
        assert!(
            selection
                .archived
                .iter()
                .any(|message| message.content.starts_with("[Conversation checkpoint]"))
        );
        assert_eq!(selection.tail.last().unwrap().content, "a2");
    }

    #[tokio::test]
    async fn run_compaction_uses_provider_summary() {
        let mut history = vec![
            Message::new(Role::System, "system"),
            Message::new(Role::User, "old question"),
            Message::new(Role::Assistant, "old answer"),
            Message::new(Role::User, "recent question"),
            Message::new(Role::Assistant, "recent answer"),
        ];
        let provider: Arc<dyn Provider> = Arc::new(CompactionProvider);

        let result = run_compaction(&mut history, 10_000, 1, Some(provider), Vec::new())
            .await
            .unwrap()
            .unwrap();

        // The mock provider's canned reply becomes the checkpoint summary.
        assert!(result.model_window[0].content.contains("mock AI"));
        assert_eq!(result.model_window[1].content, "recent question");
        assert!(result.model_window[0].hidden);
    }

    #[tokio::test]
    async fn run_compaction_falls_back_when_provider_errors() {
        // Pass a provider that always errors and assert we still get an
        // excerpt-based checkpoint.
        struct FailingProvider;
        #[async_trait]
        impl Provider for FailingProvider {
            async fn chat(
                &self,
                _request: muta_contracts::ModelRequest,
            ) -> Result<Message, String> {
                Err("boom".to_string())
            }
            async fn stream_chat(
                &self,
                _request: muta_contracts::ModelRequest,
            ) -> Result<futures::stream::BoxStream<'static, Result<String, String>>, String>
            {
                Err("boom".to_string())
            }
        }

        let mut history = vec![
            Message::new(Role::User, "old question"),
            Message::new(Role::Assistant, "old answer"),
            Message::new(Role::User, "recent question"),
            Message::new(Role::Assistant, "recent answer"),
        ];
        let provider: Arc<dyn Provider> = Arc::new(FailingProvider);

        let result = run_compaction(&mut history, 10_000, 1, Some(provider), Vec::new())
            .await
            .unwrap()
            .unwrap();

        // Fallback excerpt summary references the old question.
        assert!(result.model_window[0].content.contains("old question"));
    }
}
