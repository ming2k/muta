//! Concrete [`OffstreamSource`] implementations for `muta-runtime` (ADR-0262).
//!
//! Provides lock-free offstream readers for:
//! - `sub:`: Subagent session transcripts and trajectories
//! - `call:`: Pruned tool execution outputs restored from CAS BlobStore / SessionIR
//! - `fold:`: Folded causal subgraphs behind compaction horizons

use std::sync::Arc;

use async_trait::async_trait;
use muta_contracts::{
    NodePayload, OffstreamEntry, OffstreamRegistry, OffstreamSource, OffstreamStatus,
    PagedOffstreamContent, Role,
};
use muta_persistence::blobs::BlobStore;
use muta_persistence::db::get_persistence_handle;

/// Pagination and text filtering helper for offstream sources.
pub fn paginate_text(
    full_text: &str,
    cursor: Option<&str>,
    query: Option<&str>,
    budget_tokens: usize,
) -> PagedOffstreamContent {
    let lines: Vec<&str> = full_text.lines().collect();
    let total_lines = lines.len();

    // 1. Filter lines if query is specified
    let filtered_lines: Vec<(usize, &str)> = if let Some(q) = query.filter(|s| !s.trim().is_empty()) {
        let q_lower = q.to_lowercase();
        lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.to_lowercase().contains(&q_lower))
            .map(|(idx, line)| (idx + 1, *line))
            .collect()
    } else {
        lines.iter().enumerate().map(|(idx, line)| (idx + 1, *line)).collect()
    };

    // 2. Resolve cursor offset
    let start_idx = cursor
        .and_then(|c| c.parse::<usize>().ok())
        .unwrap_or(0);

    let mut accumulated_tokens = 0;
    let mut collected = Vec::new();
    let mut next_cursor = None;

    for (pos, (line_num, line)) in filtered_lines.iter().enumerate().skip(start_idx) {
        let line_fmt = if query.is_some() {
            format!("L{line_num}: {line}")
        } else {
            line.to_string()
        };

        let line_tokens = muta_contracts::tokenizer::count_tokens(&line_fmt).max(1);
        if accumulated_tokens + line_tokens > budget_tokens && !collected.is_empty() {
            next_cursor = Some(pos.to_string());
            break;
        }

        accumulated_tokens += line_tokens;
        collected.push(line_fmt);
    }

    PagedOffstreamContent {
        text: collected.join("\n"),
        next_cursor,
        total_lines,
    }
}

/// Source for inspecting subagent session lineages (`sub:<session_id>`).
pub struct SubagentSource {
    session_id: String,
}

impl SubagentSource {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
        }
    }
}

#[async_trait]
impl OffstreamSource for SubagentSource {
    fn scheme(&self) -> &'static str {
        "sub"
    }

    async fn enumerate(&self, session_id: &str) -> Result<Vec<OffstreamEntry>, String> {
        let sid = if session_id.is_empty() {
            &self.session_id
        } else {
            session_id
        };

        let reader = get_persistence_handle()
            .reader()
            .map_err(|e| format!("Failed to acquire reader: {e}"))?;

        let subagents = reader
            .list_subagent_sessions(sid)
            .map_err(|e| e.to_string())?;

        let mut entries = Vec::new();
        for sub in subagents {
            let mut status = OffstreamStatus::Ready;
            let mut size_tokens = None;

            // Inspect child session termination status dynamically without modifying SubagentRef
            if let Ok(Some(child_ir)) = reader.load_session_ir(&sub.id) {
                let total_text: usize = child_ir
                    .history
                    .nodes
                    .values()
                    .map(|n| match &n.payload {
                        NodePayload::Message { message } => message.content.len(),
                        NodePayload::Compaction { summary, .. } => summary.len(),
                        _ => 0,
                    })
                    .sum();
                size_tokens = Some(total_text.div_ceil(4));

                for node in child_ir.history.nodes.values() {
                    if let NodePayload::Termination { reason, .. } = &node.payload {
                        status = match reason {
                            muta_contracts::TerminationReason::UserInterrupt => {
                                OffstreamStatus::Interrupted
                            }
                            muta_contracts::TerminationReason::FatalError { .. } => {
                                OffstreamStatus::Failed
                            }
                            _ => OffstreamStatus::Ready,
                        };
                        break;
                    }
                }
            }

            let label = sub
                .title
                .or(sub.last_user_prompt)
                .unwrap_or_else(|| "Subagent session".to_string());

            entries.push(OffstreamEntry {
                handle: format!("sub:{}", sub.id),
                label,
                status,
                size_tokens,
            });
        }

        Ok(entries)
    }

    async fn read(
        &self,
        key: &str,
        cursor: Option<&str>,
        query: Option<&str>,
        budget_tokens: usize,
    ) -> Result<PagedOffstreamContent, String> {
        let reader = get_persistence_handle()
            .reader()
            .map_err(|e| format!("Failed to acquire reader: {e}"))?;

        if let Ok(Some(ir)) = reader.load_session_ir(key) {
            let active_leaf = ir.state.active_leaf.as_deref().unwrap_or("");
            let nodes = ir.history.linear_path(active_leaf);

            let mut formatted = format!("# Subagent Session Transcript ({key})\n\n");
            for node in nodes {
                match &node.payload {
                    NodePayload::Message { message } => {
                        let role_label = match message.role {
                            Role::User => "User",
                            Role::Assistant => "Assistant",
                            Role::Tool => "Tool Result",
                            Role::System => "System",
                        };
                        formatted.push_str(&format!("### [{role_label}]\n{}\n\n", message.content));
                        if let Some(calls) = &message.tool_calls {
                            for c in calls {
                                formatted.push_str(&format!("-> Tool Call: {}({})\n\n", c.name, c.arguments));
                            }
                        }
                    }
                    NodePayload::Compaction { summary, .. } => {
                        formatted.push_str(&format!("### [Compacted Checkpoint]\n{summary}\n\n"));
                    }
                    NodePayload::Termination { reason, partial_output, .. } => {
                        formatted.push_str(&format!(
                            "### [Execution Terminated: {:?}]\n{}\n\n",
                            reason,
                            partial_output.as_deref().unwrap_or("(no partial output)")
                        ));
                    }
                    _ => {}
                }
            }
            return Ok(paginate_text(&formatted, cursor, query, budget_tokens));
        }

        Err(format!("Subagent session '{key}' not found"))
    }
}

/// Source for inspecting pruned tool outputs (`call:<tool_call_id>`).
pub struct PrunedToolSource {
    session_id: String,
    blob_store: BlobStore,
}

impl PrunedToolSource {
    pub fn new(session_id: impl Into<String>, blob_store: BlobStore) -> Self {
        Self {
            session_id: session_id.into(),
            blob_store,
        }
    }
}

#[async_trait]
impl OffstreamSource for PrunedToolSource {
    fn scheme(&self) -> &'static str {
        "call"
    }

    async fn enumerate(&self, session_id: &str) -> Result<Vec<OffstreamEntry>, String> {
        let sid = if session_id.is_empty() {
            &self.session_id
        } else {
            session_id
        };

        let reader = get_persistence_handle()
            .reader()
            .map_err(|e| format!("Failed to acquire reader: {e}"))?;

        let mut entries = Vec::new();
        let mut seen_calls = std::collections::HashSet::new();

        // 1. Scan transcript directives and entries
        if let Ok(Some(data)) = reader.load_session_full(sid) {
            for dir in &data.transcript().directives {
                if let muta_contracts::DirectivePayload::Prune { elided } = &dir.payload {
                    for item in elided {
                        if seen_calls.insert(item.tool_call_id.clone()) {
                            entries.push(OffstreamEntry {
                                handle: format!("call:{}", item.tool_call_id),
                                label: item.placeholder.chars().take(80).collect(),
                                status: OffstreamStatus::Pruned,
                                size_tokens: None,
                            });
                        }
                    }
                }
            }

            for entry in &data.transcript().entries {
                if let muta_contracts::EntryPayload::Message(payload) = &entry.payload
                    && let Some(call_id) = &payload.tool_call_id
                {
                    let content = entry.content.as_deref().unwrap_or("");
                    if (content.starts_with(muta_contracts::pressure::CLEARED_TOOL_PREFIX)
                        || payload.content_blob.is_some())
                        && seen_calls.insert(call_id.clone())
                    {
                        entries.push(OffstreamEntry {
                            handle: format!("call:{call_id}"),
                            label: content.chars().take(80).collect(),
                            status: OffstreamStatus::Pruned,
                            size_tokens: None,
                        });
                    }
                }
            }
        }

        // 2. Scan SessionIR nodes
        if let Ok(Some(ir)) = reader.load_session_ir(sid) {
            for node in ir.history.nodes.values() {
                if let NodePayload::Message { message } = &node.payload
                    && message.role == Role::Tool
                    && let Some(call_id) = &message.tool_call_id
                {
                    let content = &message.content;
                    if (content.starts_with(muta_contracts::pressure::CLEARED_TOOL_PREFIX)
                        || message.content_blob.is_some())
                        && seen_calls.insert(call_id.clone())
                    {
                        entries.push(OffstreamEntry {
                            handle: format!("call:{call_id}"),
                            label: content.chars().take(80).collect(),
                            status: OffstreamStatus::Pruned,
                            size_tokens: None,
                        });
                    }
                }
            }
        }

        Ok(entries)
    }

    async fn read(
        &self,
        key: &str,
        cursor: Option<&str>,
        query: Option<&str>,
        budget_tokens: usize,
    ) -> Result<PagedOffstreamContent, String> {
        let reader = get_persistence_handle()
            .reader()
            .map_err(|e| format!("Failed to acquire reader: {e}"))?;

        // 1. Try transcript entries: entry.content in transcript.entries has the
        // unpruned, original verbatim text (Prune directive only affects projected views)
        if let Ok(Some(data)) = reader.load_session_full(&self.session_id) {
            for entry in &data.transcript().entries {
                if let muta_contracts::EntryPayload::Message(payload) = &entry.payload
                    && payload.tool_call_id.as_deref() == Some(key)
                {
                    if let Some(blob_hash) = &payload.content_blob
                        && let Some(bytes) = self.blob_store.get(blob_hash)
                        && let Ok(text) = String::from_utf8(bytes)
                    {
                        return Ok(paginate_text(&text, cursor, query, budget_tokens));
                    }
                    if let Some(content) = &entry.content {
                        if !content.starts_with(muta_contracts::pressure::CLEARED_TOOL_PREFIX) {
                            return Ok(paginate_text(content, cursor, query, budget_tokens));
                        }
                    }
                }
            }
        }

        // 2. Try SessionIR nodes
        if let Ok(Some(ir)) = reader.load_session_ir(&self.session_id) {
            for node in ir.history.nodes.values() {
                if let NodePayload::Message { message } = &node.payload
                    && message.tool_call_id.as_deref() == Some(key)
                {
                    if let Some(blob_hash) = &message.content_blob
                        && let Some(bytes) = self.blob_store.get(blob_hash)
                        && let Ok(text) = String::from_utf8(bytes)
                    {
                        return Ok(paginate_text(&text, cursor, query, budget_tokens));
                    }
                    if !message.content.starts_with(muta_contracts::pressure::CLEARED_TOOL_PREFIX) {
                        return Ok(paginate_text(&message.content, cursor, query, budget_tokens));
                    }
                }
            }
        }

        Err(format!("Original unpruned tool output for tool_call_id '{key}' not found"))
    }
}

/// Source for inspecting folded causal subgraphs (`fold:<compaction_node_id>`).
pub struct FoldedCausalSource {
    session_id: String,
}

impl FoldedCausalSource {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
        }
    }
}

#[async_trait]
impl OffstreamSource for FoldedCausalSource {
    fn scheme(&self) -> &'static str {
        "fold"
    }

    async fn enumerate(&self, session_id: &str) -> Result<Vec<OffstreamEntry>, String> {
        let sid = if session_id.is_empty() {
            &self.session_id
        } else {
            session_id
        };

        let reader = get_persistence_handle()
            .reader()
            .map_err(|e| format!("Failed to acquire reader: {e}"))?;

        let ir = match reader.load_session_ir(sid) {
            Ok(Some(ir)) => ir,
            _ => return Ok(Vec::new()),
        };

        let mut entries = Vec::new();
        for (node_id, node) in &ir.history.nodes {
            if let NodePayload::Compaction {
                summary,
                tokens_before,
                ..
            } = &node.payload
            {
                let first_line = summary.lines().next().unwrap_or("Compaction checkpoint");
                entries.push(OffstreamEntry {
                    handle: format!("fold:{node_id}"),
                    label: first_line.chars().take(80).collect(),
                    status: OffstreamStatus::Compacted,
                    size_tokens: Some(*tokens_before),
                });
            }
        }

        Ok(entries)
    }

    async fn read(
        &self,
        key: &str,
        cursor: Option<&str>,
        query: Option<&str>,
        budget_tokens: usize,
    ) -> Result<PagedOffstreamContent, String> {
        let reader = get_persistence_handle()
            .reader()
            .map_err(|e| format!("Failed to acquire reader: {e}"))?;

        let ir = reader
            .load_session_ir(&self.session_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("Session '{}' not found", self.session_id))?;

        let target_node = ir
            .history
            .nodes
            .get(key)
            .ok_or_else(|| format!("Compaction node '{key}' not found"))?;

        let mut formatted = format!("# Folded Causal Checkpoint ({key})\n\n");
        if let NodePayload::Compaction {
            summary,
            tokens_before,
            read_files,
            modified_files,
            ..
        } = &target_node.payload
        {
            formatted.push_str(&format!(
                "Tokens before compaction: ~{tokens_before}\n\n## Summary\n{summary}\n\n"
            ));
            if !modified_files.is_empty() {
                formatted.push_str(&format!("Modified Files: {}\n", modified_files.join(", ")));
            }
            if !read_files.is_empty() {
                formatted.push_str(&format!("Consulted Files: {}\n\n", read_files.join(", ")));
            }
        }

        // Trace upstream parent nodes that were folded into this compaction
        let mut curr_parent = target_node.parent_id.as_deref();
        let mut folded_nodes = Vec::new();
        while let Some(pid) = curr_parent {
            if let Some(pnode) = ir.history.nodes.get(pid) {
                if matches!(pnode.payload, NodePayload::Compaction { .. }) {
                    break; // stop at previous compaction boundary
                }
                folded_nodes.push(pnode);
                curr_parent = pnode.parent_id.as_deref();
            } else {
                break;
            }
        }
        folded_nodes.reverse();

        if !folded_nodes.is_empty() {
            formatted.push_str("## Folded Dialogue History\n\n");
            for node in folded_nodes {
                if let NodePayload::Message { message } = &node.payload {
                    let role_str = match message.role {
                        Role::User => "User",
                        Role::Assistant => "Assistant",
                        Role::Tool => "Tool Result",
                        Role::System => "System",
                    };
                    formatted.push_str(&format!("### [{role_str}]\n{}\n\n", message.content));
                }
            }
        }

        Ok(paginate_text(&formatted, cursor, query, budget_tokens))
    }
}

/// Helper to build an [`OffstreamRegistry`] for a session.
pub fn build_offstream_registry(session_id: &str, blob_store: BlobStore) -> Arc<OffstreamRegistry> {
    let mut registry = OffstreamRegistry::empty();
    registry.register(Arc::new(SubagentSource::new(session_id)));
    registry.register(Arc::new(PrunedToolSource::new(session_id, blob_store)));
    registry.register(Arc::new(FoldedCausalSource::new(session_id)));
    Arc::new(registry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_paginate_text_filtering_and_cursor() {
        let text = "Line 1: setup\nLine 2: error in test\nLine 3: clean\nLine 4: error in build\nLine 5: done";

        // Query filter: only matching lines
        let res = paginate_text(text, None, Some("error"), 1000);
        assert_eq!(res.total_lines, 5);
        assert!(res.text.contains("L2: Line 2: error in test"));
        assert!(res.text.contains("L4: Line 4: error in build"));
        assert!(!res.text.contains("Line 1: setup"));
        assert!(res.next_cursor.is_none());

        // Low budget triggering cursor pagination
        let paged1 = paginate_text(text, None, None, 4);
        assert!(paged1.next_cursor.is_some());
        let next = paged1.next_cursor.as_deref().unwrap();

        let paged2 = paginate_text(text, Some(next), None, 1000);
        assert!(!paged2.text.is_empty());
    }

    #[tokio::test]
    async fn test_pruned_tool_source_reads_from_blob_store() {
        let tmp = tempfile::tempdir().unwrap();
        let blob_store = BlobStore::new(tmp.path().to_path_buf());
        let unpruned_text = "fn main() {\n    println!(\"raw unpruned output\");\n}";
        let hash = blob_store.put(unpruned_text.as_bytes()).unwrap();

        let bytes = blob_store.get(&hash).unwrap();
        let retrieved = String::from_utf8(bytes).unwrap();
        assert_eq!(retrieved, unpruned_text);
    }
}

