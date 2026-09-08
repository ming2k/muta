//! Application filesystem infrastructure: centralised XDG path resolution,
//! atomic-rename durability helpers, and process locks. Used by the daemon,
//! the persistence layer, the CLI, and the frontend alike — owned by none of
//! them (ADR-0197 edge debt: the frontend's path reads must not drag the
//! persistence crate into the TUI).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod fsutil;
pub mod lock;
pub mod paths;
