//! Live-network end-to-end checks for the web tools.
//!
//! These are `#[ignore]`d by default (they hit the real network and depend on
//! the local environment — e.g. a socks5 proxy on 127.0.0.1:1080). Run with:
//! `cargo test -p muta-agent --test webtool_e2e -- --ignored`.
//!
//! Together they verify the two-stage research pipeline end to end:
//! `websearch` (breadth, via the configured search provider chain) finds
//! URLs, `webfetch` (depth, via the configured reader) reads one of them.

// Failure paths are the interesting part of an E2E test, so panicking with
// the message beats propagating errors here (same rationale as the other
// integration tests).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use muta_agent::tools::{WebFetchTool, WebSearchTool};
use muta_contracts::{Tool, WebSearchConfig};

/// Shape a config mirroring the developer workstation: socks5 proxy, Exa
/// primary + Parallel fallback, Jina reader.
fn proxied_config() -> WebSearchConfig {
    WebSearchConfig {
        proxy: Some("socks5h://127.0.0.1:1080".into()),
        timeout_secs: 30,
        reader: "jina".into(),
        ..WebSearchConfig::default()
    }
}

#[tokio::test]
#[ignore = "live network"]
async fn builtin_and_jina_readers_work() {
    let mut cfg = proxied_config();
    cfg.reader = "builtin".into();
    let builtin = WebFetchTool::with_config(cfg.clone());
    let out = builtin
        .call(r#"{"url":"https://example.com"}"#)
        .await
        .expect("builtin fetch");
    assert!(out.contains("Example Domain"), "got: {out:.200}");

    let jina = WebFetchTool::with_config(cfg);
    let out = jina
        .call(r#"{"url":"https://example.com"}"#)
        .await
        .expect("jina fetch");
    assert!(
        out.to_lowercase().contains("example domain"),
        "got: {out:.200}"
    );
}

#[tokio::test]
#[ignore = "live network"]
async fn search_then_fetch_pipeline_works() {
    let cfg = proxied_config();
    let search = WebSearchTool::with_config(cfg.clone());
    let results = search
        .call(r#"{"query":"rust async traits"}"#)
        .await
        .expect("websearch through proxy");
    assert!(results.contains("Search results"), "got: {results:.200}");
    assert!(results.contains("http"), "results should carry URLs");

    // Depth stage: read one of the returned documents through the reader.
    // Handles both result shapes: the blob backends (Exa/Parallel) emit
    // `URL: https://...` lines; the structured backends emit a numbered list
    // with the bare URL on its own indented line.
    let url = results
        .lines()
        .find_map(|line| {
            let trimmed = line.trim();
            let candidate = trimmed
                .strip_prefix("URL:")
                .map(str::trim)
                .unwrap_or(trimmed);
            candidate.starts_with("https://").then(|| {
                candidate
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_string()
            })
        })
        .expect("a https URL in the search results");
    let fetch = WebFetchTool::with_config(cfg);
    let page = fetch
        .call(&format!(r#"{{"url":"{url}"}}"#))
        .await
        .expect("webfetch of a search hit");
    assert!(!page.trim().is_empty(), "page body should not be empty");
}
