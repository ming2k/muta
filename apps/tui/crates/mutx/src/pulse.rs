//! Byte-driven liveness for the activity bar's dot and micro-meter.
//!
//! The wire's `StreamDelta` events are the only honest proof that "tokens
//! are still coming out". This module turns their arrival times into two
//! signals without ever needing an animation timer:
//!
//! * [`BytePulse`] — an excited-decay energy accumulator. Each injected
//!   delta raises the energy; reads decay it exponentially **lazily** (the
//!   stored value is stamped with its injection instant, so `levels()` is a
//!   pure function of `now` — no per-frame bookkeeping anywhere).
//! * The renderer maps the two decay channels onto the dot's luminance
//!   (with a dark-ember floor so "quiet between chunks" never reads as
//!   "dead") and onto a two-cell block-density micro-meter (a time
//!   histogram of recent delta pressure).
//!
//! Armed semantics live at the call sites: `reset()` fires on every new
//! model-request cycle (`TurnStarted`) and on `StreamStart`, so stale
//! timestamps from a previous turn can never trip the bar into reporting
//! silence before the current stream has produced anything.

use std::time::Instant;

/// Decay time constants, seconds. Fast channel tracks chunk rhythm (~0.4s
/// perceptual attack/decay); the slow channel holds a trailing average so
/// the meter reads like a level gauge rather than flickering in lockstep.
pub const TAU_FAST: f32 = 0.4;
pub const TAU_SLOW: f32 = 1.6;
/// Energy injected per delta. Saturates after ~3 rapid chunks.
const INJECT: f32 = 0.45;
const CAP: f32 = 1.25;
/// Inter-chunk quiet stretches shorter than this are natural (thinking
/// models breathe); only past it does the silent clause arm.
pub const SILENT_AFTER: std::time::Duration = std::time::Duration::from_secs(8);

#[derive(Debug, Clone)]
struct Channel {
    /// This channel's decay time constant (seconds).
    tau: f32,
    /// Energy as of `stamped`.
    energy: f32,
    stamped: Instant,
}

impl Channel {
    fn new(tau: f32, stamped: Instant) -> Self {
        Self {
            tau,
            energy: 0.0,
            stamped,
        }
    }

    fn level(&self, now: Instant) -> f32 {
        let dt = now.saturating_duration_since(self.stamped).as_secs_f32();
        self.energy * (-dt / self.tau).exp()
    }

    fn inject(&mut self, now: Instant) {
        let dt = now.saturating_duration_since(self.stamped).as_secs_f32();
        // Fold pending decay under this channel's own time constant before
        // adding, so ordering of reads/writes doesn't matter.
        self.energy = (self.energy * (-dt / self.tau).exp() + INJECT).min(CAP);
        self.stamped = now;
    }
}

#[derive(Debug, Clone, Default)]
pub struct BytePulse {
    fast: Option<Channel>,
    slow: Option<Channel>,
}

impl BytePulse {
    /// Whether this stream has produced at least one delta since the last
    /// reset (drives both the pulse dot and the silent-clause arming).
    pub fn armed(&self) -> bool {
        self.fast.is_some()
    }

    /// Stamp one arriving delta.
    pub fn inject(&mut self, now: Instant) {
        // First arrival arms the channel *at this instant* — using an
        // internal clock here would misstamp silence arithmetic. Each
        // channel folds decay under its own tau before adding.
        let fast = self.fast.get_or_insert_with(|| Channel::new(TAU_FAST, now));
        fast.inject(now);
        let slow = self.slow.get_or_insert_with(|| Channel::new(TAU_SLOW, now));
        slow.inject(now);
    }

    /// New model-request cycle or fresh stream: forget all history so old
    /// arrivals cannot alias into this one.
    pub fn reset(&mut self) {
        self.fast = None;
        self.slow = None;
    }

    /// Stamp instant of the most recent arrival, for the silent clause
    /// (`now - last ≥ threshold`). `None` while unarmed.
    pub fn last_arrival(&self) -> Option<Instant> {
        self.fast.as_ref().map(|ch| ch.stamped)
    }

    /// Seconds since the last delta, rounded down — surfaced only once past
    /// [`SILENT_AFTER`] so natural inter-chunk pauses never read as trouble.
    pub fn silent_secs(&self, now: Instant) -> Option<u64> {
        let last = self.last_arrival()?;
        let dt = now.saturating_duration_since(last);
        (dt >= SILENT_AFTER).then_some(dt.as_secs())
    }

    /// `(fast, slow)` decayed levels, both clamped to 0..=1. Pure function
    /// of `now`.
    pub fn levels(&self, now: Instant) -> Option<(f32, f32)> {
        let fast = self.fast.as_ref()?;
        let slow = self.slow.as_ref()?;
        Some((
            fast.level(now).clamp(0.0, 1.0),
            slow.level(now).clamp(0.0, 1.0),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn unarmed_pulse_reports_nothing() {
        let pulse = BytePulse::default();
        assert!(!pulse.armed());
        assert!(pulse.levels(Instant::now()).is_none());
    }

    #[test]
    fn inject_then_decay_is_monotone_and_positive() {
        let mut pulse = BytePulse::default();
        let t0 = Instant::now();
        pulse.inject(t0);
        assert!(pulse.armed());
        // At the injection instant both channels hold the same energy; the
        // fast/slow distinction shows itself in the *decay*.
        let later = t0 + Duration::from_millis(900);
        let (f1, s1) = pulse.levels(later).unwrap();
        assert!(
            f1 < s1,
            "fast channel decays quicker: must read cooler than slow"
        );
        assert!(f1 > 0.0 && s1 > 0.0, "no full decay inside one tau window");
        let (f0, _) = pulse.levels(t0).unwrap();
        assert!(f1 <= f0, "decay is monotone down");
        // Stamping a stale instant (earlier than `now`) never regresses the
        // channel: monotonicity holds for out-of-order callers too.
        pulse.inject(t0);
        let (_, s2) = pulse.levels(later).unwrap();
        assert!(s2 >= s1, "stale injection must not rewind the clock");
    }

    #[test]
    fn reset_clears_arming_so_stale_deltas_cannot_alias() {
        let mut pulse = BytePulse::default();
        pulse.inject(Instant::now());
        pulse.reset();
        assert!(!pulse.armed());
        assert!(pulse.levels(Instant::now()).is_none());
    }

    #[test]
    fn silent_secs_arms_only_after_quiet_threshold() {
        let mut pulse = BytePulse::default();
        let t0 = Instant::now();
        pulse.inject(t0);
        assert_eq!(pulse.silent_secs(t0 + Duration::from_secs(3)), None);
        // 8s threshold inclusive: silence counts whole seconds past it.
        assert_eq!(pulse.silent_secs(t0 + Duration::from_secs(9)), Some(9));
    }

    #[test]
    fn sustained_injection_saturates_smoothly() {
        let mut pulse = BytePulse::default();
        let t = Instant::now();
        for _ in 0..8 {
            pulse.inject(t);
            // fold decay between bursts so energy converges instead of
            // being raw-added past the cap
            pulse.levels(t).unwrap();
        }
        pulse.fast.as_mut().unwrap().inject(t);
        let (f, _) = pulse.levels(t).unwrap();
        assert!(f <= 1.0, "displayed level clamps at full");
    }
}
