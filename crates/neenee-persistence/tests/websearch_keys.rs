#![cfg(test)]
//! Migration tests for the `[websearch]` key split (config → credentials).

use neenee_contracts::WebSearchConfig;
use neenee_persistence::config::{Config, Credentials, WebSearchKeys};

use std::sync::Mutex;

/// Install a sandboxed `Dirs` pair; the returned guard holds the crate-wide
/// test lock until dropped (keeps other tests from racing the override).
fn sandbox() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
    let guard = neenee_persistence::paths::TEST_OVERRIDE_GUARD
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let root = tempfile::tempdir().unwrap();
    neenee_persistence::paths::set_test_default(Some(neenee_persistence::paths::Dirs {
        config_dir: root.path().join("config"),
        data_dir: root.path().join("data"),
        state_dir: root.path().join("state"),
        cache_dir: root.path().join("cache"),
        runtime_dir: None,
    }));
    (root, guard)
}

#[test]
fn websearch_keys_are_not_serialized_into_config_toml() {
    // The shareability contract: `config.toml` must never carry a secret.
    let mut cfg = Config::default();
    cfg.websearch.exa_api_key = Some(neenee_contracts::SecretString::new("exa-1"));
    cfg.websearch.provider = "tavily".into();
    let toml = toml::to_string_pretty(&cfg).unwrap();
    assert!(
        !toml.contains("exa-1"),
        "secret leaked into config.toml: {toml}"
    );
    assert!(
        !toml.contains("api_key"),
        "key field emitted at all: {toml}"
    );
    assert!(
        toml.contains("provider = \"tavily\""),
        "behavior keys stay: {toml}"
    );
}

#[test]
fn websearch_keys_round_trip_through_credentials_toml() {
    let mut creds = Credentials::default();
    creds.websearch = WebSearchKeys {
        tavily_api_key: Some(neenee_contracts::SecretString::new("tvly-1")),
        ..Default::default()
    };
    let toml = toml::to_string_pretty(&creds).unwrap();
    assert!(toml.contains("[websearch]"));
    let parsed: Credentials = toml::from_str(&toml).unwrap();
    assert_eq!(
        parsed
            .websearch
            .tavily_api_key
            .as_ref()
            .map(|k| k.expose_secret()),
        Some("tvly-1")
    );
}

#[test]
fn empty_websearch_keys_omit_the_table() {
    let toml = toml::to_string_pretty(&Credentials::default()).unwrap();
    assert!(!toml.contains("[websearch]"), "empty table emitted: {toml}");
}

#[test]
fn load_migrates_keys_from_config_toml_into_credentials_toml_once() {
    let (_root, _guard) = {
        let (root, guard) = sandbox();
        (root, guard)
    };

    // Seed a pre-migration config.toml with keys inline.
    let pre = r#"
[websearch]
provider = "bocha"
bocha_api_key = "sk-old"
"#;
    std::fs::create_dir_all(neenee_persistence::paths::get().config_dir.clone()).unwrap();
    std::fs::write(Config::config_file_path(), pre).unwrap();

    // First load: keys move into credentials.toml and stay in memory.
    let cfg = Config::load();
    assert_eq!(
        cfg.websearch
            .bocha_api_key
            .as_ref()
            .map(|k| k.expose_secret()),
        Some("sk-old")
    );
    assert_eq!(cfg.websearch.provider, "bocha", "behavior keys untouched");
    let creds = Credentials::load();
    assert_eq!(
        creds
            .websearch
            .bocha_api_key
            .as_ref()
            .map(|k| k.expose_secret()),
        Some("sk-old"),
        "the key must now live in credentials.toml"
    );

    // The config file on disk keeps its (stale) inline key — the next save
    // writes behavior-only — but a *reload* must not duplicate or flip it:
    let again = Config::load();
    assert_eq!(
        again
            .websearch
            .bocha_api_key
            .as_ref()
            .map(|k| k.expose_secret()),
        Some("sk-old")
    );
    let creds_again = Credentials::load();
    assert_eq!(
        creds_again
            .websearch
            .bocha_api_key
            .as_ref()
            .map(|k| k.expose_secret()),
        Some("sk-old")
    );

    // A save of the migrated config must not write any key back.
    let serialized = toml::to_string_pretty(&again).unwrap();
    assert!(!serialized.contains("sk-old"));

    neenee_persistence::paths::set_test_default(None);
}

#[test]
fn credentials_entry_wins_over_stale_config_inline_key() {
    let _guard = {
        let (_root, guard) = sandbox();
        guard
    };

    // credentials.toml already holds the canonical key; config.toml still
    // carries an outdated inline one. The credentials file is the location
    // the user edits going forward, so it wins.
    std::fs::create_dir_all(neenee_persistence::paths::get().config_dir.clone()).unwrap();
    std::fs::write(
        Config::config_file_path(),
        "[websearch]\nexa_api_key = \"exa-stale\"\n",
    )
    .unwrap();
    let mut creds = Credentials::default();
    creds.websearch.exa_api_key = Some(neenee_contracts::SecretString::new("exa-fresh"));
    creds.save().unwrap();

    let cfg = Config::load();
    assert_eq!(
        cfg.websearch
            .exa_api_key
            .as_ref()
            .map(|k| k.expose_secret()),
        Some("exa-fresh")
    );

    neenee_persistence::paths::set_test_default(None);
}

#[test]
fn secret_keys_only_extractor_leaves_behavior_defaults() {
    let mut cfg = WebSearchConfig::default();
    cfg.provider = "bocha".into();
    cfg.timeout_secs = 99;
    cfg.bocha_api_key = Some(neenee_contracts::SecretString::new("k"));
    let keys = cfg.secret_keys_only();
    assert_eq!(keys.provider, WebSearchConfig::default().provider);
    assert_eq!(keys.timeout_secs, WebSearchConfig::default().timeout_secs);
    assert_eq!(
        keys.bocha_api_key.as_ref().map(|k| k.expose_secret()),
        Some("k")
    );
}
