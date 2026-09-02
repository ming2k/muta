//! Handlers for the wire-level websearch configuration entry points
//! (`AgentRequest::QueryWebSearchConfig` / `UpdateWebSearchConfig`).
//!
//! The `[websearch]` table selects the `websearch` backends and the
//! `read_url` reader (ADR-0117/0118's two-stage research pipeline). Before
//! these handlers existed the table was read exactly once at bootstrap and
//! could only be changed by editing `config.toml` and restarting. The wire
//! entry points make it a live setting like any other:
//!
//! * **Query** replies with [`WebSearchConfigView`] — every behavior field
//!   plus per-key *presence* flags. Plaintext API keys never cross the wire
//!   in a reply.
//! * **Update** is a PATCH: absent fields keep their values. Behavior
//!   fields (`provider`/`fallback`/`reader`/`proxy`/`timeout_secs`/
//!   `searxng_url`) persist to `config.toml`'s `[websearch]`; API keys
//!   persist to `credentials.toml`'s `[websearch]` (the two-file discipline:
//!   config is behavior-only and shareable, credentials hold secrets). An
//!   empty-string key **clears** it.
//! * After persisting, the effective config is pushed into the shared
//!   hot-reload handle ([`muta_contracts::SharedWebSearchConfig`]) so the
//!   live `websearch`/`read_url` tools rebuild their provider chain / HTTP
//!   client on the next call — no toolset rebuild, no restart.
//!
//! Keys sent in an update replace the whole credentials `[websearch]` table
//! *field-wise through `merge_into` semantics* is deliberately NOT the model
//! here: the frontend PATCHes only the keys it wants to change, absent ones
//! keep their stored value, exactly like the behavior fields.

use std::sync::Arc;

use muta_contracts::{
    AgentResponse, SecretString, WebSearchConfig, WebSearchConfigUpdate, WebSearchConfigView,
};
use muta_persistence::config::Config;
use tokio::sync::mpsc;

/// Known search backend names accepted by `[websearch] provider` /
/// `fallback`. Mirrors the tool layer's `KNOWN_PROVIDERS` so a typo is
/// rejected at the wire boundary with a pointing error instead of silently
/// falling back to Exa at call time.
const KNOWN_BACKENDS: &[&str] = &[
    "exa",
    "exa-default",
    "parallel",
    "parallel-default",
    "duckduckgo",
    "duckduckgo-builtin",
    "ddg",
    "searxng",
    "searxng-default",
    "tavily",
    "tavily-default",
    "bocha",
    "bocha-default",
    "none",
    "(none)",
    "disabled",
];

/// Known reader names accepted by `[websearch] reader`.
const KNOWN_READERS: &[&str] = &["jina", "jina-default", "none", "(none)", "disabled"];

fn validate_backend(label: &str, name: &str) -> Result<(), String> {
    if KNOWN_BACKENDS.contains(&name) {
        Ok(())
    } else {
        Err(format!(
            "Unknown {label} backend '{name}'. Known backends: {}.",
            KNOWN_BACKENDS.join(", ")
        ))
    }
}

/// `AgentRequest::QueryWebSearchConfig`.
pub fn query(config: &Config, resp_tx: &mpsc::UnboundedSender<AgentResponse>) {
    let conns = muta_persistence::web_connections::WebConnections::load();
    let view = WebSearchConfigView::from(&config.websearch)
        .with_connections(conns.search_connections, conns.reader_connections);
    let _ = resp_tx.send(AgentResponse::WebSearchConfigSnapshot(view));
}

/// `AgentRequest::UpdateWebSearchConfig`.
///
/// Validates the PATCH, persists behavior fields to `config.toml` and key
/// fields to `credentials.toml`, hot-applies the effective config through
/// the shared handle, and replies with the authoritative post-update view.
/// On any failure the in-memory config is left untouched and an
/// [`AgentResponse::Error`] is sent instead — a bad update never half-applies.
pub async fn update(
    config: &mut Config,
    shared: &Arc<muta_contracts::SharedWebSearchConfig>,
    update: WebSearchConfigUpdate,
    resp_tx: &mpsc::UnboundedSender<AgentResponse>,
) {
    // 1. Validate against current effective config
    let mut next: WebSearchConfig = config.websearch.clone();
    if let Some(provider) = update
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if let Err(err) = validate_backend("primary", provider) {
            let _ = resp_tx.send(AgentResponse::Error(err));
            return;
        }
        next.provider = provider.to_string();
    }
    if let Some(reader) = update
        .reader
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if !KNOWN_READERS.contains(&reader) {
            let _ = resp_tx.send(AgentResponse::Error(format!(
                "Unknown reader '{reader}'. Known readers: jina."
            )));
            return;
        }
        next.reader = reader.to_string();
    }
    if let Some(proxy) = update.proxy.as_deref().map(str::trim) {
        next.proxy = (!proxy.is_empty()).then(|| proxy.to_string());
    }
    if let Some(timeout) = update.timeout_secs {
        next.timeout_secs = timeout.max(1);
    }
    if let Some(url) = update.searxng_url.as_deref().map(str::trim) {
        next.searxng_url = (!url.is_empty()).then(|| url.to_string());
    }
    // Cross-field rule: searxng as provider needs a URL set
    // (now or in this same PATCH).
    let searxng_in_use = next.provider == "searxng";
    if searxng_in_use
        && next
            .searxng_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_none()
    {
        let _ = resp_tx.send(AgentResponse::Error(
            "searxng backend selected but `[websearch].searxng_url` is not set. \
             Provide searxng_url in the same update."
                .to_string(),
        ));
        return;
    }

    // Keys: normalize "some empty string" → clear, "some non-empty" → set.
    fn norm(key: Option<&str>) -> Option<Option<SecretString>> {
        key.map(|k| {
            let trimmed = k.trim();
            (!trimmed.is_empty()).then(|| SecretString::new(trimmed.to_string()))
        })
    }
    if let Some(exa) = norm(update.exa_api_key.as_deref()) {
        next.exa_api_key = exa;
    }
    if let Some(parallel) = norm(update.parallel_api_key.as_deref()) {
        next.parallel_api_key = parallel;
    }
    if let Some(tavily) = norm(update.tavily_api_key.as_deref()) {
        next.tavily_api_key = tavily;
    }
    if let Some(bocha) = norm(update.bocha_api_key.as_deref()) {
        next.bocha_api_key = bocha;
    }
    if let Some(jina) = norm(update.jina_api_key.as_deref()) {
        next.jina_api_key = jina;
    }

    // 2. Persist behavior fields to config.toml and keys to credentials.toml
    // The key fields are `#[serde(skip_serializing)]` on `WebSearchConfig`,
    // so writing `config.websearch = next` and saving cannot leak them into
    // config.toml; they are persisted explicitly below.
    let behavior_only = next.clone();
    config.websearch = behavior_only;
    if let Err(error) = config.save_preserving_connection_selection() {
        let _ = resp_tx.send(AgentResponse::Error(format!(
            "Could not save [websearch] to config.toml: {error}"
        )));
        return;
    }

    let mut creds = muta_persistence::config::Credentials::load();
    let mut keys = creds.websearch.clone();
    // Fold the update's key decisions into the stored table (absent → keep).
    if let Some(exa) = next.exa_api_key.clone() {
        keys.exa_api_key = Some(exa);
    } else if update.exa_api_key.is_some() {
        keys.exa_api_key = None;
    }
    if let Some(parallel) = next.parallel_api_key.clone() {
        keys.parallel_api_key = Some(parallel);
    } else if update.parallel_api_key.is_some() {
        keys.parallel_api_key = None;
    }
    if let Some(tavily) = next.tavily_api_key.clone() {
        keys.tavily_api_key = Some(tavily);
    } else if update.tavily_api_key.is_some() {
        keys.tavily_api_key = None;
    }
    if let Some(bocha) = next.bocha_api_key.clone() {
        keys.bocha_api_key = Some(bocha);
    } else if update.bocha_api_key.is_some() {
        keys.bocha_api_key = None;
    }
    if let Some(jina) = next.jina_api_key.clone() {
        keys.jina_api_key = Some(jina);
    } else if update.jina_api_key.is_some() {
        keys.jina_api_key = None;
    }
    creds.set_websearch_keys(keys);
    if let Err(error) = creds.save() {
        let _ = resp_tx.send(AgentResponse::Error(format!(
            "Could not save [websearch] keys to credentials.toml: {error}"
        )));
        return;
    }

    // 2b. Web connections update
    let mut conns = muta_persistence::web_connections::WebConnections::load();
    let mut conns_modified = false;
    if let Some(conn) = update.upsert_search_connection {
        conns.upsert_search(conn);
        conns_modified = true;
    }
    if let Some(del_id) = update.delete_search_connection {
        conns.remove_search(&del_id);
        conns_modified = true;
    }
    if let Some(conn) = update.upsert_reader_connection {
        conns.upsert_reader(conn);
        conns_modified = true;
    }
    if let Some(del_id) = update.delete_reader_connection {
        conns.remove_reader(&del_id);
        conns_modified = true;
    }
    if conns_modified && let Err(e) = conns.save() {
        tracing::warn!("Could not save web_connections.toml: {e}");
    }

    // 3. Hot-apply through the shared handle
    // `next` still carries the key values (they were folded into the
    // credentials table above), which is exactly what the tools need at
    // call time.
    shared.set(next);
    let view = WebSearchConfigView::from(&config.websearch)
        .with_connections(conns.search_connections, conns.reader_connections);
    let _ = resp_tx.send(AgentResponse::WebSearchConfigUpdated(view));
}

#[cfg(test)]
mod tests {
    use super::*;
    use muta_contracts::AgentRequest;

    /// Install a sandboxed `Dirs` pair; the returned guard holds the
    /// crate-wide test lock until dropped (keeps other tests from racing the
    /// override).
    fn sandbox() -> (tempfile::TempDir, std::sync::MutexGuard<'static, ()>) {
        let guard = muta_persistence::paths::TEST_OVERRIDE_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let root = tempfile::tempdir().unwrap();
        muta_persistence::paths::set_test_default(Some(muta_persistence::paths::Dirs {
            config_dir: root.path().join("config"),
            data_dir: root.path().join("data"),
            state_dir: root.path().join("state"),
            cache_dir: root.path().join("cache"),
            runtime_dir: None,
        }));
        (root, guard)
    }

    fn update_patch() -> WebSearchConfigUpdate {
        WebSearchConfigUpdate {
            provider: Some("tavily".to_string()),
            reader: Some("jina".to_string()),
            proxy: None,
            timeout_secs: Some(30),
            searxng_url: None,
            exa_api_key: None,
            parallel_api_key: None,
            tavily_api_key: Some("tvly-test".to_string()),
            bocha_api_key: None,
            jina_api_key: None,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn update_persists_behaviour_fields_and_hot_applies() {
        let _sandbox = sandbox();
        let mut config = Config::default();
        let shared = Arc::new(muta_contracts::SharedWebSearchConfig::new(
            config.websearch.clone(),
        ));
        let (resp_tx, mut resp_rx) = tokio::sync::mpsc::unbounded_channel();

        update(&mut config, &shared, update_patch(), &resp_tx).await;

        let Some(AgentResponse::WebSearchConfigUpdated(view)) = resp_rx.recv().await else {
            panic!("expected a WebSearchConfigUpdated reply");
        };
        assert_eq!(view.provider, "tavily");
        assert_eq!(view.reader, "jina");
        assert_eq!(view.timeout_secs, 30);
        assert!(view.tavily_api_key_set);
        assert!(!view.exa_api_key_set);
        // Hot-apply reached the shared handle the tools read.
        assert_eq!(shared.get().provider, "tavily");
        assert_eq!(
            shared
                .get()
                .tavily_api_key
                .map(|k| k.expose_secret().to_string()),
            Some("tvly-test".to_string())
        );
        // Persistence: behavior fields in config.toml, key in credentials.
        let reloaded = Config::load();
        assert_eq!(reloaded.websearch.provider, "tavily");
        assert_eq!(reloaded.websearch.reader, "jina");
        let creds = muta_persistence::config::Credentials::load();
        assert_eq!(
            creds
                .websearch
                .tavily_api_key
                .map(|k| k.expose_secret().to_string()),
            Some("tvly-test".to_string())
        );
    }

    #[tokio::test]
    async fn update_rejects_unknown_backend_without_touching_state() {
        let _sandbox = sandbox();
        let mut config = Config::default();
        config.websearch.provider = "exa".to_string();
        let before = config.websearch.clone();
        let shared = Arc::new(muta_contracts::SharedWebSearchConfig::new(before.clone()));
        let (resp_tx, mut resp_rx) = tokio::sync::mpsc::unbounded_channel();

        let bad = WebSearchConfigUpdate {
            provider: Some("brave".to_string()), // not a known backend
            ..Default::default()
        };
        update(&mut config, &shared, bad, &resp_tx).await;

        let Some(AgentResponse::Error(err)) = resp_rx.recv().await else {
            panic!("expected an error reply");
        };
        assert!(
            err.contains("brave"),
            "error should name the bad backend: {err}"
        );
        // Nothing applied or persisted.
        assert_eq!(config.websearch.provider, "exa");
        assert_eq!(shared.get().provider, "exa");
    }

    #[tokio::test]
    async fn update_rejects_searxng_without_url() {
        let _sandbox = sandbox();
        let mut config = Config::default();
        let shared = Arc::new(muta_contracts::SharedWebSearchConfig::new(
            config.websearch.clone(),
        ));
        let (resp_tx, mut resp_rx) = tokio::sync::mpsc::unbounded_channel();

        let bad = WebSearchConfigUpdate {
            provider: Some("searxng".to_string()),
            ..Default::default()
        };
        update(&mut config, &shared, bad, &resp_tx).await;

        let Some(AgentResponse::Error(err)) = resp_rx.recv().await else {
            panic!("expected an error reply");
        };
        assert!(
            err.contains("searxng_url"),
            "error should point at the missing url: {err}"
        );
    }

    #[tokio::test]
    async fn empty_string_clears_a_stored_key() {
        let _sandbox = sandbox();
        let mut config = Config::default();
        config.websearch.exa_api_key = Some(SecretString::new("exa-1"));
        let shared = Arc::new(muta_contracts::SharedWebSearchConfig::new(
            config.websearch.clone(),
        ));
        let (resp_tx, mut resp_rx) = tokio::sync::mpsc::unbounded_channel();

        let patch = WebSearchConfigUpdate {
            exa_api_key: Some("   ".to_string()), // whitespace-only clears
            ..Default::default()
        };
        update(&mut config, &shared, patch, &resp_tx).await;

        let Some(AgentResponse::WebSearchConfigUpdated(view)) = resp_rx.recv().await else {
            panic!("expected a WebSearchConfigUpdated reply");
        };
        assert!(!view.exa_api_key_set);
        assert!(shared.get().exa_api_key.is_none());
        let creds = muta_persistence::config::Credentials::load();
        assert!(creds.websearch.exa_api_key.is_none());
    }

    #[tokio::test]
    async fn query_replies_with_presence_only_view() {
        let _sandbox = sandbox();
        let mut config = Config::default();
        config.websearch.bocha_api_key = Some(SecretString::new("bocha-secret"));
        let (resp_tx, mut resp_rx) = tokio::sync::mpsc::unbounded_channel();

        query(&config, &resp_tx);

        let Some(AgentResponse::WebSearchConfigSnapshot(view)) = resp_rx.recv().await else {
            panic!("expected a WebSearchConfigSnapshot reply");
        };
        assert!(view.bocha_api_key_set);
        // The serialized view must not carry the plaintext anywhere.
        let json = serde_json::to_string(&view).unwrap();
        assert!(!json.contains("bocha-secret"), "secret leaked: {json}");
    }

    #[test]
    fn request_round_trips_through_serde() {
        // The PATCH must survive the wire both ways with absent fields
        // staying absent (never explicit nulls).
        let req = AgentRequest::UpdateWebSearchConfig(Box::new(WebSearchConfigUpdate {
            provider: Some("bocha".to_string()),
            bocha_api_key: Some("sk-1".to_string()),
            ..Default::default()
        }));
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"UpdateWebSearchConfig\""));
        assert!(
            !json.contains("fallback"),
            "absent fields stay absent: {json}"
        );
        let back: AgentRequest = serde_json::from_str(&json).unwrap();
        let AgentRequest::UpdateWebSearchConfig(update) = back else {
            panic!("wrong variant round-tripped");
        };
        assert_eq!(update.provider.as_deref(), Some("bocha"));
        assert_eq!(update.bocha_api_key.as_deref(), Some("sk-1"));
        assert!(update.reader.is_none());
    }
}
