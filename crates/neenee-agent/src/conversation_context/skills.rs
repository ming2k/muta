//! Implicit skill context injected when the latest visible user text names a skill.

use std::collections::HashSet;

use crate::{InjectionKind, Message, Role};

pub(crate) fn inject_mentioned_skills(
    registry: &neenee_skills::SkillRegistry,
    messages: &mut Vec<Message>,
) {
    let text = messages
        .iter()
        .filter(|message| message.role == Role::User && !message.hidden)
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        return;
    }

    let already_loaded: HashSet<String> = messages
        .iter()
        .filter(|message| message.role == Role::User && message.hidden)
        .filter_map(|message| {
            let prefix = "[Skill '";
            let start = message.content.find(prefix)? + prefix.len();
            let end = message.content[start..].find("' loaded]")?;
            Some(message.content[start..start + end].to_string())
        })
        .collect();

    let mentioned: Vec<String> = {
        let registry = registry.lock();
        registry
            .resolve_mentions(&text)
            .into_iter()
            .map(|skill| skill.name)
            .filter(|name| !already_loaded.contains(name))
            .collect()
    };

    for name in mentioned {
        // Bodies are loaded lazily and cached on first use.
        let Some(Ok(content)) = registry.body_for(&name) else {
            continue;
        };
        messages.push(super::hidden_user_with_reason(
            InjectionKind::ImplicitSkill,
            &name,
            format!("[Skill '{name}' loaded]\n{content}\n[/Skill]"),
        ));
    }
}
