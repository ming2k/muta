use async_trait::async_trait;
use muta_contracts::{Tool, ToolOutput};
use serde_json::json;

/// Recall dialogue memories shared between the user and the role.
///
/// Features cognitive retrieval with Ebbinghaus forgetting curve scoring,
/// working memory prioritization, and retrieval-practice reinforcement.
pub struct RecallMemoryTool;

#[async_trait]
impl Tool for RecallMemoryTool {
    fn name(&self) -> &str {
        "recall_memory"
    }

    fn description(&self) -> &str {
        "Recall past conversations, dialogues, and philosophical insights previously shared \
         between the user and this role. Use this to remember previous discussions, established \
         positions, user perspectives, or topics explored in earlier sessions."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The topic, philosophical concept, argument, question, or keyword to recall from past dialogues with the user."
                },
                "role": {
                    "type": "string",
                    "description": "The specific role boundary to search memories for (defaults to 'philosophist').",
                    "default": "philosophist"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of past dialogue memories to retrieve (default 5, max 10).",
                    "default": 5
                }
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }

    fn permission_label(&self) -> String {
        "Recall dialogue memory".to_string()
    }

    fn permission_description(&self) -> String {
        "Search and recall historical conversations and philosophical dialogues between user and this role.".to_string()
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        self.call_structured(arguments).await.map(|o| o.to_text())
    }

    async fn call_structured(&self, arguments: &str) -> Result<ToolOutput, String> {
        let parsed: serde_json::Value =
            serde_json::from_str(arguments).map_err(|e| format!("invalid arguments JSON: {e}"))?;

        let query = parsed
            .get("query")
            .and_then(|q| q.as_str())
            .ok_or_else(|| "missing required field 'query'".to_string())?;

        let role = parsed
            .get("role")
            .and_then(|r| r.as_str())
            .unwrap_or("philosophist");

        let limit = parsed
            .get("limit")
            .and_then(|l| l.as_u64())
            .unwrap_or(5)
            .clamp(1, 10) as usize;

        let store = muta_persistence::get_role_memory_store()?;

        let memories = store
            .recall(role, query, limit)
            .map_err(|e| format!("failed to recall memory: {e}"))?;

        if memories.is_empty() {
            return Ok(ToolOutput::Text(format!(
                "No past dialogues matching \"{query}\" were found in memory for role '{role}'."
            )));
        }

        let mut output = format!(
            "### Recalled {} Dialogue Memory Entries for Role ({role}):\n\n",
            memories.len()
        );

        for (idx, mem) in memories.iter().enumerate() {
            output.push_str(&format!(
                "#### Memory Entry #{idx} ({recency} · {mem_type} · Recalled {access} time(s))\n",
                idx = idx + 1,
                recency = mem.recency_label,
                mem_type = mem.memory_type,
                access = mem.access_count,
            ));
            output.push_str(&format!("**User:** {}\n\n", mem.user_prompt.trim()));
            output.push_str(&format!(
                "**{}:** {}\n\n",
                capitalize(role),
                mem.role_response.trim()
            ));
            output.push_str("---\n\n");
        }

        Ok(ToolOutput::Text(output))
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

muta_contracts::register_tool!(RecallMemoryFactory => RecallMemoryTool);

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn recall_memory_tool_schema_and_execution() {
        let tool = RecallMemoryTool;
        assert_eq!(tool.name(), "recall_memory");
        assert_eq!(tool.permission_label(), "Recall dialogue memory");

        let params = tool.parameters();
        assert!(
            params
                .get("properties")
                .and_then(|p| p.get("query"))
                .is_some()
        );

        // Empty query search should return empty or not found
        let res = tool
            .call(r#"{"query": "non_existent_topic_xyz123"}"#)
            .await
            .unwrap();
        assert!(
            res.contains("No past dialogues matching"),
            "expected not found message, got: {res}"
        );
    }

    #[tokio::test]
    async fn recall_memory_retrieves_recorded_dialogue() {
        let store = muta_persistence::get_role_memory_store().unwrap();
        store
            .record_dialogue(
                "philosophist",
                Some("sess-test"),
                "What do you think about Nietzsche's eternal recurrence?",
                "The eternal recurrence acts as the ultimate existential test: to live such that you would desire each moment repeated infinitely.",
            )
            .unwrap();

        let tool = RecallMemoryTool;
        let res = tool
            .call(r#"{"query": "Nietzsche eternal recurrence"}"#)
            .await
            .unwrap();

        assert!(res.contains("Recalled"));
        assert!(res.contains("eternal recurrence"));
        assert!(res.contains("Nietzsche"));
        assert!(res.contains("Working Memory"));
    }
}
