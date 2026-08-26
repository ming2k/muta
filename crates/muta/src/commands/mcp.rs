use muta_persistence::config::Config;

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::load();
    if config.mcp.is_empty() {
        println!("No MCP servers configured in config.toml.");
        println!(
            "Tip: add MCP servers in config.toml under [mcp.<name>] or use /mcp inside the TUI."
        );
        return Ok(());
    }

    println!(
        "{:<18} {:<10} {:<24} Arguments",
        "Server Name", "Status", "Command"
    );
    println!("{:-<18} {:-<10} {:-<24} {:-<20}", "", "", "", "");

    for (name, server) in &config.mcp {
        let status = if server.enabled {
            "Enabled"
        } else {
            "Disabled"
        };
        // A `url` server displays its endpoint; a stdio server displays its
        // command line. (The two transports are mutually exclusive — `url`
        // wins — so there is never both to show.)
        let (cmd, args) = if let Some(url) = server.url.as_deref() {
            (url.to_string(), String::new())
        } else {
            let cmd = server
                .command
                .first()
                .map(String::as_str)
                .unwrap_or("(none)")
                .to_string();
            let args = if server.command.len() > 1 {
                server.command[1..].join(" ")
            } else {
                String::new()
            };
            (cmd, args)
        };
        println!("{:<18} {:<10} {:<24} {}", name, status, cmd, args);
    }
    Ok(())
}
