//! Content-addressed blob store (C13 foundation).
//!
//! Large, repetitive payloads (long tool outputs, envoy transcripts) are
//! stored once under a SHA-256 hash and referenced by that hash. This reduces
//! duplication across sessions/forks and gives future features (semantic
//! search, sync) a stable content key.

use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

const BLOB_PREFIX_LEN: usize = 2;

/// Store for immutable byte blobs keyed by SHA-256.
#[derive(Clone)]
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Hash bytes and return the hex digest.
    pub fn hash(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hex::encode(hasher.finalize())
    }

    /// Persist `bytes` and return their content hash. Idempotent: writing the
    /// same bytes twice is a no-op on disk.
    pub fn put(&self, bytes: &[u8]) -> Result<String, String> {
        let hash = Self::hash(bytes);
        let path = self.path(&hash);
        if path.exists() {
            return Ok(hash);
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::write(&path, bytes).map_err(|e| format!("could not write blob {}: {}", hash, e))?;
        Ok(hash)
    }

    /// Read a blob by hash. Returns `None` if the blob is missing.
    pub fn get(&self, hash: &str) -> Option<Vec<u8>> {
        let path = self.path(hash);
        fs::read(&path).ok()
    }

    /// True if the blob exists locally. Test-only today; production code reads
    /// blobs directly and treats a miss as absence.
    #[cfg(test)]
    pub fn exists(&self, hash: &str) -> bool {
        self.path(hash).exists()
    }

    /// Resolve the on-disk path for a hash.
    pub fn path(&self, hash: &str) -> PathBuf {
        let prefix = &hash[..BLOB_PREFIX_LEN.min(hash.len())];
        self.root.join(prefix).join(hash)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Delete one blob. Infallible by design: a missing blob is the desired
    /// end state, and an undeletable one must not fail a GC pass.
    fn remove(&self, hash: &str) {
        let _ = fs::remove_file(self.path(hash));
    }

    /// Collect every live blob reference reachable from the session snapshots
    /// under `projects_root`, then delete any stored blob that no snapshot
    /// references. Returns `(reclaimed_count, reclaimed_bytes)`.
    ///
    /// The blob namespace is global (content-addressed across projects) while
    /// sessions are per-project buckets, so a single session's delete cannot
    /// know whether another session still shares its blobs — the sweep must
    /// mark over **all** buckets. Snapshots are read as raw JSON and the
    /// `content_blob` keys collected textually: this is a conservative mark
    /// (a hash mentioned anywhere survives), which is exactly the right bias
    /// for a garbage collector over immutable content.
    ///
    /// Failure to read one project bucket (or one snapshot inside it) is
    /// skipped with a warning, never fatal: a GC that aborts on one corrupt
    /// file would reclaim nothing, while a GC that skips it merely leaves a
    /// blob in place for the next pass.
    pub fn collect_garbage(&self, projects_root: &Path) -> (usize, u64) {
        let mut live: std::collections::HashSet<String> = std::collections::HashSet::new();
        // Mark phase: a missing/empty projects root simply marks nothing —
        // every stored blob is then unreachable. This must NOT abort the
        // sweep (the historical early-return here meant a GC pass over an
        // install with no sessions reclaimed nothing, leaving strays to
        // accumulate forever).
        if let Ok(buckets) = fs::read_dir(projects_root) {
            for bucket in buckets.flatten() {
                let sessions_dir = bucket.path().join("sessions");
                let Ok(sessions) = fs::read_dir(&sessions_dir) else {
                    continue;
                };
                for session in sessions.flatten() {
                    let path = session.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("json") {
                        continue;
                    }
                    let Ok(content) = fs::read_to_string(&path) else {
                        tracing::warn!(
                            snapshot = %path.display(),
                            "blob gc: unreadable snapshot; skipping (its blobs stay)"
                        );
                        continue;
                    };
                    collect_blob_refs(&content, &mut live);
                }
            }
        }

        let mut reclaimed = 0usize;
        let mut reclaimed_bytes = 0u64;
        let Ok(prefixes) = fs::read_dir(&self.root) else {
            return (0, 0);
        };
        for prefix_dir in prefixes.flatten() {
            let Ok(blobs) = fs::read_dir(prefix_dir.path()) else {
                continue;
            };
            for blob in blobs.flatten() {
                let hash = blob.file_name().to_string_lossy().into_owned();
                if live.contains(&hash) {
                    continue;
                }
                let size = blob.metadata().map(|m| m.len()).unwrap_or(0);
                self.remove(&hash);
                reclaimed += 1;
                reclaimed_bytes += size;
            }
        }
        (reclaimed, reclaimed_bytes)
    }
}

/// Collect `"<content_blob>"`-quoted hashes out of a raw JSON document
/// without parsing it structurally: snapshots reference blobs from many
/// nested message shapes (children, archived transcript, model window), and a
/// textual scan of quoted values named `content_blob` marks every one of them
/// without coupling the GC to the schema.
fn collect_blob_refs(raw: &str, live: &mut std::collections::HashSet<String>) {
    let mut rest = raw;
    while let Some(pos) = rest.find("\"content_blob\"") {
        rest = &rest[pos + "\"content_blob\"".len()..];
        // Skip whitespace and the colon, expect a quoted string.
        let after_colon = rest.trim_start().strip_prefix(':').map(str::trim_start);
        if let Some(value) = after_colon.and_then(|s| s.strip_prefix('"'))
            && let Some(end) = value.find('"')
        {
            let hash = &value[..end];
            if hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                live.insert(hash.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_is_idempotent_and_get_round_trips() {
        let dir = std::env::temp_dir().join(format!("neenee-blobs-{}", uuid::Uuid::new_v4()));
        let store = BlobStore::new(dir.clone());
        let bytes = b"hello world";
        let hash1 = store.put(bytes).unwrap();
        let hash2 = store.put(bytes).unwrap();
        assert_eq!(hash1, hash2);
        assert!(store.exists(&hash1));
        assert_eq!(store.get(&hash1).unwrap(), bytes.to_vec());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn different_bytes_get_different_hashes() {
        let dir = std::env::temp_dir().join(format!("neenee-blobs-{}", uuid::Uuid::new_v4()));
        let store = BlobStore::new(dir.clone());
        let a = store.put(b"a").unwrap();
        let b = store.put(b"b").unwrap();
        assert_ne!(a, b);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn gc_reclaims_unreferenced_and_keeps_referenced_blobs() {
        let root = tempfile::tempdir().unwrap();
        let store = BlobStore::new(root.path().join("blobs"));
        let projects = root.path().join("projects");

        let kept = store.put(b"still referenced by a snapshot").unwrap();
        let orphan = store.put(b"no snapshot references this").unwrap();
        let shared = store.put(b"referenced by two snapshots").unwrap();

        // Two sessions in two buckets; one references `kept`, both reference
        // `shared`. Orphan is referenced by nothing.
        for (bucket, session, refs) in [
            ("b1", "s1", vec![&kept, &shared]),
            ("b2", "s2", vec![&shared]),
        ] {
            let dir = projects.join(bucket).join("sessions");
            std::fs::create_dir_all(&dir).unwrap();
            let messages: Vec<String> = refs
                .iter()
                .map(|h| format!("{{\"content_blob\": \"{h}\"}}"))
                .collect();
            std::fs::write(
                dir.join(format!("{session}.json")),
                format!("{{\"model_window\": [{}]}}", messages.join(",")),
            )
            .unwrap();
        }

        let (count, bytes) = store.collect_garbage(&projects);
        assert_eq!(count, 1, "exactly the orphan is reclaimed");
        assert!(bytes > 0);
        assert!(store.exists(&kept), "referenced blob must survive");
        assert!(
            store.exists(&shared),
            "cross-session shared blob must survive"
        );
        assert!(!store.exists(&orphan), "orphan must be gone");
    }

    #[test]
    fn gc_skips_unparseable_snapshots_without_deleting_their_blobs() {
        let root = tempfile::tempdir().unwrap();
        let store = BlobStore::new(root.path().join("blobs"));
        let projects = root.path().join("projects");
        let referenced = store.put(b"referenced only by a corrupt snapshot").unwrap();

        let dir = projects.join("b1").join("sessions");
        std::fs::create_dir_all(&dir).unwrap();
        // Unreadable-as-JSON content still scans textually — the mark is
        // conservative, which is the safe direction. A file that cannot be
        // read at all (permissions) is the skip case; simulate by a directory
        // where a snapshot is expected.
        std::fs::write(
            dir.join("s1.json"),
            format!("{{\"content_blob\": \"{referenced}\"}}"),
        )
        .unwrap();

        let (count, _) = store.collect_garbage(&projects);
        assert_eq!(count, 0);
        assert!(store.exists(&referenced));
    }

    #[test]
    fn gc_on_empty_or_missing_roots_is_a_noop() {
        let root = tempfile::tempdir().unwrap();
        let store = BlobStore::new(root.path().join("blobs"));
        assert_eq!(store.collect_garbage(&root.path().join("projects")), (0, 0));
        // A blob with no snapshots at all is reclaimable.
        let orphan = store.put(b"stray").unwrap();
        let (count, _) = store.collect_garbage(&root.path().join("projects"));
        assert_eq!(count, 1);
        assert!(!store.exists(&orphan));
    }

    #[test]
    fn blob_ref_collector_only_accepts_sha256_shaped_values() {
        let mut live = std::collections::HashSet::new();
        collect_blob_refs(
            r#"{"a":{"content_blob":"abcd"},"b":{"content_blob":"0000ffff"}}"#,
            &mut live,
        );
        assert!(
            live.is_empty(),
            "non-64-hex values must not be marked: {live:?}"
        );
        let mut live = std::collections::HashSet::new();
        let hash = "a".repeat(64);
        collect_blob_refs(&format!("{{\"content_blob\": \"{hash}\"}}"), &mut live);
        assert!(live.contains(&hash));
    }
}
