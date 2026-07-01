//! Formatting skills for resolving user mentions.

use super::metadata::Skill;

/// Build a verbose listing similar to what list_skills returns.
pub fn format_skill_list(skills: &[Skill]) -> String {
    let mut lines = vec!["Available skills:".to_string()];
    for skill in skills {
        let state = if skill.enabled { "" } else { " (disabled)" };
        lines.push(format!(
            "- [{}] {}{}\n  {}",
            skill.scope,
            skill.name,
            state,
            if skill.description.as_str().trim().is_empty() {
                "No description"
            } else {
                skill.description.as_str()
            }
        ));
    }
    lines.join("\n")
}

/// Resolve which skills a piece of text is referring to.
///
/// Matches only explicit intent:
/// - `@skill-name`
/// - `skill://skill-name` or `skill://path/to/SKILL.md`
///
/// A plain skill name as a loose token does NOT match — that was too eager
/// and pulled skill bodies into context on coincidental word overlap.
pub fn resolve_mentions<'a>(text: &str, skills: &'a [Skill]) -> Vec<&'a Skill> {
    let mut matched = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for skill in skills
        .iter()
        .filter(|s| s.enabled && s.allows_implicit_invocation())
    {
        if is_mentioned(text, &skill.name, &skill.source) && seen.insert(skill.name.clone()) {
            matched.push(skill);
        }
    }

    matched
}

fn is_mentioned(text: &str, name: &str, source: &std::path::Path) -> bool {
    // Direct @mention.
    if text.contains(&format!("@{}", name)) {
        return true;
    }

    // skill:// URI by name or by source path.
    let skill_uri = format!("skill://{}", name);
    if text.contains(&skill_uri) {
        return true;
    }
    let source_str = source.to_string_lossy();
    if text.contains(&format!("skill://{}", source_str)) {
        return true;
    }

    // Deliberately do NOT match the plain skill name as a loose token: a
    // coincidental word in the user's message ("pdf", "docx", ...) would
    // otherwise pull the full skill body into context as a hidden user
    // message. Implicit loading now requires an explicit signal — @mention
    // or a skill:// URI.
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sample_skill(name: &str) -> Skill {
        Skill {
            name: name.to_string(),
            description: "desc".to_string(),
            short_description: None,
            scope: crate::skills::SkillScope::Repo,
            source: PathBuf::from(format!("skills/{}/SKILL.md", name)),
            root: PathBuf::from(format!("skills/{}", name)),
            content: "body".to_string(),
            policy: super::super::metadata::SkillPolicy::default(),
            dependencies: vec![],
            tags: vec![],
            version: None,
            enabled: true,
        }
    }

    #[test]
    fn resolves_at_mention() {
        let skills = vec![sample_skill("rust-expert")];
        let mentions = resolve_mentions("ask @rust-expert for help", &skills);
        assert_eq!(mentions.len(), 1);
        assert_eq!(mentions[0].name, "rust-expert");
    }

    #[test]
    fn does_not_match_plain_name_token() {
        // A plain skill name as a loose token must NOT trigger implicit
        // loading — only @mention or skill:// should.
        let skills = vec![sample_skill("rust-expert")];
        let mentions = resolve_mentions("rust-expert: review this", &skills);
        assert!(mentions.is_empty());
    }

    #[test]
    fn does_not_match_substring() {
        let skills = vec![sample_skill("rust")];
        let mentions = resolve_mentions("rust-expert: review this", &skills);
        assert!(mentions.is_empty());
    }

    #[test]
    fn resolves_skill_uri() {
        let skills = vec![sample_skill("rust-expert")];
        let mentions = resolve_mentions("load skill://rust-expert", &skills);
        assert_eq!(mentions.len(), 1);
    }
}
