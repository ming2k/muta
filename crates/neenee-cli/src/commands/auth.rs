use neenee_contracts::SecretString;
use neenee_runtime::startup::AuthAction;
use neenee_persistence::config::Config;

fn mask_key(secret: &Option<SecretString>) -> &'static str {
    if secret.is_some() {
        "Configured (●●●●●●)"
    } else {
        "Not Configured"
    }
}

pub fn run(action: AuthAction) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        AuthAction::List => {
            let config = Config::load();
            println!("{:<16} {:<24} {:<16}", "Provider", "Auth Status", "Active Default");
            println!("{:-<16} {:-<24} {:-<16}", "", "", "");

            let builtin_providers = [
                ("openai", mask_key(&config.openai_api_key)),
                ("anthropic", mask_key(&config.anthropic_api_key)),
                ("google", mask_key(&config.google_api_key)),
                ("deepseek", mask_key(&config.deepseek_api_key)),
                ("moonshot", mask_key(&config.moonshot_api_key)),
                ("zai", mask_key(&config.zai_api_key)),
                ("opencode_go", mask_key(&config.opencode_go_api_key)),
            ];

            for (name, status) in builtin_providers {
                let is_default = if config.default_provider == name { " [Active]" } else { "" };
                println!("{:<16} {:<24}{}", name, status, is_default);
            }

            for p in &config.providers {
                let has_key = p
                    .channels
                    .iter()
                    .any(|c| c.api_key.is_some() || c.api_key_env.is_some());
                let status = if has_key {
                    "Configured (●●●●●●)"
                } else {
                    "Not Configured"
                };
                let is_default = if config.default_provider == p.id { " [Active]" } else { "" };
                println!("{:<16} {:<24}{}", p.id, status, is_default);
            }
        }
        AuthAction::Show(provider) => {
            let config = Config::load();
            let p_lower = provider.to_lowercase();
            let status = match p_lower.as_str() {
                "openai" => mask_key(&config.openai_api_key),
                "anthropic" => mask_key(&config.anthropic_api_key),
                "google" | "gemini" => mask_key(&config.google_api_key),
                "deepseek" => mask_key(&config.deepseek_api_key),
                "moonshot" | "kimi" => mask_key(&config.moonshot_api_key),
                "zai" | "zhipu" => mask_key(&config.zai_api_key),
                "opencode_go" | "opencode" => mask_key(&config.opencode_go_api_key),
                custom => {
                    if let Some(p) = config.providers.iter().find(|p| p.id.eq_ignore_ascii_case(custom)) {
                        let has_key = p.channels.iter().any(|c| c.api_key.is_some() || c.api_key_env.is_some());
                        if has_key { "Configured (●●●●●●)" } else { "Not Configured" }
                    } else {
                        return Err(format!("unknown provider '{custom}'").into());
                    }
                }
            };
            println!("Provider '{}': {}", provider, status);
        }
        AuthAction::Set { provider, key } => {
            let mut config = Config::load();
            let p_lower = provider.to_lowercase();
            let secret = Some(SecretString::from(key.clone()));
            match p_lower.as_str() {
                "openai" => config.openai_api_key = secret,
                "anthropic" => config.anthropic_api_key = secret,
                "google" | "gemini" => config.google_api_key = secret,
                "deepseek" => config.deepseek_api_key = secret,
                "moonshot" | "kimi" => config.moonshot_api_key = secret,
                "zai" | "zhipu" => config.zai_api_key = secret,
                "opencode_go" | "opencode" => config.opencode_go_api_key = secret,
                custom => {
                    if let Some(p) = config.providers.iter_mut().find(|p| p.id.eq_ignore_ascii_case(custom)) {
                        for c in &mut p.channels {
                            c.api_key = secret.clone();
                        }
                    } else {
                        return Err(format!("unknown provider '{custom}'. To configure custom provider, use TUI or config file.").into());
                    }
                }
            }
            config.save()?;
            println!("Successfully set API key for provider '{}'.", provider);
        }
    }
    Ok(())
}
