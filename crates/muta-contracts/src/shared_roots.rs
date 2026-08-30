//! Live-updatable additional-roots handle shared between the assembling
//! bootstrap and the executing tools.
//!
//! Mirrors [`crate::SharedWebSearchConfig`]: one typed `Arc<RwLock<…>>`
//! service the bootstrap provides into the tool context, so a runtime trust
//! decision (`/trust roots`, `/trust revoke`, `/settings reload`) can
//! recompute both root sets and swap them in **without rebuilding the toolset
//! or restarting the session** — ADR-0147's "the boundary takes shape at
//! trust-decision time".
//!
//! The handle carries two sets over the *additional* roots (the cross-project
//! escape hatch, ADR-0142):
//!
//! - **admitted**: directories file tools and shell sandboxes may touch.
//! - **quarantined**: directories declared by an *untrusted* roots domain.
//!   Carried for denial-message diagnosis only; admission checks never read
//!   them, so the fail-closed property is unchanged.
//!
//! The primary workspace root is immutable for a session's lifetime — it
//! anchors path resolution and shell cwd; only sibling-tree admission moves.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// The two live additional-root sets (see module docs).
#[derive(Debug, Clone, Default)]
struct RootSets {
    admitted: Vec<PathBuf>,
    quarantined: Vec<PathBuf>,
}

/// Canonicalized additional roots, snapshotable on every operation.
#[derive(Debug, Clone, Default)]
pub struct SharedAdditionalRoots(Arc<RwLock<RootSets>>);

impl SharedAdditionalRoots {
    pub fn new(admitted: Vec<PathBuf>) -> Self {
        Self(Arc::new(RwLock::new(RootSets {
            admitted,
            quarantined: Vec::new(),
        })))
    }

    /// Empty handle: single-root admission until a trust decision widens it.
    pub fn empty() -> Self {
        Self::new(Vec::new())
    }

    /// Snapshot the current admitted set. Cheap clone per call-path use;
    /// locks are never held across await points.
    pub fn snapshot(&self) -> Vec<PathBuf> {
        self.0
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .admitted
            .clone()
    }

    /// Swap in a new canonical admitted set. Replaces — never unions — so a
    /// revoke collapses admission back to primary-only atomically.
    pub fn store(&self, roots: Vec<PathBuf>) {
        let mut sets = self
            .0
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sets.quarantined.retain(|q| !roots.contains(q));
        sets.admitted = roots;
    }

    /// Declare roots whose project source is still untrusted. Kept visible
    /// so confinement denials can name exactly what `/trust roots` would
    /// admit; never consulted for admission itself.
    pub fn declare_quarantined(&self, roots: Vec<PathBuf>) {
        let mut sets = self
            .0
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for root in roots {
            if !sets.admitted.contains(&root) && !sets.quarantined.contains(&root) {
                sets.quarantined.push(root);
            }
        }
    }

    /// Declared-but-untrusted roots, for denial hints.
    pub fn quarantined(&self) -> Vec<PathBuf> {
        self.0
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .quarantined
            .clone()
    }

    /// True when nothing beyond the primary is admitted right now.
    pub fn is_empty(&self) -> bool {
        self.0
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .admitted
            .is_empty()
    }
}
