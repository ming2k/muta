//! Overlay modal renderers, split by functional domain.
//!
//! Sub-modules:
//! - [`provider`] — provider picker + API-key / model-id editor
//! - [`session`] — sessions picker + session-context dashboard modal
//! - [`tools`] — tools manager modal (the interactive tool-list surface)
//! - [`skills`] — skills modal (loaded-skill list with detail expansion)
//! - [`mcp`] — MCP manager modal (per-server enable/reconnect surface)
//! - [`activity`] — activity modal (pursuit, prompt, status, or todos)
//! - [`permission`] — permission sheet + question modal
//! - [`history`] — history search modal
//! - [`help`] — help / keybindings modal
//! - [`config`] — full-screen dual-pane settings view
//! - [`toast`] — copy / armed-action notice bubbles
//! - [`common`] — shared helpers (time formatting, truncation, caret, glyphs)

pub mod activity;
pub mod btw;
pub mod common;
pub mod config;
pub mod dashboard;
pub mod help;
pub mod history;
pub mod mcp;
pub mod performance_report;
pub mod permission;
pub mod permissions_manager;
pub mod provider;
pub mod provider_delete_confirm;
pub mod queue;
pub mod session;
pub mod skills;
pub mod toast;
pub mod token_report;
pub mod tools;
pub mod tree;
pub mod usage_stats;
pub mod view_switcher;

pub use activity::{ActivityModalView, draw_activity_modal};
pub use config::{
    ConfigFocus, ConfigViewProps, cycle_reader, cycle_websearch_backend, draw_config_view,
};
pub use dashboard::{
    ConsoleCommand, ConsoleLine, ConsoleVerb, DashboardFocus, creation_order, draw_dashboard,
    draw_session_preview, parse_console_command,
};
// `DashboardRects` is used by the event loop via `draw_dashboard`'s return; it
// is part of the module's public API surface.
#[allow(unused_imports)]
pub use dashboard::DashboardRects;
pub use help::{HelpBinding, draw_help_modal};
pub use history::draw_history_panel;
// The old centered `/host` modal (`host.rs`) was superseded by the full-screen
// `dashboard` surface and removed; `/host` now opens the dashboard.
pub use btw::{BtwModalView, draw_btw_modal};
pub use mcp::draw_mcp_modal;
pub use performance_report::{draw_performance_report_modal, performance_report_round_count};
pub use permission::{draw_input_injection, draw_permission_sheet, draw_question_modal};
pub use permissions_manager::draw_permissions_manager;
pub use provider::{
    CustomEditorView, draw_connections_modal, draw_custom_provider_editor, draw_model_editor,
    draw_models_modal, draw_oauth_pending, draw_preset_chooser,
};
pub use provider_delete_confirm::draw_provider_delete_confirm;
pub use queue::{QueueModalView, draw_queue_modal};
pub use session::draw_sessions_modal;
pub use skills::draw_skills_modal;
pub use toast::{draw_armed_toast, draw_copy_toast, draw_notice_toast};
pub use token_report::{ContextUsageView, draw_token_report_modal, token_report_round_count};
pub use tools::draw_tools_modal;
pub use tree::draw_tree_modal;
pub use usage_stats::draw_usage_stats_modal;
// The view quick switcher (ADR-0133) is consumed crate-internally by the
// render dispatch (`event_loop::render`), so it stays crate-visible.
pub(crate) use view_switcher::draw_view_switcher;
