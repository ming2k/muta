//! Typed read/write accessors over session fields (todos, titles, usage records, provider selection, projection checkpoints, ...).

use super::*;

impl SessionStore {
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

    /// Replace the task list.
    pub async fn set_todos(&self, todos: muta_contracts::TodoList) -> Result<(), String> {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            state.data.todos = todos;
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
            state.data.scheduled_jobs = jobs;
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
    /// `manual = false` to clear.
    pub async fn set_title(&self, title: Option<String>, manual: bool) -> Result<(), String> {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            state.data.title = title;
            state.data.title_manual = manual;
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

    pub async fn archived_transcript_count(&self) -> usize {
        self.state.lock().await.data.archived_transcript.len()
    }

    /// The Chronicler's digest and the transcript anchor (char count) it
    /// was generated at. `(None, None)` for a session without a digest yet.
    pub async fn digest(&self) -> (Option<muta_contracts::SessionDigest>, Option<u64>) {
        let state = self.state.lock().await;
        (state.data.digest.clone(), state.data.digest_anchor)
    }

    /// Replace the session digest together with the transcript anchor it
    /// was generated at (the refresh throttle's watermark). `digest = None`
    /// clears both.
    pub async fn set_digest(
        &self,
        digest: Option<muta_contracts::SessionDigest>,
        anchor: u64,
    ) -> Result<(), String> {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            state.data.digest = digest;
            state.data.digest_anchor = if state.data.digest.is_some() { Some(anchor) } else { None };
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
            state.data.disabled_tools = tools;
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

    /// The session-scoped delegated-autonomous posture. `false` = attended (default).
    pub async fn delegated(&self) -> bool {
        self.state.lock().await.data.delegated
    }

    /// Replace the delegated-autonomous posture. Mirrors `Agent::delegated()` so a
    /// daemon restart restores the session in the posture it died in.
    pub async fn set_delegated(&self, enabled: bool) -> Result<(), String> {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            if state.data.delegated == enabled {
                return Ok(());
            }
            state.data.delegated = enabled;
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

    /// Durable lifecycle-aware request accounting for the active session.
    pub async fn request_usage_records(&self) -> Vec<muta_contracts::RequestUsageRecord> {
        self.state.lock().await.data.request_usage_records.clone()
    }

    /// Replace the session's request ledger. Callers pass records already
    /// scoped to the active session; the store validates that boundary before
    /// updating.
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
            state.data.request_usage_records = records;
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

    /// The session-scoped provider + model pin (C6). `None` means "follow the
    /// global default"; the harness seeds this on first `/models` switch.
    pub async fn provider_selection(&self) -> Option<ProviderSelection> {
        self.state.lock().await.data.provider_selection.clone()
    }

    /// Replace the session-scoped provider + model pin (C6). Persists to SQLite
    /// so resume restores the session's own provider instead of the global default.
    pub async fn set_provider_selection(
        &self,
        selection: Option<ProviderSelection>,
    ) -> Result<(), String> {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            state.data.provider_selection = selection;
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
}
