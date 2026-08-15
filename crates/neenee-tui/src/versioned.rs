//! A mutex paired with a monotonically increasing version counter.
//!
//! The render loop reads the shared transcript every frame. Deep-cloning the
//! whole `Vec<TranscriptMessage>` on every frame is O(n) in the transcript
//! length, which becomes the dominant per-frame cost in a long session — the
//! reason the TUI grows sluggish the longer it runs. `Versioned` lets the loop
//! skip the clone entirely while nothing has changed: a [`Versioned::write`]
//! guard bumps the version on drop, and the loop only re-clones when
//! [`Versioned::version`] advances past the value it last synced.
//!
//! Correctness rule: any access that mutates the inner value MUST go through
//! [`Versioned::write`]. Over-bumping (taking a `write()` guard for a read)
//! only costs an extra clone; under-bumping (mutating via [`Versioned::read`])
//! leaves the loop rendering stale state. So [`Versioned::read`] is reserved
//! for genuinely read-only access (the per-frame sync), and every mutation
//! site uses `write()`.

use std::collections::HashSet;
use std::ops::{Deref, DerefMut};
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicU64, Ordering};

use neenee_contracts::{EnvoyEvent, ToolStream};
use tokio::sync::{Mutex, MutexGuard};

/// Shared state guarded by a mutex and tagged with a version that advances on
/// every mutation, so readers can cheaply detect "nothing changed".
pub(super) struct Versioned<T> {
    inner: Mutex<T>,
    version: AtomicU64,
    /// Layout-height entries invalidated since the renderer last consumed the
    /// version. Most mutations affect the transcript's shape, so ordinary
    /// [`Self::write`]s conservatively invalidate everything. Streaming deltas
    /// are the important exception: they mutate only the live tail message and
    /// must not evict the measured heights of a long, otherwise frozen history.
    height_invalidation: StdMutex<HeightInvalidation>,
    /// Incremental updates the app can replay locally. Structural writes set a
    /// replacement snapshot; hot streaming writes add only their live-tail
    /// operation and avoid cloning every historical message into `App`.
    transcript_patch: StdMutex<TranscriptPatch>,
}

/// Which cached message heights became stale during one or more writes.
///
/// Kept next to the version counter rather than inferred by comparing two
/// complete transcript snapshots: walking and string-comparing the old history
/// on every streaming delta would reintroduce the O(transcript) work this
/// module exists to avoid.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) enum HeightInvalidation {
    #[default]
    None,
    Messages(HashSet<u64>),
    All,
}

impl HeightInvalidation {
    fn merge(&mut self, newer: Self) {
        match newer {
            Self::None => {}
            Self::All => *self = Self::All,
            Self::Messages(ids) => match self {
                Self::None => *self = Self::Messages(ids),
                Self::Messages(current) => current.extend(ids),
                Self::All => {}
            },
        }
    }
}

/// A batch of mutations the rendering copy of the transcript can replay.
#[derive(Debug, Default)]
pub(super) enum TranscriptPatch {
    #[default]
    None,
    Updates(Vec<TranscriptUpdate>),
    /// A structural mutation (insert/remove/finalization) requires the safe
    /// snapshot path and dominates every pending incremental update.
    Replace,
}

#[derive(Debug)]
pub(super) enum TranscriptUpdate {
    TextDelta {
        message_id: u64,
        delta: String,
    },
    ReasoningDelta {
        message_id: u64,
        delta: String,
    },
    ToolStream {
        id: String,
        stream: ToolStream,
    },
    EnvoyEvent {
        parent_call_id: String,
        event: EnvoyEvent,
    },
}

impl TranscriptPatch {
    fn merge(&mut self, newer: Self) {
        match newer {
            Self::None => {}
            Self::Replace => *self = Self::Replace,
            Self::Updates(mut updates) => match self {
                Self::None => *self = Self::Updates(updates),
                Self::Updates(current) => {
                    for update in updates.drain(..) {
                        Self::push_update(current, update);
                    }
                }
                Self::Replace => {}
            },
        }
    }

    fn push_update(updates: &mut Vec<TranscriptUpdate>, update: TranscriptUpdate) {
        // Provider chunks commonly outpace the render heartbeat. Coalesce
        // adjacent text-like chunks so Markdown is parsed once per visible
        // frame rather than once per token, without changing their order.
        match (updates.last_mut(), update) {
            (
                Some(TranscriptUpdate::TextDelta {
                    message_id: previous_id,
                    delta: previous,
                }),
                TranscriptUpdate::TextDelta { message_id, delta },
            ) if *previous_id == message_id => previous.push_str(&delta),
            (
                Some(TranscriptUpdate::ReasoningDelta {
                    message_id: previous_id,
                    delta: previous,
                }),
                TranscriptUpdate::ReasoningDelta { message_id, delta },
            ) if *previous_id == message_id => previous.push_str(&delta),
            (_, update) => updates.push(update),
        }
    }

    fn push_pending(&mut self, update: TranscriptUpdate) {
        match self {
            Self::None => *self = Self::Updates(vec![update]),
            Self::Updates(updates) => Self::push_update(updates, update),
            Self::Replace => {}
        }
    }
}

impl<T> Versioned<T> {
    /// Wrap `value`. The version starts at 1 so a loop tracking the sentinel
    /// `0` always performs its first sync.
    pub(super) fn new(value: T) -> Self {
        Self {
            inner: Mutex::new(value),
            version: AtomicU64::new(1),
            height_invalidation: StdMutex::new(HeightInvalidation::None),
            // `App` starts empty, so it must take a snapshot for version 1.
            transcript_patch: StdMutex::new(TranscriptPatch::Replace),
        }
    }

    /// The current version. Lock-free; safe to poll every frame.
    pub(super) fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    /// Acquire a read-only lock. Does **not** bump the version.
    pub(super) async fn read(&self) -> MutexGuard<'_, T> {
        self.inner.lock().await
    }

    /// Acquire a mutating lock. The version is bumped when the returned guard
    /// is dropped, so the next reader observes the change.
    pub(super) async fn write(&self) -> WriteGuard<'_, T> {
        self.write_with_invalidation(HeightInvalidation::All).await
    }

    /// Acquire a mutating guard for a high-frequency streaming update.
    ///
    /// Call [`WriteGuard::invalidate_message_height`] for every changed
    /// text/notice message. Tool and reasoning deltas have no height-cache
    /// entry, so they intentionally leave the invalidation empty. Structural
    /// mutations must keep using [`Self::write`].
    pub(super) async fn write_streaming(&self) -> WriteGuard<'_, T> {
        self.write_with_invalidation(HeightInvalidation::None).await
    }

    async fn write_with_invalidation(
        &self,
        height_invalidation: HeightInvalidation,
    ) -> WriteGuard<'_, T> {
        let transcript_patch = if matches!(height_invalidation, HeightInvalidation::All) {
            TranscriptPatch::Replace
        } else {
            TranscriptPatch::None
        };
        WriteGuard {
            guard: self.inner.lock().await,
            version: &self.version,
            target_invalidation: &self.height_invalidation,
            height_invalidation,
            target_patch: &self.transcript_patch,
            transcript_patch,
        }
    }

    /// Take the layout invalidations paired with transcript versions already
    /// observed by the event loop. A concurrent writer may add an extra
    /// invalidation before this call; evicting that entry one frame early is
    /// safe, while missing one would be incorrect.
    pub(super) fn take_height_invalidation(&self) -> HeightInvalidation {
        let mut guard = self
            .height_invalidation
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        std::mem::take(&mut *guard)
    }

    /// Consume the operations that correspond to the transcript version the
    /// event loop is about to display.
    pub(super) fn take_transcript_patch(&self) -> TranscriptPatch {
        let mut guard = self
            .transcript_patch
            .lock()
            .unwrap_or_else(|err| err.into_inner());
        std::mem::take(&mut *guard)
    }
}

/// A mutable guard that bumps the owning [`Versioned`]'s version on drop.
pub(super) struct WriteGuard<'a, T> {
    guard: MutexGuard<'a, T>,
    version: &'a AtomicU64,
    target_invalidation: &'a StdMutex<HeightInvalidation>,
    height_invalidation: HeightInvalidation,
    target_patch: &'a StdMutex<TranscriptPatch>,
    transcript_patch: TranscriptPatch,
}

impl<T> Deref for WriteGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.guard
    }
}

impl<T> DerefMut for WriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.guard
    }
}

impl<T> WriteGuard<'_, T> {
    /// Mark one text-like message whose cached wrapped height is now stale.
    /// This is meaningful only for a guard returned by [`Versioned::write_streaming`];
    /// ordinary guards are already conservatively `All`.
    pub(super) fn invalidate_message_height(&mut self, message_id: u64) {
        if let HeightInvalidation::Messages(ids) = &mut self.height_invalidation {
            ids.insert(message_id);
        } else if matches!(&self.height_invalidation, HeightInvalidation::None) {
            self.height_invalidation = HeightInvalidation::Messages(HashSet::from([message_id]));
        }
    }

    pub(super) fn record_text_delta(&mut self, message_id: u64, delta: String) {
        self.transcript_patch
            .push_pending(TranscriptUpdate::TextDelta { message_id, delta });
    }

    pub(super) fn record_reasoning_delta(&mut self, message_id: u64, delta: String) {
        self.transcript_patch
            .push_pending(TranscriptUpdate::ReasoningDelta { message_id, delta });
    }

    pub(super) fn record_tool_stream(&mut self, id: String, stream: ToolStream) {
        self.transcript_patch
            .push_pending(TranscriptUpdate::ToolStream { id, stream });
    }

    pub(super) fn record_envoy_event(&mut self, parent_call_id: String, event: EnvoyEvent) {
        self.transcript_patch
            .push_pending(TranscriptUpdate::EnvoyEvent {
                parent_call_id,
                event,
            });
    }

    /// Upgrade a streaming write if it discovered that it actually changed the
    /// transcript structure (for example, the first reasoning delta replacing
    /// an empty assistant placeholder).
    pub(super) fn require_transcript_snapshot(&mut self) {
        self.transcript_patch = TranscriptPatch::Replace;
    }
}

impl<T> Drop for WriteGuard<'_, T> {
    fn drop(&mut self) {
        {
            let mut invalidation = self
                .target_invalidation
                .lock()
                .unwrap_or_else(|err| err.into_inner());
            invalidation.merge(std::mem::take(&mut self.height_invalidation));
        }
        {
            let mut patch = self
                .target_patch
                .lock()
                .unwrap_or_else(|err| err.into_inner());
            patch.merge(std::mem::take(&mut self.transcript_patch));
        }
        // Release so the loop's `Acquire` load in `version()` sees the bump
        // together with the mutation it is paired with.
        self.version.fetch_add(1, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::{HeightInvalidation, TranscriptPatch, TranscriptUpdate, Versioned};

    #[tokio::test]
    async fn streaming_write_invalidates_only_the_changed_message() {
        let buffer = Versioned::new(vec!["before".to_string()]);
        {
            let mut messages = buffer.write_streaming().await;
            messages.push("after".to_string());
            messages.invalidate_message_height(42);
        }

        match buffer.take_height_invalidation() {
            HeightInvalidation::Messages(ids) => {
                assert_eq!(ids.len(), 1);
                assert!(ids.contains(&42));
            }
            other => panic!("expected one targeted invalidation, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn ordinary_write_remains_a_conservative_global_invalidation() {
        let buffer = Versioned::new(Vec::<String>::new());
        buffer.write().await.push("structural change".to_string());

        assert_eq!(buffer.take_height_invalidation(), HeightInvalidation::All);
    }

    #[tokio::test]
    async fn adjacent_text_deltas_coalesce_into_one_replay_operation() {
        let buffer = Versioned::new(Vec::<String>::new());
        assert!(matches!(
            buffer.take_transcript_patch(),
            TranscriptPatch::Replace
        ));
        {
            let mut messages = buffer.write_streaming().await;
            messages.record_text_delta(7, "hello ".to_string());
            messages.record_text_delta(7, "world".to_string());
        }

        match buffer.take_transcript_patch() {
            TranscriptPatch::Updates(updates) => {
                assert_eq!(updates.len(), 1);
                assert!(matches!(
                    &updates[0],
                    TranscriptUpdate::TextDelta { message_id: 7, delta }
                        if delta == "hello world"
                ));
            }
            other => panic!("expected coalesced replay update, got {other:?}"),
        }
    }
}
