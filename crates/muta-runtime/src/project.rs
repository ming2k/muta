//! muta configuration initialization.
//!
//! `init_muta_config` materializes a `.muta/` configuration tree in a
//! directory (skills, commands, agents) for the `/init` slash command.

use std::path::Path;

/// Materialize a `.muta/` tree. Returns the list of newly created relative
/// paths (existing files are left untouched and not reported).
pub fn init_muta_config(base: &Path) -> Result<Vec<String>, String> {
    let mut created = Vec::new();
    let dirs = ["skills", "commands", "agents"];
    for dir in dirs {
        let path = base.join(".muta").join(dir);
        if !path.exists() {
            std::fs::create_dir_all(&path)
                .map_err(|e| format!("Failed to create '{}': {}", path.display(), e))?;
            created.push(format!(".muta/{}/.keep", dir));
            std::fs::write(path.join(".keep"), "")
                .map_err(|e| format!("Failed to write keep file: {}", e))?;
        }
    }

    // Drop a starter skill template so users can see the SKILL.md format.
    let example_skill = base.join(".muta/skills/example/SKILL.md");
    if !example_skill.exists() {
        if let Some(parent) = example_skill.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create '{}': {}", parent.display(), e))?;
        }
        std::fs::write(&example_skill, example_skill_template())
            .map_err(|e| format!("Failed to write '{}': {}", example_skill.display(), e))?;
        created.push(".muta/skills/example/SKILL.md".to_string());
    }

    let agents_md = base.join("AGENTS.md");
    if !agents_md.exists() {
        std::fs::write(&agents_md, agents_md_template(base))
            .map_err(|e| format!("Failed to write AGENTS.md: {}", e))?;
        created.push("AGENTS.md".to_string());
    }

    let gitignore = base.join(".gitignore");
    if !gitignore.exists() {
        std::fs::write(&gitignore, muta_gitignore())
            .map_err(|e| format!("Failed to write .gitignore: {}", e))?;
        created.push(".gitignore".to_string());
    }

    Ok(created)
}

fn example_skill_template() -> &'static str {
    "---\n\
     name: example\n\
     description: An example skill showing the frontmatter format.\n\
     short-description: Example skill\n\
     ---\n\
     \n\
     # Example Skill\n\
     \n\
     Edit this file or add more `.muta/skills/<name>/SKILL.md` files to teach\n\
     muta domain-specific conventions, build steps, or review checklists.\n"
}

fn agents_md_template(base: &Path) -> String {
    let project_name = base
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("this project");
    format!(
        "# {name} — Agent Guide\n\n\
         Background, architecture, and conventions coding agents need to work\n\
         effectively in this repository. Fill in the sections below as the\n\
         project matures.\n\n\
         ## Overview\n\n\
         Describe what `{name}` does and its high-level architecture.\n\n\
         ## Build & Test\n\n\
         ```\n\
         # build\n\
         # test\n\
         # lint\n\
         ```\n\n\
         ## Conventions\n\n\
         - Coding style and patterns\n\
         - Where new code should go\n\
         - Anything an agent must know before editing\n",
        name = project_name
    )
}

fn muta_gitignore() -> &'static str {
    "# muta\n.muta/session.json\n.muta/sessions/\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_muta_is_idempotent() {
        let dir = std::env::temp_dir().join(format!("muta-init-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = init_muta_config(&dir).unwrap();
        assert!(first.iter().any(|p| p == "AGENTS.md"));
        let second = init_muta_config(&dir).unwrap();
        assert!(second.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
