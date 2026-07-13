//! Bridge between agent-owned runtime state and concrete `neenee-tools`.
//!
//! Keep concrete-tool construction here so the turn loop continues to work
//! only with `dyn Tool` / `ToolSet`. Tools that merely need injected state
//! belong in `neenee-tools`; tools that construct or control agents belong in
//! this crate.

use std::sync::{Arc, Mutex};

/// Add the concrete tools whose lifetime is tied to one agent instance.
pub(crate) fn install_agent_owned_tools(
    toolset: &mut neenee_core::ToolSet,
    todos: Arc<Mutex<neenee_core::TodoList>>,
    turn_counter: Arc<Mutex<u64>>,
) {
    let context = neenee_tools::TodoToolContext::new(todos, turn_counter);
    toolset.upsert(Arc::new(neenee_tools::TodoWriteTool::new(context.clone())));
    toolset.upsert(Arc::new(neenee_tools::TodoUpdateTool::new(context)));
}
