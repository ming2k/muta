use crate::cli::ConfigAction;
use neenee_persistence::config::Config;

pub fn run(action: ConfigAction) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        ConfigAction::Path => {
            println!("{}", Config::config_file_path().display());
        }
        ConfigAction::Check => {
            let findings = neenee_persistence::config_check::check_config_file(None);
            if findings.is_empty() {
                println!("config.toml is valid and every key is understood by this version.");
                return Ok(());
            }
            let mut legacy = 0;
            for finding in &findings {
                if finding.is_legacy {
                    legacy += 1;
                }
                println!("  {}: {}", finding.key, finding.message);
            }
            println!(
                "\n{} finding(s), {} legacy key(s). Unknown keys are ignored at \
                 load, so none of these block startup — but a typo silently \
                 falls back to the default.",
                findings.len(),
                legacy
            );
            std::process::exit(1);
        }
        ConfigAction::List => {
            let config = Config::load();
            println!(
                "Configuration file: {}\n",
                Config::config_file_path().display()
            );
            println!(
                "default_connection: {}",
                if config.default_connection.is_empty() {
                    "(none)"
                } else {
                    &config.default_connection
                }
            );
            println!(
                "default_model:      {}",
                config.default_model.as_deref().unwrap_or("(none)")
            );
            println!(
                "retry_max_attempts:  {}",
                config.connection_retry_max_attempts
            );
            println!("retry_base_ms:       {}ms", config.connection_retry_base_ms);
            println!("retry_max_ms:        {}ms", config.connection_retry_max_ms);
            println!(
                "compaction_preserve_rounds: {}",
                config.compaction_preserve_rounds
            );
            println!(
                "compaction_summarize:       {}",
                config.compaction_summarize
            );
            println!("compaction_prune:           {}", config.compaction_prune);
            println!("mcp_servers_count:          {}", config.mcp.len());
            println!(
                "connections_count:          {}",
                neenee_persistence::connections::Connections::load()
                    .connections
                    .len()
            );
        }
        ConfigAction::Get(key) => {
            let config = Config::load();
            match key.as_str() {
                "default_connection" | "default_provider" => {
                    println!("{}", config.default_connection)
                }
                "default_model" => println!("{}", config.default_model.as_deref().unwrap_or("")),
                "connection_retry_max_attempts" | "provider_retry_max_attempts" => {
                    println!("{}", config.connection_retry_max_attempts)
                }
                "connection_retry_base_ms" | "provider_retry_base_ms" => {
                    println!("{}", config.connection_retry_base_ms)
                }
                "connection_retry_max_ms" | "provider_retry_max_ms" => {
                    println!("{}", config.connection_retry_max_ms)
                }
                "compaction_preserve_rounds" => println!("{}", config.compaction_preserve_rounds),
                "compaction_summarize" => println!("{}", config.compaction_summarize),
                "compaction_prune" => println!("{}", config.compaction_prune),
                other => {
                    return Err(format!("unknown configuration key '{other}'").into());
                }
            }
        }
        ConfigAction::Set { key, value } => {
            let mut config = Config::load();
            match key.as_str() {
                "default_connection" | "default_provider" => {
                    config.default_connection = value.clone();
                }
                "default_model" => {
                    config.default_model = Some(value.clone());
                }
                "connection_retry_max_attempts" | "provider_retry_max_attempts" => {
                    config.connection_retry_max_attempts = value
                        .parse()
                        .map_err(|_| "invalid integer for retry_max_attempts")?;
                }
                "connection_retry_base_ms" | "provider_retry_base_ms" => {
                    config.connection_retry_base_ms = value
                        .parse()
                        .map_err(|_| "invalid integer for retry_base_ms")?;
                }
                "connection_retry_max_ms" | "provider_retry_max_ms" => {
                    config.connection_retry_max_ms = value
                        .parse()
                        .map_err(|_| "invalid integer for retry_max_ms")?;
                }
                "compaction_preserve_rounds" => {
                    config.compaction_preserve_rounds = value
                        .parse()
                        .map_err(|_| "invalid integer for compaction_preserve_rounds")?;
                }
                "compaction_summarize" => {
                    config.compaction_summarize = value
                        .parse()
                        .map_err(|_| "invalid boolean (true/false) for compaction_summarize")?;
                }
                "compaction_prune" => {
                    config.compaction_prune = value
                        .parse()
                        .map_err(|_| "invalid boolean (true/false) for compaction_prune")?;
                }
                other => {
                    return Err(
                        format!("unsupported or read-only configuration key '{other}'").into(),
                    );
                }
            }
            config.save()?;
            println!("Updated {} = {}", key, value);
        }
    }
    Ok(())
}
