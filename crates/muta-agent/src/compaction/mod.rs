pub mod branch_summary;
pub mod causal_compactor;
pub mod file_tracker;
pub mod heuristic;
pub mod observation_folding;
pub mod split_compaction;

pub use branch_summary::generate_branch_summary;
pub use causal_compactor::{CausalCompactionOutcome, CausalCompactor};
pub use file_tracker::FileOperations;
pub use heuristic::{HeuristicCompactionEvaluator, HeuristicDecision, TaskMilestone};
pub use observation_folding::fold_historical_observations;
pub use split_compaction::{
    CutPointResult, compact_causal_nodes, estimate_causal_node_tokens, find_cut_point_nodes,
    serialize_nodes_for_summary,
};
