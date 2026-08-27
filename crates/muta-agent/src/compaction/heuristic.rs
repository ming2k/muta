//! Heuristic compaction evaluation based on semantic task boundaries.

use muta_contracts::{TodoList, TodoStatus};

/// An identified logical milestone in conversation progression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskMilestone {
    /// All active task items in the todo list are settled (all completed).
    TodosSettled,
    /// User prompt indicates a deliberate subject/topic transition.
    TopicShift,
    /// High consecutive round count on a single branch.
    LongRunningBranch { round_count: usize },
}

/// The compaction recommendation at the current conversational boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeuristicDecision {
    /// Recommend compaction immediately at this clean logical milestone.
    Recommended(TaskMilestone),
    /// Defer compaction; current state is mid-task or too brief.
    Defer { reason: &'static str },
}

/// Evaluator analyzing whether the current turn constitutes an optimal compaction boundary.
pub struct HeuristicCompactionEvaluator;

impl HeuristicCompactionEvaluator {
    /// Evaluate whether a compaction is logically recommended at the current boundary.
    pub fn evaluate(
        rounds_since_last_compaction: usize,
        latest_user_prompt: &str,
        todos: Option<&TodoList>,
    ) -> HeuristicDecision {
        if rounds_since_last_compaction < 3 {
            return HeuristicDecision::Defer {
                reason: "Too few rounds since previous compaction",
            };
        }

        // 1. Check if all tasks in TodoList have completed
        if let Some(list) = todos
            && !list.items.is_empty()
            && list.items.iter().all(|t| t.status == TodoStatus::Completed)
        {
            return HeuristicDecision::Recommended(TaskMilestone::TodosSettled);
        }

        // 2. Check for explicit topic shift phrases in user prompt
        let prompt_lower = latest_user_prompt.trim().to_lowercase();
        let topic_shift_markers = [
            "now let's switch to",
            "next task:",
            "moving on to",
            "let's move to",
            "that works, now",
            "now that that's done",
            "下一步",
            "接下来开始",
            "切换到",
        ];

        for marker in topic_shift_markers {
            if prompt_lower.starts_with(marker) || prompt_lower.contains(marker) {
                return HeuristicDecision::Recommended(TaskMilestone::TopicShift);
            }
        }

        // 3. Fallback to branch length pressure
        if rounds_since_last_compaction >= 15 {
            return HeuristicDecision::Recommended(TaskMilestone::LongRunningBranch {
                round_count: rounds_since_last_compaction,
            });
        }

        HeuristicDecision::Defer {
            reason: "No semantic task milestone reached yet",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muta_contracts::{TodoId, TodoItem, TodoStatus};

    #[test]
    fn heuristic_evaluation_detects_topic_shift_and_todos() {
        // Test topic shift phrase
        let res = HeuristicCompactionEvaluator::evaluate(
            5,
            "Next task: implement user profile auth",
            None,
        );
        assert_eq!(
            res,
            HeuristicDecision::Recommended(TaskMilestone::TopicShift)
        );

        // Test settled todos
        let mut list = TodoList::default();
        list.items.push(TodoItem {
            id: TodoId(1),
            content: "Fix bug".to_string(),
            status: TodoStatus::Completed,
            created_at: 0,
            updated_at: 0,
        });
        let res2 = HeuristicCompactionEvaluator::evaluate(4, "What should we do?", Some(&list));
        assert_eq!(
            res2,
            HeuristicDecision::Recommended(TaskMilestone::TodosSettled)
        );

        // Test defer
        let res3 = HeuristicCompactionEvaluator::evaluate(1, "Fix bug", None);
        assert!(matches!(res3, HeuristicDecision::Defer { .. }));
    }
}
