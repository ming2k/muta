use crate::cli::McpAction;
use muta_contracts::mcp::McpServerConfig;
use muta_persistence::config::Config;
use std::io::Read;

/// `muta mcp …` — manage the user-level `[mcp.*]` table in config.toml.
///
/// Project-scope MCP (`.muta/config.toml`, `.muta/mcp.json`) stays
/// file-authored and trust-gated (ADR-0085); these verbs only ever touch the
/// user config, which the user already owns.
pub fn run(action: McpAction) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        McpAction::List => list(),
        McpAction::Add {
            name,
            url,
            command,
            environment,
            read_only,
            disabled,
            allow_tools,
            deny_tools,
        } => {
            let mut config = Config::load();
            if config.mcp.contains_key(&name) {
                return Err(format!(
                    "server '{name}' already exists (edit config.toml or `muta mcp rm {name}` first)"
                )
                .into());
            }
            config.mcp.insert(
                name.clone(),
                McpServerConfig {
                    url,
                    command,
                    environment: environment.into_iter().collect(),
                    enabled: !disabled,
                    read_only,
                    allow_tools,
                    deny_tools,
                    sandbox_root: None,
                },
            );
            config.save()?;
            println!(
                "Added [mcp.{name}] to {}",
                Config::config_file_path().display()
            );
            Ok(())
        }
        McpAction::Remove { name } => {
            let mut config = Config::load();
            match config.mcp.remove(&name) {
                Some(_) => {
                    config.save()?;
                    println!("Removed [mcp.{name}]");
                    Ok(())
                }
                None => Err(format!("no MCP server named '{name}' in config.toml").into()),
            }
        }
        McpAction::SetEnabled { name, enabled } => {
            let mut config = Config::load();
            let Some(server) = config.mcp.get_mut(&name) else {
                return Err(format!("no MCP server named '{name}' in config.toml").into());
            };
            server.enabled = enabled;
            config.save()?;
            println!(
                "{} [mcp.{name}]",
                if enabled { "Enabled" } else { "Disabled" }
            );
            Ok(())
        }
        McpAction::Get { name } => {
            let config = Config::load();
            let Some(server) = config.mcp.get(&name) else {
                return Err(format!("no MCP server named '{name}' in config.toml").into());
            };
            print_server(&name, server);
            Ok(())
        }
        McpAction::Import { source } => {
            let content = if source == "-" {
                let mut buf = String::new();
                std::io::stdin()
                    .read_to_string(&mut buf)
                    .map_err(|e| format!("could not read stdin: {e}"))?;
                buf
            } else {
                std::fs::read_to_string(&source)
                    .map_err(|e| format!("could not read '{source}': {e}"))?
            };
            let servers = Config::parse_mcp_toml(&content)?;
            if servers.is_empty() {
                return Err("input contains no [mcp.<name>] tables".into());
            }
            let mut config = Config::load();
            let mut added = Vec::new();
            let mut skipped = Vec::new();
            for (name, server) in servers {
                if config.mcp.contains_key(&name) {
                    skipped.push(name);
                } else {
                    config.mcp.insert(name.clone(), server);
                    added.push(name);
                }
            }
            config.save()?;
            println!(
                "Imported {} server(s) into {}",
                added.len(),
                Config::config_file_path().display()
            );
            for name in &added {
                println!("  + [mcp.{name}]");
            }
            for name in &skipped {
                println!(
                    "  = [mcp.{name}] already configured — left unchanged (rm it first to re-import)"
                );
            }
            Ok(())
        }
        McpAction::Probe { .. } => unreachable!("probe is async; dispatched in main"),
    }
}

fn list() -> Result<(), Box<dyn std::error::Error>> {
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
        print_row(name, server, status);
    }
    Ok(())
}

fn print_row(name: &str, server: &McpServerConfig, status: &str) {
    // A `url` server displays its endpoint; a stdio server displays its
    // program and arguments.
    let (command, args) = match &server.url {
        Some(url) => ("http", url.clone()),
        None => (
            server.command.first().map(String::as_str).unwrap_or(""),
            server
                .command
                .iter()
                .skip(1)
                .cloned()
                .collect::<Vec<_>>()
                .join(" "),
        ),
    };
    println!("{:<18} {:<10} {:<24} {}", name, status, command, args);
}

fn print_server(name: &str, server: &McpServerConfig) {
    println!("[mcp.{name}]");
    if let Some(url) = &server.url {
        println!("url = {url:?}");
    }
    if !server.command.is_empty() {
        let pretty: Vec<String> = server.command.iter().map(|c| format!("{c:?}")).collect();
        println!("command = [{}]", pretty.join(", "));
    }
    if !server.environment.is_empty() {
        let mut env: Vec<_> = server.environment.iter().collect();
        env.sort();
        let pairs: Vec<String> = env.iter().map(|(k, v)| format!("{k:?} = {v:?}")).collect();
        println!("environment = {{ {} }}", pairs.join(", "));
    }
    println!("enabled = {}", server.enabled);
    println!("read_only = {}", server.read_only);
    if !server.allow_tools.is_empty() {
        let tools: Vec<String> = server
            .allow_tools
            .iter()
            .map(|t| format!("{t:?}"))
            .collect();
        println!("allow_tools = [{}]", tools.join(", "));
    }
    if !server.deny_tools.is_empty() {
        let tools: Vec<String> = server.deny_tools.iter().map(|t| format!("{t:?}")).collect();
        println!("deny_tools = [{}]", tools.join(", "));
    }
}

/// `muta mcp probe <name>` — connect to one configured server, list the tools
/// it advertises, then drop the connection. Async because the MCP client is
/// tokio-based; dispatched directly from `main`.
pub async fn probe(name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::load();
    let Some(server) = config.mcp.get(name) else {
        return Err(format!("no MCP server named '{name}' in config.toml").into());
    };
    if !server.enabled {
        println!("(server '{name}' is disabled — probing anyway)");
    }
    println!("Connecting to [mcp.{name}] …");
    match muta_mcp::connect_server(name, server).await {
        Ok((_handle, tools)) => {
            println!(
                "Connected. {} tool(s) advertised (names as published to the agent):",
                tools.len()
            );
            for tool in &tools {
                println!("  {:<32} {}", tool.name(), first_line(tool.description()));
            }
            // Dropping the handle terminates the child process tree
            // (`kill_on_drop` + native tree containment in the transport).
            drop(_handle);
            Ok(())
        }
        Err(error) => Err(format!("could not connect to '{name}': {error}").into()),
    }
}

fn first_line(description: &str) -> String {
    description
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim()
        .chars()
        .take(72)
        .collect()
}
