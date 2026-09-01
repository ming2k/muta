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

/// Format available skills as a lean XML block for system prompt progressive disclosure.
/// The model inspects this metadata and reads the full SKILL.md on demand using read tools.
pub fn format_skills_for_prompt(skills: &[Skill]) -> String {
    let visible: Vec<&Skill> = skills
        .iter()
        .filter(|s| s.enabled && !s.quarantined && s.allows_implicit_invocation())
        .collect();
    if visible.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "\nThe following skills provide specialized instructions for specific tasks.\n\
         Read the full skill file using file reading tools when the task matches its description.\n\n\
         <available_skills>\n",
    );
    for skill in visible {
        out.push_str("  <skill>\n");
        out.push_str(&format!("    <name>{}</name>\n", escape_xml(&skill.name)));
        out.push_str(&format!(
            "    <description>{}</description>\n",
            escape_xml(&skill.description)
        ));
        out.push_str(&format!(
            "    <location>{}</location>\n",
            escape_xml(&skill.source.to_string_lossy())
        ));
        out.push_str("  </skill>\n");
    }
    out.push_str("</available_skills>");
    out
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Resolve which skills a piece of text is referring to.
///
/// Matches only explicit intent:
/// - `@skill-name`
/// - `@skill:skill-name` or `@skills:skill-name` (disambiguated namespace)
/// - `skill://skill-name` or `skill://path/to/SKILL.md`
///
/// A plain skill name as a loose token does NOT match — that was too eager
/// and pulled skill bodies into context on coincidental word overlap.
///
/// Matching is **token-boundary aware**: the mention forms are scanned out of
/// the text as whole identifiers first, then compared for equality. So
/// `@rust-expert` does not match a skill named `rust` (the identifier runs on
/// past it), and neither does `@skill:rust-expert`. The previous
/// substring-`contains` check could not distinguish these.
pub fn resolve_mentions<'a>(text: &str, skills: &'a [Skill]) -> Vec<&'a Skill> {
    let names = at_mention_names(text);
    let uris = skill_uris(text);
    if names.is_empty() && uris.is_empty() {
        return Vec::new();
    }

    let mut matched = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for skill in skills
        .iter()
        .filter(|s| s.enabled && s.allows_implicit_invocation())
    {
        let uri_hit = uris
            .iter()
            .any(|uri| uri == &skill.name || uri == &skill.source.to_string_lossy().to_string());
        let hit = names.contains(skill.name.as_str()) || uri_hit;
        if hit && seen.insert(skill.name.clone()) {
            matched.push(skill);
        }
    }

    matched
}

/// Characters allowed inside a skill name or `skill://` path segment.
/// Skill names are conventionally `[A-Za-z0-9._-]` (e.g. `rust-expert`,
/// `code_review`, `v1.2`); the `skill://` form additionally permits `/` so a
/// full source path like `skills/rust-expert/SKILL.md` round-trips.
fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '-' | '_' | '.')
}

/// Extract every skill identifier the text *explicitly* refers to via an
/// `@`-mention, returning borrowed slices into `text`. The three forms all
/// collapse to the bare name:
/// - `@name`        — bare mention
/// - `@skill:name`  — disambiguated namespace
/// - `@skills:name` — plural namespace (mirrors `@files:`)
///
/// Because identifiers are read to their boundary, `@rust` in the text
/// `@rust-expert` is never produced (the run-on `rust-expert` is), so prefix
/// collisions cannot inflate matches.
fn at_mention_names(text: &str) -> std::collections::HashSet<&str> {
    let mut names = std::collections::HashSet::new();
    let mut search_from = 0;
    while let Some(rel) = text[search_from..].find('@') {
        let at = search_from + rel;
        let after_at = at + 1;
        let rest = text.get(after_at..).unwrap_or("");
        // Optional `skill:` / `skills:` namespace prefix.
        let name_start = rest
            .strip_prefix("skills:")
            .or_else(|| rest.strip_prefix("skill:"))
            .map(|stripped| after_at + (rest.len() - stripped.len()))
            .unwrap_or(after_at);
        // Read the identifier to its boundary.
        let mut end = name_start;
        while let Some(ch) = text[end..].chars().next()
            && is_name_char(ch)
        {
            end += ch.len_utf8();
        }
        if end > name_start {
            names.insert(&text[name_start..end]);
        }
        search_from = after_at;
    }
    names
}

/// Extract `skill://{path}` references. The path segment admits `/` in
/// addition to name chars so both `skill://rust-expert` and
/// `skill://skills/rust-expert/SKILL.md` are captured whole.
fn skill_uris(text: &str) -> Vec<String> {
    const SCHEME: &str = "skill://";
    let mut out = Vec::new();
    let mut search_from = 0;
    while let Some(rel) = text[search_from..].find(SCHEME) {
        let start = search_from + rel + SCHEME.len();
        let mut end = start;
        while let Some(ch) = text[end..].chars().next()
            && (is_name_char(ch) || ch == '/')
        {
            end += ch.len_utf8();
        }
        if end > start {
            out.push(text[start..end].to_string());
        }
        search_from = start;
    }
    out
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
            scope: crate::SkillScope::Repo,
            source: PathBuf::from(format!("skills/{}/SKILL.md", name)),
            root: PathBuf::from(format!("skills/{}", name)),
            content: "body".to_string(),
            policy: super::super::metadata::SkillPolicy::default(),
            dependencies: vec![],
            tags: vec![],
            version: None,
            enabled: true,
            quarantined: false,
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

    #[test]
    fn resolves_at_skill_namespace() {
        // `@skill:{name}` is the disambiguated form — useful when the skill
        // name is also a common word. `@skills:{name}` is the accepted plural.
        let skills = vec![sample_skill("rust-expert")];
        let mentions = resolve_mentions("请按 @skill:rust-expert 规范处理", &skills);
        assert_eq!(mentions.len(), 1);
        assert_eq!(mentions[0].name, "rust-expert");

        let mentions = resolve_mentions("load @skills:rust-expert now", &skills);
        assert_eq!(mentions.len(), 1);
        assert_eq!(mentions[0].name, "rust-expert");
    }

    #[test]
    fn skill_namespace_does_not_match_partial_name() {
        // `@skill:rust` must not match a skill named `rust-expert` (no prefix
        // match), and `@skill:rust-expert` must not match one named `rust`.
        let long = vec![sample_skill("rust-expert")];
        assert!(resolve_mentions("use @skill:rust", &long).is_empty());

        let short = vec![sample_skill("rust")];
        assert!(resolve_mentions("use @skill:rust-expert", &short).is_empty());
    }

    #[test]
    fn resolves_multiple_distinct_skills_via_namespace() {
        let skills = vec![sample_skill("rust-expert"), sample_skill("pdf")];
        let mentions = resolve_mentions("use @skill:rust-expert and @skills:pdf here", &skills);
        assert_eq!(mentions.len(), 2);
    }
}
