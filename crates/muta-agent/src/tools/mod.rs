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
