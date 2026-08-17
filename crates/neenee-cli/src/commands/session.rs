use neenee_runtime::client;
use neenee_runtime::serve::ControlRequest;
use neenee_runtime::startup::SessionAction;
use std::path::PathBuf;

pub async fn run(
    action: SessionAction,
    project_override: Option<PathBuf>,
    autopilot: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let project_root = project_override
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    match action {
        SessionAction::List {
            watch,
            json,
            include_idle,
        } => {
            crate::status::run(
                &project_root,
                crate::status::StatusOptions {
                    watch,
                    json,
                    include_idle,
                    diagnostic: false,
                },
            )
            .await?;
        }
        SessionAction::Attach(id) => {
            crate::run_attached(id, false, project_override, autopilot, false, None).await?;
        }
        SessionAction::Delete(id) => {
            let info = client::discover(&project_root).ok_or_else(|| {
                "no daemon is running. Start or discover one before managing sessions.".to_string()
            })?;
            if !client::versions_compatible(&info) {
                return Err(client::version_mismatch(&info).into());
            }
            client::control(
                &info,
                ControlRequest::KillSession {
                    session_id: id.clone(),
                },
            )
            .await?;
            println!("Session '{}' has been terminated.", id);
        }
        SessionAction::Dashboard => {
            crate::run_dashboard(project_override, autopilot).await?;
        }
    }
    Ok(())
}
