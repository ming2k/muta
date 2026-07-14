//! Anthropic Messages SDK adapter.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod anthropic;

pub use anthropic::{AnthropicMessagesProvider, Effort, ThinkingConfig, ThinkingMode};
