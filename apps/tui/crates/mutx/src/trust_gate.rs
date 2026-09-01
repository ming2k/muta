//! Pre-view workspace trust gate.
//!
//! When the client attaches to a session whose project root carries
//! project-authored contributions (skills, MCP, hooks, rules) that were
//! **never** trusted — `WorkspaceTrustState::Quarantined` from the durable
//! `WorkspaceSecurityStore` — the daemon's attach-sync `HarnessState`
//! already carries the security snapshot. This module turns that snapshot
//! into the *first* thing the user sees: a blocking question dialog opened
//! before the composer takes input, so the trust decision happens up front
//! instead of via a passive banner the user can (and usually does) scroll
//! past.
//!
//! The dialog is deliberately not a new modal. It reuses the ask_user
//! question sheet ([`crate::question_model::QuestionModel`]) by synthesizing
//! a [`UserQuestionRequest`] into the pending-question queue, so navigation,
//! rendering, scrolling, and multi-select semantics are the ones the user
//! already knows. Two seams keep the synthetic request honest:
//!
//! - **Identity.** The request id is the constant
//!   [`TRUST_GATE_REQUEST_ID`]. The reply path intercepts that id and maps
//!   the answer to the canonical `/trust …` slash command — never a bespoke
//!   wire message — so persistence *and* the atomic live reload stay owned
//!   by the one code path that already handles them. A synthetic request is
//!   never forwarded as `UserQuestionReply` (the daemon has no parked round
//!   waiting for it).
//! - **Lifecycle.** The gate is fed by `HarnessState` snapshots. It opens
//!   once per quarantined attach and closes as soon as a snapshot reports
//!   the workspace trusted (the `/trust` handler republishes the snapshot),
//!   so a stale dialog can never linger after the decision.
//!
//! Escaping the dialog is an explicit "keep quarantined" — the same outcome
//! as picking the option — which mirrors how the old banner left the
//! workspace untrusted, just with a decision the user actually made.

use muta_contracts::{
    TrustDomain, UserQuestion, UserQuestionOption, UserQuestionRequest, WorkspaceSecuritySnapshot,
    WorkspaceTrustState,
};

/// Request id marking the synthesized trust-gate question. Recognized by the
/// reply path (`super::event_loop`) and never sent to the daemon as a
/// `UserQuestionReply`.
pub const TRUST_GATE_REQUEST_ID: &str = "__workspace_trust_gate__";

/// Which quarantined domains the snapshot advertises, in display order.
/// Only `Absent` domains are skipped: there is nothing to decide about a
/// domain the workspace does not use.
fn quarantined_domains(snapshot: &WorkspaceSecuritySnapshot) -> Vec<TrustDomain> {
    [
        (TrustDomain::Mcp, snapshot.mcp),
        (TrustDomain::Skills, snapshot.skills),
        (TrustDomain::Hooks, snapshot.hooks),
        (TrustDomain::Instructions, snapshot.instructions),
        (TrustDomain::ExWorkspace, snapshot.ex_workspace),
    ]
    .into_iter()
    .filter(|(_, state)| *state != WorkspaceTrustState::Absent)
    .map(|(domain, _)| domain)
    .collect()
}

/// Build the trust-gate question request for a quarantined workspace, or
/// `None` when nothing needs gating (workspace trusted/absent, or already
/// previously trusted and merely changed — the changed case is escalated by
/// the daemon's banner instead, since the user already made a decision for
/// this workspace once).
pub fn gate_request(snapshot: &WorkspaceSecuritySnapshot) -> Option<UserQuestionRequest> {
    let domains = quarantined_domains(snapshot);
    if domains.is_empty() || snapshot.aggregate() == WorkspaceTrustState::Trusted {
        return None;
    }
    // Previously trusted content that changed on disk is not a first-contact
    // decision; the daemon's attach banner covers it.
    if snapshot.aggregate() == WorkspaceTrustState::Changed {
        return None;
    }
    let domain_rows: Vec<String> = domains
        .iter()
        .map(|d| format!("• {}", domain_label(*d)))
        .collect();
    let question = format!(
        "This workspace contains project-authored configurations that are not loaded until you trust them:\n{}\n\
         Trust them for this workspace? Trust is content-bound: if the files change (git pull, checkout), \
         trust drops back to quarantined until reviewed again.",
        domain_rows.join("\n")
    );
    Some(UserQuestionRequest {
        id: TRUST_GATE_REQUEST_ID.to_string(),
        questions: vec![UserQuestion {
            header: Some("Workspace trust".to_string()),
            question,
            options: vec![
                UserQuestionOption {
                    label: "Trust all domains (Recommended)".to_string(),
                    description: Some(
                        "Run `/trust` — trust every domain listed above for this workspace."
                            .to_string(),
                    ),
                },
                UserQuestionOption {
                    label: "Choose domains".to_string(),
                    description: Some(
                        "Pick specific domains to trust (e.g. `/trust rules`, `/trust skills`) via `/trust <domain>`."
                            .to_string(),
                    ),
                },
                UserQuestionOption {
                    label: "Keep quarantined".to_string(),
                    description: Some(
                        "Load nothing project-authored now. You can trust later with `/trust`."
                            .to_string(),
                    ),
                },
            ],
            multi_select: false,
        }],
        origin: Some("workspace trust".to_string()),
    })
}

/// Map a trust-gate dialog answer back to the slash command the reply path
/// should dispatch, or `None` for "no mutation" (keep quarantined / Esc).
///
/// `answers` is the `ask_user` reply shape: one `Vec<String>` of selected
/// option labels per question.
pub fn answer_to_command(answers: &[Vec<String>]) -> Option<String> {
    let labels: Vec<String> = answers
        .first()
        .map(|labels| {
            labels
                .iter()
                .map(|l| l.trim().to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default();
    if labels.is_empty() {
        return None;
    }
    if labels.iter().any(|l| l.starts_with("trust all")) {
        return Some("/trust".to_string());
    }
    if labels.iter().any(|l| l.starts_with("choose domains")) {
        // "Choose domains" needs a follow-up; without a second question we
        // send the user to the status view, which lists every domain and
        // the exact narrow-grant commands.
        return Some("/trust status".to_string());
    }
    None
}

fn domain_label(domain: TrustDomain) -> &'static str {
    match domain {
        TrustDomain::Mcp => "MCP servers (.muta/mcp.json or project MCP config)",
        TrustDomain::Skills => "Skills (.muta/skills or .agents/skills)",
        TrustDomain::Hooks => "Hooks (project hook config)",
        TrustDomain::Instructions => "Instructions (AGENTS.md / project rules)",
        TrustDomain::ExWorkspace => "External workspaces (.muta/config.toml [workspace].additional_roots)",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(
        mcp: WorkspaceTrustState,
        skills: WorkspaceTrustState,
        hooks: WorkspaceTrustState,
        instructions: WorkspaceTrustState,
    ) -> WorkspaceSecuritySnapshot {
        WorkspaceSecuritySnapshot {
            root: "/tmp/proj".to_string(),
            mcp,
            skills,
            hooks,
            instructions,
            ex_workspace: WorkspaceTrustState::Absent,
        }
    }

    #[test]
    fn absent_workspace_gates_nothing() {
        assert!(
            gate_request(&snapshot(
                WorkspaceTrustState::Absent,
                WorkspaceTrustState::Absent,
                WorkspaceTrustState::Absent,
                WorkspaceTrustState::Absent
            ))
            .is_none()
        );
    }

    #[test]
    fn trusted_and_changed_do_not_gate() {
        assert!(
            gate_request(&snapshot(
                WorkspaceTrustState::Trusted,
                WorkspaceTrustState::Trusted,
                WorkspaceTrustState::Absent,
                WorkspaceTrustState::Trusted
            ))
            .is_none()
        );
        // Changed: the user already decided once; banner territory.
        assert!(
            gate_request(&snapshot(
                WorkspaceTrustState::Changed,
                WorkspaceTrustState::Trusted,
                WorkspaceTrustState::Absent,
                WorkspaceTrustState::Absent
            ))
            .is_none()
        );
    }

    #[test]
    fn quarantined_opens_gate_listing_only_present_domains() {
        let req = gate_request(&snapshot(
            WorkspaceTrustState::Quarantined,
            WorkspaceTrustState::Quarantined,
            WorkspaceTrustState::Absent,
            WorkspaceTrustState::Quarantined,
        ))
        .expect("quarantined workspace must gate");
        assert_eq!(req.id, TRUST_GATE_REQUEST_ID);
        let q = req.questions.first().unwrap();
        assert_eq!(q.options.len(), 3);
        assert!(q.question.contains("MCP servers"));
        assert!(q.question.contains("Skills"));
        assert!(q.question.contains("Instructions"));
        assert!(!q.question.contains("Hooks"));
        assert!(!q.multi_select);
    }

    #[test]
    fn answers_map_to_commands() {
        assert_eq!(
            answer_to_command(&[vec!["Trust all domains (Recommended)".to_string()]]),
            Some("/trust".to_string())
        );
        assert_eq!(
            answer_to_command(&[vec!["Choose domains".to_string()]]),
            Some("/trust status".to_string())
        );
        assert_eq!(
            answer_to_command(&[vec!["Keep quarantined".to_string()]]),
            None
        );
        // Esc / empty reply: no mutation.
        assert_eq!(answer_to_command(&[]), None);
        assert_eq!(answer_to_command(&[Vec::new()]), None);
    }
}
