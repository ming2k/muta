//! Workspace isolation and shadow worktree management for subagents.

pub mod isolated;
pub mod manager;

pub use isolated::IsolatedWorkspace;
pub use manager::WorktreeManager;

#[cfg(test)]
mod tests;
