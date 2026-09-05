//! Built-in tools (filesystem, shell, web, ask-user, todo).
//!
//! Most tools self-register from their own module via
//! [`muta_contracts::register_tool!`] (collected by `inventory` at link time).
//! The stateful todo tools are constructed by `muta-agent` with their shared
//! task-list context. Shared helpers live in `helpers`, and pluggable
//! web-search backends in `search`.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod reader;
pub mod search;
mod ssrf;

mod ask_user;
mod edit_text;
mod execute_command;
mod file_search;
mod find_files;
mod helpers;
mod list_dir;
pub mod process_jobs;
mod read_image;
mod read_text;
mod search_text;
pub mod syntax_guard;
mod todo;
mod web;
mod write_file;

pub use syntax_guard::{SyntaxCheckResult, verify_syntax};

// Re-export every tool struct at the module root so existing consumers
// (`crate::tools::ReadTextTool`, etc.) keep resolving unchanged.
pub use ask_user::AskUserTool;
pub use edit_text::EditTextTool;
pub use execute_command::ExecuteCommandTool;
pub use find_files::FindFilesTool;
pub use list_dir::ListDirTool;
pub use process_jobs::{ProcessKillTool, ProcessLogsTool, ProcessPollTool, ProcessWaitTool};
pub use read_image::ReadImageTool;
pub use read_text::{ReadTextTerseTool, ReadTextTool};
pub use search_text::SearchTextTool;
pub use todo::{TodoToolContext, TodoUpdateTool, TodoWriteTool};
pub(crate) use web::html_to_text;
pub use web::{WebPageSnapshot, WebReaderTool, WebSearchTool, WebSnapshotResult};
pub use write_file::WriteFileTool;

#[cfg(test)]
mod tests;
