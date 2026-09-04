//! Construction, load/persist, snapshots, event-log replay and compaction trigger, the armed-schedule disk scan, the list/detail/active views, and the offline corruption scan tools of [`SessionStore`].

use super::*;

impl SessionStore {
    /// Open a per-project store pinned to a **fresh** session file.
    ///
    /// Under ADR-0168 all session state is stored authoritatively in SQLite (`muta.db`).
    pub fn load_for_project(project_root: PathBuf) -> Self {
        // Establish one physical identity at the persistence boundary.
        let project_root = project_root.canonicalize().unwrap_or(project_root);
        let dirs = paths::get();
        let sessions_dir = dirs.project_sessions_dir(&project_root);
        if let Err(e) = std::fs::create_dir_all(&sessions_dir) {
            tracing::warn!(error = %e, "could not create project sessions dir");
        }
        let db_path = dirs.db_file();
        if let Some(parent) = db_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let blob_store = BlobStore::new(dirs.blobs_dir());

        Self::pin_fresh(project_root, sessions_dir, db_path, blob_store)
    }

    /// Backwards-compatible alias for [`Self::load_for_project`] using the
    /// current process cwd.
    pub fn load() -> Self {
        let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self::load_for_project(project_root)
    }

    /// Open a `SessionStore` pinned to an explicit snapshot `path`.
    /// In the unified SQLite architecture, `sessions_dir` hosts `muta.db`.
    pub fn for_path(path: PathBuf) -> Self {
        let sessions_dir = path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let event_log_path = path.with_extension("jsonl");
        let project_root = sessions_dir.clone();
        let db_path = sessions_dir.join("muta.db");
        let blob_store = BlobStore::new(sessions_dir.join("blobs"));
        let id_stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("default");
        let data = load_or_seed(&db_path, id_stem, &blob_store, &project_root, Some(&path));
        let event_log = EventLog::new(event_log_path);
        let defer_persist = !path.exists() && data.is_user_facing_empty();
        Self {
            project_root,
            sessions_dir,
            db_path,
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
    /// `sessions_dir`. The session is **not** written until the session gains
    /// real content, so a `muta` that starts and exits without a round
    /// leaves no empty-file litter behind.
    fn pin_fresh(
        project_root: PathBuf,
        sessions_dir: PathBuf,
        db_path: PathBuf,
        blob_store: BlobStore,
    ) -> Self {
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
            db_path,
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
    pub(crate) fn should_skip_persist(state: &SessionState) -> bool {
        state.defer_persist && !state.path.exists() && state.data.is_user_facing_empty()
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
        let db_path = self.db_path.clone();
        let project_root = self.project_root.clone();
        let blob_store = self.blob_store.clone();
        let load_path = path.clone();
        let resolved_id = resolved.clone();
        let data = tokio::task::spawn_blocking(move || {
            load_or_seed(&db_path, &resolved_id, &blob_store, &project_root, Some(&load_path))
        })
        .await
        .map_err(|e| format!("session open task failed: {e}"))?;
        state.path = path;
        state.event_log = EventLog::new(state.path.with_extension("jsonl"));
        state.data = data;
        state.defer_persist = false;
        Ok(())
    }

    /// Delete a session by id or short id prefix.
    pub async fn delete(&self, id: &str) -> Result<String, String> {
        let (resolved, snapshot, is_active) = {
            let state = self.state.lock().await;
            let (resolved, path) = self.resolve_session(id, &state)?;
            (resolved.clone(), path, state.data.id == resolved)
        };

        let db_path = self.db_path.clone();
        let resolved_for_task = resolved.clone();
        let log = snapshot.with_extension("jsonl");
        tokio::task::spawn_blocking(move || {
            let engine = crate::db::DatabaseEngine::open(&db_path, None)
                .map_err(|e| e.to_string())?;
            let db_deleted = engine.delete_session(&resolved_for_task).map_err(|e| e.to_string())?;

            let file_deleted = snapshot.exists() || log.exists();
            let _ = fs::remove_file(&snapshot);
            let _ = fs::remove_file(&log);

            if !db_deleted && !file_deleted {
                return Err(format!(
                    "Could not delete session '{}': session not found.",
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

    /// Set (or clear) a session's manual title by id or short id prefix.
    pub async fn rename(&self, id: &str, title: Option<String>) -> Result<(), String> {
        let manual = title.is_some();
        let (resolved, is_active) = {
            let state = self.state.lock().await;
            let (resolved, _) = self.resolve_session(id, &state)?;
            (resolved.clone(), state.data.id == resolved)
        };
        if is_active {
            return self.set_title(title, manual).await;
        }
        let db_path = self.db_path.clone();
        let title_opt = title.clone();
        tokio::task::spawn_blocking(move || {
            let engine = crate::db::DatabaseEngine::open(&db_path, None)
                .map_err(|e| e.to_string())?;
            engine
                .rename_session(&resolved, title_opt.as_deref(), manual)
                .map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .map_err(|e| format!("rename task failed: {e}"))?
    }

    /// The picker-style summary of the pinned session, synthesized from
    /// in-memory state.
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
            digest: data.digest.clone(),
        }
    }

    pub async fn list(&self) -> Result<Vec<SessionSummary>, String> {
        let active_id = self.state.lock().await.data.id.clone();
        let db_path = self.db_path.clone();
        let sessions_dir = self.sessions_dir.clone();
        let project_root_str = self.project_root.to_string_lossy().into_owned();
        let blob_store = self.blob_store.clone();
        tokio::task::spawn_blocking(move || {
            if sessions_dir.exists() {
                if let Ok(entries) = fs::read_dir(&sessions_dir) {
                    for entry in entries.flatten() {
                        let path = entry.path();
                        if path.extension().and_then(|s| s.to_str()) == Some("json") {
                            if let Ok(content) = fs::read_to_string(&path) {
                                if let Ok(session) = serde_json::from_str::<serde_json::Value>(&content) {
                                    let is_empty = session.get("model_window").and_then(|v| v.as_array()).map(|a| a.is_empty()).unwrap_or(true)
                                        && session.get("archived_transcript").and_then(|v| v.as_array()).map(|a| a.is_empty()).unwrap_or(true);
                                    let sess_id = session.get("id").and_then(|v| v.as_str()).unwrap_or("");
                                    if is_empty && sess_id != active_id {
                                        let _ = fs::remove_file(&path);
                                        let _ = fs::remove_file(path.with_extension("jsonl"));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            let engine = crate::db::DatabaseEngine::open(&db_path, Some(blob_store))
                .map_err(|e| e.to_string())?;
            engine.list_session_summaries(Some(&project_root_str), &active_id)
                .map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| format!("session list task failed: {e}"))?
    }

    /// Full detail for one session, requested on demand by the session-info sub-view.
    pub async fn detail(&self, id: &str) -> Result<SessionDetail, String> {
        let active_id = self.state.lock().await.data.id.clone();
        let (resolved, _) = {
            let state = self.state.lock().await;
            self.resolve_session(id, &state)?
        };
        let db_path = self.db_path.clone();
        let blob_store = self.blob_store.clone();
        tokio::task::spawn_blocking(move || {
            let engine = crate::db::DatabaseEngine::open(&db_path, Some(blob_store))
                .map_err(|e| e.to_string())?;
            engine
                .get_session_detail(&resolved, &active_id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("Session '{resolved}' not found."))
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
    pub(crate) async fn persist_off_runtime(
        &self,
        path: PathBuf,
        data: SessionData,
        blob_store: BlobStore,
    ) -> Result<(), String> {
        let _persist_guard = self.persist_gate.lock().await;
        let db_path = self.db_path.clone();
        let log_path = path.with_extension("jsonl");
        tokio::task::spawn_blocking(move || {
            let mut data = data;
            if log_path.exists() {
                let event_log = EventLog::new(log_path.clone());
                if let Some(high) = event_log.high_seq() {
                    data.applied_seq = Some(high);
                }
            }
            compact_log_if_needed(&log_path, &data)?;
            persist_to(&db_path, &data, &blob_store)
        })
        .await
        .map_err(|e| format!("session persist task failed: {e}"))?
    }

    /// Write `data` to the store's SQLite database.
    #[cfg(test)]
    pub(crate) fn persist_archive(&self, data: &SessionData) -> Result<(), String> {
        persist_to(&self.db_path, data, &self.blob_store)
    }

    /// Resolve `input` (a 4+ char hex id or prefix) to the full session id
    /// and the path that identifies it.
    pub(crate) fn resolve_session(
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

        // Query SQLite database
        if let Ok(engine) = crate::db::DatabaseEngine::open(&self.db_path, None) {
            let project_root_str = self.project_root.to_string_lossy();
            if let Ok(found) = engine.resolve_session_prefix(input, Some(&project_root_str)) {
                for id in found {
                    if !matches.iter().any(|(m_id, _)| m_id == &id) {
                        let path = self.sessions_dir.join(format!("{id}.json"));
                        matches.push((id, path));
                    }
                }
            }
        }

        // Fallback to scanning legacy sessions directory
        if matches.is_empty() && self.sessions_dir.exists() {
            if let Ok(entries) = fs::read_dir(&self.sessions_dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|s| s.to_str()) != Some("json") {
                        continue;
                    }
                    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                        continue;
                    };
                    if stem.starts_with(input) && !matches.iter().any(|(id, _)| id == stem) {
                        matches.push((stem.to_string(), path));
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
