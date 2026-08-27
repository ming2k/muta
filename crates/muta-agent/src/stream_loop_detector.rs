//! In-flight streaming loop detector: continuity-verified degenerative-output
//! circuit breaker.
//!
//! # Judging philosophy
//!
//! The previous generation fired *edge-triggered*: the moment a sliding
//! window contained a periodic suffix of nominal length, the stream was
//! interrupted for semantic review. Legitimate content — table borders,
//! rules, fenced data — enters such a state almost immediately, so the
//! expensive arbiter was consulted for nearly every decorated response and
//! carried all of the accuracy on its own.
//!
//! This implementation judges **continuity and volume**, not instantaneous
//! state:
//!
//! 1. *Dwell*: a periodic tail counts only while the stream keeps extending
//!    it. Suspicion accumulates in [`DwellTrail`] push by push and discharges
//!    to zero the moment the tail leaves the cycle — an acquitted pattern
//!    starts fresh. Escalation therefore requires roughly
//!    [`MIN_DWELL_CHARS`] of uninterrupted repetition, which no legitimate
//!    decoration reaches and every genuine runaway blows past.
//! 2. *Budget*: character-class density (digit/data floods) likewise spends
//!    toward [`MAX_DEGENERATE_BUDGET_CHARS`] before becoming actionable, with
//!    exponential decay when density lapses, so bounded dumps of hashes or
//!    coordinates never trip it.
//!
//! There is deliberately **no glyph whitelist**: box-drawing characters,
//! emoji, ASCII art, ligatures — all symbol systems pass through the same two
//! behavioral rules. The escape hatch is whether the model *stopped
//! repeating*, never *which characters it used*.
//!
//! Mechanical detectors (pure functions over the current window snapshot):
//!
//! 1. **Arbitrary periodic loops**: KMP prefix-function border analysis for
//!    wholly-periodic windows (any period), plus a bounded-block tail-run
//!    scan for windows that merely *end* in a periodic run. Detectors return
//!    an exact description of the trailing run; they never fire early.
//! 2. **Monotonic progression** (`Step 1 … Step 5`): skeleton abstraction of
//!    consecutive lines tracked as a cross-push streak.
//! 3. **Unbounded data streams**: digit-density classification feeding the
//!    budget described above.

/// Chars of *continuous* periodic repetition required before a mechanical
/// candidate escalates. Three kilobytes of unbroken tail far exceeds every
/// legitimate decoration and sits orders of magnitude below real runaway
/// losses. Tunable expectation: killing a true doom loop at ~3KB costs
/// pennies; interrupting live output any earlier taxes correct responses.
pub const MIN_DWELL_CHARS: usize = 3_000;

/// Cumulative digit-dense chars (within decay accounting) before a data
/// flood hard-stops the stream. Deliberately generous: byte dumps are boring,
/// not broken.
pub const MAX_DEGENERATE_BUDGET_CHARS: usize = 8_192;

/// Upper unit length for the tail-run block scan. Windows that are *entirely*
/// periodic of any period are handled exactly by the KMP border path, so this
/// bound only limits the mixed-prefix phase; combined with the fact that any
/// sufficiently long runaway eventually fills the window and trips the exact
/// path, precision is unaffected.
const MAX_TAIL_SCAN_UNIT: usize = 64;

/// Digit-density threshold classifying a window as raw-data flood.
const DIGIT_DENSITY_RATIO: f32 = 0.88;

/// Classification of detected degenerative output patterns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DegeneratePattern {
    /// Arbitrary periodic repetition ending at the current tail
    /// (e.g. `...abcabcabc`, trailing 12 chars, "abc" x 4).
    Periodic {
        period: usize,
        /// Whole copies of `pattern` visible in the trailing run.
        repetitions: usize,
        /// The repeating unit, `period` chars.
        pattern: String,
        /// Chars at the tail belonging to this run (≥ `repetitions * period`;
        /// may end mid-copy).
        suffix_len: usize,
    },
    /// Monotonic sequence repetition (e.g. `Step 1, Step 2, Step 3 …`).
    MonotonicSequence { template: String, count: usize },
    /// Unbounded digit or raw data generation (e.g. endless π digits).
    UnboundedDigitStream { length: usize },
}

impl DegeneratePattern {
    /// Human-readable summary of the detected pattern for logs and steering prompts.
    pub fn description(&self) -> String {
        match self {
            Self::Periodic {
                period,
                repetitions,
                pattern,
                suffix_len,
            } => {
                let preview: String = {
                    let mut s: String = pattern.chars().take(20).collect();
                    if pattern.chars().count() > 20 {
                        s.push_str("...");
                    }
                    s
                };
                format!(
                    "periodic loop (period={period}, repetitions={repetitions}, \
                     suffix={suffix_len} chars, pattern='{preview}')"
                )
            }
            Self::MonotonicSequence { template, count } => {
                format!("monotonic sequence '{template}' repeated {count} times")
            }
            Self::UnboundedDigitStream { length } => {
                format!("unbounded digit/data stream ({length} chars)")
            }
        }
    }

    /// Extent of the degenerate tail this verdict accounts for, in chars.
    pub fn tail_chars(&self) -> usize {
        match self {
            Self::Periodic { suffix_len, .. } => *suffix_len,
            Self::MonotonicSequence { count, .. } => *count * 24,
            Self::UnboundedDigitStream { length } => *length,
        }
    }
}

/// One candidate observation for the dwell trail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrailObservation {
    pub period: usize,
    pub unit: Vec<u8>,
    pub suffix_len: usize,
}

/// Continuity ledger for periodic candidates across chunk pushes.
///
/// Being *in* a periodic state is not evidence of a loop; *staying* in it is.
/// Each push reports what the tail currently looks like; matching fingerprints
/// accrue the freshly streamed chars as verified-continuous dwell, any regime
/// change restarts the ledger, and leaving the cycle clears it outright.
#[derive(Debug, Default)]
struct DwellTrail {
    active: Option<TrailObservation>,
    depth: usize,
}

impl DwellTrail {
    /// Observe the tail after `pushed_chars` new chars were streamed.
    ///
    /// Returns `true` once continuous dwell reached [`MIN_DWELL_CHARS`].
    fn observe(
        &mut self,
        observation: Option<TrailObservation>,
        pushed_chars: usize,
        dwell_threshold: usize,
    ) -> bool {
        let Some(next) = observation else {
            // Tail left the cycle: acquittal. Suspicion does not survive its
            // own refutation.
            self.active = None;
            self.depth = 0;
            return false;
        };

        match &self.active {
            Some(prev) if prev.period == next.period => {
                // Same period regime continuing: credit the newly streamed
                // chars as continuous dwell. Fingerprints compare by *period
                // only*: the sliding window may rotate which copy sits first
                // (byte-exact unit can flip phase on window drain), which is
                // bookkeeping noise, not a behavior change from the model.
                self.depth = self.depth.saturating_add(pushed_chars);
                self.depth = self.depth.min(dwell_threshold);
            }
            _ => {
                // Fresh or changed regime: count only what it shows now.
                self.depth = next.suffix_len.min(dwell_threshold);
            }
        }
        self.active = Some(next);
        self.depth >= dwell_threshold
    }
}

pub struct StreamLoopDetector {
    buffer: String,
    window_size: usize,
    /// `(last skeleton, consecutive-line streak)` for monotonic sequences.
    monotonic_streak: (Option<String>, usize),
    /// Budget spent on digit-dense windows, with decay on density lapse.
    digit_budget_spent: usize,
    trail: DwellTrail,
    max_degenerate_budget_chars: usize,
}

impl Default for StreamLoopDetector {
    fn default() -> Self {
        Self::new(1024)
    }
}

impl StreamLoopDetector {
    pub fn new(window_size: usize) -> Self {
        Self {
            buffer: String::new(),
            window_size,
            monotonic_streak: (None, 0),
            digit_budget_spent: 0,
            trail: DwellTrail::default(),
            max_degenerate_budget_chars: MAX_DEGENERATE_BUDGET_CHARS,
        }
    }

    /// Feed a stream chunk; returns a mechanical verdict only when continuity
    /// plus volume clear their bars. `None` means "not yet actionable" — it
    /// deliberately says nothing about the current instant.
    pub fn push_and_check(&mut self, chunk: &str) -> Option<DegeneratePattern> {
        if chunk.is_empty() {
            return None;
        }
        let pushed_chars = chunk.chars().count();

        self.buffer.push_str(chunk);
        if self.buffer.len() > self.window_size {
            let excess = self.buffer.len() - self.window_size;
            // Drain at a char boundary.
            let mut cut = excess;
            while !self.buffer.is_char_boundary(cut) && cut < self.buffer.len() {
                cut += 1;
            }
            self.buffer.drain(..cut);
        }

        // --- Data flood budget ------------------------------------------------
        // Density is evaluated first and *shields* the periodic trail: a
        // window of raw data needs no continuity analysis (its danger is
        // volumetric), and repeating numeric columns must not double-count.
        match Self::classify_digit_density(&self.buffer) {
            Some(length) => {
                // While the recent window stays data-dense, every streamed
                // char is degenerate spend.
                self.digit_budget_spent = self.digit_budget_spent.saturating_add(pushed_chars);
                if self.digit_budget_spent >= self.max_degenerate_budget_chars {
                    self.digit_budget_spent = 0;
                    return Some(DegeneratePattern::UnboundedDigitStream { length });
                }
                return None;
            }
            None => {
                // Density lapsed: forgive the accrued spend geometrically so
                // ordinary numeric-heavy prose drains the balance instead of
                // carrying it forever.
                self.digit_budget_spent /= 2;
            }
        }

        // --- Periodic tail, gated by continuity -----------------------------
        if let Some(observation) = Self::observe_periodic_tail(&self.buffer) {
            if self
                .trail
                .observe(Some(observation), pushed_chars, MIN_DWELL_CHARS)
                && let Some(obs) = self.trail.active.clone()
            {
                return Some(DegeneratePattern::Periodic {
                    period: obs.period,
                    repetitions: obs.suffix_len / obs.period,
                    pattern: String::from_utf8_lossy(&obs.unit).into_owned(),
                    suffix_len: obs.suffix_len,
                });
            }
        } else {
            self.trail.observe(None, pushed_chars, MIN_DWELL_CHARS);
        }

        // --- Monotonic line skeleton ----------------------------------------
        if let Some(pat) = Self::advance_monotonic_streak(&self.buffer, &mut self.monotonic_streak)
        {
            return Some(pat);
        }

        None
    }

    /// Forget accumulated suspicion (fresh provider request, channel switch).
    pub fn reset(&mut self) {
        self.buffer.clear();
        self.monotonic_streak = (None, 0);
        self.digit_budget_spent = 0;
        self.trail = DwellTrail::default();
    }

    /// Inspect the buffer's tail for a periodic run.
    ///
    /// Two exact paths, preferred in order:
    ///
    /// 1. **Wholly periodic window** (KMP String Periodicity Theorem): if
    ///    $\pi[L]$ implies the minimal period $p$ divides $L$, the entire
    ///    buffer — whatever its unit length — is one periodic run. Any period
    ///    size is caught here once a runaway floods past window-prefix noise.
    /// 2. **Bounded tail-run scan**: otherwise find, over unit lengths
    ///    $p \le$ [`MAX_TAIL_SCAN_UNIT`], the longest suffix consisting of
    ///    `>= 2` aligned copies. Handles a legitimately-varied document that
    ///    merely *ends* in repetition.
    ///
    /// Returns `None` when neither applies; never fires on nominal lengths.
    pub fn observe_periodic_tail(buffer: &str) -> Option<TrailObservation> {
        let chars: Vec<char> = buffer.chars().collect();
        let n = chars.len();
        if n < 18 {
            return None;
        }

        // Path 1 — whole-window periodicity via prefix function.
        let mut pi = vec![0usize; n];
        for i in 1..n {
            let mut j = pi[i - 1];
            while j > 0 && chars[i] != chars[j] {
                j = pi[j - 1];
            }
            if chars[i] == chars[j] {
                j += 1;
            }
            pi[i] = j;
        }
        let kmp_period = n - pi[n - 1];
        // Weak-periodicity rule: `chars[i] == chars[i+p]` for every i means
        // the whole window rides one period — divisibility is NOT required.
        // A sliding window drained to an arbitrary byte length still reports
        // its true period instead of falling through on `n % p != 0`.
        //
        // Evidence floor: at least 4 copies of the claimed unit must fit in
        // the window. Two cross-aligned copies of an ordinary sentence can
        // satisfy the character-equality identity by accident; four cannot
        // arise outside genuine cycles (or single-copy floods handled via
        // period 1..2 anyway).
        if kmp_period < n
            && n / kmp_period >= 4
            && (0..n - kmp_period).all(|i| chars[i] == chars[i + kmp_period])
        {
            let unit: String = chars[n - kmp_period..].iter().collect();
            return Some(TrailObservation {
                period: kmp_period,
                unit: unit.into_bytes(),
                suffix_len: n,
            });
        }

        // Path 2 — longest `>= 2`-copy run at the tail, small units only.
        // A run counts only if the final unit repeats at least twice in full;
        // mid-copy tails are fine because alignment is by offset from the end.
        let mut best: Option<(usize, usize)> = None; // (suffix_len, period); ties prefer smaller p
        for p in 1..=MAX_TAIL_SCAN_UNIT.min(n / 2) {
            // Last position whose p-forward comparison fails.
            let last_mismatch = (0..n - p).rev().find(|&i| chars[i] != chars[i + p]);
            let run_start = match last_mismatch {
                Some(i) => i + 1,
                // Every comparison passed — the window is fully p-periodic,
                // which path 1 would have classified; skip to a shorter unit.
                None => continue,
            };
            let run = n - run_start;
            if run >= 2 * p && best.is_none_or(|(b, bp)| run > b || (run == b && p < bp)) {
                best = Some((run, p));
            }
        }
        let (suffix_len, p) = best?;
        let unit: String = chars[suffix_len - p..suffix_len].iter().collect();
        Some(TrailObservation {
            period: p,
            unit: unit.into_bytes(),
            suffix_len,
        })
    }

    /// Advance the monotonic-sequence streak over the buffer's non-empty
    /// lines; returns a verdict once five consecutive tail skeletons agree
    /// and their first embedded numbers strictly ascend by one (the sole
    /// progression signature observed in pathological generator streams —
    /// descending or gapped numbering stays legal content).
    fn advance_monotonic_streak(
        buffer: &str,
        streak: &mut (Option<String>, usize),
    ) -> Option<DegeneratePattern> {
        const MONOTONIC_MIN_LINES: usize = 5;

        let lines: Vec<&str> = buffer
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect();

        // Skeletons and their embedded numbers for the last comparable lines,
        // tail-first.
        let mut skeletons: Vec<String> = Vec::with_capacity(8);
        let mut numbers: Vec<f64> = Vec::with_capacity(8);

        for line in lines.iter().rev().take(8) {
            let mut skeleton = String::new();
            let mut line_nums: Vec<f64> = Vec::new();
            let mut digits = String::new();

            for c in line.chars() {
                if c.is_ascii_digit() {
                    digits.push(c);
                } else {
                    if !digits.is_empty() {
                        line_nums.push(digits.parse::<f64>().unwrap_or(0.0));
                        digits.clear();
                    }
                    let mapped = if c.is_alphanumeric() { 'x' } else { c };
                    skeleton.push(mapped);
                }
            }
            if !digits.is_empty() {
                line_nums.push(digits.parse::<f64>().unwrap_or(0.0));
            }
            // First embedded number is the sequence position slot.
            numbers.push(line_nums.first().copied().unwrap_or(f64::NAN));
            skeletons.push(skeleton);
        }

        // How many tail-consecutive lines share the leading skeleton?
        let Some(first) = skeletons.first() else {
            *streak = (None, 0);
            return None;
        };
        let run = skeletons.iter().take_while(|sk| *sk == first).count();
        if streak.0.as_ref() != Some(first) {
            *streak = (Some(first.clone()), run);
        } else {
            streak.1 = run;
        }

        if run < MONOTONIC_MIN_LINES {
            return None;
        }

        // positions[0] is the newest line; consecutive ascending lines run
        // newest→oldest, so ascending-by-one means earlier entries are larger.
        let positions = &numbers[..run];
        let all_numbered = positions.iter().all(|n| n.is_finite());
        let ascending_step_one = positions.windows(2).all(|w| w[0] - w[1] == 1.0);
        if !(all_numbered && ascending_step_one) {
            return None;
        }

        Some(DegeneratePattern::MonotonicSequence {
            template: first.clone(),
            count: run,
        })
    }

    /// Classify the window as a digit/data flood; `Some(total_chars)` when
    /// density crosses [`DIGIT_DENSITY_RATIO`] over at least 64 chars.
    pub fn classify_digit_density(buffer: &str) -> Option<usize> {
        let chars: Vec<char> = buffer.chars().collect();
        let total = chars.len();
        if total < 64 {
            return None;
        }
        let digits = chars
            .iter()
            .filter(|c| c.is_ascii_digit() || **c == '.' || **c == ',')
            .count();
        let ratio = digits as f32 / total as f32;
        (ratio > DIGIT_DENSITY_RATIO).then_some(total)
    }

    /// Trim the degenerative repeating suffix from the full accumulated text.
    pub fn trim_suffix(full_text: &str, pattern: &DegeneratePattern) -> String {
        const NOTE: &str = "[... stream truncated: repetitive pattern aborted ...]";
        match pattern {
            DegeneratePattern::Periodic { pattern: unit, .. } => {
                if unit.is_empty() {
                    return full_text.to_string();
                }
                let trimmed = full_text.trim_end_matches(unit.as_str());
                format!("{trimmed}{unit}\n\n{NOTE}")
            }
            DegeneratePattern::MonotonicSequence { .. }
            | DegeneratePattern::UnboundedDigitStream { .. } => {
                format!("{}\n\n{NOTE}", full_text.trim_end())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- continuity semantics: transient repetition must NOT escalate ----

    #[test]
    fn table_border_run_stays_silent() {
        let mut detector = StreamLoopDetector::new(1024);
        let border = "┌─ Context Usage ─".to_string() + &"─".repeat(120) + "┐\n";
        assert!(
            detector.push_and_check(&border).is_none(),
            "a bounded box-drawing run must never escalate"
        );
    }

    #[test]
    fn repeated_short_rule_lines_stay_silent() {
        let mut detector = StreamLoopDetector::new(1024);
        let rule = "══════════\n";
        for _ in 0..40 {
            assert!(detector.push_and_check(rule).is_none());
        }
    }

    #[test]
    fn verdict_requires_uninterrupted_dwell_not_cumulative_spread() {
        let mut detector = StreamLoopDetector::new(2048);
        let unit = "-=".repeat(500); // 1000 chars
        // 8 interleaved bursts with prose in between: > dwell total if summed,
        // but each burst is acquitted by the intervening pattern break.
        for _ in 0..8 {
            detector.push_and_check(&unit);
            detector.push_and_check("\nAnd now some ordinary prose continues the document.\n");
        }
        let tail: String = std::iter::repeat_n("-=", MIN_DWELL_CHARS / 2 - 200).collect();
        assert!(detector.push_and_check(&tail).is_none());
    }

    #[test]
    fn continuous_run_beyond_threshold_escalates_once_prosthetic_chunked() {
        let mut detector = StreamLoopDetector::new(1024);
        let chunk = "~".repeat(256);
        let mut last = None;
        for i in 0..16 {
            last = detector.push_and_check(&chunk);
            if i < 11 {
                assert!(last.is_none(), "below dwell threshold must stay silent");
            }
        }
        assert!(
            matches!(last, Some(DegeneratePattern::Periodic { period: 1, .. })),
            "an unbroken 4KB single-char flood is the canonical runaway"
        );
    }

    #[test]
    fn multi_byte_glyph_unit_treated_like_ascii() {
        // Multi-byte glyphs flow through the same semantic rules as ASCII:
        // continuous 2-char-unit repetition reaches dwell and escalates.
        let mut detector = StreamLoopDetector::new(1024);
        let unit = "─│".repeat(128); // 2 chars x 128 = 256 chars per push
        let mut saw = None;
        for _ in 0..13 {
            saw = detector.push_and_check(&unit);
        }
        assert!(
            matches!(saw, Some(DegeneratePattern::Periodic { period: 2, .. })),
            "got {saw:?}"
        );
    }

    #[test]
    fn acquittal_resets_after_pattern_breaks() {
        let mut detector = StreamLoopDetector::new(1024);
        let chunk = "~".repeat(700);
        for _ in 0..3 {
            detector.push_and_check(&chunk);
        }
        // Break the cycle well short of dwell, then re-enter briefly.
        detector.push_and_check("plain closing sentence. ");
        assert!(detector.push_and_check(&"~".repeat(600)).is_none());
    }

    #[test]
    fn wholly_periodic_large_unit_window_is_exact() {
        // Unit of 48 chars; window fully periodic => KMP path regardless of size.
        let unit = "The quick brown fox jumps over the lazy dog! ";
        let text = unit.repeat(24);
        let obs = StreamLoopDetector::observe_periodic_tail(&text).expect("periodic");
        assert_eq!(obs.period, unit.chars().count());
        assert_eq!(obs.suffix_len, text.chars().count());
    }

    #[test]
    fn mixed_prefix_with_long_period_tail_found_by_scan() {
        let head = "Intro paragraph with varied sentence structure and numbers like 42. ";
        let unit = "==[ SECTION ]==";
        let text = format!("{head}{}", unit.repeat(6));
        let obs = StreamLoopDetector::observe_periodic_tail(&text).expect("tail run found");
        assert_eq!(obs.period, unit.len());
        assert!(obs.suffix_len >= 2 * unit.len());
    }

    // ---- monotonic sequence ----------------------------------------------

    #[test]
    fn detects_monotonic_step_numbering() {
        let mut detector = StreamLoopDetector::new(512);
        let input = "Step 1: check files\nStep 2: check files\nStep 3: check files\nStep 4: check files\nStep 5: check files\n";
        assert!(matches!(
            detector.push_and_check(input),
            Some(DegeneratePattern::MonotonicSequence { count: 5, .. })
        ));
    }

    #[test]
    fn four_step_sequence_insufficient() {
        let mut detector = StreamLoopDetector::new(512);
        let input = "Step 1\nStep 2\nStep 3\nStep 4";
        assert!(detector.push_and_check(input).is_none());
    }

    // ---- data flood budget ------------------------------------------------

    #[test]
    fn digit_dump_below_budget_stays_silent() {
        let mut detector = StreamLoopDetector::new(1024);
        let digits = "3.14159265358979323846 ".repeat(40);
        assert!(detector.push_and_check(&digits).is_none());
    }

    #[test]
    fn sustained_digit_flood_crosses_budget() {
        let mut detector = StreamLoopDetector::new(1024);
        // ~46 chars/push x 200 pushes ≈ 9.2K chars, clearing the 8192 budget.
        let digits = "3.14159265358979323846 ".repeat(2);
        let mut hit = None;
        for _ in 0..200 {
            if let Some(pat) = detector.push_and_check(&digits) {
                hit = Some(pat);
                break;
            }
        }
        assert!(matches!(
            hit,
            Some(DegeneratePattern::UnboundedDigitStream { .. })
        ));
    }

    #[test]
    fn density_lapse_drains_budget() {
        let mut detector = StreamLoopDetector::new(1024);
        let digits = "3.14159265358979323846 ".repeat(30);
        let prose = "In summary, the results are encouraging across every benchmark suite. ";
        for _ in 0..5 {
            detector.push_and_check(&digits);
            detector.push_and_check(prose);
        }
        assert!(detector.digit_budget_spent < MAX_DEGENERATE_BUDGET_CHARS / 4);
    }

    // ---- reset + trim ------------------------------------------------------

    #[test]
    fn reset_clears_trail() {
        let mut detector = StreamLoopDetector::new(1024);
        detector.push_and_check(&"~".repeat(900));
        detector.reset();
        assert!(detector.push_and_check(&"~".repeat(300)).is_none());
    }

    #[test]
    fn trim_suffix_peels_literal_unit_copies() {
        let original = "Report:\nABABABABAB";
        let trimmed = StreamLoopDetector::trim_suffix(
            original,
            &DegeneratePattern::Periodic {
                period: 2,
                repetitions: 5,
                pattern: "AB".to_string(),
                suffix_len: 10,
            },
        );
        assert_eq!(
            trimmed,
            "Report:\nAB\n\n[... stream truncated: repetitive pattern aborted ...]"
        );
    }

    #[test]
    fn description_mentions_span() {
        let d = DegeneratePattern::Periodic {
            period: 1,
            repetitions: 100,
            pattern: "─".to_string(),
            suffix_len: 100,
        };
        assert!(d.description().contains("suffix=100"));
    }
}
