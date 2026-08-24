//! Implicit skill context injected when the latest visible user text names a skill.

use std::collections::HashSet;

use crate::{InjectionKind, Message, Role};

pub(crate) fn inject_mentioned_skills(
    registry: &muta_skills::SkillRegistry,
    messages: &mut Vec<Message>,
) {
    // Fast pre-check before building any joined text: mentions have an
    // explicit grammar (`@name`, `@skill:name`, `skill://…`), and most turns
    // contain none of it. Scanning for the trigger characters first keeps the
    // common path O(recent user chars) without a full-history join — the old
    // unconditional join was O(total transcript chars) per call, and this
    // runs multiple times per ReAct turn (once per model_request/estimate).
    //
    // The mention scan itself is windowed to the most recent
    // [`MENTION_SCAN_WINDOW`] *visible user* messages: the mention grammar
    // is explicit user intent, so it lives in recent input; older mentions
    // have already produced their `[Skill '…' loaded]` marker, which the
    // full-history `already_loaded` set below still honors (hidden markers
    // are few, so that scan stays cheap).
    const MENTION_SCAN_WINDOW: usize = 32;
    let recent_user_texts: Vec<&str> = messages
        .iter()
        .filter(|message| message.role == Role::User && !message.hidden)
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>();
    let window_start = recent_user_texts.len().saturating_sub(MENTION_SCAN_WINDOW);
    let scan_slice = &recent_user_texts[window_start..];
    let mentions_present = scan_slice
        .iter()
        .any(|t| t.contains('@') || t.contains("skill://"));
    if !mentions_present {
        return;
    }
    let text = scan_slice.join("\n");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, Role};

    fn registry_with_skill(name: &str) -> muta_skills::SkillRegistry {
        // Minimal registry whose single skill matches `@name` mentions:
        // build via serde defaults (all fields default sensibly, implicit
        // invocation allowed by the default policy), then `replace` it into
        // an empty registry — the same public surface the agent path uses.
        let mut skill: muta_skills::Skill =
            serde_json::from_value(serde_json::json!({ "name": name, "description": "d", "scope": "User", "source": "/nonexistent-skill.md", "root": ".", "content": "", "version": null, "policy": { "allow_implicit_invocation": true } }))
                .unwrap();
        skill.policy.allow_implicit_invocation = true;
        let registry = muta_skills::SkillRegistry::empty();
        registry.replace(vec![skill]);
        registry
    }

    /// A `@name` mention in the latest user message loads the skill.
    #[test]
    fn mention_in_recent_message_loads_skill() {
        let registry = registry_with_skill("rust-expert");
        let mut messages = vec![Message::new(Role::User, "please use @rust-expert here")];
        inject_mentioned_skills(&registry, &mut messages);
        assert!(
            messages
                .iter()
                .any(|m| m.hidden && m.content.contains("[Skill 'rust-expert' loaded]")),
            "mention must inject the skill body"
        );
    }

    /// Text with no mention grammar (`@`, `skill://`) exits before any
    /// matching — the common fast path.
    #[test]
    fn plain_history_loads_nothing() {
        let registry = registry_with_skill("rust-expert");
        let mut messages = vec![Message::new(Role::User, "just a normal prompt")];
        inject_mentioned_skills(&registry, &mut messages);
        assert_eq!(messages.len(), 1, "nothing injected");
    }
}
