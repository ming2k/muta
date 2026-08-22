use crate::cli::AuthAction;
use neenee_persistence::config::{Config, Credentials};
use neenee_persistence::connections::Connections;

fn mask_key(secret: &neenee_contracts::SecretString) -> &'static str {
    if !secret.expose_secret().trim().is_empty() {
        "Configured (●●●●●●)"
    } else {
        "Not Configured"
    }
}

/// The auth status of one connection: env override, stored credential, or none.
fn instance_status(connections: &Connections, creds: &Credentials, id: &str) -> &'static str {
    if let Some(instance) = connections.get(id) {
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
            let connections = Connections::load();
            let creds = Credentials::load();
            println!(
                "{:<24} {:<24} {:<16}",
                "Connection", "Auth Status", "Active Default"
            );
            println!("{:-<24} {:-<24} {:-<16}", "", "", "");

            for p in &connections.connections {
                let is_default = if config.default_connection == p.id {
                    " [Active]"
                } else {
                    ""
                };
                println!(
                    "{:<24} {:<24}{}",
                    p.display_name(),
                    instance_status(&connections, &creds, &p.id),
                    is_default
                );
            }
        }
        AuthAction::Show(provider) => {
            let config = Config::load();
            let connections = Connections::load();
            let creds = Credentials::load();
            let found = connections
                .connections
                .iter()
                .find(|p| p.id.eq_ignore_ascii_case(&provider));
            let Some(found) = found else {
                return Err(format!("unknown connection '{provider}'").into());
            };
            let status = instance_status(&connections, &creds, &found.id);
            let is_default = if config.default_connection == found.id {
                " [Active]"
            } else {
                ""
            };
            println!(
                "Connection '{}': {}{}",
                found.display_name(),
                status,
                is_default
            );
        }
        AuthAction::Set { provider, key } => {
            let mut connections = Connections::load();
            let Some(id) = connections
                .connections
                .iter()
                .find(|p| p.id.eq_ignore_ascii_case(&provider))
                .map(|p| p.id.clone())
            else {
                return Err(format!(
                    "unknown connection '{provider}'. To configure a connection, use the TUI or add it to connections.toml."
                )
                .into());
            };
            let mut creds = Credentials::load();
            creds.set_api_key(&id, Some(neenee_contracts::SecretString::from(key)));
            creds.save()?;
            println!("Successfully set API key for connection '{id}'.");
            let _ = &mut connections;
        }
    }
    Ok(())
}
