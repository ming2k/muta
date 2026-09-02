//! Persisted session state: the [`SessionStore`] event-sourced model and
//! its split submodules.
//!
//! The module root keeps the data model (`SessionData`), schema migration,
//! checksum/blob-offload file plumbing, the free helper functions, and the
//! pure compaction pipeline; the `impl SessionStore` surface is split by
//! concern:
//!
//! - `fields`: typed read/write accessors over session fields.
//! - `history`: transcript append/replace, rounds, retry bookkeeping,
//!   fork/lineage queries, and the session tree.
//! - `store`: construction, load/persist, snapshots, event-log replay,
//!   armed-schedule scan, list/detail/active views, offline scan tools.
//! - `tests`: embedded test suite.
//!
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
pub(crate) struct SessionData {
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
    /// AI-generated session digest (title + intent + history checklist) —
    /// the resume-time working-memory projection shown by the session
    /// picker's detail view. `None` for legacy snapshots and sessions that
    /// have not yet generated one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    digest: Option<muta_contracts::SessionDigest>,
    /// Transcript char count when `digest` was generated — the watermark the
    /// refresh throttle measures growth against. `None` while `digest` is
    /// `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    digest_anchor: Option<u64>,
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
    /// Session-scoped delegated posture: `true` means the agent
    /// runs in full auto-approve mode (bypasses tool permission prompts).
    #[serde(
        default,
        alias = "yolo",
        alias = "autopilot",
        skip_serializing_if = "std::ops::Not::not"
    )]
    delegated: bool,
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
            digest: None,
            digest_anchor: None,
            applied_seq: None,
            provider_selection: None,
            disabled_tools: std::collections::HashSet::new(),
            round_counter: 0,
            request_usage_records: Vec::new(),
            commands: Vec::new(),
            round_interrupts: Vec::new(),
            retry_pending: None,
            delegated: false,
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
    /// The user-facing-emptiness rule deliberately excludes `delegated`:
    /// toggling delegated mode on an otherwise-fresh session is a posture change
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
    let write_res = fsutil::atomic_write_json(path, &data);

    // Synchronize to unified SQLite database via Single-Writer Actor (ADR-0163 / ADR-0168)
    let handle = crate::db::get_persistence_handle();
    let fork_str = match data.fork_kind {
        muta_contracts::SessionForkKind::Trunk => "trunk",
        muta_contracts::SessionForkKind::Fork => "fork",
        muta_contracts::SessionForkKind::Aside => "aside",
    };
    let session_rec = crate::db::SessionRecord {
        id: data.id.clone(),
        parent_id: data.parent_id.clone(),
        fork_kind: fork_str.to_string(),
        title: data.title.clone(),
        title_manual: data.title_manual,
        created_at_ms: data.created_at as i64,
        updated_at_ms: data.updated_at as i64,
        project_root: data.project_root.to_string_lossy().into_owned(),
    };
    handle.try_upsert_session(session_rec);

    // Materialize active model window messages into messages table & FTS index
    for (seq, msg) in data.model_window.iter().enumerate() {
        let role_str = match msg.role {
            muta_contracts::Role::User => "user",
            muta_contracts::Role::Assistant => "assistant",
            muta_contracts::Role::System => "system",
            muta_contracts::Role::Tool => "tool",
        };
        handle.try_insert_message(crate::db::MessageRecord {
            id: format!("{}:{}", data.id, seq),
            session_id: data.id.clone(),
            seq: seq as i64,
            role: role_str.to_string(),
            content: msg.content.clone(),
            content_blob_hash: msg.content_blob.clone(),
            reasoning_content: msg.reasoning_content.clone(),
            provider: None,
            model: None,
            created_at_ms: data.updated_at as i64,
        });
    }

    write_res
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
            SessionEvent::DigestSet { digest, anchor } => {
                data.digest = digest.clone();
                data.digest_anchor = digest.as_ref().map(|_| *anchor);
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
            SessionEvent::DelegatedSet { enabled } => {
                data.delegated = *enabled;
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
    if let Some(digest) = &data.digest {
        events.push(crate::events::EventEnvelope {
            seq: events.len() as u64,
            timestamp: data.updated_at,
            event: SessionEvent::DigestSet {
                digest: Some(digest.clone()),
                anchor: data.digest_anchor.unwrap_or(0),
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
    if data.delegated {
        events.push(crate::events::EventEnvelope {
            seq: events.len() as u64,
            timestamp: data.updated_at,
            event: SessionEvent::DelegatedSet { enabled: true },
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
    /// The Chronicler's structured digest (intent + history checklist), if present.
    pub digest: Option<muta_contracts::SessionDigest>,
}

/// The mutable bits a [`SessionStore`] pins to one session file: the snapshot
/// path, its event log, and the in-memory session data. Grouped under a single
/// [`tokio::sync::Mutex`] so repointing the store (reset / fork / open) — which
/// swaps both the path and the event log — is atomic with respect to every
/// reader and writer. There is no second lock to deadlock against.
pub(crate) struct SessionState {
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
    digest: Option<muta_contracts::SessionDigest>,
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
        digest: data.digest.clone(),
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
            let nested =
                serialize_runner_transcript_for_summary(children, SUMMARY_RUNNER_CAP_TOKENS);
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
    let instructions = muta_contracts::InstructionBundle::from_single(
        "compaction.summarization",
        muta_contracts::InstructionTier::Task,
        SUMMARIZATION_SYSTEM_PROMPT,
    );
    let messages = vec![Message::new(Role::User, user_prompt)];
    // Bound the summarization call so a stalled or overloaded provider
    // triggers the excerpt fallback instead of hanging the turn (and the
    // entire frontend) forever. Two minutes is generous for a single
    // summarization response.
    const SUMMARIZATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
    let response = match tokio::time::timeout(
        SUMMARIZATION_TIMEOUT,
        provider.chat(
            muta_contracts::ModelRequest::ephemeral(messages).with_instructions(instructions),
        ),
    )
    .await
    {
        Ok(result) => result.map_err(|e| e.to_string())?,
        Err(_elapsed) => {
            return Err(format!(
                "Summarization timed out after {} seconds; using excerpt fallback.",
                SUMMARIZATION_TIMEOUT.as_secs()
            ));
        }
    };
    let summary = response.message.content.trim().to_string();
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

mod fields;
mod history;

pub use history::CommitTurn;
mod store;
#[cfg(test)]
mod tests;
