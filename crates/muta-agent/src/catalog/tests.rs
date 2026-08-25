//! Unit tests for the catalog modules: runtime derivation of routes from
//! instances + templates + discovery cache, credential resolution, per-route
//! reasoning, the fitted-model overlay, live discovery, and the one-shot
//! legacy migration.

use super::derive::{
    derive_channel, derive_entries, resolve_credential, route_models, transport_for_protocol,
};
use super::legacy::migrate_legacy_state;
use super::picker::channel_model_info;
use super::{
    build_catalog, build_picker_state, discover_provider_models, sync_fitted_model_registry,
};
use muta_contracts::catalog::Transport;
use muta_contracts::{Effort, RemoteModelEndpoint, ThinkingMode};
use muta_persistence::config::{
    Config, Credentials, DiscoveryCache, FittedModelInfo, UserTransport,
};
use muta_persistence::connections::{Connection, Connections};
use muta_persistence::route_settings::RouteSettingsStore;
use muta_providers::{DEEPSEEK_BUILTIN_MODELS, route_for_model};

use std::sync::Mutex;

/// Tests that mutate process-wide env vars or the paths override must
/// serialize against each other so the parallel runner never observes a
/// half-set environment or a foreign `Dirs`.
static ENV_GUARD: Mutex<()> = Mutex::new(());

/// RAII sandbox: holds the `TEST_OVERRIDE_GUARD` and an isolated `Dirs`
/// install for the duration of a test; `Drop` clears the override so later
/// tests see the real roots again.
struct PathsSandbox {
    _guard: std::sync::MutexGuard<'static, ()>,
    _tmp: tempfile::TempDir,
}

impl Drop for PathsSandbox {
    fn drop(&mut self) {
        muta_persistence::paths::set_test_default(None);
    }
}

/// Sandbox the process-wide XDG roots for the duration of a test. Tests that
/// write the instance store / credentials / discovery cache must bind the
/// result to `_sandbox` for the whole body.
fn sandboxed_paths() -> PathsSandbox {
    let guard = muta_persistence::paths::TEST_OVERRIDE_GUARD
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().expect("tempdir");
    let dirs = muta_persistence::paths::Dirs {
        config_dir: tmp.path().join("config"),
        data_dir: tmp.path().join("data"),
        state_dir: tmp.path().join("state"),
        cache_dir: tmp.path().join("cache"),
        runtime_dir: None,
    };
    muta_persistence::paths::set_test_default(Some(dirs));
    PathsSandbox {
        _guard: unsafe {
            std::mem::transmute::<std::sync::MutexGuard<'_, ()>, std::sync::MutexGuard<'static, ()>>(
                guard,
            )
        },
        _tmp: tmp,
    }
}

fn instance(id: &str, preset_id: Option<&str>) -> Connection {
    Connection {
        id: id.to_string(),
        name: Some(id.to_string()),
        preset_id: preset_id.map(str::to_string),
        ..Default::default()
    }
}

// ── derivation ─────────────────────────────────────────────────────────────

#[test]
fn template_instance_derives_models_from_the_template() {
    let deepseek = instance("deepseek", Some("deepseek"));
    let models = route_models(&deepseek, &DiscoveryCache::default());
    assert_eq!(models, DEEPSEEK_BUILTIN_MODELS);
    // Discovery-enabled templates without a cache fall back to the snapshot.
    let openai = instance("openai", Some("openai"));
    assert!(!route_models(&openai, &DiscoveryCache::default()).is_empty());
}

#[test]
fn discovered_model_list_prefers_the_cache() {
    let mut cache = DiscoveryCache::default();
    cache.connection_models.insert(
        "deepseek".to_string(),
        vec!["deepseek-v4-flash".to_string()],
    );
    let deepseek = instance("deepseek", Some("deepseek"));
    assert_eq!(route_models(&deepseek, &cache), vec!["deepseek-v4-flash"]);
}

#[test]
fn custom_instance_serves_its_declared_models() {
    let mut custom = instance("relay", None);
    custom.models = vec!["a".to_string(), "b".to_string()];
    custom.transport = Some(UserTransport::OpenAi);
    custom.base_url = Some("https://relay.example.com/v1/chat/completions".to_string());
    assert_eq!(
        route_models(&custom, &DiscoveryCache::default()),
        vec!["a", "b"]
    );
    let entry = derive_entries(
        &Connections {
            connections: vec![custom],
        },
        &DiscoveryCache::default(),
        &RouteSettingsStore::default(),
        &Credentials::default(),
    )
    .pop()
    .expect("one entry");
    assert_eq!(entry.name, "relay");
    assert_eq!(entry.channels.len(), 2);
    assert!(matches!(
        entry.channels[0].transport,
        Transport::OpenAi { .. }
    ));
}

#[test]
fn deepseek_route_is_the_responses_transport() {
    let deepseek = instance("deepseek", Some("deepseek"));
    let channel = derive_channel(
        &deepseek,
        "deepseek-v4-flash",
        &DiscoveryCache::default(),
        &RouteSettingsStore::default(),
        &Credentials::default(),
    );
    match &channel.transport {
        Transport::OpenAiResponses { base_url, .. } => {
            assert_eq!(base_url, "https://api.deepseek.com/v1/responses");
        }
        other => panic!("expected Responses transport, got {other:?}"),
    }
}

#[test]
fn opencode_go_routes_models_by_wire_format() {
    // The opencode-go relay serves models over different wire formats; the
    // derivation routes each by its registered format.
    let go = instance("opencode-go", Some("opencode-go"));
    let glm = derive_channel(
        &go,
        "glm-5.2",
        &DiscoveryCache::default(),
        &RouteSettingsStore::default(),
        &Credentials::default(),
    );
    assert!(
        matches!(&glm.transport, Transport::OpenAi { base_url, .. } if base_url == "https://opencode.ai/zen/go/v1/chat/completions"),
        "glm-5.2 must route to OpenAI chat-completions"
    );
    let minimax = derive_channel(
        &go,
        "minimax-m3",
        &DiscoveryCache::default(),
        &RouteSettingsStore::default(),
        &Credentials::default(),
    );
    assert!(
        matches!(&minimax.transport, Transport::Anthropic { base_url, .. } if base_url == "https://opencode.ai/zen/go/v1/messages"),
        "minimax-m3 must route to Anthropic /messages"
    );
    // route_for_model agrees (the standalone resolver used by discovery).
    assert_eq!(
        route_for_model("opencode-go", "minimax-m3").map(|(p, b, _)| (p, b)),
        Some(("anthropic", "https://opencode.ai/zen/go/v1/messages"))
    );
}

#[test]
fn instance_base_url_override_wins_over_the_template_default() {
    let mut deepseek = instance("deepseek", Some("deepseek"));
    deepseek.base_url = Some("https://relay.example.com/v1/responses".to_string());
    let channel = derive_channel(
        &deepseek,
        "deepseek-v4-flash",
        &DiscoveryCache::default(),
        &RouteSettingsStore::default(),
        &Credentials::default(),
    );
    match &channel.transport {
        Transport::OpenAiResponses { base_url, .. } => {
            assert_eq!(base_url, "https://relay.example.com/v1/responses");
        }
        other => panic!("expected Responses transport, got {other:?}"),
    }
}

#[test]
fn credential_resolves_env_then_credentials_then_empty() {
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let _sandbox = sandboxed_paths();
    let mut creds = Credentials::default();
    creds.set_api_key("deepseek", Some("from-file".into()));
    creds.save().unwrap();

    // No env → the stored credential.
    unsafe {
        std::env::remove_var("DEEPSEEK_API_KEY");
    }
    let deepseek = instance("deepseek", Some("deepseek"));
    assert_eq!(
        resolve_credential(&deepseek, &creds).expose_secret(),
        "from-file"
    );

    // `api_key_env` set and populated → env wins.
    let mut env_instance = instance("deepseek", Some("deepseek"));
    env_instance.api_key_env = Some("DEEPSEEK_API_KEY".to_string());
    unsafe {
        std::env::set_var("DEEPSEEK_API_KEY", "from-env");
    }
    assert_eq!(
        resolve_credential(&env_instance, &creds).expose_secret(),
        "from-env"
    );

    // No credential anywhere → empty (a keyless relay sends no bearer).
    let bare = instance("relay", None);
    assert!(
        resolve_credential(&bare, &Credentials::default())
            .expose_secret()
            .is_empty()
    );
}

#[test]
fn reasoning_route_settings_apply_to_anthropic_routes() {
    let cache = DiscoveryCache::default();
    let mut routes = RouteSettingsStore::default();
    routes
        .settings_for_mut("anthropic", "claude-opus-4-8")
        .effort = Some("max".to_string());
    routes
        .settings_for_mut("anthropic", "claude-opus-4-8")
        .thinking = Some(false);

    let anthropic = instance("anthropic", Some("anthropic"));
    let channel = derive_channel(
        &anthropic,
        "claude-opus-4-8",
        &cache,
        &routes,
        &Credentials::default(),
    );
    match &channel.transport {
        Transport::Anthropic {
            effort, thinking, ..
        } => {
            assert_eq!(*effort, Some(Effort::Max));
            assert_eq!(*thinking, Some(ThinkingMode::Off), "explicit off wins");
        }
        other => panic!("expected Anthropic transport, got {other:?}"),
    }
    // A sibling model with no entry stays at the opt-in default (off).
    let sonnet = derive_channel(
        &anthropic,
        "claude-sonnet-4-6",
        &cache,
        &routes,
        &Credentials::default(),
    );
    match &sonnet.transport {
        Transport::Anthropic {
            effort, thinking, ..
        } => {
            assert!(effort.is_none());
            assert!(thinking.is_none());
        }
        other => panic!("expected Anthropic transport, got {other:?}"),
    }
}

#[test]
fn copilot_route_uses_remote_endpoint_metadata() {
    let mut cache = DiscoveryCache::default();
    cache.remote_metadata.insert("copilot".to_string(), {
        let mut m = std::collections::BTreeMap::new();
        m.insert(
            "gpt-5".to_string(),
            muta_contracts::RemoteModelMetadata {
                endpoint: Some(RemoteModelEndpoint::Responses),
                ..Default::default()
            },
        );
        m
    });
    let copilot = Connection {
        id: "copilot".to_string(),
        preset_id: Some("copilot-oauth".to_string()),
        auth: muta_contracts::ChannelAuth::CopilotOAuth,
        ..Default::default()
    };
    let channel = derive_channel(
        &copilot,
        "gpt-5",
        &cache,
        &RouteSettingsStore::default(),
        &Credentials::default(),
    );
    assert!(
        matches!(
            &channel.transport,
            Transport::OpenAiResponses { copilot: true, .. }
        ),
        "advertised Responses endpoint routes to the Responses transport"
    );
}

// ── picker ─────────────────────────────────────────────────────────────────

#[test]
fn build_picker_state_reflects_instances() {
    let _sandbox = sandboxed_paths();
    let instances = Connections {
        connections: vec![instance("deepseek", Some("deepseek"))],
    };
    instances.save().unwrap();
    let config = Config {
        default_connection: "deepseek".to_string(),
        default_model: Some("deepseek-v4-flash".to_string()),
        ..Default::default()
    };
    let snapshot = build_picker_state(
        &config,
        &muta_persistence::connection_usage::ConnectionUsage::default(),
    );
    assert_eq!(snapshot.default_id, "deepseek");
    let row = snapshot
        .rows
        .iter()
        .find(|r| r.id == "deepseek")
        .expect("deepseek row");
    assert_eq!(row.name, "deepseek");
    assert_eq!(row.preset_id, "deepseek");
    assert!(row.models.contains(&"deepseek-v4-flash".to_string()));
}

#[test]
fn channel_model_info_effort_ladders_survive() {
    // A Gemini model advertises an effort ladder, so its picker row exposes an
    // effort defaulting to `high` (the ladder's top rung) when unset.
    let gemini37 = muta_contracts::catalog::Channel {
        id: "default".to_string(),
        label: "gemini-3.7-flash".to_string(),
        transport: Transport::Google {
            base_url: "https://cloudcode-pa.googleapis.com".to_string(),
            user_agent: "antigravity/1.23.2 windows/amd64".to_string(),
            effort: None,
            project_id: None,
        },
        api_key: "".into(),
        model: "gemini-3.7-flash".to_string(),
        remote: None,
    };
    let info = channel_model_info(&gemini37);
    assert_eq!(info.protocol, "google");
    assert_eq!(info.effort.as_deref(), Some("high"));
    assert_eq!(info.thinking, None);
}

// ── discovery + fitted overlay ─────────────────────────────────────────────

#[tokio::test]
async fn live_discovery_writes_the_per_instance_cache() {
    let _sandbox = sandboxed_paths();
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/v1/models")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"data":[{"id":"deepseek-v4-flash"},{"id":"deepseek-v4-pro"}]}"#)
        .create_async()
        .await;

    // An instance pointed at the mock; its base_url override feeds discovery.
    let instances = Connections {
        connections: vec![Connection {
            id: "deepseek".to_string(),
            preset_id: Some("deepseek".to_string()),
            base_url: Some(format!("{}/v1/responses", server.url())),
            ..Default::default()
        }],
    };
    instances.save().unwrap();
    let mut creds = Credentials::default();
    creds.set_api_key("deepseek", Some("sk-test".into()));
    creds.save().unwrap();

    let outcome = discover_provider_models().await;
    assert!(outcome.changed, "discovery must record a change");
    assert!(outcome.failures.is_empty());

    let cache = DiscoveryCache::load();
    assert_eq!(
        cache.connection_models.get("deepseek").map(|m| m.len()),
        Some(2),
        "the discovered list lands in the cache"
    );
    assert!(cache.connection_models["deepseek"].contains(&"deepseek-v4-flash".to_string()));
}

#[tokio::test]
async fn discovery_failure_keeps_the_previous_subset_and_reports() {
    let _sandbox = sandboxed_paths();
    let mut server = mockito::Server::new_async().await;
    server
        .mock("GET", "/v1/models")
        .with_status(401)
        .with_body("Authentication Fails")
        .create_async()
        .await;

    let instances = Connections {
        connections: vec![Connection {
            id: "deepseek".to_string(),
            preset_id: Some("deepseek".to_string()),
            base_url: Some(format!("{}/v1/responses", server.url())),
            ..Default::default()
        }],
    };
    instances.save().unwrap();

    let outcome = discover_provider_models().await;
    assert!(!outcome.changed);
    assert_eq!(outcome.failures.len(), 1);
    assert_eq!(outcome.failures[0].0, "deepseek");
    // The previous subset is untouched (there was none → snapshot still wins).
    let cache = DiscoveryCache::load();
    assert!(cache.connection_models.is_empty());
}

#[test]
fn sync_fitted_model_registry_overlays_fitted_ids() {
    let _sandbox = sandboxed_paths();
    let instances = Connections {
        connections: vec![instance("kimi", Some("kimi-code"))],
    };
    instances.save().unwrap();
    let mut cache = DiscoveryCache::default();
    cache.fitted_models.insert("kimi".to_string(), {
        let mut m = std::collections::BTreeMap::new();
        m.insert(
            "kimi-for-coding".to_string(),
            FittedModelInfo {
                context_window: 262_144,
                reasoning: true,
                vision: true,
                efforts: vec!["max".to_string()],
            },
        );
        m
    });
    cache.save().unwrap();

    sync_fitted_model_registry();
    let resolved = muta_contracts::model::resolve("kimi-for-coding");
    assert_eq!(resolved.context_window, 262_144);
    assert!(resolved.reasoning());
}

// ── legacy migration ───────────────────────────────────────────────────────

#[test]
fn legacy_state_migrates_to_instances_credentials_and_route_facts() {
    let _sandbox = sandboxed_paths();
    let dirs = muta_persistence::paths::get();
    std::fs::create_dir_all(&dirs.config_dir).unwrap();
    std::fs::write(
        dirs.config_file(),
        r#"default_provider = "deepseek"
default_model = "deepseek-v4-flash"
[model_reasoning."deepseek-v4-flash"]
effort = "max"
[[providers]]
id = "deepseek"
name = "DeepSeek"
template_id = "deepseek"
[[providers.channels]]
label = "deepseek-v4-flash"
transport = "OpenAiResponses"
model = "deepseek-v4-flash"
base_url = "https://api.deepseek.com/v1/responses"
auth = "ApiKey"
[[providers.channels]]
label = "deepseek-v4-pro"
transport = "OpenAiResponses"
model = "deepseek-v4-pro"
base_url = "https://api.deepseek.com/v1/responses"
auth = "ApiKey"
effort = "high"
"#,
    )
    .unwrap();
    std::fs::write(
        dirs.credentials_file(),
        r#"[user.deepseek]
api_key = "sk-legacy"
"#,
    )
    .unwrap();

    assert!(migrate_legacy_state(), "migration must run on legacy data");
    // Idempotent: the store now exists, a second call is a no-op.
    assert!(!migrate_legacy_state());

    let instances = Connections::load();
    assert_eq!(instances.connections.len(), 1);
    let deepseek = &instances.connections[0];
    assert_eq!(deepseek.id, "deepseek");
    assert_eq!(deepseek.preset_id.as_deref(), Some("deepseek"));
    // Preset connections do not duplicate their derived model set.
    assert!(
        deepseek.models.is_empty(),
        "routes are derived, not persisted"
    );

    let creds = Credentials::load();
    assert_eq!(
        creds.api_key("deepseek").map(|k| k.expose_secret()),
        Some("sk-legacy")
    );

    // Per-channel reasoning rode into the route store (state, not cache).
    let routes = RouteSettingsStore::load();
    assert_eq!(
        routes
            .settings_for("deepseek", "deepseek-v4-pro")
            .and_then(|r| r.effort.as_deref()),
        Some("high")
    );
    // Legacy `[model_reasoning]` applied to every serving instance (it wins
    // over the per-channel value, since it is applied last).
    assert_eq!(
        routes
            .settings_for("deepseek", "deepseek-v4-flash")
            .and_then(|r| r.effort.as_deref()),
        Some("max")
    );
}

// ── helpers ────────────────────────────────────────────────────────────────

#[test]
fn transport_for_protocol_maps_wire_labels() {
    assert_eq!(
        transport_for_protocol("anthropic"),
        UserTransport::Anthropic
    );
    assert_eq!(transport_for_protocol("google"), UserTransport::Google);
    assert_eq!(
        transport_for_protocol("openai-responses"),
        UserTransport::OpenAiResponses
    );
    assert_eq!(transport_for_protocol("openai"), UserTransport::OpenAi);
}

#[test]
fn catalog_builds_from_the_state_store_only() {
    let _sandbox = sandboxed_paths();
    let instances = Connections {
        connections: vec![instance("deepseek", Some("deepseek"))],
    };
    instances.save().unwrap();
    let entries = build_catalog();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, "deepseek");
    // A config without any provider info still derives from the store.
    let _empty = build_catalog();
}

#[test]
fn antigravity_models_derivation_and_hidden_filter() {
    let _sandbox = sandboxed_paths();
    let mut conn = instance("g11", Some("antigravity-oauth"));
    conn.auth = muta_contracts::ChannelAuth::AntigravityOAuth;
    let connections = Connections {
        connections: vec![conn],
    };
    connections.save().unwrap();

    let mut config = Config::default();
    config.default_connection = "g11".to_string();
    config.default_model = Some("gemini-3.7-flash".to_string());
    config.hidden_models = vec!["gemini-3.6-flash*".to_string(), "gemini-3-flash*".to_string()];

    let usage = muta_persistence::connection_usage::ConnectionUsage::default();
    let picker = build_picker_state(&config, &usage);
    let g11_row = picker.rows.iter().find(|r| r.id == "g11").expect("g11 in picker");

    assert!(g11_row.models.contains(&"gemini-3.7-flash".to_string()));
    assert!(!g11_row.models.iter().any(|m| m.starts_with("gemini-3.6-flash")));
    assert!(!g11_row.models.iter().any(|m| m.starts_with("gemini-3-flash")));
}

