//! Provider/model usage telemetry, persisted under XDG state.
//!
//! Drives recency ordering in the provider picker. This is
//! program-generated usage signal, not user preference: it lives under
//! `$XDG_STATE_HOME` next to `history.json`, and losing it only flattens the
//! sort order — never configuration. Favorites and the default-model pointer
//! belong in `config.toml` and are not stored here.
//!
//! Three maps are persisted:
//! - `providers`: provider id → recency (drives stage-1 ordering).
//! - `models`: model id → recency (drives stage-2 model ordering).
//! - `last_models`: provider id → the model id last activated under it, so a
//!   provider re-opens on the exact model it was left at (not a re-derived
//!   default). See ADR-0018 for the concurrent-merge rationale.

use crate::paths;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// Per-entity usage record. Stored as a JSON object keyed by canonical id
/// (provider id or model id).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
struct UsageEntry {
    /// Unix epoch milliseconds of the most recent activation. Milliseconds
    /// (not seconds) so two activations within the same second still order
    /// deterministically rather than colliding.
    last_used_ms: u64,
    /// Total times the entity was activated. Kept for future tie-breaking and
    /// "most used" views; not used by the current recency sort.
    use_count: u64,
}

/// The on-disk usage map. Serialized as `provider_usage.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderUsage {
    /// Provider id → recency/count. Drives stage-1 provider ordering.
    #[serde(default)]
    providers: HashMap<String, UsageEntry>,
    /// Model id → recency/count. Drives stage-2 model ordering.
    #[serde(default)]
    models: HashMap<String, UsageEntry>,
    /// Provider id → the wire model id last activated under it. Restores a
    /// provider's exact model on re-open instead of re-deriving a default.
    #[serde(default)]
    last_models: HashMap<String, String>,
}

/// Intermediate deserialization shape that captures the legacy flat `entries`
/// map (pre-split provider recency). On load the legacy `entries` are folded
/// into the new `providers` map so the split does not drop existing users'
/// ordering. Never serialized directly.
#[derive(Debug, Default, Deserialize)]
struct RawUsage {
    #[serde(default)]
    entries: HashMap<String, UsageEntry>,
}

impl ProviderUsage {
    /// Load from the well-known state file. Returns an empty store when the
    /// file is missing or unreadable, since the data is fully rebuildable.
    ///
    /// Legacy stores wrote provider recency under a flat `entries` key; those
    /// are max-merged into the new `providers` map so the split does not reset
    /// anyone's ordering. (A file may carry both a legacy `entries` map and
    /// the new split maps during the transition; both are honored.)
    pub fn load() -> Self {
        let path = paths::get().provider_usage_file();
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        Self::parse(&content)
    }

    /// Parse a store from JSON file content, folding any legacy flat `entries`
    /// map into `providers`. Split out of [`load`](Self::load) so the migration
    /// is unit-testable without touching the process-wide paths override.
    fn parse(content: &str) -> Self {
        let mut store: ProviderUsage = serde_json::from_str(content).unwrap_or_default();
        let raw: RawUsage = serde_json::from_str(content).unwrap_or_default();
        for (id, entry) in raw.entries {
            let disk = store.providers.entry(id).or_default();
            disk.last_used_ms = disk.last_used_ms.max(entry.last_used_ms);
            disk.use_count = disk.use_count.max(entry.use_count);
        }
        store
    }

    /// Record an activation of provider `id`. Bumps `last_used_ms` to now, and
    /// increments `use_count`.
    pub fn record(&mut self, id: &str) {
        let now = now_ms();
        let entry = self.providers.entry(id.to_string()).or_default();
        // `now` is monotonic-ish per wall clock; only advance the timestamp so
        // a clock skew backwards does not erase a more recent activation.
        entry.last_used_ms = entry.last_used_ms.max(now);
        entry.use_count = entry.use_count.saturating_add(1);
    }

    /// Record an activation of `model` under `provider_id`: bumps model
    /// recency/count and pins it as that provider's last-used model so the
    /// provider re-opens on it.
    pub fn record_model(&mut self, provider_id: &str, model: &str) {
        let now = now_ms();
        let entry = self.models.entry(model.to_string()).or_default();
        entry.last_used_ms = entry.last_used_ms.max(now);
        entry.use_count = entry.use_count.saturating_add(1);
        self.last_models
            .insert(provider_id.to_string(), model.to_string());
    }

    /// The wire model id last activated under `provider_id`, if any. Lets a
    /// provider re-open on the exact model it was left at rather than a
    /// re-derived default.
    pub fn last_model_for(&self, provider_id: &str) -> Option<&str> {
        self.last_models.get(provider_id).map(|m| m.as_str())
    }

    /// Persist atomically, merged with whatever another `neenee` instance may
    /// have written since this store was loaded. The merge is per-key and
    /// **commutative**: each entry keeps `max(last_used_ms)` and
    /// `max(use_count)` of the in-memory and on-disk values, and each provider
    /// keeps the last-model with the greater recorded recency (falling back to
    /// the in-memory value on a tie), so two instances recording concurrently
    /// never regress recency or lose an activation regardless of write order
    /// (ADR-0018). The whole reload-merge-write window is serialised by a
    /// companion `flock` so the merge reads a consistent snapshot.
    ///
    /// Best-effort: callers ignore the result since usage tracking is
    /// non-critical. `use_count` is merged by `max` (not sum) because a sum
    /// would require a per-process baseline that is not tracked; `max` still
    /// preserves recency, which is the only field the picker reads today.
    pub fn save(&self) -> Result<(), String> {
        let path = paths::get().provider_usage_file();
        let _lock = crate::fsutil::FileLock::acquire(&path)
            .map_err(|e| format!("could not lock usage file: {e}"))?;
        // Re-read under the lock so we merge against the latest on-disk state,
        // not the snapshot this process loaded at startup.
        let mut merged = ProviderUsage::load();
        for (id, entry) in &self.providers {
            let disk = merged.providers.entry(id.clone()).or_default();
            disk.last_used_ms = disk.last_used_ms.max(entry.last_used_ms);
            disk.use_count = disk.use_count.max(entry.use_count);
        }
        for (model, entry) in &self.models {
            let disk = merged.models.entry(model.clone()).or_default();
            disk.last_used_ms = disk.last_used_ms.max(entry.last_used_ms);
            disk.use_count = disk.use_count.max(entry.use_count);
        }
        for (provider_id, model) in &self.last_models {
            // Keep the last-model whose model has the more recent activation;
            // on a tie prefer the in-memory value (this process just set it).
            let kept = merged
                .last_models
                .get(provider_id)
                .and_then(|disk_model| {
                    let disk_ts = merged.models.get(disk_model).map_or(0, |e| e.last_used_ms);
                    let mem_ts = self.models.get(model).map_or(0, |e| e.last_used_ms);
                    (disk_ts > mem_ts).then(|| disk_model.clone())
                })
                .unwrap_or_else(|| model.clone());
            merged.last_models.insert(provider_id.clone(), kept);
        }
        let bytes = serde_json::to_vec_pretty(&merged).map_err(|e| e.to_string())?;
        crate::fsutil::atomic_write_bytes(&path, &bytes).map_err(|e| e.to_string())
    }

    /// Last-used timestamp (epoch ms) for a provider id. `None` when the
    /// provider has never been activated, which sorts as "oldest".
    pub fn last_used_ms(&self, id: &str) -> Option<u64> {
        self.providers.get(id).map(|e| e.last_used_ms)
    }

    /// Last-used timestamp (epoch ms) for a model id. `None` when the model
    /// has never been activated, which sorts as "oldest" in the stage-2 model
    /// list.
    pub fn model_last_used_ms(&self, model: &str) -> Option<u64> {
        self.models.get(model).map(|e| e.last_used_ms)
    }

    /// Number of times provider `id` was activated. `0` for unknown ids.
    ///
    /// Consumed by the provider picker's tie-breaking and future "most used"
    /// views.
    #[allow(dead_code)]
    pub fn use_count(&self, id: &str) -> u64 {
        self.providers.get(id).map_or(0, |e| e.use_count)
    }
}

/// Current wall-clock time as Unix epoch milliseconds. Saturates on the
/// far-future overflow, which is irrelevant for sort ordering.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_sets_last_used_and_increments_count() {
        let mut usage = ProviderUsage::default();
        assert_eq!(usage.use_count("google"), 0);
        assert!(usage.last_used_ms("google").is_none());

        usage.record("google");
        assert_eq!(usage.use_count("google"), 1);
        let first = usage.last_used_ms("google").expect("recorded");

        usage.record("google");
        assert_eq!(usage.use_count("google"), 2);
        // A second activation never moves the clock backwards.
        assert!(usage.last_used_ms("google").unwrap() >= first);
    }

    #[test]
    fn record_stores_id_verbatim() {
        let mut usage = ProviderUsage::default();
        // Ids are stored as given; there is no alias canonicalization.
        usage.record("deepseek-v4-flash");
        assert_eq!(usage.use_count("deepseek-v4-flash"), 1);
        // A stale id does not get merged into the current entry.
        assert_eq!(usage.use_count("deepseek"), 0);
        assert!(usage.last_used_ms("deepseek").is_none());
    }

    #[test]
    fn unknown_id_has_no_last_used_and_zero_count() {
        let usage = ProviderUsage::default();
        assert!(usage.last_used_ms("never-used").is_none());
        assert_eq!(usage.use_count("never-used"), 0);
    }

    #[test]
    fn record_never_moves_clock_backwards() {
        let mut usage = ProviderUsage::default();
        usage.record("glm");
        let real_now = usage.last_used_ms("glm").unwrap();
        // Inject an artificially older timestamp directly, then record again:
        // the real clock must win, not regress toward the stale value.
        usage.providers.get_mut("glm").unwrap().last_used_ms = real_now + 3_600_000;
        usage.record("glm");
        assert!(
            usage.last_used_ms("glm").unwrap() >= real_now + 3_600_000,
            "a newer activation must not be overwritten by an older clock reading"
        );
    }

    #[test]
    fn record_model_tracks_per_model_recency_and_last_model() {
        let mut usage = ProviderUsage::default();
        assert!(usage.model_last_used_ms("claude-opus-4-8").is_none());
        assert!(usage.last_model_for("anthropic").is_none());

        usage.record_model("anthropic", "claude-sonnet-5");
        assert!(usage.model_last_used_ms("claude-sonnet-5").is_some());
        assert_eq!(
            usage.last_model_for("anthropic").unwrap(),
            "claude-sonnet-5"
        );

        usage.record_model("anthropic", "claude-opus-4-8");
        assert_eq!(
            usage.last_model_for("anthropic").unwrap(),
            "claude-opus-4-8",
            "the most recently activated model wins"
        );
    }

    #[test]
    fn usage_round_trips_through_json() {
        let mut usage = ProviderUsage::default();
        usage.record("qwen");
        usage.record("qwen");
        usage.record("glm");
        usage.record_model("opencode-go", "minimax-m3");
        let json = serde_json::to_string(&usage).unwrap();
        let restored: ProviderUsage = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.use_count("qwen"), 2);
        assert_eq!(restored.use_count("glm"), 1);
        assert!(restored.last_used_ms("qwen").is_some());
        assert_eq!(
            restored.last_model_for("opencode-go").unwrap(),
            "minimax-m3"
        );
        assert!(restored.model_last_used_ms("minimax-m3").is_some());
    }

    #[test]
    fn legacy_json_without_model_maps_loads_cleanly() {
        // An older store wrote only the flat `entries` map (provider recency).
        // `parse` (used by `load`) must fold that legacy map into the new
        // `providers` map so the split does not reset anyone's ordering, and
        // tolerate the absent model/last-model maps.
        let legacy = r#"{"entries":{"gemini":{"last_used_ms":42,"use_count":1}}}"#;
        let restored = ProviderUsage::parse(legacy);
        assert_eq!(restored.use_count("gemini"), 1);
        assert_eq!(restored.last_used_ms("gemini"), Some(42));
        assert!(restored.last_model_for("gemini").is_none());
        assert!(restored.model_last_used_ms("gemini").is_none());
    }
}
