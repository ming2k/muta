//! Core types of the Session Intermediate Representation (Session IR).
//!
//! Defined by ADR-0241, Session IR is organized into:
//! - [`CausalGraph`]: immutable, append-only historical facts (`history`)
//! - [`SessionState`]: mutable cursor, status machine, and event mailbox (`state`)
//! - [`SessionPolicy`]: declarative governance rules, capabilities, and budgets (`policy`)

use crate::message::Message;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Unique identifier for a node in the causal fact graph.
pub type NodeId = String;

/// The canonical, in-memory Session Intermediate Representation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionIR {
    /// Unique identifier of the session.
    pub session_id: String,
    /// Parent session identifier if this session was forked or spawned as a subagent.
    pub parent_session_id: Option<String>,
    /// Creation timestamp in epoch seconds.
    pub created_at_s: u64,
    /// Last update timestamp in epoch seconds.
    pub updated_at_s: u64,
    /// 1. Immutable Causal Fact Graph (what happened).
    pub history: CausalGraph,
    /// 2. Mutable Working State & Cursor Registers (where we are).
    pub state: SessionState,
    /// 3. Declarative Policy & Constraints (rules and limits).
    pub policy: SessionPolicy,
}

impl SessionIR {
    /// Initialize a fresh Session IR with the given policy and initial root state.
    pub fn new(session_id: impl Into<String>, policy: SessionPolicy, timestamp_s: u64) -> Self {
        Self {
            session_id: session_id.into(),
            parent_session_id: None,
            created_at_s: timestamp_s,
            updated_at_s: timestamp_s,
            history: CausalGraph::new(),
            state: SessionState::new(),
            policy,
        }
    }

    /// Append a dialogue message node to the active branch.
    pub fn append_message(&mut self, node_id: impl Into<String>, timestamp_ms: u64, message: Message) -> NodeId {
        let node_id = node_id.into();
        let parent_id = self.state.active_leaf.clone();
        let seq = self.history.next_seq();

        let node = CausalNode {
            id: node_id.clone(),
            parent_id,
            seq,
            timestamp_ms,
            kind: NodeKind::Dialogue,
            payload: NodePayload::Message { message },
        };

        self.history.insert_node(node);
        self.state.active_leaf = Some(node_id.clone());
        self.updated_at_s = timestamp_ms / 1000;
        node_id
    }

    /// Record an execution termination event (e.g. human interrupt, fatal fault).
    pub fn record_termination(
        &mut self,
        node_id: impl Into<String>,
        timestamp_ms: u64,
        reason: TerminationReason,
        partial_output: Option<String>,
        interrupted_tool_call_id: Option<String>,
        duration_ms: Option<u64>,
    ) -> NodeId {
        let node_id = node_id.into();
        let parent_id = self.state.active_leaf.clone();
        let seq = self.history.next_seq();

        let node = CausalNode {
            id: node_id.clone(),
            parent_id,
            seq,
            timestamp_ms,
            kind: NodeKind::Termination,
            payload: NodePayload::Termination {
                reason,
                partial_output,
                interrupted_tool_call_id,
                duration_ms,
            },
        };

        self.history.insert_node(node);
        self.state.active_leaf = Some(node_id.clone());
        self.state.status = ExecutionStatus::Idle;
        self.updated_at_s = timestamp_ms / 1000;
        node_id
    }

    /// Insert an asynchronous system notice into the fact graph.
    pub fn append_system_notice(
        &mut self,
        node_id: impl Into<String>,
        timestamp_ms: u64,
        source: String,
        notice_type: String,
        content: String,
    ) -> NodeId {
        let node_id = node_id.into();
        let parent_id = self.state.active_leaf.clone();
        let seq = self.history.next_seq();

        let node = CausalNode {
            id: node_id.clone(),
            parent_id,
            seq,
            timestamp_ms,
            kind: NodeKind::SystemNotice,
            payload: NodePayload::SystemNotice {
                source,
                notice_type,
                content,
            },
        };

        self.history.insert_node(node);
        self.state.active_leaf = Some(node_id.clone());
        self.updated_at_s = timestamp_ms / 1000;
        node_id
    }

    /// Resolve the active linear branch (Pass 1 of compiler lowering).
    pub fn resolve_active_branch(&self) -> Vec<&CausalNode> {
        match &self.state.active_leaf {
            Some(leaf_id) => self.history.linear_path(leaf_id),
            None => Vec::new(),
        }
    }

    /// Extract messages along the active branch for consumption.
    pub fn resolve_active_messages(&self) -> Vec<Message> {
        self.resolve_active_branch()
            .into_iter()
            .filter_map(|node| match &node.payload {
                NodePayload::Message { message } => Some(message.clone()),
                _ => None,
            })
            .collect()
    }
}

/// An immutable, append-only causal directed acyclic graph of conversation events.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CausalGraph {
    /// Node map keyed by unique NodeId.
    pub nodes: HashMap<NodeId, CausalNode>,
    /// Root node ID of the graph.
    pub root_id: Option<NodeId>,
    /// Highest assigned sequence watermark.
    pub max_seq: u64,
}

impl CausalGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocate the next monotonically increasing sequence number.
    pub fn next_seq(&mut self) -> u64 {
        self.max_seq += 1;
        self.max_seq
    }

    /// Insert a node into the graph.
    pub fn insert_node(&mut self, node: CausalNode) {
        if node.parent_id.is_none() && self.root_id.is_none() {
            self.root_id = Some(node.id.clone());
        }
        if node.seq > self.max_seq {
            self.max_seq = node.seq;
        }
        self.nodes.insert(node.id.clone(), node);
    }

    /// Look up a node by its identifier.
    pub fn get_node(&self, id: &str) -> Option<&CausalNode> {
        self.nodes.get(id)
    }

    /// Find all immediate children of a parent node.
    pub fn children_of(&self, parent_id: &str) -> Vec<&CausalNode> {
        let mut children: Vec<&CausalNode> = self
            .nodes
            .values()
            .filter(|n| n.parent_id.as_deref() == Some(parent_id))
            .collect();
        children.sort_by_key(|n| n.seq);
        children
    }

    /// Retrieve all leaf nodes (nodes with no children).
    pub fn leaves(&self) -> Vec<NodeId> {
        let mut parent_set = std::collections::HashSet::new();
        for node in self.nodes.values() {
            if let Some(ref p) = node.parent_id {
                parent_set.insert(p.as_str());
            }
        }
        self.nodes
            .keys()
            .filter(|k| !parent_set.contains(k.as_str()))
            .cloned()
            .collect()
    }

    /// Trace the linear lineage from a target leaf backward to root or compaction anchor.
    /// Returns the nodes ordered chronologically (root/anchor -> leaf).
    pub fn linear_path(&self, leaf_id: &str) -> Vec<&CausalNode> {
        let mut path = Vec::new();
        let mut current_id = Some(leaf_id);

        while let Some(id) = current_id {
            if let Some(node) = self.nodes.get(id) {
                let is_compaction = matches!(node.kind, NodeKind::Compaction);
                path.push(node);
                if is_compaction {
                    // Compaction acts as a causal horizon; ancestor nodes prior to
                    // compaction anchor are elided from the active linear view.
                    break;
                }
                current_id = node.parent_id.as_deref();
            } else {
                break;
            }
        }

        path.reverse();
        path
    }
}

/// A discrete immutable fact node in the causal graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CausalNode {
    pub id: NodeId,
    pub parent_id: Option<NodeId>,
    pub seq: u64,
    pub timestamp_ms: u64,
    pub kind: NodeKind,
    pub payload: NodePayload,
}

/// Classification of causal graph nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// Dialogue turns: User, Assistant, or Tool result messages.
    Dialogue,
    /// Compaction horizon replacing historical turns up to a checkpoint.
    Compaction,
    /// Execution termination: human interrupt, timeout, or fatal fault.
    Termination,
    /// Asynchronous system notification / background wake.
    SystemNotice,
}

/// Detailed payload of a causal graph node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NodePayload {
    Message {
        message: Message,
    },
    Compaction {
        summary: String,
        first_kept_node_id: String,
        tokens_before: usize,
        #[serde(default)]
        read_files: Vec<String>,
        #[serde(default)]
        modified_files: Vec<String>,
    },
    Termination {
        reason: TerminationReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        partial_output: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        interrupted_tool_call_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
    },
    SystemNotice {
        source: String,
        notice_type: String,
        content: String,
    },
}

/// Causality classifier for execution terminations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TerminationReason {
    /// User intentionally stopped execution (Ctrl+C, cancel verb).
    UserInterrupt,
    /// Wall-clock or per-step timeout exceeded.
    Timeout,
    /// Fatal provider, network, or sandbox failure.
    FatalError { error: String },
    /// Superseded by a newer user turn before completion.
    Superseded,
}

/// Mutable working memory, cursor registers, and execution status.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionState {
    /// Current active leaf cursor in the causal graph.
    pub active_leaf: Option<NodeId>,
    /// Current execution state machine position.
    pub status: ExecutionStatus,
    /// Queue of asynchronous notifications received during sleep/suspension.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_notifications: Vec<SystemNoticePayload>,
    /// Turn counter within the current session.
    pub round_counter: u64,
}

impl SessionState {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Dynamic execution status of the session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ExecutionStatus {
    #[default]
    Idle,
    Running {
        turn: u64,
        started_at_ms: u64,
    },
    Suspended {
        reason: SuspensionReason,
    },
}

/// Specific cause of an execution suspension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SuspensionReason {
    /// Waiting for human authorization to execute a restricted tool.
    NeedsApproval {
        tool_call_id: String,
        action: String,
    },
    /// Waiting for required user clarification or input.
    NeedsInput {
        prompt: String,
    },
    /// Waiting for automated transient retry with backoff.
    RetryPending {
        retry_token: String,
        attempts: u32,
    },
}

/// Payload for pending notifications held in the state mailbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemNoticePayload {
    pub source: String,
    pub notice_type: String,
    pub content: String,
    pub timestamp_ms: u64,
}

/// Declarative governance policy, capabilities, and resource boundaries.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionPolicy {
    /// Cognitive rules, persona, and workspace conventions.
    pub rules: RuleSet,
    /// Capabilities granted to the session.
    pub capabilities: CapabilityPolicy,
    /// Safety boundaries and approval guardrails.
    pub guardrails: GuardrailPolicy,
    /// Resource consumption budgets and compaction triggers.
    pub budget: BudgetPolicy,
}

/// Cognitive rules and prompt instructions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSet {
    /// System baseline identity and core behavioral policies.
    pub system_persona: Option<String>,
    /// Workspace root directory path.
    pub workspace_root: Option<String>,
    /// Project rules read from repository governance (e.g. AGENTS.md).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub project_rules: Vec<String>,
}

/// Capabilities and tools available to the model.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityPolicy {
    /// Whitelisted tool names permitted in this session.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enabled_tools: Vec<String>,
    /// Explicitly disabled tools.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_tools: Vec<String>,
    /// Pinned model provider connection name.
    pub provider_pin: Option<String>,
}

/// Safety guardrails and approval requirements.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuardrailPolicy {
    /// Whether the session is in unattended mode (auto-approves within policy).
    pub unattended: bool,
    /// Tool names that strictly require explicit human approval.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub require_approval_tools: Vec<String>,
}

/// Context window budgets and compaction triggers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetPolicy {
    /// Maximum model context window in tokens.
    pub max_context_tokens: usize,
    /// Token threshold triggering background compaction.
    pub compaction_trigger_tokens: usize,
    /// Maximum tokens reserved for tool output results.
    pub max_tool_output_tokens: usize,
}

impl Default for BudgetPolicy {
    fn default() -> Self {
        Self {
            max_context_tokens: 128_000,
            compaction_trigger_tokens: 96_000,
            max_tool_output_tokens: 8_000,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Role;

    #[test]
    fn test_session_ir_initialization_and_append() {
        let mut ir = SessionIR::new("session-1", SessionPolicy::default(), 1000);
        assert_eq!(ir.session_id, "session-1");
        assert!(ir.state.active_leaf.is_none());

        let msg1 = Message::new(Role::User, "Hello muta");
        let id1 = ir.append_message("node-1", 1000_000, msg1);

        assert_eq!(ir.state.active_leaf, Some(id1.clone()));
        assert_eq!(ir.history.nodes.len(), 1);
        assert_eq!(ir.history.max_seq, 1);

        let msg2 = Message::new(Role::Assistant, "Hello! How can I help?");
        let id2 = ir.append_message("node-2", 1001_000, msg2);

        assert_eq!(ir.state.active_leaf, Some(id2.clone()));
        assert_eq!(ir.history.nodes.len(), 2);
        assert_eq!(ir.history.max_seq, 2);

        let path = ir.resolve_active_branch();
        assert_eq!(path.len(), 2);
        assert_eq!(path[0].id, id1);
        assert_eq!(path[1].id, id2);
    }

    #[test]
    fn test_session_ir_forensic_interrupt() {
        let mut ir = SessionIR::new("session-2", SessionPolicy::default(), 1000);
        ir.state.status = ExecutionStatus::Running {
            turn: 1,
            started_at_ms: 1000_000,
        };

        let user_msg = Message::new(Role::User, "Run long test");
        ir.append_message("node-1", 1000_000, user_msg);

        // User hits Ctrl+C
        let term_id = ir.record_termination(
            "node-term",
            1005_000,
            TerminationReason::UserInterrupt,
            Some("Compiling...".to_string()),
            Some("call_123".to_string()),
            Some(5000),
        );

        assert_eq!(ir.state.status, ExecutionStatus::Idle);
        assert_eq!(ir.state.active_leaf, Some(term_id.clone()));

        let term_node = ir.history.get_node(&term_id).unwrap();
        assert_eq!(term_node.kind, NodeKind::Termination);
        if let NodePayload::Termination {
            reason,
            partial_output,
            interrupted_tool_call_id,
            duration_ms,
        } = &term_node.payload
        {
            assert_eq!(*reason, TerminationReason::UserInterrupt);
            assert_eq!(partial_output.as_deref(), Some("Compiling..."));
            assert_eq!(interrupted_tool_call_id.as_deref(), Some("call_123"));
            assert_eq!(*duration_ms, Some(5000));
        } else {
            panic!("expected termination payload");
        }
    }

    #[test]
    fn test_causal_graph_branching_and_leaves() {
        let mut graph = CausalGraph::new();

        let n1 = CausalNode {
            id: "root".into(),
            parent_id: None,
            seq: 1,
            timestamp_ms: 100,
            kind: NodeKind::Dialogue,
            payload: NodePayload::Message {
                message: Message::new(Role::User, "Root"),
            },
        };
        let n2a = CausalNode {
            id: "branch-a".into(),
            parent_id: Some("root".into()),
            seq: 2,
            timestamp_ms: 200,
            kind: NodeKind::Dialogue,
            payload: NodePayload::Message {
                message: Message::new(Role::Assistant, "Path A"),
            },
        };
        let n2b = CausalNode {
            id: "branch-b".into(),
            parent_id: Some("root".into()),
            seq: 3,
            timestamp_ms: 300,
            kind: NodeKind::Dialogue,
            payload: NodePayload::Message {
                message: Message::new(Role::Assistant, "Path B"),
            },
        };

        graph.insert_node(n1);
        graph.insert_node(n2a);
        graph.insert_node(n2b);

        let children = graph.children_of("root");
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].id, "branch-a");
        assert_eq!(children[1].id, "branch-b");

        let mut leaves = graph.leaves();
        leaves.sort();
        assert_eq!(leaves, vec!["branch-a", "branch-b"]);

        let path_b = graph.linear_path("branch-b");
        assert_eq!(path_b.len(), 2);
        assert_eq!(path_b[0].id, "root");
        assert_eq!(path_b[1].id, "branch-b");
    }

    #[test]
    fn test_compaction_linear_horizon() {
        let mut graph = CausalGraph::new();

        let n1 = CausalNode {
            id: "n1".into(),
            parent_id: None,
            seq: 1,
            timestamp_ms: 100,
            kind: NodeKind::Dialogue,
            payload: NodePayload::Message {
                message: Message::new(Role::User, "Ancient turn"),
            },
        };
        let n2 = CausalNode {
            id: "compaction-checkpoint".into(),
            parent_id: Some("n1".into()),
            seq: 2,
            timestamp_ms: 200,
            kind: NodeKind::Compaction,
            payload: NodePayload::Compaction {
                summary: "Summary of ancient turns".into(),
                first_kept_node_id: "n1".into(),
                tokens_before: 5000,
                read_files: vec![],
                modified_files: vec![],
            },
        };
        let n3 = CausalNode {
            id: "n3".into(),
            parent_id: Some("compaction-checkpoint".into()),
            seq: 3,
            timestamp_ms: 300,
            kind: NodeKind::Dialogue,
            payload: NodePayload::Message {
                message: Message::new(Role::User, "Recent turn"),
            },
        };

        graph.insert_node(n1);
        graph.insert_node(n2);
        graph.insert_node(n3);

        let path = graph.linear_path("n3");
        assert_eq!(path.len(), 2);
        assert_eq!(path[0].id, "compaction-checkpoint");
        assert_eq!(path[1].id, "n3");
    }
}
