//! Native operating-system capabilities used by neenee's business layers.
//!
//! The public API is expressed in semantic operations (local IPC, an owned
//! process tree, daemon detachment, and an advisory process lock). OS-specific
//! mechanisms stay behind those boundaries so callers never emulate a missing
//! capability with a successful no-op.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod ipc;
pub mod lock;
pub mod process;
pub mod secure_file;
pub mod shell;

#[cfg(windows)]
mod windows_security;

#[cfg(not(any(unix, windows)))]
compile_error!("neenee-platform supports Unix and Windows targets only");
