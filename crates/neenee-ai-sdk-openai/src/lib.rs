//! OpenAI-compatible chat-completions SDK adapter.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod openai;

pub use openai::OpenAiCompatProvider;
