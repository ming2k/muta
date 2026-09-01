use super::model::*;
use super::view::*;
use crate::modal::TelemetryTab;
use crate::view::Theme;
use muta_contracts::{
    RequestPerformance, RequestUsageKey, RequestUsageRecord, RequestUsageSource,
    RequestUsageStatus, TokenSourceReport, TokenSourceRow,
};

#[test]
fn test_extract_telemetry_rounds_filters_terminal_only() {
    let mut report = TokenSourceReport::default();
    let row = TokenSourceRow {
        provider: "anthropic".to_string(),
        model: "claude-3-7-sonnet".to_string(),
        turns: Vec::new(),
        requests: vec![
            RequestUsageRecord {
                key: RequestUsageKey {
                    session_id: "s1".to_string(),
                    round: 1,
                    turn: 1,
                    attempt: 1,
                    actor_id: "master".to_string(),
                },
                provider: "anthropic".to_string(),
                model: "claude-3-7-sonnet".to_string(),
                status: RequestUsageStatus::InFlight, // Non-terminal
                source: RequestUsageSource::Reported,
                prompt_tokens: 100,
                completion_tokens: 10,
                total_tokens: 110,
                generation_ms: 500,
                ..Default::default()
            },
            RequestUsageRecord {
                key: RequestUsageKey {
                    session_id: "s1".to_string(),
                    round: 1,
                    turn: 1,
                    attempt: 2,
                    actor_id: "master".to_string(),
                },
                provider: "anthropic".to_string(),
                model: "claude-3-7-sonnet".to_string(),
                status: RequestUsageStatus::Completed, // Terminal
                source: RequestUsageSource::Reported,
                prompt_tokens: 1000,
                completion_tokens: 200,
                cache_read_tokens: 800,
                total_tokens: 1200,
                generation_ms: 1500,
                performance: Some(RequestPerformance {
                    stream_ready_us: Some(100_000),
                    ttft_us: Some(300_000),
                    stream_us: Some(1_200_000),
                    tail_us: Some(20_000),
                    e2e_us: Some(1_520_000),
                    streamed_output_tokens: 200,
                    first_output_tokens: 1,
                    output_events: 50,
                    ..Default::default()
                }),
                ..Default::default()
            },
        ],
        totals: Default::default(),
    };
    report.rows.push(row);

    let rounds = extract_telemetry_rounds(&report);
    assert_eq!(rounds.len(), 1);
    let r1 = &rounds[0];
    assert_eq!(r1.round_number, 1);
    assert_eq!(r1.attempts.len(), 1); // Running attempt filtered out!
    assert_eq!(r1.prompt_tokens, 1000);
    assert_eq!(r1.completion_tokens, 200);
    assert_eq!(r1.cache_read_tokens, 800);
    assert_eq!(r1.cache_hit_rate(), 80.0);
    assert!(r1.preferred_tps().is_some());
}

#[test]
fn test_telemetry_round_and_turn_helpers() {
    let mut report = TokenSourceReport::default();
    let row = TokenSourceRow {
        provider: "anthropic".to_string(),
        model: "claude-3-7-sonnet".to_string(),
        turns: Vec::new(),
        requests: vec![
            RequestUsageRecord {
                key: RequestUsageKey {
                    session_id: "s1".to_string(),
                    round: 2,
                    turn: 1,
                    attempt: 1,
                    actor_id: "master".to_string(),
                },
                provider: "anthropic".to_string(),
                model: "claude-3-7-sonnet".to_string(),
                status: RequestUsageStatus::Completed,
                source: RequestUsageSource::Reported,
                prompt_tokens: 2000,
                completion_tokens: 150,
                cache_read_tokens: 1600,
                total_tokens: 2150,
                generation_ms: 1200,
                performance: Some(RequestPerformance {
                    stream_ready_us: Some(150_000),
                    ttft_us: Some(350_000),
                    stream_us: Some(1_000_000),
                    tail_us: Some(30_000),
                    e2e_us: Some(1_200_000),
                    streamed_output_tokens: 150,
                    ..Default::default()
                }),
                ..Default::default()
            },
            RequestUsageRecord {
                key: RequestUsageKey {
                    session_id: "s1".to_string(),
                    round: 1,
                    turn: 1,
                    attempt: 1,
                    actor_id: "master".to_string(),
                },
                provider: "anthropic".to_string(),
                model: "claude-3-7-sonnet".to_string(),
                status: RequestUsageStatus::Completed,
                source: RequestUsageSource::Reported,
                prompt_tokens: 500,
                completion_tokens: 50,
                total_tokens: 550,
                generation_ms: 600,
                ..Default::default()
            },
        ],
        totals: Default::default(),
    };
    report.rows.push(row);

    assert_eq!(telemetry_round_count(&report), 2);
    // Round 2 is first (descending)
    assert_eq!(telemetry_attempt_count(&report, 0), 1);
    assert_eq!(telemetry_attempt_count(&report, 1), 1);
    assert_eq!(telemetry_attempt_key(&report, 0, 0), Some((2, 1)));
    assert_eq!(telemetry_attempt_key(&report, 1, 0), Some((1, 1)));
}

#[test]
fn test_build_attempt_inspector_waterfall_nodes() {
    let theme = Theme::from_color_scheme("dark", &Default::default());
    let rounds = vec![TelemetryRound {
        round_number: 1,
        prompt_tokens: 4000,
        completion_tokens: 300,
        cache_read_tokens: 3000,
        total_tokens: 4300,
        turns_count: 1,
        e2e_duration_ms: 3_500,
        attempts: vec![TelemetryAttempt {
            round: 1,
            turn: 1,
            attempt: 1,
            model: "claude-3-7-sonnet".to_string(),
            provider: "anthropic".to_string(),
            status: RequestUsageStatus::Completed,
            prompt_tokens: 4000,
            completion_tokens: 300,
            cache_read_tokens: 3000,
            cache_write_tokens: 0,
            performance: Some(RequestPerformance {
                stream_ready_us: Some(120_000),
                ttft_us: Some(280_000),
                stream_us: Some(3_000_000),
                output_events: 10,
                tail_us: Some(25_000),
                e2e_us: Some(3_425_000),
                streamed_output_tokens: 300,
                first_output_tokens: 0,
                ..Default::default()
            }),
            e2e_duration_ms: 3500,
        }],
    }];

    let lines = build_attempt_inspector_body(
        &rounds,
        1,
        1,
        ContextUsageView {
            window_tokens: Some(200_000),
            ..Default::default()
        },
        100,
        &theme,
    );

    let full_text: String = lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<Vec<_>>()
                .join("")
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert!(full_text.contains("Target:  claude-3-7-sonnet @ anthropic"));
    assert!(full_text.contains("CONTEXT SPACE"));
    assert!(full_text.contains("75.0% Cache Hit"));
    assert!(full_text.contains("LATENCY TIMELINE WATERFALL"));
    assert!(full_text.contains("Request Dispatched"));
    assert!(full_text.contains("Connect & Handshake"));
    assert!(full_text.contains("Stream Ready"));
    assert!(full_text.contains("Prefill & Server Queue"));
    assert!(full_text.contains("First Token Arrived"));
    assert!(full_text.contains("Stream Decode"));
    assert!(full_text.contains("Last Token Received"));
    assert!(full_text.contains("Tail & Commit"));
    assert!(full_text.contains("Request Completed"));
}

#[test]
fn test_build_overview_and_sticky_table_headers() {
    let theme = Theme::from_color_scheme("dark", &Default::default());
    let rounds = vec![TelemetryRound {
        round_number: 1,
        prompt_tokens: 4000,
        completion_tokens: 300,
        cache_read_tokens: 3000,
        total_tokens: 4300,
        turns_count: 1,
        e2e_duration_ms: 3_500,
        attempts: vec![TelemetryAttempt {
            round: 1,
            turn: 1,
            attempt: 1,
            model: "claude-3-7-sonnet".to_string(),
            provider: "anthropic".to_string(),
            status: RequestUsageStatus::Completed,
            prompt_tokens: 4000,
            completion_tokens: 300,
            cache_read_tokens: 3000,
            cache_write_tokens: 500,
            performance: Some(RequestPerformance {
                stream_ready_us: Some(120_000),
                ttft_us: Some(280_000),
                stream_us: Some(3_000_000),
                output_events: 10,
                tail_us: Some(25_000),
                e2e_us: Some(3_425_000),
                streamed_output_tokens: 300,
                first_output_tokens: 0,
                ..Default::default()
            }),
            e2e_duration_ms: 3500,
        }],
    }];

    let report = TokenSourceReport {
        rows: Vec::new(),
        grand_total: muta_contracts::TokenSourceTotals {
            prompt_tokens: 4000,
            completion_tokens: 300,
            cache_read_tokens: 3000,
            cache_write_tokens: 500,
            reported_tokens: 4300,
            ..Default::default()
        },
    };

    // 1. Test Overview Tab
    let overview = build_overview_body(
        &report,
        &rounds,
        ContextUsageView {
            snapshot: Some(muta_contracts::ContextTokenSnapshot {
                tokens: 24_500,
                source: muta_contracts::ContextTokenSource::Api,
                overhead_tokens: None,
                history_tokens: None,
            }),
            window_tokens: Some(200_000),
            draft_content_tokens: 50,
            draft_tokens: 60,
        },
        80,
        &theme,
    );
    let ov_text: String = overview
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<Vec<_>>()
                .join("")
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert!(ov_text.contains("CONTEXT WINDOW"));
    assert!(ov_text.contains("24.5k / 200.0k (12%)"));
    assert!(ov_text.contains("SESSION TOKEN TOTALS"));
    assert!(ov_text.contains("Grand Total"));
    assert!(ov_text.contains("4.3k (4300)"));
    assert!(ov_text.contains("75.0% hit rate"));
    assert!(ov_text.contains("STREAM PERFORMANCE & ACTIVITY"));
    assert!(ov_text.contains("100.0 tok/s"));
    assert!(ov_text.contains("280ms"));

    // 2. Test Rounds Sticky Table (Header is separated from Rows)
    let (header, rows, follow) = build_rounds_table(&rounds, 0, 80, &theme);
    assert_eq!(header.len(), 1, "header must be 1 fixed row");
    assert_eq!(rows.len(), 1, "rows must contain only data lines");
    assert_eq!(follow, Some(0));

    let header_str = header[0]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<Vec<_>>()
        .join("");
    assert!(header_str.contains("Round"));
    assert!(header_str.contains("Tokens"));
    assert!(header_str.contains("Stream TPS"));

    // 3. Test Turns Sticky Table
    let (turns_header, turns_rows, turn_follow) = build_turns_table(&rounds, 0, 0, 80, &theme);
    assert_eq!(turns_header.len(), 1);
    assert_eq!(turns_rows.len(), 1);
    assert_eq!(turn_follow, Some(0));

    let turns_header_str = turns_header[0]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<Vec<_>>()
        .join("");
    assert!(turns_header_str.contains("Turn"));
    assert!(turns_header_str.contains("TTFT"));
    assert!(turns_header_str.contains("Status"));

    // 4. Test Tab Strip
    let ov_tab = tab_strip_line(TelemetryTab::Overview, 1, &theme);
    let ov_tab_str = ov_tab
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<Vec<_>>()
        .join("");
    assert!(ov_tab_str.contains("[ 1 Overview ]"));
    assert!(ov_tab_str.contains("2 Activity (1)"));

    let act_tab = tab_strip_line(TelemetryTab::Activity, 1, &theme);
    let act_tab_str = act_tab
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect::<Vec<_>>()
        .join("");
    assert!(act_tab_str.contains("1 Overview"));
    assert!(act_tab_str.contains("[ 2 Activity (1) ]"));
}

#[test]
fn test_telemetry_burst_arrival_defensible_tps_fallback() {
    let theme = Theme::from_color_scheme("dark", &Default::default());
    // Simulates a provider (e.g. Gemini) returning 200 tokens in a sub-20ms burst (200µs).
    // Naive division 200 / 0.0002s would give 1,000,000 tok/s.
    // Defensible calculation should filter the burst and fall back to e2e rate (200 / 1.5s = 133.3 tok/s).
    let burst_attempt = TelemetryAttempt {
        round: 1,
        turn: 1,
        attempt: 1,
        model: "gemini-2.5-pro".to_string(),
        provider: "google".to_string(),
        status: RequestUsageStatus::Completed,
        prompt_tokens: 1000,
        completion_tokens: 200,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        performance: Some(RequestPerformance {
            stream_ready_us: Some(50_000),
            ttft_us: Some(200_000),
            stream_us: Some(200),    // sub-20ms burst!
            output_events: 1,        // single event arrival
            e2e_us: Some(1_500_000), // 1.5s total e2e
            streamed_output_tokens: 200,
            first_output_tokens: 200,
            ..Default::default()
        }),
        e2e_duration_ms: 1500,
    };

    let preferred = burst_attempt.preferred_tps();
    assert!(preferred.is_some());
    let tps = preferred.unwrap();
    assert!(tps < 2000.0, "TPS must not explode to 1,000,000: {tps}");
    assert!(
        (tps - 133.33).abs() < 1.0,
        "Expected e2e fallback ~133.3 tok/s, got {tps}"
    );

    let rounds = vec![TelemetryRound {
        round_number: 1,
        prompt_tokens: 1000,
        completion_tokens: 200,
        cache_read_tokens: 0,
        total_tokens: 1200,
        turns_count: 1,
        e2e_duration_ms: 1500,
        attempts: vec![burst_attempt],
    }];

    let round_tps = rounds[0].preferred_tps().unwrap();
    assert!(round_tps < 2000.0);
    assert!((round_tps - 133.33).abs() < 1.0);

    let report = TokenSourceReport::default();
    let overview = build_overview_body(&report, &rounds, ContextUsageView::default(), 80, &theme);
    let ov_text = overview
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.as_ref()))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        ov_text.contains("133.3 tok/s"),
        "Overview must show defensible TPS: {ov_text}"
    );
}
