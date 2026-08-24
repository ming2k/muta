use muta_persistence::config::Config;

pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::load();
    let result = muta_skills::discovery::discover_all(&config.skills).await;

    if result.skills.is_empty() {
        println!("No skills discovered.");
        println!(
            "Tip: Place skills in .muta/skills/<name>/SKILL.md or ~/.local/share/muta/skills/."
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
    Ok(())
}
