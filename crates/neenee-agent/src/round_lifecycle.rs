//! One session's round lifecycle: at most one active round, superseded by the
//! next `begin`, with stale rounds detected by a generation counter.
//!
//! This type consolidates the cancellation-token slot + generation-counter
//! protocol that used to be threaded through the transport layer as two
//! separate `Arc`s and re-implemented at four call sites (interactive round,
//! pursuit, `!` shell command, `/btw` side session). The protocol itself is
//! deliberately binary — no active round, or an active round identified by a
//! generation; display-level nuance ("running" vs. "pursue" vs.
//! awaiting-permission) lives outside it, in
//! [`neenee_core::LoopStatus`] and the parked-request tables.
//!
//! The two stop paths differ on purpose:
//!
//! - **Interrupt** (`cancel_current` alone): the in-flight round unwinds and
//!   still emits its own `[Interrupted]` cleanup, because the generation is
//!   *not* bumped.
//! - **Session switch** (`supersede` + `cancel_current`): the generation bump
//!   invalidates the in-flight round, so its generation-guarded cleanup is
//!   suppressed and the switch handler owns the terminal events.

use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::RwLock as AsyncRwLock;
use tokio_util::sync::CancellationToken;

/// At-most-one-active-round protocol for one session.
///
/// Cloned into an `Arc` by the owning session (primary or `/btw` side) and
/// shared by every driver that can start or stop a round.
#[derive(Debug, Default)]
pub struct RoundLifecycle {
    token_slot: AsyncRwLock<Option<CancellationToken>>,
    generation: AtomicU64,
}

/// The result of [`RoundLifecycle::begin`].
#[derive(Debug)]
pub struct RoundBegin {
    /// Token the new round listens on for cancellation.
    pub token: CancellationToken,
    /// Generation identifying this round; pass to
    /// [`RoundLifecycle::is_current`] / [`RoundLifecycle::finish`].
    pub generation: u64,
    /// The superseded round's token, if one was installed. Cancel it *after*
    /// rejecting pending permissions/inputs so parked replies resolve first.
    pub previous: Option<CancellationToken>,
}

impl RoundLifecycle {
    pub fn new() -> Self {
        Self::default()
    }

    /// Begin a new round: bump the generation, install a fresh cancellation
    /// token, and return the superseded predecessor (if any) for the caller
    /// to cancel.
    pub async fn begin(&self) -> RoundBegin {
        let token = CancellationToken::new();
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let previous = self.token_slot.write().await.replace(token.clone());
        RoundBegin {
            token,
            generation,
            previous,
        }
    }

    /// Whether `generation` still identifies the active round.
    pub fn is_current(&self, generation: u64) -> bool {
        self.generation.load(Ordering::SeqCst) == generation
    }

    /// End-of-round cleanup: release the token slot, but only when
    /// `generation` is still the active round. Returns whether the caller is
    /// still current — and should therefore emit its terminal idle snapshot.
    pub async fn finish(&self, generation: u64) -> bool {
        let mut slot = self.token_slot.write().await;
        if !self.is_current(generation) {
            return false;
        }
        slot.take();
        true
    }

    /// Invalidate the current generation without installing a token. Session
    /// switches (`/resume`, `/session open|fork|new`, …) use this to suppress
    /// the in-flight round's generation-guarded cleanup events.
    pub fn supersede(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    /// Take and cancel the live token, if any; returns whether one was
    /// cancelled. Does not bump the generation — see the module docs for the
    /// interrupt vs. session-switch distinction.
    pub async fn cancel_current(&self) -> bool {
        if let Some(token) = self.token_slot.write().await.take() {
            token.cancel();
            true
        } else {
            false
        }
    }

    /// Coarse activity signal for watchers (e.g. the `/btw` parent-status
    /// banner): a live, uncancelled token means a round is running.
    pub async fn is_running(&self) -> bool {
        matches!(
            self.token_slot.read().await.as_ref(),
            Some(token) if !token.is_cancelled()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn begin_supersedes_predecessor() {
        let lifecycle = RoundLifecycle::new();
        let first = lifecycle.begin().await;
        assert!(first.previous.is_none());
        assert!(lifecycle.is_current(first.generation));
        assert!(lifecycle.is_running().await);

        let second = lifecycle.begin().await;
        let previous = second.previous.expect("second begin supersedes first");
        assert!(!first.token.is_cancelled(), "begin does not cancel");
        previous.cancel();
        assert!(lifecycle.is_current(second.generation));
        assert!(!lifecycle.is_current(first.generation));
    }

    #[tokio::test]
    async fn finish_is_generation_guarded() {
        let lifecycle = RoundLifecycle::new();
        let first = lifecycle.begin().await;
        let second = lifecycle.begin().await;

        // The stale round's cleanup is a no-op: slot keeps the successor's
        // token and the caller is told it is no longer current.
        assert!(!lifecycle.finish(first.generation).await);
        assert!(lifecycle.is_running().await);

        assert!(lifecycle.finish(second.generation).await);
        assert!(!lifecycle.is_running().await);
    }

    #[tokio::test]
    async fn supersede_invalidates_without_token_install() {
        let lifecycle = RoundLifecycle::new();
        let active = lifecycle.begin().await;
        lifecycle.supersede();
        assert!(!lifecycle.is_current(active.generation));
        // The token itself is untouched; cancelling it is a separate step.
        assert!(lifecycle.is_running().await);
        assert!(lifecycle.cancel_current().await);
        assert!(active.token.is_cancelled());
    }

    #[tokio::test]
    async fn cancel_current_on_empty_slot_is_a_no_op() {
        let lifecycle = RoundLifecycle::new();
        assert!(!lifecycle.cancel_current().await);
        let active = lifecycle.begin().await;
        assert!(lifecycle.cancel_current().await);
        assert!(active.token.is_cancelled());
        assert!(!lifecycle.is_running().await);
        // The generation is deliberately not bumped by cancellation.
        assert!(lifecycle.is_current(active.generation));
    }
}
