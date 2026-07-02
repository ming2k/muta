//! Google Gemini native SDK adapter.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod google;

pub use google::{GOOGLE_DEFAULT_BASE_URL, GoogleProvider};
