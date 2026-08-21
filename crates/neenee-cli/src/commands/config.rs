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
                "default_provider: {}",
                if config.default_provider.is_empty() {
                    "(none)"
                } else {
                    &config.default_provider
                }
            );
            println!(
                "default_model:    {}",
                config.default_model.as_deref().unwrap_or("(none)")
            );
            println!("retry_max_attempts: {}", config.provider_retry_max_attempts);
            println!("retry_base_ms:      {}ms", config.provider_retry_base_ms);
            println!("retry_max_ms:       {}ms", config.provider_retry_max_ms);
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
                "instances_count:            {}",
                neenee_persistence::instances::Instances::load()
                    .providers
                    .len()
            );
        }
        ConfigAction::Get(key) => {
            let config = Config::load();
            match key.as_str() {
                "default_provider" => println!("{}", config.default_provider),
                "default_model" => println!("{}", config.default_model.as_deref().unwrap_or("")),
                "provider_retry_max_attempts" => println!("{}", config.provider_retry_max_attempts),
                "provider_retry_base_ms" => println!("{}", config.provider_retry_base_ms),
                "provider_retry_max_ms" => println!("{}", config.provider_retry_max_ms),
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
                "default_provider" => {
                    config.default_provider = value.clone();
                }
                "default_model" => {
                    config.default_model = Some(value.clone());
                }
                "provider_retry_max_attempts" => {
                    config.provider_retry_max_attempts = value
                        .parse()
                        .map_err(|_| "invalid integer for retry_max_attempts")?;
                }
                "provider_retry_base_ms" => {
                    config.provider_retry_base_ms = value
                        .parse()
                        .map_err(|_| "invalid integer for retry_base_ms")?;
                }
                "provider_retry_max_ms" => {
                    config.provider_retry_max_ms = value
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
