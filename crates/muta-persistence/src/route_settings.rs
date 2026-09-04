//! The user's per-(instance, model) reasoning choices — a **state** store.
//!
//! This is *not* a cache. `effort` / `thinking` are the user's own per-route
//! settings (set from the model `e` editor); deleting them loses user
//! configuration that no endpoint can re-derive. They therefore live in
//! `$XDG_STATE_HOME/muta/route_settings.json`, separate from
//! `$XDG_CACHE_HOME/muta/models_discovery.json`, whose contents are all
//! derived and safe to drop at any time ("reset caches" must not erase the
//! user's reasoning overrides).
//!
//! ## Migration
//!
//! Releases before this split kept `route_settings` inside the discovery
//! cache. [`RouteSettingsStore::load`] folds any such entries into this store
//! one-shot and idempotently (presence check, not a version flag): the first
//! load after upgrade moves the map, clears it from the cache file, and a
//! marker field (`migrated_from_cache`) keeps later loads from re-reading a
//! cache that has since legitimately grown a fresh (empty) map.
//!
//! See ADR-0014 for the category rules this split follows.

use std::collections::BTreeMap;
use std::fs;

use serde::{Deserialize, Serialize};

use crate::config::RouteSettings;
use crate::fsutil;
use crate::paths;

/// Read the historical `route_settings` map out of a pre-split
/// `models_discovery.json`. Returns an empty map for a missing file, a
/// post-split file (no such key), or an unparseable file — migration must
/// never fail startup.
fn read_legacy_cache_route_settings() -> BTreeMap<String, BTreeMap<String, RouteSettings>> {
    let path = paths::get().discovery_cache_file();
    let Ok(content) = fs::read_to_string(&path) else {
        return BTreeMap::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) else {
        return BTreeMap::new();
    };
    serde_json::from_value(
        value
            .get("route_settings")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    )
    .unwrap_or_default()
}

/// The persisted shape: the user's route settings plus the migration marker.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct RouteSettingsFile {
    /// `connection_id -> model_id -> settings`
    #[serde(alias = "providers")]
    connections: BTreeMap<String, BTreeMap<String, RouteSettings>>,
    /// `true` once the one-shot fold out of the discovery cache has run.
    /// Distinguishes "not yet migrated" from "migrated and empty".
    migrated_from_cache: bool,
}

/// The user's per-route reasoning overrides, backed by
/// `$XDG_STATE_HOME/muta/route_settings.json`.
#[derive(Debug, Clone, Default)]
pub struct RouteSettingsStore {
    file: RouteSettingsFile,
}

impl RouteSettingsStore {
    /// Load the store, running the one-shot migration from the discovery
    /// cache when it has not happened yet. Missing or unparseable file → an
    /// empty store.
    pub fn load() -> Self {
        let mut store = Self::read_file();
        if !store.file.migrated_from_cache {
            store.migrate_from_cache();
        }
        store
    }

    fn read_file() -> Self {
        if let Ok(engine) = crate::db::DatabaseEngine::open(&paths::get().db_file(), None)
            && let Ok(Some(file)) = engine.get_json::<RouteSettingsFile>("state:route_settings")
        {
            return Self { file };
        }
        let path = paths::get().route_settings_file();
        let Ok(content) = fs::read_to_string(&path) else {
            return Self::default();
        };
        let Ok(file) = serde_json::from_str::<RouteSettingsFile>(&content) else {
            tracing::warn!(
                path = %path.display(),
                "route settings file unparseable; starting from an empty store"
            );
            return Self::default();
        };
        Self { file }
    }

    /// One-shot fold of the pre-split layout.
    fn migrate_from_cache(&mut self) {
        let legacy = read_legacy_cache_route_settings();
        if !legacy.is_empty() {
            for (conn, models) in legacy {
                let target = self.file.connections.entry(conn).or_default();
                for (model, settings) in models {
                    target.entry(model).or_insert(settings);
                }
            }
        }
        self.file.migrated_from_cache = true;
        if let Err(e) = self.save() {
            tracing::warn!("could not persist route settings migration: {e}");
        }
        let cache = crate::config::DiscoveryCache::load();
        if let Err(e) = cache.save() {
            tracing::warn!("could not clear route settings from discovery cache: {e}");
        }
    }

    /// Persist atomically into SQLite (SSOT), with fallback to legacy file when DB unavailable.
    pub fn save(&self) -> Result<(), String> {
        if let Ok(engine) = crate::db::DatabaseEngine::open(&paths::get().db_file(), None) {
            engine
                .set_json("state:route_settings", &self.file)
                .map_err(|e| e.to_string())
        } else {
            let path = paths::get().route_settings_file();
            fsutil::atomic_write_json(&path, &self.file).map_err(|e| e.to_string())
        }
    }

    /// The reasoning override for one route, if set.
    pub fn settings_for(&self, connection_id: &str, model_id: &str) -> Option<&RouteSettings> {
        self.file
            .connections
            .get(connection_id)
            .and_then(|models| models.get(model_id))
    }

    /// Borrow a route's settings mutably, inserting a default entry when
    /// absent, so a caller can set one field without rebuilding the store.
    pub fn settings_for_mut(&mut self, connection_id: &str, model_id: &str) -> &mut RouteSettings {
        self.file
            .connections
            .entry(connection_id.to_string())
            .or_default()
            .entry(model_id.to_string())
            .or_default()
    }

    /// Remove one route's entry (the `e` editor's "back to default" path).
    pub fn remove(&mut self, connection_id: &str, model_id: &str) {
        if let Some(models) = self.file.connections.get_mut(connection_id) {
            models.remove(model_id);
            if models.is_empty() {
                self.file.connections.remove(connection_id);
            }
        }
    }

    /// Whether any route carries a setting.
    pub fn is_empty(&self) -> bool {
        self.file.connections.iter().all(|(_, m)| m.is_empty())
    }

    /// Drop every route setting for `connection_id` (connection deletion).
    pub fn retain_connection_except(&mut self, connection_id: &str) {
        self.file.connections.remove(connection_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with(instance: &str, model: &str, effort: &str) -> RouteSettingsStore {
        let mut store = RouteSettingsStore::default();
        store.settings_for_mut(instance, model).effort = Some(effort.to_string());
        store
    }

    #[test]
    fn mut_insert_remove_and_empty_semantics() {
        let mut store = RouteSettingsStore::default();
        assert!(store.is_empty());
        store.settings_for_mut("p", "m").effort = Some("high".into());
        assert!(!store.is_empty());
        assert_eq!(
            store.settings_for("p", "m").unwrap().effort.as_deref(),
            Some("high")
        );
        store.remove("p", "m");
        assert!(store.is_empty(), "empty inner map must be dropped");
        assert!(store.settings_for("p", "m").is_none());
    }

    #[test]
    fn round_trips_through_disk() {
        let _guard = crate::paths::TEST_OVERRIDE_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let root = tempfile::tempdir().unwrap();
        crate::paths::set_test_default(Some(crate::paths::Dirs {
            config_dir: root.path().join("config"),
            data_dir: root.path().join("data"),
            state_dir: root.path().join("state"),
            cache_dir: root.path().join("cache"),
            runtime_dir: None,
        }));

        let store = store_with("anthropic", "claude-x", "high");
        store.save().unwrap();

        // A fresh load must see the same entry and not re-run the migration
        // into a different (empty) cache.
        let reloaded = RouteSettingsStore::load();
        assert_eq!(
            reloaded
                .settings_for("anthropic", "claude-x")
                .unwrap()
                .effort
                .as_deref(),
            Some("high")
        );

        crate::paths::set_test_default(None);
    }

    #[test]
    fn migration_moves_cache_entries_once_and_clears_the_cache() {
        let _guard = crate::paths::TEST_OVERRIDE_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let root = tempfile::tempdir().unwrap();
        crate::paths::set_test_default(Some(crate::paths::Dirs {
            config_dir: root.path().join("config"),
            data_dir: root.path().join("data"),
            state_dir: root.path().join("state"),
            cache_dir: root.path().join("cache"),
            runtime_dir: None,
        }));

        // Seed the legacy layout the way a pre-split release wrote it: a raw
        // cache file carrying a `route_settings` key (the typed struct no
        // longer has the field — that is the point).
        let legacy_json = serde_json::json!({
            "route_settings": {
                "kimi": {
                    "kimi-k2": { "effort": "medium", "thinking": false }
                }
            }
        });
        std::fs::create_dir_all(root.path().join("cache")).unwrap();
        std::fs::write(
            crate::paths::get().discovery_cache_file(),
            serde_json::to_string_pretty(&legacy_json).unwrap(),
        )
        .unwrap();

        let store = RouteSettingsStore::load();
        assert_eq!(
            store.settings_for("kimi", "kimi-k2").unwrap(),
            &RouteSettings {
                effort: Some("medium".into()),
                thinking: Some(false),
                capability_overrides: None,
                prompt_cache: None,
            },
            "the cache entry must land in the state store"
        );
        let cache_after = crate::config::DiscoveryCache::load();
        assert!(
            cache_after.connection_models.is_empty(),
            "the cache file must have been rewritten without the legacy key"
        );
        let raw = std::fs::read_to_string(crate::paths::get().discovery_cache_file()).unwrap();
        assert!(
            !raw.contains("route_settings"),
            "the legacy key must be gone from the cache file: {raw}"
        );

        // Idempotency: a second load must not re-fold or lose entries.
        let again = RouteSettingsStore::load();
        assert_eq!(
            again
                .settings_for("kimi", "kimi-k2")
                .unwrap()
                .effort
                .as_deref(),
            Some("medium")
        );

        crate::paths::set_test_default(None);
    }
}
