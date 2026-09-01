//! Workspace security attestation, trusted asset reloading, and trust command handlers.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::commands::CustomCommand;
use muta_agent::Agent;
use muta_contracts::TrustDomain;
use muta_mcp::McpRuntime;
use muta_persistence::config::Config;
use muta_persistence::workspace_security::WorkspaceSecurityStore;
use muta_skills::SkillRegistry;

pub(crate) fn runtime_workspace_security(
    store: &WorkspaceSecurityStore,
    root: &Path,
) -> muta_contracts::WorkspaceSecuritySnapshot {
    store.snapshot(root)
}

pub(crate) fn live_custom_commands(
    store: &WorkspaceSecurityStore,
    root: &Path,
) -> HashMap<String, CustomCommand> {
    let rules_state = runtime_workspace_security(store, root).rules;
    crate::commands::discover_commands_with_trust(root, rules_state)
        .commands
        .into_iter()
        .map(|command| (command.name.clone(), command))
        .collect()
}

pub(crate) struct AssetReloadReport {
    pub snapshot: muta_contracts::WorkspaceSecuritySnapshot,
    pub connected_mcp: Vec<String>,
    pub removed_mcp: Vec<String>,
}

/// Rebuild every project-asset consumer from one freshly attested snapshot.
pub(crate) async fn reload_trusted_assets(
    agent: &Arc<Agent>,
    mcp_runtime: &Arc<McpRuntime>,
    workspace_security: &WorkspaceSecurityStore,
    project_root: &Path,
    skills_registry: &SkillRegistry,
) -> Result<AssetReloadReport, String> {
    let snapshot = workspace_security.snapshot(project_root);
    let mut effective = Config::load();
    if snapshot.mcp.is_trusted() {
        effective.merge_project_mcp(Config::load_project_mcp(project_root));
    }
    if snapshot.hooks.is_trusted() {
        effective.merge_project_hooks(Config::load_project_hooks(project_root));
    }

    let mcp_report = mcp_runtime.reconfigure(effective.mcp.clone()).await;
    agent.set_hooks(crate::hooks::build_hook_registry(&effective.hooks, agent));
    skills_registry.reload().await;
    let rules = if snapshot.rules.is_trusted() {
        crate::project::load_project_rules(project_root)?
    } else {
        String::new()
    };
    agent.set_project_rules(rules);
    agent.set_workspace_security(snapshot.clone());

    Ok(AssetReloadReport {
        snapshot,
        connected_mcp: mcp_report
            .connected
            .into_iter()
            .filter_map(|(name, ok)| ok.then_some(name))
            .collect(),
        removed_mcp: mcp_report.removed,
    })
}

#[allow(dead_code)]
pub(crate) fn parse_trust_domain(sub: &str) -> Result<TrustDomain, String> {
    match sub {
        "mcp" => Ok(TrustDomain::Mcp),
        "skills" => Ok(TrustDomain::Skills),
        "hooks" => Ok(TrustDomain::Hooks),
        "rules" => Ok(TrustDomain::Rules),
        other => Err(format!(
            "Unknown trust domain `{other}`. Valid domains: `mcp`, `skills`, `hooks`, `rules`, or `all`."
        )),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrustRoute {
    GrantAll,
    Grant(TrustDomain),
    Revoke,
    Status,
}

pub(crate) fn trust_route(name: &str, parts: &[&str]) -> Result<TrustRoute, String> {
    if name == "untrust" {
        return if parts.len() == 1 {
            Ok(TrustRoute::Revoke)
        } else {
            Err("/untrust accepts no arguments.".to_string())
        };
    }
    match parts.get(1).copied() {
        None | Some("all") => Ok(TrustRoute::GrantAll),
        Some("mcp") => Ok(TrustRoute::Grant(TrustDomain::Mcp)),
        Some("skills") => Ok(TrustRoute::Grant(TrustDomain::Skills)),
        Some("hooks") => Ok(TrustRoute::Grant(TrustDomain::Hooks)),
        Some("rules") => Ok(TrustRoute::Grant(TrustDomain::Rules)),
        Some("status") => Ok(TrustRoute::Status),
        Some("revoke") => Ok(TrustRoute::Revoke),
        Some(other) => Err(format!(
            "Unknown /trust subcommand '{other}'. Use `/trust`, `/trust all`, `/trust mcp`, \
             `/trust skills`, `/trust hooks`, `/trust rules`, `/trust status`, or `/trust revoke`."
        )),
    }
}
