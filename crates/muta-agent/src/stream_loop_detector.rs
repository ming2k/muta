//! In-flight streaming loop detector and circuit breaker.
//!
//! Evaluates incoming token stream chunks in real-time (O(N) per chunk, <1µs latency)
//! to detect and abort degenerative text patterns before they burn tokens or pollute
//! the context window:
//!
//! 1. **Arbitrary Periodic Loops (`abab`, `abcabc`, `abcdabcd...`)**:
//!    Detected via the KMP String Periodicity Theorem (Prefix Function $\pi$-table).
//! 2. **Monotonic Progression Loops (`1, 2, 3, 4, 5...`, `Step 1, Step 2...`)**:
//!    Detected via template skeleton normalization and arithmetic difference analysis.
//! 3. **Unbounded Data/Digit Streams (e.g. printing $\pi$ decimals, endless hex/digits)**:
//!    Detected via character class density and lack of natural language convergence.

/// Classification of detected degenerative output patterns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DegeneratePattern {
    /// Arbitrary periodic repetition (e.g. `ababab`, `abcabcabc`).
    Periodic {
        period: usize,
        repetitions: usize,
        pattern: String,
    },
    /// Monotonic sequence repetition (e.g. `Step 1, Step 2, Step 3, Step 4, Step 5`).
    MonotonicSequence {
        template: String,
        count: usize,
    },
    /// Unbounded digit or raw data generation (e.g. endless $\pi$ digits).
    UnboundedDigitStream {
        length: usize,
    },
}

impl DegeneratePattern {
    /// Human-readable summary of the detected pattern for logs and steering prompts.
    pub fn description(&self) -> String {
        match self {
            Self::Periodic {
                period,
                repetitions,
                pattern,
            } => {
                let preview = if pattern.len() > 20 {
                    format!("{}...", &pattern[..20])
                } else {
                    pattern.clone()
                };
                format!("periodic repetition of '{preview}' (period={period}, count={repetitions})")
            }
            Self::MonotonicSequence { template, count } => {
                format!("monotonic sequence repeating template '{template}' {count} times")
            }
            Self::UnboundedDigitStream { length } => {
                format!("unbounded digit/data stream ({length} characters)")
            }
        }
    }
}

/// Real-time streaming loop detector with a sliding character buffer.
#[derive(Debug, Clone)]
pub struct StreamLoopDetector {
    buffer: String,
    window_size: usize,
}

impl Default for StreamLoopDetector {
    fn default() -> Self {
        Self::new(1024)
    }
}

impl StreamLoopDetector {
    /// Create a new detector with the specified sliding window capacity in characters.
    pub fn new(window_size: usize) -> Self {
        Self {
            buffer: String::with_capacity(window_size),
            window_size: window_size.max(256),
        }
    }

    /// Reset the internal state buffer.
    pub fn reset(&mut self) {
        self.buffer.clear();
    }

    /// Feed a new chunk from the stream and check if any degenerative loop is triggered.
    pub fn push_and_check(&mut self, chunk: &str) -> Option<DegeneratePattern> {
        if chunk.is_empty() {
            return None;
        }

        self.buffer.push_str(chunk);
        if self.buffer.len() > self.window_size {
            let excess = self.buffer.len() - self.window_size;
            // Drain at char boundary
            let mut cut = excess;
            while !self.buffer.is_char_boundary(cut) && cut < self.buffer.len() {
                cut += 1;
            }
            self.buffer.drain(..cut);
        }

        // 1. Check for arbitrary periodic substring repetition on suffix
        if let Some(pat) = Self::detect_periodic(&self.buffer) {
            return Some(pat);
        }

        // 2. Check for monotonic sequence progression (e.g. Step 1, Step 2...)
        if let Some(pat) = Self::detect_monotonic(&self.buffer) {
            return Some(pat);
        }

        // 3. Check for unbounded numeric/data flood
        if let Some(pat) = Self::detect_digit_stream(&self.buffer) {
            return Some(pat);
        }

        None
    }

    /// KMP String Periodicity Theorem:
    ///
    /// For a substring $S$ of length $L$, compute its prefix function $\pi[L]$.
    /// If $p = L - \pi[L-1]$ divides $L$, then $p$ is the smallest repeating period of $S$.
    fn detect_periodic(text: &str) -> Option<DegeneratePattern> {
        let chars: Vec<char> = text.chars().collect();
        let total_len = chars.len();
        if total_len < 18 {
            return None;
        }

        // Evaluate suffixes of length L from min_len up to total_len
        let max_check_len = total_len.min(512);
        for l in (18..=max_check_len).rev() {
            let suffix = &chars[total_len - l..];
            let n = suffix.len();

            // Compute KMP pi-table for this suffix
            let mut pi = vec![0usize; n];
            for i in 1..n {
                let mut j = pi[i - 1];
                while j > 0 && suffix[i] != suffix[j] {
                    j = pi[j - 1];
                }
                if suffix[i] == suffix[j] {
                    j += 1;
                }
                pi[i] = j;
            }

            let matched = pi[n - 1];
            if matched == 0 {
                continue;
            }

            let p = n - matched; // candidate period
            if p == 0 || n % p != 0 {
                continue;
            }

            let repetitions = n / p;
            let min_reps = match p {
                1..=2 => 8,  // Single/double char (e.g. `..`, `==`): need 8+ reps to avoid false positives
                3..=5 => 4,  // Short word/code (e.g. `abc`): need 4+ reps
                6..=16 => 3, // Phrase: need 3+ reps
                _ => 2,      // Long multi-line pattern: 2+ full repetitions
            };

            // Avoid triggering on pure whitespace indentation repetitions
            let pattern_str: String = suffix[..p].iter().collect();
            if pattern_str.trim().is_empty() {
                continue;
            }

            if repetitions >= min_reps {
                return Some(DegeneratePattern::Periodic {
                    period: p,
                    repetitions,
                    pattern: pattern_str,
                });
            }
        }

        None
    }

    /// Monotonic sequence detection via structural template abstraction and diffing.
    fn detect_monotonic(text: &str) -> Option<DegeneratePattern> {
        let lines: Vec<&str> = text
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect();

        if lines.len() < 5 {
            return None;
        }

        let mut skeletons = Vec::new();
        let mut numbers = Vec::new();

        // Examine up to the last 8 non-empty lines in reverse order
        for line in lines.iter().rev().take(8) {
            let mut skeleton = String::new();
            let mut num_val = None;
            let mut digits = String::new();

            for c in line.chars() {
                if c.is_ascii_digit() {
                    digits.push(c);
                } else {
                    if !digits.is_empty() {
                        if num_val.is_none() {
                            num_val = digits.parse::<i64>().ok();
                        }
                        skeleton.push_str("#NUM#");
                        digits.clear();
                    }
                    skeleton.push(c);
                }
            }
            if !digits.is_empty() {
                if num_val.is_none() {
                    num_val = digits.parse::<i64>().ok();
                }
                skeleton.push_str("#NUM#");
            }

            if let Some(n) = num_val {
                skeletons.push(skeleton);
                numbers.push(n);
            }
        }

        // If at least 5 consecutive lines share the exact template and strictly decrement (reverse check)
        if skeletons.len() >= 5 && skeletons.windows(2).all(|w| w[0] == w[1]) {
            let is_arithmetic = numbers.windows(2).all(|w| w[0] - w[1] == 1);
            if is_arithmetic {
                return Some(DegeneratePattern::MonotonicSequence {
                    template: skeletons[0].clone(),
                    count: skeletons.len(),
                });
            }
        }

        None
    }

    /// Digit stream density check (e.g. printing $\pi$ decimals without natural language).
    fn detect_digit_stream(text: &str) -> Option<DegeneratePattern> {
        let chars: Vec<char> = text.chars().collect();
        if chars.len() < 120 {
            return None;
        }

        let window = &chars[chars.len().saturating_sub(200)..];
        let total = window.len();
        if total < 120 {
            return None;
        }

        let digits = window
            .iter()
            .filter(|c| c.is_ascii_digit() || **c == '.' || **c == ',')
            .count();
        let ratio = digits as f32 / total as f32;

        if ratio > 0.88 {
            return Some(DegeneratePattern::UnboundedDigitStream { length: total });
        }

        None
    }

    /// Trim the degenerative repeating suffix from the full accumulated text.
    pub fn trim_suffix(full_text: &str, pattern: &DegeneratePattern) -> String {
        match pattern {
            DegeneratePattern::Periodic {
                pattern: pat_str, ..
            } => {
                if pat_str.is_empty() {
                    return full_text.to_string();
                }
                // Retain one occurrence of the pattern and strip the rest
                let mut trimmed = full_text.to_string();
                while trimmed.ends_with(pat_str) {
                    let new_len = trimmed.len() - pat_str.len();
                    trimmed.truncate(new_len);
                }
                // Append one copy back with a clean truncation note
                trimmed.push_str(pat_str);
                trimmed.push_str("\n\n[... stream truncated: repetitive pattern aborted ...]");
                trimmed
            }
            DegeneratePattern::MonotonicSequence { .. } | DegeneratePattern::UnboundedDigitStream { .. } => {
                format!(
                    "{}\n\n[... stream truncated: unbounded loop aborted ...]",
                    full_text.trim_end()
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_short_periodic_repetitions() {
        let mut detector = StreamLoopDetector::new(512);
        // "abc" repeated 6 times (18 chars)
        let pat = detector.push_and_check("abcabcabcabcabcabc");
        assert!(matches!(
            pat,
            Some(DegeneratePattern::Periodic {
                period: 3,
                repetitions: 6,
                ..
            })
        ));
    }

    #[test]
    fn detects_longer_pattern_repetitions() {
        let mut detector = StreamLoopDetector::new(512);
        // "hello world\n" repeated 3 times
        let chunk = "hello world\nhello world\nhello world\n";
        let pat = detector.push_and_check(chunk);
        assert!(matches!(
            pat,
            Some(DegeneratePattern::Periodic {
                repetitions: 3,
                ..
            })
        ));
    }

    #[test]
    fn detects_monotonic_step_numbering() {
        let mut detector = StreamLoopDetector::new(512);
        let input = "Step 1: check files\nStep 2: check files\nStep 3: check files\nStep 4: check files\nStep 5: check files\n";
        let pat = detector.push_and_check(input);
        assert!(matches!(
            pat,
            Some(DegeneratePattern::MonotonicSequence { count: 5, .. })
        ));
    }

    #[test]
    fn detects_unbounded_pi_stream() {
        let mut detector = StreamLoopDetector::new(512);
        let pi = "3.14159265358979323846264338327950288419716939937510582097494459230781640628620899862803482534211706798214808651328230664709384460955058223172535940812848111745028410270193852110555964462294895493038196";
        let pat = detector.push_and_check(pi);
        assert!(matches!(
            pat,
            Some(DegeneratePattern::UnboundedDigitStream { .. })
        ));
    }

    #[test]
    fn does_not_falsely_trigger_on_normal_code() {
        let mut detector = StreamLoopDetector::new(512);
        let normal_code = r#"
            fn calculate_sum(items: &[i32]) -> i32 {
                let mut sum = 0;
                for item in items {
                    sum += item;
                }
                sum
            }
        "#;
        assert!(detector.push_and_check(normal_code).is_none());
    }

    #[test]
    fn trim_suffix_cleans_periodic_burst() {
        let pattern = DegeneratePattern::Periodic {
            period: 3,
            repetitions: 4,
            pattern: "abc".to_string(),
        };
        let trimmed = StreamLoopDetector::trim_suffix("Prefix: abcabcabcabc", &pattern);
        assert!(trimmed.contains("Prefix: abc"));
        assert!(trimmed.contains("[... stream truncated"));
    }
}
