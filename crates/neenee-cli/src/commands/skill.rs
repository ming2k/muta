use neenee_persistence::config::Config;
use neenee_runtime::startup::SkillAction;

pub async fn run(action: SkillAction) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        SkillAction::List => {
            let config = Config::load();
            let result = neenee_skills::discovery::discover_all(&config.skills).await;

            if result.skills.is_empty() {
                println!("No skills discovered.");
                println!(
                    "Tip: Place skills in .neenee/skills/<name>/SKILL.md or ~/.local/share/neenee/skills/."
                );
                return Ok(());
            }

            println!(
                "{:<20} {:<12} {:<10} Description",
                "Skill Name", "Scope", "Version"
            );
            println!("{:-<20} {:-<12} {:-<10} {:-<30}", "", "", "", "");

            for skill in &result.skills {
                let scope = format!("{:?}", skill.scope);
                let version = skill.version.as_deref().unwrap_or("–");
                let desc = skill
                    .short_description
                    .as_deref()
                    .unwrap_or(skill.description.as_str());
                let short_desc = if desc.chars().count() > 50 {
                    format!("{}...", desc.chars().take(47).collect::<String>())
                } else {
                    desc.to_string()
                };
                println!(
                    "{:<20} {:<12} {:<10} {}",
                    skill.name, scope, version, short_desc
                );
            }

            if !result.errors.is_empty() {
                eprintln!("\nWarnings during discovery:");
                for err in &result.errors {
                    eprintln!("  • {err}");
                }
            }
        }
    }
    Ok(())
}
