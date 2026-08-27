//! Public API integration tests for StreamLoopDetector and in-flight circuit breaking.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use muta_agent::stream_loop_detector::{DegeneratePattern, StreamLoopDetector};

#[test]
fn detects_arbitrary_periodic_repetitions() {
    let mut detector = StreamLoopDetector::new(512);
    // Short period repetition: "abc" x 6 (18 chars)
    let pat1 = detector.push_and_check("abcabcabcabcabcabc");
    assert!(matches!(
        pat1,
        Some(DegeneratePattern::Periodic {
            period: 3,
            repetitions: 6,
            ..
        })
    ));

    // Multi-word periodic repetition
    let mut detector2 = StreamLoopDetector::new(512);
    let pat2 = detector2.push_and_check("hello world\nhello world\nhello world\n");
    assert!(matches!(
        pat2,
        Some(DegeneratePattern::Periodic { repetitions: 3, .. })
    ));
}

#[test]
fn detects_monotonic_step_progressions() {
    let mut detector = StreamLoopDetector::new(512);
    let step_sequence = "Step 1: check files\nStep 2: check files\nStep 3: check files\nStep 4: check files\nStep 5: check files\n";
    let pat = detector.push_and_check(step_sequence);
    assert!(matches!(
        pat,
        Some(DegeneratePattern::MonotonicSequence { count: 5, .. })
    ));
}

#[test]
fn detects_unbounded_pi_and_digit_floods() {
    let mut detector = StreamLoopDetector::new(512);
    let pi_stream = "3.14159265358979323846264338327950288419716939937510582097494459230781640628620899862803482534211706798214808651328230664709384460955058223172535940812848111745028410270193852110555964462294895493038196";
    let pat = detector.push_and_check(pi_stream);
    assert!(matches!(
        pat,
        Some(DegeneratePattern::UnboundedDigitStream { .. })
    ));
}

#[test]
fn does_not_falsely_trigger_on_code_and_text() {
    let mut detector = StreamLoopDetector::new(512);
    let code = r#"
        pub fn process_items(items: &[String]) -> Vec<String> {
            let mut result = Vec::new();
            for item in items {
                if !item.is_empty() {
                    result.push(item.to_uppercase());
                }
            }
            result
        }
    "#;
    assert!(detector.push_and_check(code).is_none());
}

#[test]
fn repeated_long_expression_is_not_a_loop() {
    let mut detector = StreamLoopDetector::new(1024);
    let analysis = " Graph length u64 = 0x09bab64a at ~0x0ee84663? Then graph would span from 0x0ee84673 - ... hmm: if graph_len counts bytes of graph ending just before\n\
        its own length field: graph_start = 0x0ee84663 - 0x09bab64a = 0x0ee84663 - 0x09bab64a";

    assert!(detector.push_and_check(analysis).is_none());
}

#[test]
fn trim_suffix_preserves_single_copy_and_truncation_notice() {
    let pattern = DegeneratePattern::Periodic {
        period: 3,
        repetitions: 5,
        pattern: "abc".to_string(),
    };
    let trimmed = StreamLoopDetector::trim_suffix("Content start: abcabcabcabcabc", &pattern);
    assert!(trimmed.contains("Content start: abc"));
    assert!(trimmed.contains("[... stream truncated"));
}
