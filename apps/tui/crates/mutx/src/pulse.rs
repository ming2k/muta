//! Token-stall watchdog for the activity bar's silence clause.
//!
//! Per ADR-0154 the bar no longer cares whether output is *continuous*
//! (the old `BytePulse` energy meter drove a luminance shimmer and a
//! two-cell block-density micro-meter — visual noise, retired). The one
//! honest signal worth surfacing is a **stall**: the HTTP request is open,
//! the connection is held, but no token has arrived for a long time.
//!
//! [`TokenWatch`] is the minimal state for that question — a single
//! `last_token_at` stamp plus an armed flag — with lazy reads (no timers,
//! no per-frame bookkeeping):
//!
//! * [`TokenWatch::arm`] — a new model-request cycle opens: start (or
//!   restart) the clock at *this* instant, so silence counts from the
//!   request, not from stale stamps of a previous turn.
//! * [`TokenWatch::note_token`] — each arriving delta re-stamps the clock.
//! * [`TokenWatch::stalled_secs`] — `Some(secs)` only once the quiet
//!   stretch exceeds the threshold, which differs by regime: waiting for
//!   the *first* byte of a fresh request tolerates more (TTFT is routinely
//!   slow on reasoning models), while a stream that has already produced
//!   tokens and then went quiet is more suspicious and arms sooner.
//!
//! Armed semantics live at the call sites: `arm()` fires on every new
//! model-request cycle (`TurnStarted`); a token arrival keeps the stream
//! regime in force for the rest of the turn.

use std::time::{Duration, Instant};

/// Quiet tolerance for the first byte of a fresh request (TTFT). Reasoning
/// models legitimately spend tens of seconds before the first payload, and
/// the client's own request timeout is the backstop — this clause is a
/// courtesy heads-up, not a failure detector.
pub const FIRST_BYTE_AFTER: Duration = Duration::from_secs(45);

/// Quiet tolerance once a stream has already produced tokens: any token
/// proves the connection is live, so a long gap afterwards reads as a held
/// connection with nothing coming out — the exact case this clause exists
/// to flag. Thinking models do breathe between chunks, so this stays
/// comfortably above natural inter-chunk pauses.
pub const STREAM_SILENT_AFTER: Duration = Duration::from_secs(8);

#[derive(Debug, Clone)]
pub struct TokenWatch {
    /// Clock origin: when the current model request opened (armed), or the
    /// most recent token arrival once one has landed.
    last_event_at: Instant,
    /// Whether at least one token has arrived since the last `arm`.
    /// Selects which tolerance applies and drives clause wording.
    saw_token: bool,
    armed: bool,
}

impl Default for TokenWatch {
    fn default() -> Self {
        Self {
            last_event_at: Instant::now(),
            saw_token: false,
            armed: false,
        }
    }
}

impl TokenWatch {
    /// A new model-request cycle opens: forget all history so old stamps
    /// cannot alias into this one, and start the clock at `now` so silence
    /// is measured from the request itself.
    pub fn arm(&mut self, now: Instant) {
        self.last_event_at = now;
        self.saw_token = false;
        self.armed = true;
    }

    /// Stamp one arriving token. Idempotent with respect to regime: the
    /// first arrival flips `saw_token`, later ones only refresh the clock.
    pub fn note_token(&mut self, now: Instant) {
        self.last_event_at = now;
        self.saw_token = true;
    }

    /// Disarm (round end / abort): no clause may read against a closed
    /// request.
    pub fn disarm(&mut self) {
        self.armed = false;
    }

    /// Whether a request cycle is currently open.
    pub fn is_armed(&self) -> bool {
        self.armed
    }

    /// Whether the current cycle has produced at least one token.
    pub fn saw_token(&self) -> bool {
        self.saw_token
    }

    /// Seconds of quiet past the applicable threshold, rounded down —
    /// `None` while unarmed or within tolerance. Pure function of `now`.
    pub fn stalled_secs(&self, now: Instant) -> Option<u64> {
        if !self.armed {
            return None;
        }
        let dt = now.saturating_duration_since(self.last_event_at);
        let threshold = if self.saw_token {
            STREAM_SILENT_AFTER
        } else {
            FIRST_BYTE_AFTER
        };
        (dt >= threshold).then_some(dt.as_secs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn unarmed_watch_reports_nothing() {
        let watch = TokenWatch::default();
        assert!(!watch.is_armed());
        assert_eq!(watch.stalled_secs(t0()), None);
    }

    #[test]
    fn first_byte_tolerance_is_generous() {
        let mut watch = TokenWatch::default();
        let start = t0();
        watch.arm(start);
        assert!(watch.is_armed());
        assert!(!watch.saw_token());
        // Natural TTFT (up to 45s) stays silent — the clause must not cry
        // wolf on reasoning models' slow first payload.
        assert_eq!(watch.stalled_secs(start + Duration::from_secs(30)), None);
        // Past the first-byte threshold the clause arms, counting from the
        // request itself.
        assert_eq!(
            watch.stalled_secs(start + FIRST_BYTE_AFTER),
            Some(FIRST_BYTE_AFTER.as_secs())
        );
    }

    #[test]
    fn one_token_tightens_the_threshold() {
        let mut watch = TokenWatch::default();
        let start = t0();
        watch.arm(start);
        let token_at = start + Duration::from_secs(5);
        watch.note_token(token_at);
        assert!(watch.saw_token());
        // Quiet measured from the token, not the arm: 8s window restarts.
        assert_eq!(
            watch.stalled_secs(token_at + Duration::from_secs(7)),
            None
        );
        assert_eq!(
            watch.stalled_secs(token_at + STREAM_SILENT_AFTER),
            Some(STREAM_SILENT_AFTER.as_secs())
        );
    }

    #[test]
    fn rearming_resets_regime_and_clock() {
        let mut watch = TokenWatch::default();
        let start = t0();
        watch.arm(start);
        watch.note_token(start + Duration::from_secs(2));
        // New cycle mid-stream: back to first-byte tolerance, clock at now.
        let cycle2 = start + Duration::from_secs(60);
        watch.arm(cycle2);
        assert!(!watch.saw_token());
        assert_eq!(
            watch.stalled_secs(cycle2 + Duration::from_secs(20)),
            None,
            "fresh cycle uses the generous TTFT budget"
        );
        assert_eq!(
            watch.stalled_secs(cycle2 + FIRST_BYTE_AFTER),
            Some(FIRST_BYTE_AFTER.as_secs())
        );
    }

    #[test]
    fn disarm_silences_the_clause() {
        let mut watch = TokenWatch::default();
        let start = t0();
        watch.arm(start);
        watch.disarm();
        assert!(!watch.is_armed());
        assert_eq!(
            watch.stalled_secs(start + Duration::from_secs(120)),
            None,
            "a closed request cannot report a stall"
        );
    }
}
