use crate::cli::ConfigAction;
use muta_persistence::config::Config;

pub fn run(action: ConfigAction) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        ConfigAction::Path => {
            println!("{}", Config::config_file_path().display());
        }
        ConfigAction::Check => {
            let findings = muta_persistence::config_check::check_config_file(None);
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
                "compaction.preserve_rounds: {}",
                config.compaction.preserve_rounds
            );
            println!(
                "compaction.summarize:       {}",
                config.compaction.summarize
            );
            println!("compaction.prune:           {}", config.compaction.prune);
            println!(
                "compaction.prune_protect_tokens: {}",
                config.compaction.prune_protect_tokens
            );
            println!(
                "compaction.utilization:     {}",
                config.compaction.utilization
            );
            println!("mcp_servers_count:          {}", config.mcp.len());
            println!(
                "connections_count:          {}",
                muta_persistence::connections::Connections::load()
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
                "compaction.preserve_rounds" | "compaction_preserve_rounds" => {
                    println!("{}", config.compaction.preserve_rounds)
                }
                "compaction.summarize" | "compaction_summarize" => {
                    println!("{}", config.compaction.summarize)
                }
                "compaction.prune" | "compaction_prune" => {
                    println!("{}", config.compaction.prune)
                }
                "compaction.prune_protect_tokens" | "compaction_prune_protect_tokens" => {
                    println!("{}", config.compaction.prune_protect_tokens)
                }
                "compaction.utilization" => println!("{}", config.compaction.utilization),
                "compaction.target_utilization" => {
                    println!("{}", config.compaction.target_utilization)
                }
                "compaction.prune_utilization" => {
                    println!("{}", config.compaction.prune_utilization)
                }
                "compaction.fallback_window_tokens" => {
                    println!("{}", config.compaction.fallback_window_tokens)
                }
                "master.hard_stop_turns" => println!("{}", config.master.hard_stop_turns),
                "master.allow_model_stdin" => println!("{}", config.master.allow_model_stdin),
                "master.skip_interactive_input" => {
                    println!("{}", config.master.skip_interactive_input)
                }
                "master.doom_guard.enabled" => println!("{}", config.master.doom_guard.enabled),
                "master.doom_guard.window" => println!("{}", config.master.doom_guard.window),
                "daemon.shutdown_grace_secs" => println!("{}", config.daemon.shutdown_grace_secs),
                "daemon.idle_exit_minutes" => println!("{}", config.daemon.idle_exit_minutes),
                "daemon.local_auth" => println!("{}", config.daemon.local_auth),
                "daemon.rehost_armed_schedules" => {
                    println!("{}", config.daemon.rehost_armed_schedules)
                }
                "tui.color_scheme"
                | "tui.transcript_layout"
                | "tui.click_outside_dismiss"
                | "tui.expand_auto_scroll"
                | "input_history.dedup"
                | "input_history.record_commands" => {
                    return Err(
                        "TUI presentation settings have been decoupled to $XDG_CONFIG_HOME/mutx/config.toml (ADR-0136)"
                            .into(),
                    );
                }
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
                "compaction.preserve_rounds" | "compaction_preserve_rounds" => {
                    config.compaction.preserve_rounds = value
                        .parse()
                        .map_err(|_| "invalid integer for compaction.preserve_rounds")?;
                }
                "compaction.summarize" | "compaction_summarize" => {
                    config.compaction.summarize = value
                        .parse()
                        .map_err(|_| "invalid boolean (true/false) for compaction.summarize")?;
                }
                "compaction.prune" | "compaction_prune" => {
                    config.compaction.prune = value
                        .parse()
                        .map_err(|_| "invalid boolean (true/false) for compaction.prune")?;
                }
                "compaction.prune_protect_tokens" | "compaction_prune_protect_tokens" => {
                    config.compaction.prune_protect_tokens = value
                        .parse()
                        .map_err(|_| "invalid integer for compaction.prune_protect_tokens")?;
                }
                "compaction.utilization" => {
                    config.compaction.utilization = value
                        .parse()
                        .map_err(|_| "invalid float for compaction.utilization")?;
                }
                "compaction.target_utilization" => {
                    config.compaction.target_utilization = value
                        .parse()
                        .map_err(|_| "invalid float for compaction.target_utilization")?;
                }
                "compaction.prune_utilization" => {
                    config.compaction.prune_utilization = value
                        .parse()
                        .map_err(|_| "invalid float for compaction.prune_utilization")?;
                }
                "compaction.fallback_window_tokens" => {
                    config.compaction.fallback_window_tokens = value
                        .parse()
                        .map_err(|_| "invalid integer for compaction.fallback_window_tokens")?;
                }
                "master.hard_stop_turns" => {
                    config.master.hard_stop_turns = value
                        .parse()
                        .map_err(|_| "invalid integer for master.hard_stop_turns")?;
                }
                "master.allow_model_stdin" => {
                    config.master.allow_model_stdin = value
                        .parse()
                        .map_err(|_| "invalid boolean for master.allow_model_stdin")?;
                }
                "master.skip_interactive_input" => {
                    config.master.skip_interactive_input = value
                        .parse()
                        .map_err(|_| "invalid boolean for master.skip_interactive_input")?;
                }
                "master.doom_guard.enabled" => {
                    config.master.doom_guard.enabled = value
                        .parse()
                        .map_err(|_| "invalid boolean for master.doom_guard.enabled")?;
                }
                "master.doom_guard.window" => {
                    config.master.doom_guard.window = value
                        .parse()
                        .map_err(|_| "invalid integer for master.doom_guard.window")?;
                }
                "daemon.shutdown_grace_secs" => {
                    config.daemon.shutdown_grace_secs = value
                        .parse()
                        .map_err(|_| "invalid integer for daemon.shutdown_grace_secs")?;
                }
                "daemon.idle_exit_minutes" => {
                    config.daemon.idle_exit_minutes = value
                        .parse()
                        .map_err(|_| "invalid integer for daemon.idle_exit_minutes")?;
                }
                "daemon.local_auth" => {
                    config.daemon.local_auth = value
                        .parse()
                        .map_err(|_| "invalid boolean for daemon.local_auth")?;
                }
                "daemon.rehost_armed_schedules" => {
                    config.daemon.rehost_armed_schedules = value
                        .parse()
                        .map_err(|_| "invalid boolean for daemon.rehost_armed_schedules")?;
                }
                "tui.color_scheme"
                | "tui.transcript_layout"
                | "tui.click_outside_dismiss"
                | "tui.expand_auto_scroll"
                | "input_history.dedup"
                | "input_history.record_commands" => {
                    return Err(
                        "TUI presentation settings have been decoupled to $XDG_CONFIG_HOME/mutx/config.toml (ADR-0136)"
                            .into(),
                    );
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
