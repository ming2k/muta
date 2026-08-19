//! Built-in tools (filesystem, shell, web, ask-user, todo).
//!
//! Most tools self-register from their own module via
//! [`neenee_contracts::register_tool!`] (collected by `inventory` at link time).
//! The stateful todo tools are constructed by `neenee-agent` with their shared
//! task-list context. Shared helpers live in `helpers`, and pluggable
//! web-search backends in `search`.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod search;
mod ssrf;

/// Kill an external child's whole process group. `.process_group(0)` made the
/// child a group leader (pgid == pid) at spawn, so `-pid` reaches
/// grandchildren a bare `start_kill()` misses — the classic leak being
/// `sh -c "server & echo hi"`, where the backgrounded `server` survives the
/// shell's death and reparents to init. Long-running agents spawn thousands
/// of shell commands; without a group kill the machine accumulates orphans.
///
/// Callers must still `wait()` the child afterwards so it does not linger as
/// a zombie. Non-Unix targets fall back to killing the direct child (the
/// Windows Job-object equivalent is out of scope for now).
pub(crate) fn kill_process_group(child: &tokio::process::Child) {
    #[cfg(unix)]
    {
        if let Some(pid) = child.id() {
            // SAFETY: libc::kill with a plain integer signal number. Errors
            // (ESRCH if the group is already gone) are intentionally ignored:
            // this runs on teardown paths where the group's death is the goal.
            unsafe {
                libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
            }
        }
    }
    #[cfg(not(unix))]
    let _ = child.start_kill();
}

mod ask_user;
mod bash;
mod edit;
mod glob;
mod grep;
mod helpers;
mod list;
mod read;
mod read_image;
mod todo;
mod web;
mod write;

// Re-export every tool struct at the module root so existing consumers
// (`crate::tools::ReadTextTool`, etc.) keep resolving unchanged.
pub use ask_user::AskUserTool;
pub use bash::BashTool;
pub use edit::EditFileTool;
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use list::ListDirTool;
pub use read::{ReadTextTerseTool, ReadTextTool};
pub use read_image::ReadImageTool;
pub use todo::{TodoToolContext, TodoUpdateTool, TodoWriteTool};
pub(crate) use web::html_to_text;
pub use web::{WebFetchTool, WebPageSnapshot, WebSearchTool, WebSnapshotResult};
pub use write::WriteFileTool;

#[cfg(test)]
mod tests;
