//! Process-lock compatibility facade.
//!
//! Native lock semantics live in `neenee-platform`; persistence keeps this
//! re-export so callers do not need to know which crate owns the OS adapter.

pub use neenee_platform::lock::ProcessLock;
