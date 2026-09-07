//! Typed read/write accessors over session fields (ADR-0186): the model
//! window derives from the transcript, the todo list derives from `state`
//! entries, and the remaining working state lives on the session row.

use super::*;

impl SessionStore {
    /// The projected view (ADR-0186): what the next provider request starts
    /// from. The single source of truth for message truth is the transcript;
    /// this is its pure derivation.
    pub async fn model_window(&self) -> Vec<Message> {
        self.state.lock().await.data.transcript.project_messages()
    }

    /// The full factual transcript — every message entry, unprojected (no
    /// prune placeholders, no compaction checkpoint substitution). Consumers
    /// that need the real content (summarizers, review, export) read this.
    pub async fn full_transcript(&self) -> Vec<Message> {
        self.state
            .lock()
            .await
            .data
            .transcript
            .entries
            .iter()
            .filter_map(|entry| entry.to_message())
            .collect()
    }

    /// The current todo list, derived from the newest `state` entry.
    pub async fn todos(&self) -> muta_contracts::TodoList {
        self.state
            .lock()
            .await
            .data
            .transcript
            .derive_todos()
            .unwrap_or_default()
    }

    /// Mirror the agent's todo list into the transcript as a `state` entry
    /// (latest wins on derivation) and persist.
    pub async fn set_todos(&self, todos: muta_contracts::TodoList) -> Result<(), String> {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            state
                .data
                .transcript
                .push(muta_contracts::TranscriptEntry::from_state(
                    0,
                    Some(todos),
                ));
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

    /// The last projection decision, reconstructed from the directive
    /// history (latest `compact` or `prune` wins).
    pub async fn last_projection(&self) -> Option<ContextProjectionCheckpoint> {
        let state = self.state.lock().await;
        let transcript = &state.data.transcript;
        let directive = transcript.directives.last()?;
        let (operation, archived_messages) = match directive.kind {
            muta_contracts::DirectiveKind::Compact => (
                ContextProjectionKind::Compact,
                transcript.projected_out().len(),
            ),
            _ => (
                ContextProjectionKind::Prune,
                transcript.entries.len().saturating_sub(transcript.project().len()),
            ),
        };
        let window = transcript.project();
        Some(ContextProjectionCheckpoint {
            operation,
            archived_messages,
            active_messages: window.len(),
            window_tokens_before: 0,
            window_tokens_after: estimate_tokens(
                &window.iter().map(|(_, m)| m).cloned().collect::<Vec<_>>(),
            ),
        })
    }

    /// The session title. The second element reports whether a title exists
    /// (ADR-0186: a non-`NULL` title is terminal — AI generation only fills
    /// `None`, so "has title" doubles as the former manual-lock signal).
    pub async fn title(&self) -> (Option<String>, bool) {
        let state = self.state.lock().await;
        let title = state.data.title.clone();
        let has_title = title.is_some();
        (title, has_title)
    }

    /// Set (or clear) the title. A non-`NULL` title is terminal; AI
    /// generation must only call this when the title is currently `None`.
    pub async fn set_title(&self, title: Option<String>, _manual: bool) -> Result<(), String> {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            state.data.title = title;
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

    /// Entries projected out of the view by the current directives — the
    /// recoverable originals.
    pub async fn archived_transcript_count(&self) -> usize {
        self.state.lock().await.data.transcript.projected_out().len()
    }

    pub async fn digest(&self) -> (Option<muta_contracts::SessionDigest>, Option<u64>) {
        let state = self.state.lock().await;
        (state.data.digest.clone(), state.data.digest_anchor)
    }

    pub async fn set_digest(
        &self,
        digest: Option<muta_contracts::SessionDigest>,
        anchor: Option<u64>,
    ) -> Result<(), String> {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            state.data.digest = digest;
            state.data.digest_anchor = anchor.filter(|_| state.data.digest.is_some());
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

    pub async fn disabled_tools(&self) -> std::collections::HashSet<String> {
        self.state.lock().await.data.disabled_tools.clone()
    }

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

    pub async fn unattended(&self) -> bool {
        self.state.lock().await.data.unattended
    }

    pub async fn set_unattended(&self, enabled: bool) -> Result<(), String> {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
            state.data.unattended = enabled;
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

    pub async fn round_counter(&self) -> u64 {
        self.state.lock().await.data.round_counter
    }

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

    pub async fn request_usage_records(&self) -> Vec<muta_contracts::RequestUsageRecord> {
        self.state.lock().await.data.request_usage_records.clone()
    }

    pub async fn set_request_usage_records(
        &self,
        records: Vec<muta_contracts::RequestUsageRecord>,
    ) -> Result<(), String> {
        let (path, data, should_persist) = {
            let mut state = self.state.lock().await;
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
    /// global default". Connection-level pinning (provider+account+endpoint)
    /// is the ADR-0186 target shape; the pin upgrades when the selection
    /// machinery migrates.
    pub async fn provider_selection(&self) -> Option<ProviderSelection> {
        self.state.lock().await.data.provider_selection.clone()
    }

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
