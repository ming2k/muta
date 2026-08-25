//! Connection/model usage telemetry, persisted under XDG state.
//!
//! Drives recency ordering in the connection picker. This is
//! program-generated usage signal, not user preference: it lives under
//! `$XDG_STATE_HOME` next to `history.json`, and losing it only flattens the
//! sort order — never configuration. Favorites and the default-model pointer
//! belong in `config.toml` and are not stored here.
//!
//! Three maps are persisted:
//! - `connections`: connection id → recency (drives stage-1 ordering).
//! - `models`: model id → recency (drives stage-2 model ordering).
//! - `last_models`: connection id → the model id last activated under it, so a
//!   connection re-opens on the exact model it was left at (not a re-derived
//!   default).

use crate::paths;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// Per-entity usage record. Stored as a JSON object keyed by canonical id
/// (connection id or model id).
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

/// The on-disk usage map. Serialized as `connection_usage.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ConnectionUsage {
    /// Connection id → recency/count. Drives stage-1 connection ordering.
    #[serde(default)]
    connections: HashMap<String, UsageEntry>,
    /// Model id → recency/count. Drives stage-2 model ordering.
    #[serde(default)]
    models: HashMap<String, UsageEntry>,
    /// Connection id → the wire model id last activated under it. Restores a
    /// connection's exact model on re-open instead of re-deriving a default.
    #[serde(default)]
    last_models: HashMap<String, String>,
}

impl ConnectionUsage {
    /// Load from the well-known state file. Returns an empty store when the
    /// file is missing or unreadable, since the data is fully rebuildable.
    pub fn load() -> Self {
        let path = paths::get().connection_usage_file();
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        serde_json::from_str(&content).unwrap_or_default()
    }

    /// Record an activation of connection `id`. Bumps `last_used_ms` to now, and
    /// increments `use_count`.
    pub fn record(&mut self, id: &str) {
        let now = now_ms();
        let entry = self.connections.entry(id.to_string()).or_default();
        entry.last_used_ms = entry.last_used_ms.max(now);
        entry.use_count = entry.use_count.saturating_add(1);
    }

    /// Record an activation of `model` under `connection_id`: bumps model
    /// recency/count and pins it as that connection's last-used model so the
    /// connection re-opens on it.
    pub fn record_model(&mut self, connection_id: &str, model: &str) {
        let now = now_ms();
        let entry = self.models.entry(model.to_string()).or_default();
        entry.last_used_ms = entry.last_used_ms.max(now);
        entry.use_count = entry.use_count.saturating_add(1);
        self.last_models
            .insert(connection_id.to_string(), model.to_string());
    }

    /// The wire model id last activated under `connection_id`, if any. Lets a
    /// connection re-open on the exact model it was left at rather than a
    /// re-derived default.
    pub fn last_model_for(&self, connection_id: &str) -> Option<&str> {
        self.last_models.get(connection_id).map(|m| m.as_str())
    }

    /// Persist atomically, merged with whatever another `muta` instance may
    /// have written since this store was loaded.
    pub fn save(&self) -> Result<(), String> {
        let path = paths::get().connection_usage_file();
        let _lock = crate::fsutil::FileLock::acquire(&path)
            .map_err(|e| format!("could not lock usage file: {e}"))?;
        let mut merged = ConnectionUsage::load();
        for (id, entry) in &self.connections {
            let disk = merged.connections.entry(id.clone()).or_default();
            disk.last_used_ms = disk.last_used_ms.max(entry.last_used_ms);
            disk.use_count = disk.use_count.max(entry.use_count);
        }
        for (model, entry) in &self.models {
            let disk = merged.models.entry(model.clone()).or_default();
            disk.last_used_ms = disk.last_used_ms.max(entry.last_used_ms);
            disk.use_count = disk.use_count.max(entry.use_count);
        }
        for (connection_id, model) in &self.last_models {
            let in_mem_recency = self.model_recency(model);
            let on_disk_model = merged.last_models.get(connection_id);
            let on_disk_recency = on_disk_model.map(|m| merged.model_recency(m)).unwrap_or(0);
            if in_mem_recency >= on_disk_recency {
                merged
                    .last_models
                    .insert(connection_id.clone(), model.clone());
            }
        }
        let json = serde_json::to_string_pretty(&merged)
            .map_err(|e| format!("could not serialize usage store: {e}"))?;
        crate::fsutil::atomic_write_bytes(&path, json.as_bytes())
            .map_err(|e| format!("could not persist usage store: {e}"))
    }

    /// Remove a connection and its associated last_model pointer.
    pub fn remove_connection(&mut self, id: &str) {
        self.connections.remove(id);
        self.last_models.remove(id);
    }

    /// Remove a model from usage telemetry and any last_model pointers targeting it.
    pub fn remove_model(&mut self, model: &str) {
        self.models.remove(model);
        self.last_models.retain(|_, m| m != model);
    }

    /// Prune stale connections, models, and connection-model pairs. Returns whether any entry was removed.
    pub fn prune(
        &mut self,
        mut is_valid_connection: impl FnMut(&str) -> bool,
        mut is_valid_model: impl FnMut(&str) -> bool,
        mut is_valid_pair: impl FnMut(&str, &str) -> bool,
    ) -> bool {
        let mut changed = false;
        let prev_conn_len = self.connections.len();
        self.connections.retain(|id, _| is_valid_connection(id));
        if self.connections.len() != prev_conn_len {
            changed = true;
        }

        let prev_models_len = self.models.len();
        self.models.retain(|m, _| is_valid_model(m));
        if self.models.len() != prev_models_len {
            changed = true;
        }

        let prev_last_len = self.last_models.len();
        self.last_models
            .retain(|conn_id, m| is_valid_connection(conn_id) && is_valid_pair(conn_id, m));
        if self.last_models.len() != prev_last_len {
            changed = true;
        }

        changed
    }

    /// Persist this usage state directly and atomically to disk without resurrecting pruned entries.
    pub fn save_exact(&self) -> Result<(), String> {
        let path = paths::get().connection_usage_file();
        let _lock = crate::fsutil::FileLock::acquire(&path)
            .map_err(|e| format!("could not lock usage file: {e}"))?;
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("could not serialize usage store: {e}"))?;
        crate::fsutil::atomic_write_bytes(&path, json.as_bytes())
            .map_err(|e| format!("could not persist usage store: {e}"))
    }

    /// Recency (epoch ms) of connection `id`, or `0` if never activated.
    pub fn recency_of(&self, id: &str) -> u64 {
        self.connections.get(id).map_or(0, |e| e.last_used_ms)
    }

    /// Recency (epoch ms) of `model`, or `0` if never activated.
    pub fn model_recency(&self, model: &str) -> u64 {
        self.models.get(model).map_or(0, |e| e.last_used_ms)
    }

    /// Total activation count for connection `id`.
    pub fn count_of(&self, id: &str) -> u64 {
        self.connections.get(id).map_or(0, |e| e.use_count)
    }

    /// Total activation count for `model`.
    pub fn model_count(&self, model: &str) -> u64 {
        self.models.get(model).map_or(0, |e| e.use_count)
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
