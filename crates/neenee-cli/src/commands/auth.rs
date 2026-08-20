use crate::cli::AuthAction;
use neenee_persistence::config::{Config, Credentials};
use neenee_persistence::instances::Instances;

fn mask_key(secret: &neenee_contracts::SecretString) -> &'static str {
    if !secret.expose_secret().trim().is_empty() {
        "Configured (●●●●●●)"
    } else {
        "Not Configured"
    }
}

/// The auth status of one instance: env override, stored credential, or none.
fn instance_status(instances: &Instances, creds: &Credentials, id: &str) -> &'static str {
    if let Some(instance) = instances.get(id) {
        if instance.api_key_env.is_some() {
            return "Configured (env)";
        }
        if instance.auth.is_oauth() {
            return "OAuth";
        }
    }
    match creds.api_key(id) {
        Some(key) => mask_key(key),
        None => "Not Configured",
    }
}

pub fn run(action: AuthAction) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        AuthAction::List => {
            let config = Config::load();
            let instances = Instances::load();
            let creds = Credentials::load();
            println!(
                "{:<24} {:<24} {:<16}",
                "Provider", "Auth Status", "Active Default"
            );
            println!("{:-<24} {:-<24} {:-<16}", "", "", "");

            for p in &instances.providers {
                let is_default = if config.default_provider == p.id {
                    " [Active]"
                } else {
                    ""
                };
                println!(
                    "{:<24} {:<24}{}",
                    p.display_name(),
                    instance_status(&instances, &creds, &p.id),
                    is_default
                );
            }
        }
        AuthAction::Show(provider) => {
            let config = Config::load();
            let instances = Instances::load();
            let creds = Credentials::load();
            let found = instances
                .providers
                .iter()
                .find(|p| p.id.eq_ignore_ascii_case(&provider));
            let Some(found) = found else {
                return Err(format!("unknown provider '{provider}'").into());
            };
            let status = instance_status(&instances, &creds, &found.id);
            let is_default = if config.default_provider == found.id {
                " [Active]"
            } else {
                ""
            };
            println!(
                "Provider '{}': {}{}",
                found.display_name(),
                status,
                is_default
            );
        }
        AuthAction::Set { provider, key } => {
            let mut instances = Instances::load();
            let Some(id) = instances
                .providers
                .iter()
                .find(|p| p.id.eq_ignore_ascii_case(&provider))
                .map(|p| p.id.clone())
            else {
                return Err(format!(
                    "unknown provider '{provider}'. To configure a provider, use the TUI or add it to the state store."
                )
                .into());
            };
            let mut creds = Credentials::load();
            creds.set_api_key(&id, Some(neenee_contracts::SecretString::from(key)));
            creds.save()?;
            println!("Successfully set API key for provider '{id}'.");
            // `instances` was only loaded to look up the id; keep the borrow alive
            // so the list stays consistent (no-op otherwise).
            let _ = &mut instances;
        }
    }
    Ok(())
}
