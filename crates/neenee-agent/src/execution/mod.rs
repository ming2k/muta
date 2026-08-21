//! Concrete execution environments and pipeline middleware.

pub mod in_memory;
pub mod local;
pub mod middleware;

pub use in_memory::{InMemoryExecutionEnvironment, InMemoryFsProvider, MockProcessRunner};
pub use local::{LocalExecutionEnvironment, LocalFsProvider, LocalProcessRunner};
pub use middleware::{SecretScrubMiddleware, SpillMiddleware, WorkspaceJailMiddleware};

#[cfg(test)]
mod tests;
