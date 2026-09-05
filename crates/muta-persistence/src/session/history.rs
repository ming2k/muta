//! Transcript append/replace/mutation, round and interrupt records, retry bookkeeping, fork/lineage queries, and the session tree.

use super::*;

/// One unified turn commit for [`SessionStore::commit_turn`]. `messages` is
/// the full current window (O(delta) against the durable prefix);
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
}

impl<'a> CommitTurn<'a> {
    pub fn new(messages: &'a [Message]) -> Self {
        Self {
            messages,
            round_counter: None,
            usage_records: &[],
            retry_point: None,
            round_interrupt: None,
        }
    }
}

impl SessionStore {
    /// The durable round-interrupt records (C11): one per round stopped
    /// before completing, newest last. Pure projection state — never part
    /// of the model-visible transcript.
    pub async fn round_interrupts(&self) -> Vec<muta_contracts::RoundInterrupt> {
        self.state.lock().await.data.round_interrupts.clone()
    }

    /// Append one round-interrupt record (C11). Called on every round stop
    /// path with the observed reason and a wall-clock timestamp. Best-effort
    /// duplicate guard: a record for the same round with the same reason is
    /// not appended twice.
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

    /// Clear every round-interrupt record (C11). Called when the interrupted
    /// round's outcome is superseded — the user re-sent and the fresh round
    /// completed, or the session moved on.
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

    /// The durable `/retry` resume point (C12): `Some` while a stopped round
    /// is parked for `/retry`, `None` once it completed or was retired. Pure
    /// projection state — never part of the model-visible transcript.
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

    pub async fn replace_messages(&self, messages: Vec<Message>) -> Result<(), String> {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            state.data.model_window = messages;
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

    /// Apply `f` to the live message window under the session lock and persist — atomically (ADR-0048).
    pub async fn mutate_messages<F>(&self, f: F) -> Result<(), String>
    where
        F: FnOnce(&mut Vec<Message>),
    {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            f(&mut state.data.model_window);
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

    /// The durable command ledger (ADR-0091): every slash command and `!cmd`
    /// invocation with its typed result, in invocation order. Commands live
    /// here, not in the message stream, so resume/export/audit reconstruct
    /// them without polluting the dialogue.
    pub async fn commands(&self) -> Vec<muta_contracts::CommandRecord> {
        let state = self.state.lock().await;
        state.data.commands.clone()
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

    /// Incrementally persist new messages appended since the last durable write (ADR-0048).
    pub async fn append_turn(&self, current: &[Message]) -> Result<(), String> {
        let (path, data) = {
            let mut state = self.state.lock().await;
            let baseline = state.data.model_window.len();
            if current.len() <= baseline {
                return Ok(());
            }
            let durable_len = state.data.model_window.len();
            let prefix_matches = current.len() >= durable_len
                && current[..durable_len] == state.data.model_window[..];

            if !prefix_matches {
                tracing::warn!(
                    baseline,
                    incoming = current.len(),
                    "append_turn: incoming prefix diverged from durable state; full replace"
                );
                state.data.model_window = current.to_vec();
            } else {
                let delta = current[baseline..].to_vec();
                state.data.model_window.extend(delta);
            }
            state.data.updated_at = unix_timestamp();
            state.defer_persist = false;
            (state.path.clone(), state.data.clone())
        };
        self.persist_off_runtime(path, data, self.blob_store.clone()).await
    }

    /// Commit everything a finished ReAct turn changed, in **one** lock
    /// acquisition and at most **one** snapshot write to SQLite.
    pub async fn commit_turn(&self, commit: CommitTurn<'_>) -> Result<(), String> {
        #[allow(clippy::large_enum_variant)]
        enum Persist {
            None,
            Snapshot { path: PathBuf, data: SessionData },
        }
        let persist = {
            let mut state = self.state.lock().await;
            let mut persist = Persist::None;
            let mut dirty = false;

            // 1. Message-tail delta.
            let durable_len = state.data.model_window.len();
            let prefix_matches = commit.messages.len() >= durable_len
                && commit.messages[..durable_len] == state.data.model_window[..];
            if commit.messages.len() > durable_len && prefix_matches {
                let delta = commit.messages[durable_len..].to_vec();
                state.data.model_window.extend(delta);
                state.data.updated_at = unix_timestamp();
                dirty = true;
            } else if commit.messages.len() != durable_len || !prefix_matches {
                state.data.model_window = commit.messages.to_vec();
                state.data.updated_at = unix_timestamp();
                dirty = true;
            }

            // 2. Round counter.
            if let Some(counter) = commit.round_counter
                && counter != state.data.round_counter
            {
                state.data.round_counter = counter;
                state.data.updated_at = unix_timestamp();
                dirty = true;
            }

            // 3. Usage records — upsert only the records that changed.
            if !commit.usage_records.is_empty() {
                if commit
                    .usage_records
                    .iter()
                    .any(|record| record.key.session_id != state.data.id)
                {
                    return Err("request usage record belongs to another session".to_string());
                }
                let mut changed: Vec<muta_contracts::RequestUsageRecord> = Vec::new();
                for record in commit.usage_records {
                    let differs = state
                        .data
                        .request_usage_records
                        .iter()
                        .any(|existing| existing.key == record.key && existing != record);
                    let is_new = !state
                        .data
                        .request_usage_records
                        .iter()
                        .any(|existing| existing.key == record.key);
                    if differs || is_new {
                        changed.push(record.clone());
                    }
                }
                if !changed.is_empty() {
                    for record in changed {
                        match state
                            .data
                            .request_usage_records
                            .iter_mut()
                            .find(|existing| existing.key == record.key)
                        {
                            Some(existing) => *existing = record,
                            None => state.data.request_usage_records.push(record),
                        }
                    }
                    state.data.updated_at = unix_timestamp();
                    dirty = true;
                }
            }

            // 4. Retry point (None = untouched, Some(None) = clear, Some(Some(point)) = arm).
            if let Some(target) = commit.retry_point {
                match target {
                    Some(point) => {
                        if state.data.retry_pending.as_ref() != Some(&point) {
                            state.data.retry_pending = Some(point);
                            state.data.updated_at = unix_timestamp();
                            dirty = true;
                        }
                    }
                    None => {
                        if state.data.retry_pending.is_some() {
                            state.data.retry_pending = None;
                            state.data.updated_at = unix_timestamp();
                            dirty = true;
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
                    dirty = true;
                }
            }

            if dirty {
                state.defer_persist = false;
                let path = state.path.clone();
                let data = state.data.clone();
                persist = Persist::Snapshot { path, data };
            }
            persist
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
                .extend(result.archived_originals);
            state.data.model_window = result.model_window;
            state.data.last_projection = Some(result.checkpoint);
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
        child.request_usage_records.clear();

        let child_path = self.sessions_dir.join(format!("{fork_id}.json"));
        persist_to(&self.db_path, &child, &self.blob_store)?;

        // Repoint this store at the child; the parent file is already current.
        state.path = child_path;
        state.data = child;
        state.defer_persist = false;
        Ok((fork_id, parent_id))
    }

    /// Fork the current session into a **self-contained side session** without
    /// disturbing this store's active pointer (ADR-0017). Returns `(side_id, parent_id)`.
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

        persist_to(&self.db_path, &side, &self.blob_store)?;
        Ok((side_id, parent_id))
    }

    /// Construct a live [`SessionStore`] pinned to a side session.
    pub async fn open_side(&self, side_id: &str) -> Result<SessionStore, String> {
        let side_path = self.sessions_dir.join(format!("{side_id}.json"));
        let db_path = self.db_path.clone();
        let project_root = self.project_root.clone();
        let blob_store = BlobStore::new(self.blob_store.root().to_path_buf());
        let engine = crate::db::DatabaseEngine::open(&db_path, Some(blob_store.clone()))
            .map_err(|e| e.to_string())?;
        let data = if let Some(data) = engine.load_session_full(side_id).map_err(|e| e.to_string())? {
            data
        } else if side_path.exists() {
            load_or_seed(&db_path, side_id, &blob_store, &project_root, Some(&side_path))
        } else {
            return Err(format!("Side session '{side_id}' was not found."));
        };
        Ok(SessionStore {
            project_root,
            sessions_dir: self.sessions_dir.clone(),
            db_path,
            blob_store,
            writer: self.writer.clone(),
            state: Mutex::new(SessionState {
                path: side_path,
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
        persist_to(&self.db_path, &state.data, &self.blob_store)?;
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
        persist_to(&self.db_path, &state.data, &self.blob_store)?;
        Ok(messages)
    }
}
