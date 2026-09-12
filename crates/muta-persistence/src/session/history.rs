//! Transcript append/replace, round and interrupt records, retry
//! bookkeeping, fork/lineage queries, and the session tree (ADR-0186).

use super::*;

/// One unified turn commit for [`SessionStore::commit_turn`]. `messages` is
/// the full current window (O(delta) against the durable projection);
/// `round_counter` is committed only when it differs; `usage_records` are
/// upserted only where they changed; `retry_point` arms or clears the retry
/// affordance; `round_interrupt` records an interrupted outcome.
#[derive(Debug, Clone)]
pub struct CommitTurn<'a> {
    pub messages: &'a [Message],
    pub round_counter: Option<u64>,
    pub usage_records: &'a [muta_contracts::RequestUsageRecord],
    pub retry_point: Option<Option<muta_contracts::RetryPoint>>,
    pub round_interrupt: Option<muta_contracts::RoundInterrupt>,
    /// ADR-0236 D3 idempotency identity. Replaying the same key returns the
    /// original commit receipt instead of applying the deltas a second time.
    /// `None` mints a fresh identity for this commit.
    pub operation_id: Option<String>,
    /// ADR-0236 D3 revision precondition. When supplied, the writer refuses
    /// the commit if the durable session revision no longer matches — the
    /// check fails closed rather than writing over content the caller did
    /// not see.
    pub expected_revision: Option<u64>,
}

impl<'a> CommitTurn<'a> {
    pub fn new(messages: &'a [Message]) -> Self {
        Self {
            messages,
            round_counter: None,
            usage_records: &[],
            retry_point: None,
            round_interrupt: None,
            operation_id: None,
            expected_revision: None,
        }
    }
}

/// Guarded persist plumbing shared by the accessors in this module: mutate
/// under the lock, then optionally write the snapshot off the async runtime.
/// (Kept as a doc anchor; each accessor inlines the guard so the decision and
/// the persist are atomic under the session lock.)
fn _persist_guard_doc() {}

/// Wire-normalized equality: harness sidecars (timestamps, display content,
/// provenance) are not provider-visible content, so comparisons that decide
/// append-vs-rebuild must ignore them (ADR-0186).
///
/// Implemented via zero-copy `semantic_wire_eq` to eliminate repetitive heap
/// allocations and clones on the ReAct turn hot path.
fn wire_eq(a: &[Message], b: &[Message]) -> bool {
    a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| x.semantic_wire_eq(y))
}

impl SessionStore {
    /// The durable round-interrupt records (C11): one per round stopped
    /// before completing, newest last. Pure projection state — never part
    /// of the transcript.
    pub async fn round_interrupts(&self) -> Vec<muta_contracts::RoundInterrupt> {
        self.state.lock().await.data.round_interrupts.clone()
    }

    /// Append one round-interrupt record (C11). Best-effort duplicate guard:
    /// a record for the same round with the same reason is not appended twice.
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
            state.data.round_interrupts.push(record);
            state.data.updated_at = unix_timestamp();
            let empty_unpersisted = Self::should_skip_persist(&state);
            if !empty_unpersisted {
                state.defer_persist = false;
            }
            (state.path.clone(), state.data.clone(), !empty_unpersisted)
        };
        if should_persist {
            self.persist_off_runtime(path, data, self.blob_store.clone())
                .await?;
        }
        Ok(())
    }

    /// The durable request-projection archive (ADR-0218): forensic snapshots
    /// of assembled requests, oldest first. Pure projection state — never part
    /// of the transcript, never replayed into a later model request. Read from
    /// the key-addressed `request_projections` table on demand; it is not held
    /// in `SessionData`.
    pub async fn request_projections(&self) -> Vec<muta_contracts::RequestProjection> {
        let handle = self.writer.clone();
        let session_id = self.id().await;
        tokio::task::spawn_blocking(move || {
            handle
                .reader()
                .and_then(|reader| {
                    reader.load_request_projections(
                        &session_id,
                        crate::db::MAX_RETAINED_REQUEST_PROJECTIONS,
                    )
                })
                .unwrap_or_default()
        })
        .await
        .unwrap_or_default()
    }

    /// Insert one request-projection record (ADR-0218), awaiting the durable
    /// write. Keyed by `(session, round, turn)`, so re-recording a logical
    /// invocation replaces its row rather than duplicating it. Retention is a
    /// bounded ring enforced by the storage layer.
    pub async fn record_request_projection(
        &self,
        record: muta_contracts::RequestProjection,
    ) -> Result<(), String> {
        let session_id = self.id().await;
        self.writer
            .record_request_projection(session_id, record)
            .await
            .map_err(|error| error.to_string())
    }

    /// Fire-and-forget request-projection insert (ADR-0218) for the model
    /// request hot path: forensic persistence must never block dispatch. The
    /// caller passes the session id because this path cannot await the store
    /// lock to read it. A dropped write is logged, never surfaced as a round
    /// failure.
    pub fn try_record_request_projection(
        &self,
        session_id: String,
        record: muta_contracts::RequestProjection,
    ) {
        self.writer.try_record_request_projection(session_id, record);
    }

    /// Clear every round-interrupt record (C11). Called when the interrupted
    /// round's outcome is superseded.
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
                state.defer_persist = false;
            }
            (state.path.clone(), state.data.clone(), !empty_unpersisted)
        };
        if should_persist {
            self.persist_off_runtime(path, data, self.blob_store.clone())
                .await?;
        }
        Ok(())
    }

    /// The durable retry-resolution records: one per round that recovered
    /// from transient provider faults via the harness retry loop, newest
    /// last. Pure projection state — never part of the transcript.
    pub async fn retry_resolutions(&self) -> Vec<muta_contracts::RetryResolution> {
        self.state.lock().await.data.retry_resolutions.clone()
    }

    /// Append one retry-resolution record. Duplicate guard: a record for the
    /// same round is not appended twice (the live `RetryResolved` event and
    /// the durable commit can race on the same round).
    pub async fn record_retry_resolution(
        &self,
        record: muta_contracts::RetryResolution,
    ) -> Result<(), String> {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            let already = record.round.is_some_and(|round| {
                state
                    .data
                    .retry_resolutions
                    .iter()
                    .any(|existing| existing.round == Some(round))
            }) || (record.round.is_none()
                && state
                    .data
                    .retry_resolutions
                    .iter()
                    .any(|existing| existing.round.is_none()));
            if already {
                return Ok(());
            }
            state.data.retry_resolutions.push(record);
            state.data.updated_at = unix_timestamp();
            let empty_unpersisted = Self::should_skip_persist(&state);
            if !empty_unpersisted {
                state.defer_persist = false;
            }
            (state.path.clone(), state.data.clone(), !empty_unpersisted)
        };
        if should_persist {
            self.persist_off_runtime(path, data, self.blob_store.clone())
                .await?;
        }
        Ok(())
    }

    /// The durable `/retry` resume point (C12).
    pub async fn retry_pending(&self) -> Option<muta_contracts::RetryPoint> {
        self.state.lock().await.data.retry_pending.clone()
    }

    /// Arm the `/retry` resume point (C12). Snapshot semantics: the single
    /// slot is replaced (arming for a newer round retires an older point).
    pub async fn arm_retry_pending(&self, point: muta_contracts::RetryPoint) -> Result<(), String> {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            state.data.retry_pending = Some(point);
            state.data.updated_at = unix_timestamp();
            let empty_unpersisted = Self::should_skip_persist(&state);
            if !empty_unpersisted {
                state.defer_persist = false;
            }
            (state.path.clone(), state.data.clone(), !empty_unpersisted)
        };
        if should_persist {
            self.persist_off_runtime(path, data, self.blob_store.clone())
                .await?;
        }
        Ok(())
    }

    /// Clear the `/retry` resume point (C12). A no-op when nothing is armed.
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
                state.defer_persist = false;
            }
            (state.path.clone(), state.data.clone(), !empty_unpersisted)
        };
        if should_persist {
            self.persist_off_runtime(path, data, self.blob_store.clone())
                .await?;
        }
        Ok(())
    }

    /// Replace the durable dialogue with exactly `messages` (ADR-0186): the
    /// transcript's entries are rebuilt from scratch and the projection
    /// decision history is cleared — the caller asserts the window is the
    /// whole truth.
    pub async fn replace_messages(&self, messages: Vec<Message>) -> Result<(), String> {
        let (path, data, children, should_persist) = {
            let mut state = self.state.lock().await;
            state.data.transcript = rebuild_transcript_from_messages(&messages);
            state.data.generation = uuid::Uuid::new_v4().to_string();
            let children = admit_subagent_children(&mut state.data, &messages);
            state.projected_cache = Some(messages.clone());
            state.data.updated_at = unix_timestamp();
            let empty_unpersisted = Self::should_skip_persist(&state);
            if !empty_unpersisted {
                state.defer_persist = false;
            }
            (
                state.path.clone(),
                state.data.clone(),
                children,
                !empty_unpersisted,
            )
        };
        persist_subagent_children(&self.writer, &self.blob_store, &children);
        if should_persist {
            self.persist_off_runtime(path, data, self.blob_store.clone())
                .await?;
        }
        Ok(())
    }

    /// The durable command ledger (ADR-0091).
    pub async fn commands(&self) -> Vec<muta_contracts::CommandRecord> {
        self.state.lock().await.data.commands.clone()
    }

    /// Atomically mutate the command ledger in place under the lock and persist.
    pub async fn mutate_commands<F>(&self, f: F) -> Result<(), String>
    where
        F: FnOnce(&mut Vec<muta_contracts::CommandRecord>),
    {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            f(&mut state.data.commands);
            state.data.updated_at = unix_timestamp();
            let empty_unpersisted = Self::should_skip_persist(&state);
            if !empty_unpersisted {
                state.defer_persist = false;
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
    /// write (ADR-0048). The prefix is compared against the **projection**;
    /// on divergence the transcript is rebuilt (the caller's window is the
    /// truth).
    pub async fn append_turn(&self, current: &[Message]) -> Result<(), String> {
        let (path, data, children) = {
            let mut state = self.state.lock().await;
            let durable_len = state.get_or_project_messages().len();
            if current.len() <= durable_len && !wire_eq(current, state.get_or_project_messages()) {
                return Ok(());
            }
            let old_entries_len = state.data.transcript.entries.len();
            let mut children = Vec::new();
            let mut full_rewrite = false;
            if current.len() > durable_len
                && wire_eq(&current[..durable_len], state.get_or_project_messages())
            {
                let tail = &current[durable_len..];
                for message in tail {
                    state
                        .data
                        .transcript
                        .push(muta_contracts::TranscriptEntry::from_message(0, message));
                }
                children = admit_subagent_children(&mut state.data, tail);
                state.append_to_projection_cache(tail);
            } else if !wire_eq(current, state.get_or_project_messages()) {
                state.data.transcript = rebuild_transcript_from_messages(current);
                state.data.generation = uuid::Uuid::new_v4().to_string();
                children = admit_subagent_children(&mut state.data, current);
                state.invalidate_projection_cache();
                full_rewrite = true;
            }
            state.data.updated_at = unix_timestamp();
            state.defer_persist = false;

            let data = if full_rewrite {
                state.data.clone()
            } else {
                let mut delta = state.data.clone_metadata_without_history();
                delta.transcript.entries =
                    state.data.transcript.entries[old_entries_len..].to_vec();
                delta
            };
            (state.path.clone(), data, children)
        };
        persist_subagent_children(&self.writer, &self.blob_store, &children);
        self.persist_off_runtime(path, data, self.blob_store.clone())
            .await
    }

    /// Commit everything a finished ReAct turn changed, in **one** lock
    /// acquisition and at most **one** snapshot write. Returns the committed
    /// session revision (ADR-0236 D3).
    pub async fn commit_turn(&self, commit: CommitTurn<'_>) -> Result<u64, String> {
        let guard = crate::db::CommitGuard {
            operation_id: commit.operation_id.clone(),
            expected_revision: commit.expected_revision,
        };
        let (path, data, children, usage_upserts) = {
            let mut state = self.state.lock().await;

            // 1. Message-tail delta against the projection.
            let durable_len = state.get_or_project_messages().len();
            let prefix_matches = commit.messages.len() >= durable_len
                && wire_eq(
                    &commit.messages[..durable_len],
                    state.get_or_project_messages(),
                );
            let old_entries_len = state.data.transcript.entries.len();
            let mut children = Vec::new();
            let mut full_rewrite = false;
            if commit.messages.len() > durable_len && prefix_matches {
                let tail = &commit.messages[durable_len..];
                for message in tail {
                    state
                        .data
                        .transcript
                        .push(muta_contracts::TranscriptEntry::from_message(0, message));
                }
                children = admit_subagent_children(&mut state.data, tail);
                state.append_to_projection_cache(tail);
                state.data.updated_at = unix_timestamp();
            } else if !wire_eq(commit.messages, state.get_or_project_messages()) {
                state.data.transcript = rebuild_transcript_from_messages(commit.messages);
                state.data.generation = uuid::Uuid::new_v4().to_string();
                children = admit_subagent_children(&mut state.data, commit.messages);
                state.invalidate_projection_cache();
                state.data.updated_at = unix_timestamp();
                full_rewrite = true;
            }

            // 2. Round counter.
            if let Some(counter) = commit.round_counter
                && counter != state.data.round_counter
            {
                state.data.round_counter = counter;
                state.data.updated_at = unix_timestamp();
            }

            // 3. Usage records — upsert only the records that changed. The
            // durable ledger lives in its own key-addressed table, so the
            // changed records ride to the save as upserts (ADR-0187); the
            // in-memory mirror feeds the token ledger.
            let mut usage_upserts: Vec<muta_contracts::RequestUsageRecord> = Vec::new();
            if !commit.usage_records.is_empty() {
                if commit
                    .usage_records
                    .iter()
                    .any(|record| record.key.session_id != state.data.id)
                {
                    return Err("request usage record belongs to another session".to_string());
                }
                let mut index: std::collections::HashMap<muta_contracts::RequestUsageKey, usize> =
                    state
                        .data
                        .request_usage_records
                        .iter()
                        .enumerate()
                        .map(|(i, record)| (record.key.clone(), i))
                        .collect();
                for record in commit.usage_records {
                    match index.get(&record.key) {
                        Some(&i) => {
                            if state.data.request_usage_records[i] != *record {
                                state.data.request_usage_records[i] = record.clone();
                                usage_upserts.push(record.clone());
                                state.data.updated_at = unix_timestamp();
                            }
                        }
                        None => {
                            index
                                .insert(record.key.clone(), state.data.request_usage_records.len());
                            state.data.request_usage_records.push(record.clone());
                            usage_upserts.push(record.clone());
                            state.data.updated_at = unix_timestamp();
                        }
                    }
                }
            }

            // 4. Retry point (None = untouched, Some(None) = clear, Some(Some(point)) = arm).
            if let Some(target) = commit.retry_point {
                match target {
                    Some(point) => {
                        if state.data.retry_pending.as_ref() != Some(&point) {
                            state.data.retry_pending = Some(point);
                            state.data.updated_at = unix_timestamp();
                        }
                    }
                    None => {
                        if state.data.retry_pending.is_some() {
                            state.data.retry_pending = None;
                            state.data.updated_at = unix_timestamp();
                        }
                    }
                }
            }

            // 5. Round interrupt.
            if let Some(record) = commit.round_interrupt {
                let already = state.data.round_interrupts.iter().any(|existing| {
                    existing.reason == record.reason && existing.round == record.round
                });
                if !already {
                    state.data.round_interrupts.push(record);
                    state.data.updated_at = unix_timestamp();
                }
            }

            state.defer_persist = false;
            let data = if full_rewrite {
                state.data.clone()
            } else {
                let mut delta = state.data.clone_metadata_without_history();
                delta.transcript.entries =
                    state.data.transcript.entries[old_entries_len..].to_vec();
                delta
            };
            (state.path.clone(), data, children, usage_upserts)
        };
        persist_subagent_children(&self.writer, &self.blob_store, &children);
        self.persist_with_usage_guarded(path, data, usage_upserts, guard)
            .await
    }

    /// Durably classify crash residue (ADR-0236 D4): every request attempt
    /// still `InFlight` in this session becomes `Abandoned` with its projected
    /// prompt as a lower bound, committed so every reader sees the resolution
    /// instead of a silently filtered unresolved attempt. Returns the number
    /// of attempts reclassified (0 is the steady state).
    ///
    /// Called by crash recovery after it has read the store-side `InFlight`
    /// signal (which this clears). Idempotent: once reclassified in memory, a
    /// second call finds nothing to settle.
    pub async fn settle_abandoned_attempts(&self) -> Result<usize, String> {
        use muta_contracts::{RequestUsageSource, RequestUsageStatus};
        let (path, data, settled): (PathBuf, SessionData, Vec<muta_contracts::RequestUsageRecord>) = {
            let mut state = self.state.lock().await;
            let mut settled = Vec::new();
            for record in state.data.request_usage_records.iter_mut() {
                if record.status == RequestUsageStatus::InFlight {
                    record.status = RequestUsageStatus::Abandoned;
                    record.source = RequestUsageSource::Estimated;
                    record.prompt_tokens = record.projected_prompt_tokens.max(0);
                    record.total_tokens = record.prompt_tokens;
                    settled.push(record.clone());
                }
            }
            if settled.is_empty() {
                return Ok(0);
            }
            state.data.updated_at = unix_timestamp();
            state.defer_persist = false;
            (
                state.path.clone(),
                state.data.clone_metadata_without_history(),
                settled,
            )
        };
        let count = settled.len();
        self.persist_with_usage_guarded(
            path,
            data,
            settled,
            crate::db::CommitGuard::default(),
        )
        .await?;
        Ok(count)
    }

    /// Commit a model-context projection (ADR-0186): translate the
    /// agent-computed result into append-only directives where the new window
    /// is exactly reproducible, and fall back to a full transcript rebuild
    /// when it is not (correctness over cleverness — the caller's window is
    /// the truth).
    pub async fn commit_context_projection(
        &self,
        result: ContextProjectionResult,
    ) -> Result<(), String> {
        let (path, data) = {
            let mut state = self.state.lock().await;
            let translated = translate_projection(&mut state.data.transcript, &result);
            match translated {
                Some(()) => {}
                None => {
                    // Fallback: the caller's window is the truth; rebuild the
                    // entries and drop the stale directive history. The new
                    // transcript shares nothing with the durable rows, so the
                    // next save rewrites in full (ADR-0187).
                    state.data.transcript = rebuild_transcript_from_messages(&result.model_window);
                    state.data.generation = uuid::Uuid::new_v4().to_string();
                }
            }
            state.data.last_projection = Some(result.checkpoint);
            state.invalidate_projection_cache();
            state.data.updated_at = unix_timestamp();
            state.defer_persist = false;
            (state.path.clone(), state.data.clone())
        };
        self.persist_off_runtime(path, data, self.blob_store.clone())
            .await
    }

    /// Fork the current session: write its state to a new child and
    /// repoint this store at the child in SQLite. Returns `(child_id, parent_id)`.
    pub async fn fork(&self) -> Result<(String, String), String> {
        let (parent_id, child, fork_child_id, child_path) = {
            let state = self.state.lock().await;
            if state.data.transcript.entries.is_empty() {
                return Err("Cannot fork an empty session.".to_string());
            }
            let parent_id = state.data.id.clone();
            let now = unix_timestamp();

            // Build the child snapshot from the parent's current state. Entries
            // are shared by identity; the child merely gains its own memberships.
            let mut child = state.data.clone();
            let fork_child_id = uuid::Uuid::new_v4().to_string();
            child.id = fork_child_id.clone();
            child.parent_id = Some(parent_id.clone());
            child.fork_kind = muta_contracts::SessionForkKind::Fork;
            child.created_at = now;
            child.updated_at = now;
            child.request_usage_records.clear();

            let child_path = self.sessions_dir.join(format!("{fork_child_id}.json"));
            (parent_id, child, fork_child_id, child_path)
        };
        // Blocking I/O stays outside the session lock.
        persist_to(&self.writer, &child, &self.blob_store)?;

        let mut state = self.state.lock().await;
        // Repoint this store at the child; the parent state is already current.
        state.path = child_path;
        state.data = child;
        state.invalidate_projection_cache();
        state.defer_persist = false;
        Ok((fork_child_id, parent_id))
    }

    /// Fork the current session into a **self-contained side session** without
    /// disturbing this store's active pointer (ADR-0017). Returns `(side_id, parent_id)`.
    pub async fn fork_to_side(&self) -> Result<(String, String), String> {
        let (parent_id, side, side_id) = {
            let state = self.state.lock().await;
            if state.data.transcript.entries.is_empty() {
                return Err("Cannot fork an empty session.".to_string());
            }
            let parent_id = state.data.id.clone();
            let now = unix_timestamp();

            let mut side = state.data.clone();
            let side_id = uuid::Uuid::new_v4().to_string();
            side.id = side_id.clone();
            side.parent_id = Some(parent_id.clone());
            side.fork_kind = muta_contracts::SessionForkKind::Aside;
            side.created_at = now;
            side.updated_at = now;
            side.request_usage_records.clear();
            (parent_id, side, side_id)
        };
        // Blocking I/O stays outside the session lock.
        persist_to(&self.writer, &side, &self.blob_store)?;
        Ok((side_id, parent_id))
    }

    /// Construct a live [`SessionStore`] pinned to a side session.
    pub async fn open_side(&self, side_id: &str) -> Result<SessionStore, String> {
        let side_path = self.sessions_dir.join(format!("{side_id}.json"));
        let db_path = self.db_path.clone();
        let workspace = self.workspace.read().unwrap().clone();
        let persona = self.persona.clone();
        let blob_store = BlobStore::new(self.blob_store.root().to_path_buf());
        let reader = self.writer.reader().map_err(|e| e.to_string())?;
        let data = if let Some(data) = reader
            .load_session_full(side_id)
            .map_err(|e| e.to_string())?
        {
            data
        } else if side_path.exists() {
            load_or_seed(
                Some(&reader),
                Some(&self.writer),
                side_id,
                &blob_store,
                workspace.as_ref(),
                persona.as_deref(),
                Some(&side_path),
            )
        } else {
            return Err(format!("Side session '{side_id}' was not found."));
        };
        Ok(SessionStore {
            workspace: std::sync::RwLock::new(workspace),
            persona,
            sessions_dir: self.sessions_dir.clone(),
            db_path,
            blob_store,
            writer: self.writer.clone(),
            state: Mutex::new(SessionState::new(side_path, data, false)),
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
        let (data, id) = {
            let mut state = self.state.lock().await;
            let id = entry.id.clone();
            state.data.tree.insert_entry(entry);
            let messages = state.data.tree.get_context_messages(&id);
            state.data.transcript = rebuild_transcript_from_messages(&messages);
            state.data.generation = uuid::Uuid::new_v4().to_string();
            state.invalidate_projection_cache();
            state.data.updated_at = unix_timestamp();
            (state.data.clone(), id)
        };
        // Blocking I/O stays outside the session lock.
        persist_to(&self.writer, &data, &self.blob_store)?;
        Ok(id)
    }

    /// Switch active leaf in the DAG session tree and update the durable
    /// transcript to the leaf's context.
    pub async fn switch_tree_leaf(&self, target_leaf_id: &str) -> Result<Vec<Message>, String> {
        let (data, messages) = {
            let mut state = self.state.lock().await;
            if !state.data.tree.entries.contains_key(target_leaf_id) {
                return Err(format!("Node '{target_leaf_id}' not found in session tree"));
            }
            state.data.tree.active_leaf_id = Some(target_leaf_id.to_string());
            let messages = state.data.tree.get_context_messages(target_leaf_id);
            state.data.transcript = rebuild_transcript_from_messages(&messages);
            state.data.generation = uuid::Uuid::new_v4().to_string();
            state.invalidate_projection_cache();
            state.data.updated_at = unix_timestamp();
            (state.data.clone(), messages)
        };
        // Blocking I/O stays outside the session lock.
        persist_to(&self.writer, &data, &self.blob_store)?;
        Ok(messages)
    }

    /// Project the current session state into canonical [`muta_contracts::SessionIR`] (ADR-0241).
    pub async fn session_ir(&self) -> muta_contracts::SessionIR {
        let state = self.state.lock().await;
        super::ir_bridge::session_data_to_ir(&state.data)
    }

    /// Commit mutations from a [`muta_contracts::SessionIR`] back into the session store.
    pub async fn commit_session_ir(&self, ir: &muta_contracts::SessionIR) -> Result<(), String> {
        let data = {
            let mut state = self.state.lock().await;
            super::ir_bridge::apply_ir_to_session_data(ir, &mut state.data);
            state.data.generation = uuid::Uuid::new_v4().to_string();
            state.invalidate_projection_cache();
            state.data.updated_at = unix_timestamp();
            state.data.clone()
        };
        persist_to(&self.writer, &data, &self.blob_store)?;
        Ok(())
    }

    /// Compile a model request directly from the session's in-memory IR
    /// using the 4-pass optimizing compiler pipeline (ADR-0241).
    pub async fn compile_request(
        &self,
        options: muta_contracts::CompilerOptions,
    ) -> Result<muta_contracts::CompilationArtifact, muta_contracts::CompilerError> {
        let ir = self.session_ir().await;
        muta_contracts::compile_session_request(&ir, options)
    }
}

/// Intercept subagent results in an admission delta (ADR-0186 §6): each nested
/// transcript becomes a **subagent session** (its own `sessions` row, facts
/// shared by identity), and the parent's tool entry gains a `SubagentRef`
/// pointer. The in-memory `Message` keeps its `children` for the live view;
/// the persisted entry carries only the pointer. The subagent rows are
/// *returned*, not written here: the caller persists them after releasing the
/// session lock so no blocking I/O runs under the lock (ADR-0187).
fn admit_subagent_children(state: &mut SessionData, candidates: &[Message]) -> Vec<SessionData> {
    let mut subagents = Vec::new();
    for message in candidates {
        let (Some(children), Some(subagent_meta)) =
            (message.children.as_ref(), message.subagent_meta.as_ref())
        else {
            continue;
        };
        if children.is_empty() {
            continue;
        }
        let subagent_id = uuid::Uuid::new_v4().to_string();
        let mut subagent = SessionData {
            id: subagent_id.clone(),
            parent_id: Some(state.id.clone()),
            fork_kind: muta_contracts::SessionForkKind::Subagent,
            workspace: state.workspace.clone(),
            persona: state.persona.clone(),
            ..Default::default()
        };
        subagent.transcript = rebuild_transcript_from_messages(children);
        subagents.push(subagent);
        // Stamp the pointer on the already-admitted parent entry (the newest
        // tool result matching this subagent result's content).
        let call_id = message.tool_call_id.clone();
        if let Some(entry) = state
            .transcript
            .entries
            .iter_mut()
            .rev()
            .find(|entry| {
                matches!(&entry.payload, muta_contracts::EntryPayload::Message(payload)                     if payload.tool_call_id == call_id)
            })
            && let muta_contracts::EntryPayload::Message(payload) = &mut entry.payload {
                payload.subagent = Some(muta_contracts::SubagentRef {
                    session_id: subagent_id,
                    description: subagent_meta.description.clone(),
                    duration_ms: subagent_meta.duration_ms,
                    toolset_count: Some(subagent_meta.toolset_count),
                });
            }
    }
    subagents
}

/// Persist admitted subagent sessions. Called after the session lock is
/// released; failures leave the parent entry's pointer dangling, which the
/// load path reports rather than silently dropping the run.
fn persist_subagent_children(
    writer: &crate::db::PersistenceHandle,
    blob_store: &BlobStore,
    subagents: &[SessionData],
) {
    for subagent in subagents {
        if let Err(error) = crate::session::persist_to(writer, subagent, blob_store) {
            tracing::warn!(%error, subagent = %subagent.id, "could not persist subagent session; nested transcript is dropped");
        }
    }
}

/// Rebuild a transcript from a flat message window (entries get fresh ids;
/// projection history is cleared). The caller must mint a fresh
/// `SessionData::generation` afterwards: the new transcript shares no rows
/// with the durable one (ADR-0187).
pub(crate) fn rebuild_transcript_from_messages(messages: &[Message]) -> muta_contracts::Transcript {
    let mut transcript = muta_contracts::Transcript::new();
    for message in messages {
        transcript.push(muta_contracts::TranscriptEntry::from_message(0, message));
    }
    transcript
}

/// Translate a projection result into append-only directives **iff** the
/// resulting projection is exactly `result.model_window` (ADR-0186). Returns
/// `None` when translation is not possible and the caller must rebuild.
fn translate_projection(
    transcript: &mut muta_contracts::Transcript,
    result: &ContextProjectionResult,
) -> Option<()> {
    let current = transcript.project_messages();
    let target = &result.model_window;

    // Compact: the new window must be [fresh checkpoint] + tail(current).
    let checkpoint_index = target.iter().position(|message| {
        message
            .origin
            .as_ref()
            .is_some_and(|o| o.kind == InjectionKind::CompactionCheckpoint)
    });
    if let Some(checkpoint_index) = checkpoint_index {
        let (checkpoint, tail) = target.split_at(checkpoint_index + 1);
        let tail_matches =
            tail.len() <= current.len() && wire_eq(&current[current.len() - tail.len()..], tail);
        // The archived range is the current projection's head that the caller
        // reports as archived; its length anchors the directive.
        let archived_len = result.archived_originals.len();
        let head_matches = archived_len <= current.len()
            && wire_eq(&current[..archived_len], &result.archived_originals);
        if tail_matches && head_matches && checkpoint.len() == 1 {
            // The compact range ends at the last archived entry; the
            // checkpoint lands after it and the preserved tail stays visible.
            let last_seq = if archived_len == 0 {
                0
            } else {
                transcript.entries[archived_len - 1].seq
            };
            let checkpoint_entry = muta_contracts::TranscriptEntry::from_message(0, &checkpoint[0]);
            transcript.push(checkpoint_entry);
            let checkpoint_seq = transcript
                .entries
                .last()
                .map(|entry| entry.seq)
                .unwrap_or(0);
            transcript.push_directive(muta_contracts::ProjectionDirective {
                seq: 0,
                kind: muta_contracts::DirectiveKind::Compact,
                up_to_seq: last_seq,
                payload: muta_contracts::DirectivePayload::Compact { checkpoint_seq },
            });
            if wire_eq(&transcript.project_messages(), target) {
                return Some(());
            }
            // Directive application diverged; undo is impossible (append-only),
            // so fall through to a rebuild.
        }
    }

    // Prune: same length, only tool-result bodies replaced.
    if current.len() == target.len() && !current.is_empty() {
        let mut elided = Vec::new();
        let mut pruneable = true;
        for (before, after) in current.iter().zip(target.iter()) {
            if before == after {
                continue;
            }
            let is_tool_result = before.role == Role::Tool
                && before.tool_call_id == after.tool_call_id
                && before.tool_calls == after.tool_calls
                && before.role == after.role;
            if !is_tool_result {
                pruneable = false;
                break;
            }
            elided.push(muta_contracts::PrunedToolOutput {
                tool_call_id: after.tool_call_id.clone().unwrap_or_default(),
                placeholder: after.content.clone(),
            });
        }
        if pruneable && !elided.is_empty() {
            let last_seq = transcript.next_seq().saturating_sub(1);
            transcript.push_directive(muta_contracts::ProjectionDirective {
                seq: 0,
                kind: muta_contracts::DirectiveKind::Prune,
                up_to_seq: last_seq,
                payload: muta_contracts::DirectivePayload::Prune { elided },
            });
            if wire_eq(&transcript.project_messages(), target) {
                return Some(());
            }
        }
    }

    None
}
