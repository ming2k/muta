//! End-to-end check of the MCP Streamable-HTTP client against a real HTTP
//! server.
//!
//! Spins up the dependency-free `mock_mcp_http_server.py` fixture on an
//! ephemeral port, then exercises the same `load_mcp_tools` path the runtime
//! uses: initialize (session-id capture), tool discovery over an SSE-framed
//! response, a tool call, and the config-time `deny_tools` filter. Skips
//! (rather than fails) when `python3` is unavailable so CI without Python
//! stays green.

use std::collections::HashMap;

use muta_contracts::mcp::{McpConnectionStatus, McpServerConfig};
use muta_mcp::load_mcp_tools;

fn python3() -> Option<String> {
    let probe = std::process::Command::new("python3")
        .arg("--version")
        .output();
    matches!(probe, Ok(out) if out.status.success()).then(|| "python3".to_string())
}

fn fixture() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mock_mcp_http_server.py")
}

/// Spawn the mock HTTP server on an OS-assigned port; return its `/mcp` url
/// once it is accepting connections. Panics (rather than skipping) on setup
/// failure: a present-but-broken python3 is an environment bug worth failing
/// for, while an absent one keeps the skip path above.
async fn spawn_server(python: &str) -> String {
    let port = {
        let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
            Ok(listener) => listener,
            Err(error) => panic!("bind ephemeral port: {error}"),
        };
        match listener.local_addr() {
            Ok(addr) => addr.port(),
            Err(error) => panic!("read ephemeral port: {error}"),
        }
        // The listener drops here, freeing the port for the child to claim.
    };
    let mut child = match std::process::Command::new(python)
        .arg(fixture())
        .arg(port.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => panic!("spawn mock MCP HTTP server: {error}"),
    };
    // Wait for the server to accept connections (best-effort, bounded).
    for _ in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            // Park the child until process exit; the fixture is a test
            // resource and the binary exiting reaps it.
            std::mem::forget(child);
            return format!("http://127.0.0.1:{port}/mcp");
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    let _ = child.kill();
    panic!("mock MCP HTTP server never became ready");
}

fn config(url: String) -> McpServerConfig {
    McpServerConfig {
        url: Some(url),
        command: Vec::new(),
        environment: HashMap::new(),
        enabled: true,
        read_only: false,
        allow_tools: Vec::new(),
        deny_tools: vec!["hidden".to_string()],
        sandbox_root: None,
    }
}

#[tokio::test]
async fn http_transport_discovers_filters_and_calls_tools() {
    let Some(python) = python3() else {
        eprintln!("skipping: python3 unavailable");
        return;
    };
    let url = spawn_server(&python).await;

    let mut configs = HashMap::new();
    configs.insert("mock".to_string(), config(url));

    let loaded = load_mcp_tools(&configs).await;

    // Connection succeeded.
    let status = loaded
        .statuses
        .iter()
        .find(|(name, _)| name == "mock")
        .map(|(_, status)| status.clone());
    match status {
        Some(McpConnectionStatus::Connected { tools }) => {
            // `echo` survived the filter; `hidden` was denied — exactly one.
            assert_eq!(tools, 1, "deny_tools must leave only `echo`");
        }
        other => panic!("expected Connected, got {other:?}"),
    }

    // Discovery returned the un-filtered tool only.
    let names: Vec<&str> = loaded.tools.iter().map(|tool| tool.name()).collect();
    assert!(names.contains(&"mcp__mock__echo"), "tools: {names:?}");
    assert!(
        !names.iter().any(|n| n.contains("hidden")),
        "deny_tools must filter `hidden`, got: {names:?}"
    );

    // A tool call round-trips through the SSE-framed response.
    let echo = loaded
        .tools
        .iter()
        .find(|tool| tool.name() == "mcp__mock__echo");
    let Some(echo) = echo else {
        panic!("echo tool present, got: {names:?}");
    };
    let output = match echo.call(r#"{"text":"streamable"}"#).await {
        Ok(output) => output,
        Err(error) => panic!("echo call succeeds: {error}"),
    };
    assert!(output.contains("streamable"), "output: {output}");
}

#[tokio::test]
async fn http_transport_rejects_non_http_scheme() {
    let mut configs = HashMap::new();
    configs.insert(
        "bad".to_string(),
        McpServerConfig {
            url: Some("ftp://example.invalid/mcp".to_string()),
            ..config("http://127.0.0.1:1/mcp".to_string())
        },
    );
    let loaded = load_mcp_tools(&configs).await;
    let status = loaded
        .statuses
        .iter()
        .find(|(name, _)| name == "bad")
        .map(|(_, status)| status.clone());
    match status {
        Some(McpConnectionStatus::Failed(_)) => {}
        other => panic!("expected Failed, got {other:?}"),
    }
}
