//! Safety guard for model-issued `bash` commands.
//!
//! The normal permission broker answers "may this tool run?". This module adds
//! a narrower, command-aware gate for `bash`: broad approvals such as `bash *`
//! must not silently authorize destructive commands like `git reset --hard`.
//! Built-in dangerous-command rules live in code; configuration only supplies
//! user overrides/additions.

use neenee_store::config::{
    BashPolicyActionConfig, BashPolicyConfig, BashPolicyMatcherConfig, BashPolicyRuleConfig,
};
use regex::Regex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BashPolicyAction {
    Allow,
    Confirm,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuleOrigin {
    Builtin,
    User,
}

#[derive(Debug, Clone)]
struct CompiledRule {
    name: String,
    matcher: CompiledMatcher,
    action: BashPolicyAction,
    reason: String,
    origin: RuleOrigin,
}

#[derive(Debug, Clone)]
enum CompiledMatcher {
    Regex(Regex),
    Contains(String),
    StartsWith(String),
    Program(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BashPolicyMatch {
    pub(crate) action: BashPolicyAction,
    pub(crate) name: String,
    pub(crate) reason: String,
    pub(crate) builtin: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct BashPolicy {
    enabled: bool,
    unattended_confirm: BashPolicyAction,
    allow_user_override_builtin_deny: bool,
    user_rules: Vec<CompiledRule>,
    invalid_rules: Vec<String>,
}

impl Default for BashPolicy {
    fn default() -> Self {
        Self::from_config(&BashPolicyConfig::default())
    }
}

impl BashPolicy {
    pub(crate) fn from_config(config: &BashPolicyConfig) -> Self {
        let mut invalid_rules = Vec::new();
        let user_rules = config
            .rules
            .iter()
            .filter_map(|rule| match CompiledRule::from_user_config(rule) {
                Ok(rule) => Some(rule),
                Err(error) => {
                    invalid_rules.push(error);
                    None
                }
            })
            .collect();
        Self {
            enabled: config.enabled,
            unattended_confirm: match config.unattended_confirm {
                neenee_store::config::BashPolicyUnattendedAction::Deny => BashPolicyAction::Deny,
                neenee_store::config::BashPolicyUnattendedAction::Allow => BashPolicyAction::Allow,
            },
            allow_user_override_builtin_deny: config.allow_user_override_builtin_deny,
            user_rules,
            invalid_rules,
        }
    }

    pub(crate) fn invalid_rules(&self) -> &[String] {
        &self.invalid_rules
    }

    pub(crate) fn evaluate(&self, command: &str) -> Option<BashPolicyMatch> {
        if !self.enabled {
            return None;
        }

        let user_match = first_match(&self.user_rules, command);
        let builtin_deny = first_match(&builtin_deny_rules(), command);
        let builtin_confirm = first_match(&builtin_confirm_rules(), command);

        if let Some(user) = user_match {
            if matches!(
                user.action,
                BashPolicyAction::Deny | BashPolicyAction::Confirm
            ) {
                return Some(user.into_match());
            }
            // User allow can deliberately quiet built-in confirm rules, but not
            // compiled-in deny rules unless the sharp override knob is enabled.
            if builtin_deny.is_none() || self.allow_user_override_builtin_deny {
                return None;
            }
        }

        builtin_deny
            .or(builtin_confirm)
            .map(CompiledRule::into_match)
    }

    pub(crate) fn unattended_confirm_action(&self) -> BashPolicyAction {
        self.unattended_confirm
    }
}

impl CompiledRule {
    fn from_user_config(config: &BashPolicyRuleConfig) -> Result<Self, String> {
        let name = if config.name.trim().is_empty() {
            format!("user rule {:?} {:?}", config.matcher, config.pattern)
        } else {
            config.name.trim().to_string()
        };
        let reason = config
            .reason
            .as_deref()
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
            .unwrap_or("matched user bash policy rule")
            .to_string();
        let matcher = compile_matcher(config.matcher, &config.pattern)
            .map_err(|error| format!("invalid bash_policy rule '{name}': {error}"))?;
        Ok(Self {
            name,
            matcher,
            action: action_from_config(config.action),
            reason,
            origin: RuleOrigin::User,
        })
    }

    fn builtin(
        name: &'static str,
        pattern: &'static str,
        action: BashPolicyAction,
        reason: &'static str,
    ) -> Self {
        Self {
            name: name.to_string(),
            matcher: CompiledMatcher::Regex(Regex::new(pattern).unwrap_or_else(|error| {
                panic!("built-in bash policy regex must compile: {error}")
            })),
            action,
            reason: reason.to_string(),
            origin: RuleOrigin::Builtin,
        }
    }

    fn matches(&self, command: &str) -> bool {
        match &self.matcher {
            CompiledMatcher::Regex(regex) => regex.is_match(command),
            CompiledMatcher::Contains(needle) => command.contains(needle),
            CompiledMatcher::StartsWith(prefix) => command.trim_start().starts_with(prefix),
            CompiledMatcher::Program(program) => leading_program(command) == *program,
        }
    }

    fn into_match(self) -> BashPolicyMatch {
        BashPolicyMatch {
            action: self.action,
            name: self.name,
            reason: self.reason,
            builtin: self.origin == RuleOrigin::Builtin,
        }
    }
}

fn first_match(rules: &[CompiledRule], command: &str) -> Option<CompiledRule> {
    rules.iter().find(|rule| rule.matches(command)).cloned()
}

fn compile_matcher(
    matcher: BashPolicyMatcherConfig,
    pattern: &str,
) -> Result<CompiledMatcher, String> {
    match matcher {
        BashPolicyMatcherConfig::Regex => Regex::new(pattern)
            .map(CompiledMatcher::Regex)
            .map_err(|error| error.to_string()),
        BashPolicyMatcherConfig::Contains => Ok(CompiledMatcher::Contains(pattern.to_string())),
        BashPolicyMatcherConfig::StartsWith => Ok(CompiledMatcher::StartsWith(pattern.to_string())),
        BashPolicyMatcherConfig::Program => Ok(CompiledMatcher::Program(pattern.to_string())),
    }
}

fn action_from_config(action: BashPolicyActionConfig) -> BashPolicyAction {
    match action {
        BashPolicyActionConfig::Allow => BashPolicyAction::Allow,
        BashPolicyActionConfig::Confirm => BashPolicyAction::Confirm,
        BashPolicyActionConfig::Deny => BashPolicyAction::Deny,
    }
}

fn leading_program(command: &str) -> String {
    command
        .split_whitespace()
        .find(|token| !looks_like_env_assignment(token))
        .map(|token| token.rsplit('/').next().unwrap_or(token).to_string())
        .unwrap_or_default()
}

fn looks_like_env_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name.chars().all(|c| c == '_' || c.is_ascii_alphanumeric())
        && name
            .chars()
            .next()
            .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
}

fn builtin_deny_rules() -> Vec<CompiledRule> {
    use BashPolicyAction::Deny;
    vec![
        CompiledRule::builtin(
            "format filesystem",
            r"(?i)(^|[;&|()\s])mkfs(\.[\w-]+)?\b",
            Deny,
            "mkfs formats filesystems and can destroy data.",
        ),
        CompiledRule::builtin(
            "wipe filesystem signatures",
            r"(?i)(^|[;&|()\s])wipefs\b",
            Deny,
            "wipefs removes filesystem signatures from block devices.",
        ),
        CompiledRule::builtin(
            "overwrite block device with dd",
            r"(?i)(^|[;&|()\s])dd\b[^;&|]*\bof=/dev/",
            Deny,
            "dd writing to /dev devices can irreversibly destroy disks.",
        ),
        CompiledRule::builtin(
            "remove root directory",
            r"(?i)(^|[;&|()\s])(?:sudo\s+)?rm\s+-[^;&|]*[rf][^;&|]*[rf][^;&|]*\s+(?:--\s+)?/(?:\s|$)",
            Deny,
            "rm -rf / would recursively delete the filesystem root.",
        ),
    ]
}

fn builtin_confirm_rules() -> Vec<CompiledRule> {
    use BashPolicyAction::Confirm;
    vec![
        CompiledRule::builtin(
            "git reset hard",
            r"(?i)(^|[;&|()\s])git\s+reset\b[^;&|]*\s--hard\b",
            Confirm,
            "git reset --hard discards uncommitted working tree changes.",
        ),
        CompiledRule::builtin(
            "git reset potentially destructive",
            r"(?i)(^|[;&|()\s])git\s+reset\b[^;&|]*(?:--merge|--keep)\b",
            Confirm,
            "git reset can rewrite the index, HEAD, or working tree state.",
        ),
        CompiledRule::builtin(
            "git clean force",
            r"(?i)(^|[;&|()\s])git\s+clean\b[^;&|]*\s-[A-Za-z]*f[A-Za-z]*\b",
            Confirm,
            "git clean -f deletes untracked files.",
        ),
        CompiledRule::builtin(
            "discard checkout paths",
            r"(?i)(^|[;&|()\s])git\s+checkout\b[^;&|]*\s--\s+(?:\.|\*|/|~|\.\/)",
            Confirm,
            "git checkout -- <path> can discard local working tree changes.",
        ),
        CompiledRule::builtin(
            "discard restore paths",
            r"(?i)(^|[;&|()\s])git\s+restore\b[^;&|]*(?:\s\.\s*$|\s\.\s|\s--worktree\b|\s--source\b)",
            Confirm,
            "git restore can discard local working tree changes.",
        ),
        CompiledRule::builtin(
            "recursive force remove",
            r"(?i)(^|[;&|()\s])(?:sudo\s+)?rm\s+-[^;&|]*[rf][^;&|]*[rf][^;&|]*(?:\s|$)",
            Confirm,
            "rm -rf recursively deletes files.",
        ),
        CompiledRule::builtin(
            "find delete",
            r"(?i)(^|[;&|()\s])find\b[^;&|]*\s-delete\b",
            Confirm,
            "find -delete removes every matched path.",
        ),
        CompiledRule::builtin(
            "recursive chmod",
            r"(?i)(^|[;&|()\s])(?:sudo\s+)?chmod\s+-R\b",
            Confirm,
            "chmod -R can broadly rewrite permissions.",
        ),
        CompiledRule::builtin(
            "recursive chown",
            r"(?i)(^|[;&|()\s])(?:sudo\s+)?chown\s+-R\b",
            Confirm,
            "chown -R can broadly rewrite file ownership.",
        ),
        CompiledRule::builtin(
            "curl pipe shell",
            r"(?i)(^|[;&|()\s])curl\b[^;&]*\|\s*(?:sudo\s+)?(?:sh|bash)\b",
            Confirm,
            "curl | shell executes remote code.",
        ),
        CompiledRule::builtin(
            "wget pipe shell",
            r"(?i)(^|[;&|()\s])wget\b[^;&]*\|\s*(?:sudo\s+)?(?:sh|bash)\b",
            Confirm,
            "wget | shell executes remote code.",
        ),
        CompiledRule::builtin(
            "npm publish",
            r"(?i)(^|[;&|()\s])(?:npm|pnpm|yarn)\s+(?:npm\s+)?publish\b",
            Confirm,
            "Publishing packages is an external side effect.",
        ),
        CompiledRule::builtin(
            "cargo publish",
            r"(?i)(^|[;&|()\s])cargo\s+publish\b",
            Confirm,
            "Publishing crates is an external side effect.",
        ),
        CompiledRule::builtin(
            "kubectl destructive/apply",
            r"(?i)(^|[;&|()\s])kubectl\s+(?:delete|apply|replace|scale|rollout)\b",
            Confirm,
            "kubectl mutates live cluster state.",
        ),
        CompiledRule::builtin(
            "terraform apply/destroy",
            r"(?i)(^|[;&|()\s])terraform\s+(?:apply|destroy)\b",
            Confirm,
            "terraform apply/destroy mutates infrastructure.",
        ),
        CompiledRule::builtin(
            "docker system prune",
            r"(?i)(^|[;&|()\s])docker\s+system\s+prune\b",
            Confirm,
            "docker system prune deletes Docker resources.",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use neenee_store::config::{
        BashPolicyActionConfig, BashPolicyMatcherConfig, BashPolicyRuleConfig,
    };

    #[test]
    fn builtin_confirms_git_reset_hard() {
        let policy = BashPolicy::default();
        let decision = policy.evaluate("git reset --hard HEAD~1").unwrap();
        assert_eq!(decision.action, BashPolicyAction::Confirm);
        assert_eq!(decision.name, "git reset hard");
    }

    #[test]
    fn builtin_denies_root_rm_rf() {
        let policy = BashPolicy::default();
        let decision = policy.evaluate("sudo rm -rf /").unwrap();
        assert_eq!(decision.action, BashPolicyAction::Deny);
    }

    #[test]
    fn user_allow_overrides_builtin_confirm() {
        let mut config = BashPolicyConfig::default();
        config.rules.push(BashPolicyRuleConfig {
            name: "allow reset in test fixture".to_string(),
            matcher: BashPolicyMatcherConfig::Contains,
            pattern: "git reset --hard".to_string(),
            action: BashPolicyActionConfig::Allow,
            reason: None,
        });
        let policy = BashPolicy::from_config(&config);
        assert!(policy.evaluate("git reset --hard").is_none());
    }

    #[test]
    fn user_allow_does_not_override_builtin_deny_by_default() {
        let mut config = BashPolicyConfig::default();
        config.rules.push(BashPolicyRuleConfig {
            name: "reckless".to_string(),
            matcher: BashPolicyMatcherConfig::Contains,
            pattern: "rm -rf /".to_string(),
            action: BashPolicyActionConfig::Allow,
            reason: None,
        });
        let policy = BashPolicy::from_config(&config);
        let decision = policy.evaluate("rm -rf /").unwrap();
        assert_eq!(decision.action, BashPolicyAction::Deny);
    }
}
