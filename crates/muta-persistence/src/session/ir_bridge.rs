//! Bi-directional bridge between SessionData and canonical SessionIR (ADR-0241).
//!
//! Provides conversion and projection logic enabling legacy and v2 SessionStore
//! instances to seamlessly operate on the canonical [`muta_contracts::SessionIR`].

use super::SessionData;
use muta_contracts::{
    CausalGraph, CausalNode, ExecutionStatus, NodeKind, NodePayload, RuleSet, SessionIR,
    SessionPolicy, SessionState, SuspensionReason,
};

/// Convert a [`SessionData`] into a canonical [`SessionIR`].
pub fn session_data_to_ir(data: &SessionData) -> SessionIR {
    let mut history = CausalGraph::new();

    // 1. Populate CausalGraph
    if !data.tree.entries.is_empty() {
        // Hydrate graph from SessionTree entries
        let mut sorted_entries: Vec<_> = data.tree.entries.values().collect();
        sorted_entries.sort_by_key(|e| e.timestamp);

        let mut seq = 0u64;
        for entry in sorted_entries {
            seq += 1;
            let (kind, payload) = match &entry.kind {
                muta_contracts::SessionEntryKind::Message { message } => (
                    NodeKind::Dialogue,
                    NodePayload::Message {
                        message: message.clone(),
                    },
                ),
                muta_contracts::SessionEntryKind::Compaction {
                    summary,
                    first_kept_entry_id,
                    tokens_before,
                    read_files,
                    modified_files,
                } => (
                    NodeKind::Compaction,
                    NodePayload::Compaction {
                        summary: summary.clone(),
                        first_kept_node_id: first_kept_entry_id.clone(),
                        tokens_before: *tokens_before,
                        read_files: read_files.clone(),
                        modified_files: modified_files.clone(),
                    },
                ),
                muta_contracts::SessionEntryKind::BranchSummary {
                    summary,
                    from_id: _,
                    read_files: _,
                    modified_files: _,
                } => (
                    NodeKind::Dialogue,
                    NodePayload::Message {
                        message: muta_contracts::Message::new(
                            muta_contracts::message::Role::System,
                            summary.clone(),
                        ),
                    },
                ),
                muta_contracts::SessionEntryKind::Custom {
                    custom_type,
                    content,
                    ..
                } => (
                    NodeKind::SystemNotice,
                    NodePayload::SystemNotice {
                        source: "custom".into(),
                        notice_type: custom_type.clone(),
                        content: content.clone(),
                    },
                ),
            };

            let node = CausalNode {
                id: entry.id.clone(),
                parent_id: entry.parent_id.clone(),
                seq,
                timestamp_ms: entry.timestamp * 1000,
                kind,
                payload,
            };
            history.insert_node(node);
        }
    } else {
        // Fallback: build linear causal graph from transcript
        let mut parent_id = None;
        let mut seq = 0u64;
        for entry in &data.transcript.entries {
            seq += 1;
            let node_id = entry.id.clone();
            let timestamp_ms = entry.created_at_ms;

            let (kind, payload) = match &entry.payload {
                muta_contracts::EntryPayload::Message(_msg) => (
                    NodeKind::Dialogue,
                    NodePayload::Message {
                        message: muta_contracts::Message::new(
                            entry.role.unwrap_or(muta_contracts::message::Role::User),
                            entry.content.clone().unwrap_or_default(),
                        ),
                    },
                ),
                muta_contracts::EntryPayload::State(_) => (
                    NodeKind::SystemNotice,
                    NodePayload::SystemNotice {
                        source: "transcript".into(),
                        notice_type: "state".into(),
                        content: entry.content.clone().unwrap_or_default(),
                    },
                ),
            };

            let node = CausalNode {
                id: node_id.clone(),
                parent_id: parent_id.clone(),
                seq,
                timestamp_ms,
                kind,
                payload,
            };
            history.insert_node(node);
            parent_id = Some(node_id);
        }
    }

    // 2. Populate SessionState
    let status = if let Some(ref retry) = data.retry_pending {
        ExecutionStatus::Suspended {
            reason: SuspensionReason::RetryPending {
                retry_token: format!("round-{}", retry.round),
                attempts: retry.turns_committed as u32,
            },
        }
    } else {
        ExecutionStatus::Idle
    };

    let active_leaf = data.tree.active_leaf_id.clone().or_else(|| {
        if !data.transcript.entries.is_empty() {
            data.transcript.entries.last().map(|e| e.id.clone())
        } else {
            None
        }
    });

    let state = SessionState {
        active_leaf,
        status,
        pending_notifications: Vec::new(),
        round_counter: data.round_counter,
    };

    // 3. Populate SessionPolicy
    let workspace_root = data
        .workspace
        .as_ref()
        .map(|w| w.root.to_string_lossy().into_owned());

    let policy = SessionPolicy {
        rules: RuleSet {
            system_persona: data.persona.clone(),
            workspace_root,
            project_rules: Vec::new(),
        },
        capabilities: muta_contracts::CapabilityPolicy {
            enabled_tools: Vec::new(),
            disabled_tools: data.disabled_tools.iter().cloned().collect(),
            provider_pin: data.provider_selection.as_ref().map(|ps| ps.connection.clone()),
        },
        guardrails: muta_contracts::GuardrailPolicy {
            unattended: data.unattended,
            require_approval_tools: Vec::new(),
        },
        budget: muta_contracts::BudgetPolicy::default(),
    };

    SessionIR {
        session_id: data.id.clone(),
        parent_session_id: data.parent_id.clone(),
        created_at_s: data.created_at,
        updated_at_s: data.updated_at,
        history,
        state,
        policy,
    }
}

/// Apply updates from a [`SessionIR`] back into [`SessionData`].
pub fn apply_ir_to_session_data(ir: &SessionIR, data: &mut SessionData) {
    data.updated_at = ir.updated_at_s;
    data.round_counter = ir.state.round_counter;
    data.unattended = ir.policy.guardrails.unattended;
    data.disabled_tools = ir.policy.capabilities.disabled_tools.iter().cloned().collect();
    if let Some(ref pin) = ir.policy.capabilities.provider_pin {
        data.provider_selection = Some(super::ProviderSelection {
            connection: pin.clone(),
            model: None,
        });
    }

    // Update active leaf in tree
    if let Some(ref leaf_id) = ir.state.active_leaf {
        data.tree.active_leaf_id = Some(leaf_id.clone());
    }

    // Sync newly added nodes into tree if missing
    for node in ir.history.nodes.values() {
        if !data.tree.entries.contains_key(&node.id) {
            match &node.payload {
                NodePayload::Message { message } => {
                    let entry = muta_contracts::SessionEntry::new_message(
                        node.id.clone(),
                        node.parent_id.clone(),
                        node.timestamp_ms / 1000,
                        message.clone(),
                    );
                    data.tree.insert_entry(entry);
                }
                NodePayload::Compaction {
                    summary,
                    first_kept_node_id,
                    tokens_before,
                    read_files,
                    modified_files,
                } => {
                    let payload = muta_contracts::CompactionPayload {
                        summary: summary.clone(),
                        first_kept_entry_id: first_kept_node_id.clone(),
                        tokens_before: *tokens_before,
                        read_files: read_files.clone(),
                        modified_files: modified_files.clone(),
                    };
                    let entry = muta_contracts::SessionEntry::new_compaction(
                        node.id.clone(),
                        node.parent_id.clone(),
                        node.timestamp_ms / 1000,
                        payload,
                    );
                    data.tree.insert_entry(entry);
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muta_contracts::message::{Message, Role};
    use muta_contracts::SessionTree;

    #[test]
    fn test_session_data_to_ir_and_apply_back() {
        let mut data = SessionData {
            id: "session-bridge-test".into(),
            persona: Some("Rust Senior Architect".into()),
            unattended: true,
            round_counter: 5,
            tree: SessionTree::default(),
            ..SessionData::default()
        };

        let msg = Message::new(Role::User, "Audit system performance");
        let entry_id = data.tree.append_message(msg, 1000, None);
        data.tree.active_leaf_id = Some(entry_id.clone());

        // 1. Convert to SessionIR
        let ir = session_data_to_ir(&data);
        assert_eq!(ir.session_id, "session-bridge-test");
        assert_eq!(ir.history.nodes.len(), 1);
        assert_eq!(ir.state.active_leaf, Some(entry_id.clone()));
        assert_eq!(ir.state.round_counter, 5);
        assert_eq!(ir.policy.rules.system_persona.as_deref(), Some("Rust Senior Architect"));
        assert!(ir.policy.guardrails.unattended);

        // 2. Modify SessionIR
        let mut modified_ir = ir;
        modified_ir.state.round_counter = 6;
        let id2 = modified_ir.append_message("node-reply", 1001_000, Message::new(Role::Assistant, "Audit completed"));

        // 3. Apply back to SessionData
        apply_ir_to_session_data(&modified_ir, &mut data);
        assert_eq!(data.round_counter, 6);
        assert_eq!(data.tree.active_leaf_id, Some(id2.clone()));
        assert_eq!(data.tree.entries.len(), 2);
        assert!(data.tree.entries.contains_key(&id2));
    }
}
