pub(crate) fn python3() -> Option<String> {
    let probe = std::process::Command::new("python3")
        .arg("--version")
        .output();
    matches!(probe, Ok(out) if out.status.success()).then(|| "python3".to_string())
}

mod mcp_http;
mod mcp_stdio;
