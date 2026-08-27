//! The dispatch queue: block/resume, reorder, recall/restore, and the queue-pointer stash/commit cycle.

use super::*;

impl App {
    /// Reconcile [`App::pending_images`] / [`App::pending_text_pastes`]
    /// against the chips that currently survive in [`App::input`], and
    /// relabel the surviving chips so their `#N` matches their new 1-based
    /// position in the truncated vectors. Cheap to run on every input
    /// mutation: it is a single linear scan over the input string.
    ///
    /// This is the prune + relabel pass that drops orphaned staged entries
    /// whenever the user deletes or edits a chip — by backspace, selection
    /// delete, or hand-typing over the chip text. Mirrors codex's
    /// `reconcile_deleted_elements` and claude-code's `parseReferences`
    /// effect, adapted to muta's "chip text lives in the input" model.
    pub fn reconcile_attachments(&mut self) {
        let new_input = composer_attachments::reconcile(
            &self.input,
            &mut self.pending_images,
            &mut self.pending_text_pastes,
        );
        self.input = new_input;
    }

    /// How many staged messages are waiting in this session's outbox (front
    /// pops first). All entries are next-round items; a busy Enter always
    /// queues rather than injecting mid-round.
    pub fn pending_count(&self, session_id: &str) -> usize {
        self.pending_dispatch
            .iter()
            .filter(|item| item.session_id == session_id)
            .count()
    }

    pub fn remove_dispatch(&mut self, session_id: &str, input_id: &str) -> Option<QueuedDispatch> {
        let position = self
            .pending_dispatch
            .iter()
            .position(|item| item.session_id == session_id && item.id == input_id)?;
        self.pending_dispatch.remove(position)
    }

    /// Is this session's outbox hard-blocked by the user? While blocked, no
    /// queued message auto-drains — not even after natural completion + idle.
    /// The queue modal blocks on open and resumes on close; `Ctrl+P` toggles
    /// from
    /// the bar. A no-op (and leaves the block off) for a session with no
    /// staged items.
    pub fn is_queue_blocked(&self, session_id: &str) -> bool {
        self.queue_blocked_sessions.contains(session_id)
    }

    /// Toggle the user block on the viewed session's outbox. Mirrors `Ctrl+P` /
    /// the queue modal's block control. Returns the new state so the caller
    /// can reflect it in the render snapshot.
    pub fn toggle_queue_block(&mut self, session_id: &str) -> bool {
        if !self.queue_blocked_sessions.insert(session_id.to_string()) {
            // Already present → remove it (toggle off).
            self.queue_blocked_sessions.remove(session_id);
            false
        } else {
            true
        }
    }

    /// Force the block on, regardless of its current state. Used when the
    /// queue modal opens so items can be managed safely (delete / reorder /
    /// re-edit) without one auto-draining mid-edit.
    pub fn block_queue(&mut self, session_id: &str) {
        self.queue_blocked_sessions.insert(session_id.to_string());
    }

    /// Force the block off. Used when the queue modal closes (auto-resume), so
    /// the outbox returns to its normal auto-drain behavior the moment the
    /// user stops managing it — unless they explicitly blocked it with
    /// `Ctrl+P` outside the modal (that toggle is honored because the modal
    /// close path only resumes what its own open path blocked).
    pub fn resume_queue(&mut self, session_id: &str) {
        self.queue_blocked_sessions.remove(session_id);
    }

    /// Remove the viewed session's outbox item at display index `idx`. Used by
    /// the queue modal's `D` delete. Returns the removed dispatch (mostly for
    /// tests).
    pub fn remove_queued_at(&mut self, session_id: &str, idx: usize) -> Option<QueuedDispatch> {
        let position = self
            .pending_dispatch
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                item.session_id == session_id && item.state == QueuedDispatchState::Waiting
            })
            .nth(idx)
            .map(|(pos, _)| pos)?;
        self.pending_dispatch.remove(position)
    }

    /// Move the viewed session's outbox item at display index `idx` by `delta`
    /// slots within the session's Waiting slice (`delta < 0` toward the front
    /// / next to pop, `delta > 0` toward the tail). Other items in the slice
    /// shift to make room (a true reorder, not a swap). Clamped at the slice
    /// boundaries so an item can never escape into another session's region
    /// of the deque.
    pub fn move_queued(&mut self, session_id: &str, idx: usize, delta: i32) {
        // Collect the positions (into the global deque) of this session's
        // Waiting items in display order — the selectable range.
        let positions: Vec<usize> = self
            .pending_dispatch
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                item.session_id == session_id && item.state == QueuedDispatchState::Waiting
            })
            .map(|(pos, _)| pos)
            .collect();
        let count = positions.len();
        if count == 0 {
            return;
        }
        let clamped_idx = idx.min(count - 1);
        let new_idx = (clamped_idx as i32 + delta).clamp(0, count as i32 - 1) as usize;
        if new_idx == clamped_idx {
            return;
        }
        let from = positions[clamped_idx];
        let target = positions[new_idx];
        // Remove the item, then re-insert at `target`. This lands the item at
        // the destination slot while the displaced neighbors shift to fill the
        // gap — a true reorder, not a swap. The single `target` works for both
        // directions: when moving toward the tail, removal of `from` (before
        // `target`) shifts `target` down by one, exactly offset by inserting
        // one past the neighbor; when moving toward the front, no shift occurs
        // and the item lands just before the neighbor. `from` is a valid index
        // by construction (enumerated from the deque above), so the remove is
        // guarded rather than `expect`-ed.
        if let Some(item) = self.pending_dispatch.remove(from) {
            self.pending_dispatch.insert(target, item);
        }
    }

    /// A staged next-round item failed to start its round (e.g. no provider
    /// configured), or a busy-Enter steer's round ended before admission.
    ///
    /// For an item still in the outbox this just flips it back to `Waiting`.
    /// For a **transcript-owned steer** (busy Enter handed back by
    /// `UserInputUnavailable`) there is no outbox item — the content lives in
    /// the transcript entry — so the caller stages one here (`text` /
    /// attachments from the held entry) under the same id: the queue then
    /// owns its auto-dispatch / pointer-recall lifecycle, and the entry is
    /// dropped from the outbox when its round starts (`NextRoundStarted`),
    /// exactly like a busy-Enter item. Pushes to the back (FIFO among
    /// handed-back inserts; they left the running round in send order).
    pub fn requeue_dispatch(
        &mut self,
        session_id: &str,
        input_id: &str,
        held: Option<(String, Vec<ImagePart>, Vec<String>)>,
    ) {
        if let Some(item) = self
            .pending_dispatch
            .iter_mut()
            .find(|item| item.session_id == session_id && item.id == input_id)
        {
            item.state = QueuedDispatchState::Waiting;
            return;
        }
        if let Some((text, images, text_pastes)) = held {
            self.pending_dispatch.push_back(QueuedDispatch {
                id: input_id.to_string(),
                session_id: session_id.to_string(),
                state: QueuedDispatchState::Waiting,
                text,
                queued_at_ms: crate::event_loop::now_epoch_ms(),
                images,
                text_pastes,
            });
        }
    }

    /// FIFO next-round dispatch within one session. The entry remains in the
    /// outbox until its fresh round has actually started; route failure can
    /// therefore return it to `Waiting` without reconstructing user content.
    pub fn begin_next_round_dispatch(&mut self, session_id: &str) -> Option<QueuedDispatch> {
        let item = self.pending_dispatch.iter_mut().find(|item| {
            item.session_id == session_id && item.state == QueuedDispatchState::Waiting
        })?;
        item.state = QueuedDispatchState::Dispatching;
        Some(item.clone())
    }

    /// LIFO undo for the viewed session. Every queued dispatch is a
    /// next-round item, so recall pops the newest staged message and restores
    /// it into the composer immediately — no agent roundtrip to cancel.
    pub fn recall_queued(&mut self, session_id: &str) -> Option<RecallQueued> {
        let position = self.pending_dispatch.iter().rposition(|item| {
            item.session_id == session_id && item.state == QueuedDispatchState::Waiting
        })?;
        self.pending_dispatch
            .remove(position)
            .map(RecallQueued::Restored)
    }

    /// Recall a specific outbox item by display index (front-of-queue = 0).
    /// Used by the queue modal's `Enter` re-edit, which keys off the `↑/↓`
    /// selection rather than always targeting the newest — so a mid-queue item
    /// can be pulled back to the composer too. The item is removed from the
    /// outbox, exactly like [`Self::recall_queued`].
    pub fn recall_queued_at(&mut self, session_id: &str, idx: usize) -> Option<RecallQueued> {
        let position = self
            .pending_dispatch
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                item.session_id == session_id && item.state == QueuedDispatchState::Waiting
            })
            .nth(idx)
            .map(|(pos, _)| pos)?;
        self.pending_dispatch
            .remove(position)
            .map(RecallQueued::Restored)
    }

    /// Recall an outbox item back into the composer. A user gesture (queue
    /// recall / modal re-edit), so it replaces whatever the composer holds.
    pub fn restore_dispatch(&mut self, dispatch: QueuedDispatch) {
        self.adopt_as_draft(
            dispatch.text,
            dispatch.images,
            dispatch.text_pastes,
            DraftAdoption::Replace,
        );
    }

    /// The ids of this session's waiting (next-round) items, front-of-queue
    /// first. `Dispatching` items are excluded: their round has already
    /// started, so editing them would be a lie.
    /// The ids of this session's unconsumed items (pending steer messages in
    /// transcript and waiting follow-up items in outbox), in timeline order
    /// (older first, newer last).
    pub fn queue_pointer_ids(&self, session_id: &str) -> Vec<String> {
        let mut ids = Vec::new();
        // 1. Pending steer messages staged in transcript (chronologically earlier in turn)
        for msg in self.messages.iter().chain(self.side_messages.iter()) {
            if msg.delivery == crate::model::document::DeliveryStatus::Queued
                && msg.origin == crate::model::document::UserMessageOrigin::Steer
                && let Some(id) = &msg.insert_id
            {
                ids.push(id.clone());
            }
        }
        // 2. Waiting follow-up items in pending_dispatch
        for item in self.pending_dispatch.iter() {
            if item.session_id == session_id && item.state == QueuedDispatchState::Waiting {
                ids.push(item.id.clone());
            }
        }
        ids
    }

    /// Retrieve the content (text, images, text_pastes) of a queue pointer target.
    pub fn queue_pointer_content(
        &self,
        session_id: &str,
        id: &str,
    ) -> Option<(String, Vec<ImagePart>, Vec<String>)> {
        // Check transcript steer messages first
        for msg in self.messages.iter().chain(self.side_messages.iter()) {
            if msg.delivery == crate::model::document::DeliveryStatus::Queued
                && msg.origin == crate::model::document::UserMessageOrigin::Steer
                && msg.insert_id.as_deref() == Some(id)
            {
                return Some((msg.raw.clone(), Vec::new(), Vec::new()));
            }
        }
        // Check pending_dispatch
        self.pending_dispatch
            .iter()
            .find(|item| {
                item.session_id == session_id
                    && item.id == id
                    && item.state == QueuedDispatchState::Waiting
            })
            .map(|item| {
                (
                    item.text.clone(),
                    item.images.clone(),
                    item.text_pastes.clone(),
                )
            })
    }

    /// Return a human-readable badge label when the queue pointer is armed.
    /// Example: `[edit: steer #1]` or `[edit: follow-up #1]`.
    pub fn queue_pointer_badge(&self, session_id: &str) -> Option<String> {
        let id = self.queue_pointer.as_deref()?;
        let mut steer_count = 0;
        for msg in self.messages.iter().chain(self.side_messages.iter()) {
            if msg.delivery == crate::model::document::DeliveryStatus::Queued
                && msg.origin == crate::model::document::UserMessageOrigin::Steer
            {
                steer_count += 1;
                if msg.insert_id.as_deref() == Some(id) {
                    return Some(format!("[edit: steer #{steer_count}]"));
                }
            }
        }
        let mut followup_count = 0;
        for item in self.pending_dispatch.iter() {
            if item.session_id == session_id && item.state == QueuedDispatchState::Waiting {
                followup_count += 1;
                if item.id == id {
                    return Some(format!("[edit: follow-up #{followup_count}]"));
                }
            }
        }
        None
    }

    /// Resolve [`Self::queue_pointer`] to the live follow-up item it points at, if any.
    pub fn queue_pointer_target(&self, session_id: &str) -> Option<&QueuedDispatch> {
        let id = self.queue_pointer.as_deref()?;
        self.pending_dispatch
            .iter()
            .find(|item| item.session_id == session_id && item.id == id)
    }

    /// Load content into the composer as the pointer's projection.
    fn load_queue_pointer_row(
        &mut self,
        text: String,
        images: Vec<ImagePart>,
        text_pastes: Vec<String>,
    ) {
        self.input = text;
        self.pending_images = images;
        self.pending_text_pastes = text_pastes;
        self.set_cursor_end();
        self.suggestion_index = None;
        self.completion_dismissed = true;
    }

    /// `↑` from the draft (or a history row): arm the queue pointer at the
    /// **newest** waiting item (the back of the deque) and project it into the
    /// composer. Returns `false` when the session's queue has no waiting
    /// items — the caller then hands ↑ on to input history.
    pub fn queue_pointer_prev(&mut self, session_id: &str) -> bool {
        let ids = self.queue_pointer_ids(session_id);
        let Some(newest) = ids.last() else {
            return false;
        };
        if self.queue_pointer.is_none() {
            // Leaving the draft (or a history row): stash what the composer
            // held so the exit path can restore it, and leave history mode —
            // the pointer owns the composer now.
            self.history_index = None;
            self.stash_queue_pointer_draft();
        }
        // Already armed → step toward the front (older). `pos == 0` is the
        // oldest item: stay there (clamped) rather than jumping back to the
        // newest. A vanished target (not found in `ids`) resets to the
        // newest, the sensible default when the world changed under us.
        let next_id = match self
            .queue_pointer
            .as_deref()
            .and_then(|cur| ids.iter().position(|id| id == cur))
        {
            Some(pos) if pos > 0 => ids[pos - 1].clone(),
            Some(_) => self.queue_pointer.clone().unwrap_or_else(|| newest.clone()),
            None => newest.clone(),
        };
        self.queue_pointer = Some(next_id.clone());
        if let Some((text, images, text_pastes)) = self.queue_pointer_content(session_id, &next_id)
        {
            self.load_queue_pointer_row(text, images, text_pastes);
        }
        true
    }

    /// `↓` while the pointer is armed: step toward the **newer** items and,
    /// past the newest, dissolve the pointer and restore the stashed draft.
    /// Returns `true` whenever the key was consumed by the pointer (stepping
    /// *or* dissolving); `false` only when the pointer was not armed, so the
    /// caller falls through to history navigation.
    pub fn queue_pointer_next(&mut self, session_id: &str) -> bool {
        let Some(cur) = self.queue_pointer.clone() else {
            return false;
        };
        let ids = self.queue_pointer_ids(session_id);
        let pos = ids.iter().position(|id| id == &cur);
        match pos {
            Some(p) if p + 1 < ids.len() => {
                let next_id = ids[p + 1].clone();
                self.queue_pointer = Some(next_id.clone());
                if let Some((text, images, text_pastes)) =
                    self.queue_pointer_content(session_id, &next_id)
                {
                    self.load_queue_pointer_row(text, images, text_pastes);
                }
                true
            }
            // Past the newest item (or the target vanished): back to the
            // draft, exactly as the history pointer restores its stash.
            _ => self.dissolve_queue_pointer(),
        }
    }

    /// Dissolve the pointer and restore the stashed draft. Also the teardown
    /// path for sends and session switches, so a stale pointer never leaks
    /// into the next composer state. Returns `true` so callers can treat the
    /// key as consumed.
    pub fn dissolve_queue_pointer(&mut self) -> bool {
        if self.queue_pointer.is_none() {
            return false;
        }
        self.queue_pointer = None;
        self.input = std::mem::take(&mut self.queue_pointer_draft);
        self.pending_images = std::mem::take(&mut self.queue_pointer_draft_images);
        self.pending_text_pastes = std::mem::take(&mut self.queue_pointer_draft_text_pastes);
        self.set_cursor_end();
        self.suggestion_index = None;
        self.completion_dismissed = true;
        true
    }

    /// Drop the pointer and its stash **without** restoring the stash into
    /// the composer. Used when the composer's content is leaving the
    /// projection for somewhere permanent (an insert entry, a send): the
    /// content in hand supersedes whatever the stash held, and restoring it
    /// would clobber what the user is actively acting on. Idempotent.
    pub fn drop_queue_pointer_without_restore(&mut self) {
        self.queue_pointer = None;
        self.queue_pointer_draft.clear();
        self.queue_pointer_draft_images.clear();
        self.queue_pointer_draft_text_pastes.clear();
    }

    /// Commit the composer's current content into the pointed-at queue item,
    /// **in place** — the queue's length and order are untouched; only the
    /// item's content changes — and dissolve the pointer.
    pub fn commit_queue_pointer(&mut self, session_id: &str) -> Option<()> {
        let id = self.queue_pointer.clone()?;
        let text = self.input.clone();
        let images = self.pending_images.clone();
        let text_pastes = self.pending_text_pastes.clone();

        self.queue_pointer = None;
        self.queue_pointer_draft.clear();
        self.queue_pointer_draft_images.clear();
        self.queue_pointer_draft_text_pastes.clear();

        // 1. Check if it is a pending follow-up in pending_dispatch
        if let Some(item) = self.pending_dispatch.iter_mut().find(|item| {
            item.session_id == session_id
                && item.id == id
                && item.state == QueuedDispatchState::Waiting
        }) {
            item.text = text;
            item.images = images;
            item.text_pastes = text_pastes;
            return Some(());
        }

        // 2. Check if it is a pending steer message in transcript
        let steer_msg = self
            .messages
            .iter_mut()
            .chain(self.side_messages.iter_mut())
            .find(|m| {
                m.insert_id.as_deref() == Some(&id)
                    && m.delivery == crate::model::document::DeliveryStatus::Queued
            });
        if let Some(msg) = steer_msg {
            msg.raw = text.clone();
            msg.blocks = crate::model::document::parse_blocks_plain(&text);
            let expanded = crate::composer_attachments::expand_paste_chips(&text, &text_pastes);
            let expanded =
                crate::composer_attachments::strip_orphan_image_chips(&expanded, images.len());
            let sent_at_ms = msg.sent_at_ms;
            let _ = self.tx.send(AgentRequest::CancelSteer {
                session_id: session_id.to_string(),
                input_id: id.clone(),
            });
            let _ = self.tx.send(AgentRequest::Steer {
                session_id: session_id.to_string(),
                message: muta_contracts::QueuedMessage {
                    id: id.clone(),
                    text: expanded,
                    display_text: Some(text),
                    images,
                    sent_at_ms,
                },
            });
            return Some(());
        }

        None
    }

    /// Clear the remembered draft (text + attachments). Called when the
    /// draft's content is successfully sent: the input has been historicised
    /// (`record_input_history` already recorded it), so it is no longer the
    /// "unsent" slot and must not come back on a later ↓. A Phase-1 unsend
    /// re-adopts it via [`Self::adopt_as_draft`] with
    /// [`DraftAdoption::OnlyIfIdle`].
    pub fn clear_history_draft(&mut self) {
        self.history_draft.clear();
        self.history_draft_images.clear();
        self.history_draft_text_pastes.clear();
    }
}
