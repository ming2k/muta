//! Input history: record/clear, attachment pruning, session backfill, prev/next recall, draft restore.

use super::*;

impl App {
    pub(crate) const HISTORY_ATTACHMENTS_CAP: usize = 32;
    /// Rows shown in the Ctrl+R history panel, as `(original_index,
    /// FuzzyMatch)` pairs indexing into [`App::input_history`]. The single
    /// source of truth for navigation (Up/Down clamp), Enter-accept, and
    /// rendering — they all index into this same vector so the cursor never
    /// lands on a row the user cannot see.
    ///
    /// The list is always the **whole cross-session history**, independent of
    /// which session or workspace produced each entry — that is the entire
    /// point of Ctrl+R (the inline ↑/↓ recall, by contrast, is scoped to the
    /// current session via [`App::current_session_history`]). Entries are
    /// ordered newest-first by `created_at_ms`.
    ///
    /// With an empty query (`App::input`, which the panel borrows as its live
    /// filter) every entry shows, unhighlighted. Once a query is present the
    /// rows are the fuzzy-ranked matches, best score first, with the original
    /// newest-first order as the stable tiebreaker. Recomputed from scratch
    /// each call: history is small and this runs at most a few times per
    /// frame, so caching would only add stale-state risk.
    pub fn history_rows(&self) -> Vec<(usize, fuzzy::FuzzyMatch)> {
        // The display order: newest-first. The on-disk file is already stored
        // newest-first, but in-memory appends during this run land at the
        // tail, so re-sort by created_at_ms (stable) to keep the panel's order
        // correct without mutating the stored Vec.
        let order: Vec<usize> = self.history_order();
        let texts: Vec<&str> = order
            .iter()
            .map(|&i| {
                self.input_history
                    .get(i)
                    .map(|e| e.text.as_str())
                    .unwrap_or("")
            })
            .collect();
        if self.input.is_empty() {
            // Empty query → show everything newest-first, unhighlighted.
            return order
                .into_iter()
                .map(|i| {
                    (
                        i,
                        fuzzy::FuzzyMatch {
                            score: 0,
                            positions: Vec::new(),
                        },
                    )
                })
                .collect();
        }
        // `rank` returns indices into `texts`; map them back to the original
        // `input_history` indices via `order`. The matched char positions are
        // indices into the entry text itself, so they need no remap.
        let mut ranked = fuzzy::rank(&texts, &self.input);
        fuzzy::sort_by_score(&mut ranked);
        ranked.into_iter().map(|(ti, m)| (order[ti], m)).collect()
    }

    /// The newest-first ordering of [`App::input_history`] by `created_at_ms`,
    /// as original indices into that Vec. Stable on ties so the on-disk order
    /// survives. Shared by [`Self::history_rows`] (Ctrl+R) and
    /// [`Self::current_session_history`] (inline ↑/↓) so both surfaces agree
    /// on what "newest" means.
    pub fn history_order(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.input_history.len()).collect();
        // Newest-first by `created_at_ms`; entries stamped within the same
        // millisecond (a fast send burst) break ties by insertion order — the
        // later index is the newer prompt — so "newest-first" stays
        // well-defined instead of degrading to oldest-first on a tie.
        order.sort_by(|&a, &b| {
            self.input_history[b]
                .created_at_ms
                .cmp(&self.input_history[a].created_at_ms)
                .then_with(|| b.cmp(&a))
        });
        order
    }

    /// The current session's history, newest-first. This is what the inline
    /// ↑/↓ recall walks: the union of the **persisted** history
    /// ([`Self::input_history`], filtered to entries whose `session_id`
    /// matches [`App::current_session_id`]) and the **derived** transcript
    /// rows ([`Self::session_history_backfill`]), so arrow-key recall
    /// surfaces exactly the prompts of *this* conversation — including ones
    /// this client never recorded (a session resumed from elsewhere). Ctrl+R
    /// is unaffected — it searches the whole persisted list regardless of
    /// session.
    ///
    /// Returns indices into the combined row space: `0..input_history.len()`
    /// address the persisted store, `input_history.len() + i` addresses the
    /// `i`-th backfill row. [`Self::history_entry`] resolves either kind, so
    /// callers never branch on the boundary.
    pub fn current_session_history(&self) -> Vec<usize> {
        let sid = self.current_session_id.as_str();
        let mut rows: Vec<(u64, usize)> = self
            .input_history
            .iter()
            .enumerate()
            .filter(|(_, e)| e.session_id.as_deref() == Some(sid))
            .map(|(i, e)| (e.created_at_ms, i))
            .collect();
        let base = self.input_history.len();
        rows.extend(
            self.session_history_backfill
                .iter()
                .enumerate()
                // Walked newest-first below, so the backfill's oldest-first
                // storage order must be reversed to reach `created_at_ms`
                // parity — ties against persisted rows resolve to the
                // transcript's own (older-first) order via the stable sort.
                .map(|(i, e)| (e.created_at_ms, base + i)),
        );
        // Newest-first: stable sort keeps within-store order on ties, and the
        // backfill rows (transcript append order) follow persisted rows of the
        // same millisecond.
        rows.sort_by_key(|&(created_at_ms, _)| std::cmp::Reverse(created_at_ms));
        rows.into_iter().map(|(_, i)| i).collect()
    }

    /// Resolve a row index from [`Self::current_session_history`] to its
    /// entry, transparently spanning the persisted store (`0..len`) and the
    /// session backfill (`len..`). `None` when the index is out of range.
    pub fn history_entry(&self, idx: usize) -> Option<&muta_contracts::HistoryEntry> {
        if idx < self.input_history.len() {
            self.input_history.get(idx)
        } else {
            self.session_history_backfill
                .get(idx - self.input_history.len())
        }
    }

    /// Drop backfill rows whose text this session has since **recorded**
    /// (the send path persisted it, possibly by re-tagging an existing
    /// global-dedup row into this session). Called after
    /// [`Self::record_input_history`] so the union the ↑/↓ walk sees never
    /// contains the same prompt twice: without this, a prompt that was
    /// backfilled on resume and then re-sent through this client would
    /// surface as two adjacent rows.
    pub fn prune_backfill_after_record(&mut self, text: &str) {
        self.session_history_backfill.retain(|e| e.text != text);
    }

    /// Seed [`Self::session_history_backfill`] with the **viewed
    /// transcript's** genuine chat prompts, so the inline ↑/↓ recall reflects
    /// the conversation the user is actually looking at rather than only what
    /// this client's `history.json` happens to contain.
    ///
    /// This is the resume path: `ConversationReplaced` hands the TUI another
    /// session's transcript, and prompts typed into that session by a
    /// *different* client (or before this `history.json` existed) were never
    /// recorded locally. Without the backfill, `↑` after a resume comes up
    /// empty even though the conversation visibly contains prompts. The
    /// initial startup transcript is backfilled the same way before the
    /// first frame.
    ///
    /// Only `UserMessageOrigin::Chat` rows count — slash commands
    /// (`/model`, …) and `!shell` passthroughs are UI gestures excluded from
    /// the history by contract (`[input_history] record_commands = false`),
    /// and queued-but-unsent rows are not prompts yet. A prompt already
    /// recorded by this client (present in the persisted history under this
    /// session) is skipped, so live sends and backfills never duplicate a
    /// row.
    ///
    /// The backfill is **derived state, never persisted**: transcript rows
    /// already live in the session file (the durable source of truth,
    /// ADR-0018), so writing them into `history.json` would duplicate the
    /// store and race the cross-process merge. Timestamps come from the
    /// transcript where available (`sent_at_ms`, falling back to `now_ms`
    /// for legacy rows so ordering stays stable).
    ///
    /// `tail` is the transcript's unconsumed suffix as `(text, is_chat,
    /// sent_at_ms)` triples — copied out by the caller, which cannot lend
    /// the transcript while `App` is borrowed mutably here.
    pub fn backfill_session_history(&mut self, tail: &[(String, bool, u64)], now_ms: u64) {
        let sid = self.current_session_id.as_str();
        let recorded: HashSet<&str> = self
            .input_history
            .iter()
            .filter(|e| e.session_id.as_deref() == Some(sid))
            .map(|e| e.text.as_str())
            .collect();
        for (text, is_chat, sent_at_ms) in tail {
            if !is_chat || text.is_empty() || recorded.contains(text.as_str()) {
                continue;
            }
            // Same prompt twice in one conversation (an intentional resend)
            // is one recallable row — the newest position wins, matching the
            // persisted history's newest-first contract.
            if let Some(existing) = self
                .session_history_backfill
                .iter_mut()
                .find(|e| e.text == *text)
            {
                existing.created_at_ms = (*sent_at_ms).max(existing.created_at_ms);
                continue;
            }
            self.session_history_backfill
                .push(muta_contracts::HistoryEntry::new(
                    text.clone(),
                    Some(self.current_session_id.clone()),
                    Some(self.current_workspace.clone()),
                    if *sent_at_ms == 0 {
                        now_ms
                    } else {
                        *sent_at_ms
                    },
                ));
        }
    }

    /// Record `entry` in the cross-session input history, tagged with the
    /// current session id + workspace and stamped "now": reset the up/down
    /// recall cursor, dedup against the most recent same-text+same-session
    /// entry, and persist the new entry to disk immediately (off-thread) so
    /// it survives an unclean exit and is visible to concurrent sessions
    /// right away rather than only on exit.
    ///
    /// `images` / `text_pastes` are the attachments staged behind the chips
    /// in `entry` at send time. They are **not** persisted (history.json is
    /// rebuildable cosmetic telemetry, never conversation data — ADR-0018)
    /// but are cached in memory keyed by the entry's `(text, session_id)`
    /// identity, so the ↑/↓ and Ctrl+R recall paths can restore a just-sent
    /// or interrupted message's attachments instead of shipping a bare chip
    /// label the model would read as literal text.
    ///
    /// The origin (session/workspace) is what separates Ctrl+R (searches the
    /// whole history) from inline ↑/↓ (walks only this session's entries).
    pub fn record_input_history(
        &mut self,
        entry: String,
        images: Vec<ImagePart>,
        text_pastes: Vec<String>,
    ) {
        self.history_index = None;
        if entry.is_empty() && images.is_empty() && text_pastes.is_empty() {
            return;
        }
        // Slash-command invocations (`/model`, `/new`, …) are UI gestures,
        // not prompts: they are already visible in the transcript, and most
        // users don't want `/model` noise cluttering the Ctrl+R picker. Skip
        // them unless `[input_history] record_commands` opts them back in.
        if entry.starts_with('/') && !self.input_history_record_commands {
            return;
        }
        let now = crate::event_loop::now_epoch_ms();
        // Ensure strictly-increasing timestamps. `now_epoch_ms()` can return
        // the same millisecond for a rapid burst of sends, and the history
        // order's stable sort would then keep input order — putting the
        // older prompt ahead of the newer one and breaking the newest-first
        // contract (the inline ↑ would land on the stale entry first). The
        // wall clock stays the baseline; when it has not advanced past the
        // newest recorded entry, nudge the stamp forward by one.
        let latest_ts = self
            .input_history
            .iter()
            .map(|e| e.created_at_ms)
            .max()
            .unwrap_or(0);
        let now = if now > latest_ts {
            now
        } else {
            latest_ts.saturating_add(1)
        };
        let session_id = if self.current_session_id.is_empty() {
            None
        } else {
            Some(self.current_session_id.clone())
        };
        let workspace = if self.current_workspace.is_empty() {
            None
        } else {
            Some(self.current_workspace.clone())
        };
        // Cache the attachments first (before the dedup early-return) so a
        // repeat send of the same prompt refreshes the payloads a recall
        // will restore, even though no new history row is pushed.
        if !images.is_empty() || !text_pastes.is_empty() {
            let identity = (entry.clone(), session_id.clone());
            if !self.history_attachments.contains_key(&identity) {
                self.history_attachments_order.push_back(identity.clone());
            }
            self.history_attachments.insert(
                identity,
                HistoryAttachments {
                    images,
                    text_pastes,
                },
            );
            self.prune_history_attachments();
        }
        // With `[input_history] dedup` (default on) the prompt text alone is
        // the identity: the same prompt sent twice — even in a different
        // session — stays one row. Re-sending refreshes the timestamp (so the
        // entry bubbles to the top of the newest-first picker) and adopts the
        // newest known origin (so ↑/↓ in the session that last sent it still
        // finds it), then persists the refreshed entry.
        if self.input_history_dedup {
            if let Some(existing) = self.input_history.iter_mut().find(|e| e.text == entry) {
                existing.created_at_ms = now;
                if session_id.is_some() {
                    existing.session_id = session_id;
                }
                if workspace.is_some() {
                    existing.workspace = workspace;
                }
                let refreshed = existing.clone();
                // The text is now recorded under this session: drop any
                // transcript-derived backfill row for it so the ↑/↓ union
                // never shows the same prompt twice.
                self.prune_backfill_after_record(&refreshed.text);
                if self.input_history_persist {
                    tokio::task::spawn_blocking(move || {
                        let _ = crate::config::save_history(std::slice::from_ref(&refreshed), true);
                    });
                }
                return;
            }
            let recorded = muta_contracts::HistoryEntry::new(entry, session_id, workspace, now);
            self.push_history(recorded.clone());
            if self.input_history_persist {
                tokio::task::spawn_blocking(move || {
                    let _ = crate::config::save_history(std::slice::from_ref(&recorded), true);
                });
            }
            return;
        }
        // Dedup disabled: dedup against the newest same-text entry in *this*
        // session — typing the same prompt twice in a row should not produce
        // two adjacent rows, but the same words typed in a different session
        // legitimately are a distinct history entry (each keeps its own
        // origin).
        let already_latest_in_session = self
            .current_session_history()
            .first()
            .and_then(|&i| self.history_entry(i))
            .is_some_and(|e| e.text == entry && e.session_id == session_id);
        if already_latest_in_session {
            return;
        }
        let recorded = muta_contracts::HistoryEntry::new(entry, session_id, workspace, now);
        self.push_history(recorded.clone());
        // Same dedup guard as above: a backfilled row for this text is now
        // redundant with the recorded one.
        self.prune_backfill_after_record(&recorded.text);
        // `save_history` lock+merges into the on-disk union, so persisting just
        // the new entry is enough and cheap. Off-thread: the write takes a file
        // lock and must not block the event loop. Skipped entirely when disk
        // persistence is disabled (tests).
        if self.input_history_persist {
            tokio::task::spawn_blocking(move || {
                let _ = crate::config::save_history(std::slice::from_ref(&recorded), false);
            });
        }
    }

    /// Wipe the entire input history — the Ctrl+R picker's "clear" action.
    /// Clears the in-memory list, the attachment cache, and truncates the
    /// on-disk history file so the change survives an unclean exit. The caller
    /// is responsible for confirming first (see [`Self::history_clear_confirm`]).
    pub fn clear_input_history(&mut self) {
        self.input_history.clear();
        self.history_attachments.clear();
        self.history_attachments_order.clear();
        self.history_index = None;
        self.history_clear_confirm = false;
        // The modal stays open after a clear; reset its selection/preview so
        // it re-anchors to the (now empty) list instead of a stale index.
        self.modal_index = 0;
        self.history_scroll = 0;
        self.history_preview = false;
        // Only truncate the real file when disk persistence is enabled — a
        // test invoking the clear action must never wipe the user's history.
        if self.input_history_persist {
            tokio::task::spawn_blocking(|| {
                let _ = crate::config::clear_history();
            });
        }
    }

    /// Drop the oldest cached attachment entries (FIFO) once the cache
    /// exceeds [`Self::HISTORY_ATTACHMENTS_CAP`]. `history_attachments_order`
    /// records first-seen order; a re-sent identity keeps its original slot.
    fn prune_history_attachments(&mut self) {
        while self.history_attachments.len() > Self::HISTORY_ATTACHMENTS_CAP {
            let Some(key) = self.history_attachments_order.pop_front() else {
                break;
            };
            self.history_attachments.remove(&key);
        }
    }

    /// Restore the attachments cached behind the history entry at
    /// `orig_idx` (an index into [`App::input_history`], as returned by
    /// `current_session_history` / `history_rows`) into the composer's
    /// `pending_images` / `pending_text_pastes`, or clear them when the
    /// entry has no cache (e.g. loaded from disk before this process
    /// recorded it). The recalled input text already carries the matching
    /// `[Image #N …]` / `[Pasted text #N …]` chips, so staging the payloads
    /// is all that is needed to re-arm a resend.
    pub fn restore_history_attachments(&mut self, orig_idx: usize) {
        let Some(entry) = self.history_entry(orig_idx) else {
            return;
        };
        let identity = (entry.text.clone(), entry.session_id.clone());
        match self.history_attachments.get(&identity) {
            Some(attachments) => {
                self.pending_images = attachments.images.clone();
                self.pending_text_pastes = attachments.text_pastes.clone();
            }
            None => {
                // No cached payloads: a fresh send must not inherit
                // attachments staged for some other entry, so clear them.
                self.pending_images.clear();
                self.pending_text_pastes.clear();
            }
        }
    }

    /// Load the history entry at `orig_idx` (an index from
    /// [`Self::current_session_history`] — spanning the persisted store and
    /// the session backfill) into the composer: its text, its cached
    /// attachments, cursor at the end, completion popup latched closed.
    /// Shared by the ↑/↓ walk and Ctrl+R insert so every recall path stays
    /// identical on the details.
    fn load_history_row(&mut self, orig_idx: usize) {
        let Some(entry) = self.history_entry(orig_idx) else {
            return;
        };
        self.input = entry.text.clone();
        self.set_cursor_end();
        self.restore_history_attachments(orig_idx);
        // History navigation is a programmatic input replacement, not an
        // edit — so it latches `completion_dismissed` like a slash-command
        // accept rather than re-enabling the popup the way InsertChar /
        // Backspace do. This keeps a recalled slash command from flashing
        // its completion menu until the next real keystroke clears the latch.
        self.suggestion_index = None;
        self.completion_dismissed = true;
    }

    /// Advance the inline ↑/↓ history cursor one step toward **older**
    /// entries (the ↑ key). `session_rows` is the newest-first index slice
    /// from [`App::current_session_history`], so position 0 is the newest
    /// entry and larger positions are older.
    ///
    /// The first ↑ stashes the in-progress draft — text and any staged
    /// attachments together — so a later ↓ past the newest entry restores
    /// it instead of leaving the composer empty. Subsequent ↑ walk further
    /// back and clamp at the oldest entry. Returns `true` when a row was
    /// loaded; `false` when the slice is empty.
    pub fn history_prev(&mut self, session_rows: &[usize]) -> bool {
        if session_rows.is_empty() {
            return false;
        }
        let new_pos = match self.history_index {
            Some(p) => (p + 1).min(session_rows.len() - 1),
            None => {
                // First ↑: stash the in-progress draft (and its staged
                // attachments) so a later ↓ past the newest entry restores
                // it instead of leaving the composer empty.
                self.history_draft = std::mem::take(&mut self.input);
                self.history_draft_images = std::mem::take(&mut self.pending_images);
                self.history_draft_text_pastes = std::mem::take(&mut self.pending_text_pastes);
                0
            }
        };
        self.history_index = Some(new_pos);
        self.load_history_row(session_rows[new_pos]);
        true
    }

    /// Move the inline history cursor one step toward **newer** entries
    /// (the ↓ key), mirroring [`App::history_prev`]. Walking past the
    /// newest entry (position 0) restores the draft stashed on the first ↑
    /// — text and attachments together. Returns `true` when a row was
    /// loaded; `false` when the cursor is already at the newest edge (or
    /// was never armed), in which case the draft has been restored.
    pub fn history_next(&mut self, session_rows: &[usize]) -> bool {
        let Some(pos) = self.history_index else {
            return false;
        };
        if pos == 0 {
            // Walked back to the newest entry: restore the draft the user
            // was composing before the first ↑ — text and any staged
            // attachments together — rather than blanking the composer.
            self.history_index = None;
            self.input = std::mem::take(&mut self.history_draft);
            self.pending_images = std::mem::take(&mut self.history_draft_images);
            self.pending_text_pastes = std::mem::take(&mut self.history_draft_text_pastes);
            self.set_cursor_end();
            // The restored draft may be a partial slash/path the user was
            // mid-edit on, but it still arrived via navigation rather than
            // a keystroke, so hold the latch until the next edit.
            self.suggestion_index = None;
            self.completion_dismissed = true;
            return false;
        }
        let new_pos = pos - 1;
        self.history_index = Some(new_pos);
        self.load_history_row(session_rows[new_pos]);
        true
    }

    /// Cancel inline history recall and restore the draft that was saved
    /// before navigation began.
    pub fn cancel_history_recall(&mut self) {
        if self.history_index.is_some() {
            self.history_index = None;
            self.input = std::mem::take(&mut self.history_draft);
            self.pending_images = std::mem::take(&mut self.history_draft_images);
            self.pending_text_pastes = std::mem::take(&mut self.history_draft_text_pastes);
            self.set_cursor_end();
            self.suggestion_index = None;
            self.completion_dismissed = true;
        }
    }

    /// Tear down the history modal's borrowed state: hand the parked composer
    /// draft back, drop any filter query, and clear the search/preview
    /// sub-flags. Shared by the Esc (`CloseModal`) and click-outside dismiss
    /// paths so the two can never drift. Does **not** touch `active_modal` —
    /// the caller owns that transition.
    pub fn restore_history_draft(&mut self) {
        self.input = std::mem::take(&mut self.injection_stashed_input);
        self.set_cursor_end();
        self.input_scroll = 0;
        self.suggestion_index = None;
        self.modal_index = 0;
        self.history_search = false;
        self.history_preview = false;
    }

    /// Tear down the model picker's borrowed state: hand the parked composer
    /// draft back, drop any filter query, and clear the search/scroll sub-flags.
    /// Shared by the Esc (`CloseModal`), click-outside dismiss, and activation
    /// paths so they can never drift. Mirrors [`Self::restore_history_draft`];
    /// does **not** touch `active_modal` — the caller owns that transition.
    pub fn restore_model_draft(&mut self) {
        self.input = std::mem::take(&mut self.injection_stashed_input);
        self.set_cursor_end();
        self.input_scroll = 0;
        self.suggestion_index = None;
        self.modal_index = 0;
        self.model_search = false;
        self.model_scroll = 0;
        self.model_modal_follow = true;
    }
}
