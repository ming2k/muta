//! Transcript append/replace/mutation, round and interrupt records, retry bookkeeping, fork/lineage queries, and the session tree.

use super::*;

/// One unified turn commit for [`SessionStore::commit_turn`]. `messages` is
/// the full current window (O(delta) against the durable prefix);
/// `round_counter` is committed only when it differs; `usage_records` are
/// upserted only where they changed.
pub struct CommitTurn<'a> {
    pub messages: &'a [Message],
    pub round_counter: Option<u64>,
    pub usage_records: &'a [muta_contracts::RequestUsageRecord],
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
        let (path, data) = {
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
            } else {
                let delta = current[baseline..].to_vec();
                state.data.model_window.extend(delta.clone());
                state.data.updated_at = unix_timestamp();
                ensure_event_log_started(&state.event_log, &state.data)?;
                state
                    .event_log
                    .append(SessionEvent::MessagesAppended { messages: delta })?;
            }
            (state.path.clone(), state.data.clone())
        };
        self.persist_off_runtime(path, data, self.blob_store.clone()).await
    }

    /// Commit everything a finished ReAct turn changed, in **one** lock
    /// acquisition and at most **one** snapshot write:
    ///
    /// - `messages`: O(delta) `MessagesAppended` event for the new tail
    ///   (identical semantics to [`Self::append_turn`]);
    /// - `round_counter`: `RoundCounterSet` event when it advanced;
    /// - `usage_records`: `RequestUsageUpsert` per changed attempt record.
    ///
    /// This is the persistence shape the turn loop actually needs — the old
    /// flow issued the same three mutations as three separate setters, each
    /// acquiring the state lock and each writing a full session snapshot, so
    /// a turn with usage settlement paid three full JSON serializations and
    /// two extra fsync pairs per turn. The event log stays authoritative for
    /// replay; the snapshot is a cache that now refreshes once per turn.
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

            // 1. Message-tail delta (append_turn semantics, verbatim).
            let durable_len = state.data.model_window.len();
            let prefix_matches = commit.messages.len() >= durable_len
                && commit.messages[..durable_len]
                    .iter()
                    .zip(&state.data.model_window)
                    .all(|(incoming, durable)| {
                        // Compare the serialized forms: this is a
                        // persistence-layer equivalence question ("would the
                        // event log hold the same bytes?"), so JSON equality
                        // is the honest definition — no new trait surface on
                        // the contracts `Message`.
                        match (
                            serde_json::to_string(incoming),
                            serde_json::to_string(durable),
                        ) {
                            (Ok(a), Ok(b)) => a == b,
                            (Err(a), Err(b)) => a.to_string() == b.to_string(),
                            _ => false,
                        }
                    });
            if commit.messages.len() > durable_len && prefix_matches {
                let delta = commit.messages[durable_len..].to_vec();
                state.data.model_window.extend(delta.clone());
                state.data.updated_at = unix_timestamp();
                ensure_event_log_started(&state.event_log, &state.data)?;
                state
                    .event_log
                    .append(SessionEvent::MessagesAppended { messages: delta })?;
                dirty = true;
            } else if commit.messages.len() != durable_len || !prefix_matches {
                // Diverged (compaction/fork/replay or in-place edit/freeze) — full replace, exactly as
                // `append_turn` does.
                ensure_event_log_started(&state.event_log, &state.data)?;
                state.event_log.append(SessionEvent::MessagesReplaced {
                    messages: commit.messages.to_vec(),
                })?;
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
                state
                    .event_log
                    .append(SessionEvent::RoundCounterSet { counter })?;
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
                    if differs {
                        changed.push(record.clone());
                    }
                }
                if !changed.is_empty() {
                    for record in changed {
                        state.event_log.append(SessionEvent::RequestUsageUpsert {
                            record: record.clone(),
                        })?;
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

            if dirty {
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
        persist_to(&self.db_path, &child, &self.blob_store)?;

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

        persist_to(&self.db_path, &side, &self.blob_store)?;

        // Deliberately do NOT mutate `state` — the primary keeps its active
        // pointer, history, and in-flight turn intact.
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
        let event_log = EventLog::new(side_path.with_extension("jsonl"));
        Ok(SessionStore {
            project_root,
            sessions_dir: self.sessions_dir.clone(),
            db_path,
            blob_store,
            writer: self.writer.clone(),
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
