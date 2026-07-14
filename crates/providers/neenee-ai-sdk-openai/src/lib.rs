//! OpenAI-compatible chat-completions SDK adapter, plus a Responses-API
//! provider for the ChatGPT subscription backend.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod openai;
pub mod responses;

pub use openai::OpenAiCompatProvider;
pub use responses::ResponsesProvider;
