//! Construction, load/persist, snapshots, event-log replay and compaction trigger, the armed-schedule disk scan, the list/detail/active views, and the offline corruption scan tools of [`SessionStore`].

use super::*;

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
            digest: data.digest.clone(),
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
                digest: header.digest.clone(),
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
    pub(crate) async fn persist_off_runtime(
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
    pub(crate) fn persist_archive(&self, data: &SessionData) -> Result<(), String> {
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
