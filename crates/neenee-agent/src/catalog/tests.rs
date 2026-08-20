//! Unit tests for the catalog modules.

use super::discovery::{
    default_model_source_for_spec, discover_provider_models, persist_remote_model_metadata,
    reconcile_provider_models, supported_model_intersection, supported_models_for_template,
    sync_fitted_model_registry,
};
use super::migrate::{
    migrate_deepseek_channels_to_responses, migrate_legacy_provider_instances,
    opencode_go_seed_channels,
};
use super::picker::{build_picker_state, channel_model_info};
use super::translate::user_channel_to_channel;
use super::{
    build_catalog, build_provider_for, build_provider_for_model, default_provider_id,
    models_for_provider, resolved_model_name, resolved_model_name_with_usage,
};
use neenee_contracts::catalog::{Channel, Transport};
use neenee_contracts::{Effort, RemoteModelEndpoint, SecretString, ThinkingMode};
use neenee_persistence::config::{
    Config, FittedModelInfo, ModelSource, UserChannelConfig, UserProviderConfig, UserTransport,
};
use neenee_providers::{
    DEEPSEEK_BUILTIN_MODELS, KIMI_CODE_MODELS, OPENCODE_GO_MODELS, OPENCODE_GO_SERVED_MODELS,
    OPENCODE_USER_AGENT, ZCODE_USER_AGENT, provider_template_spec,
};

use super::*;
#[cfg(test)]
use neenee_providers::OPENAI_PROVIDER_SPECS;
use std::sync::Mutex;

/// Tests that mutate process-wide env vars (`*_API_KEY`, `*_MODEL`)
/// must serialize against each other so the parallel test runner never
/// observes a half-set environment. Mirrors the `ENV_GUARD` pattern in
/// `paths.rs`.
static ENV_GUARD: Mutex<()> = Mutex::new(());

/// RAII sandbox: holds the `TEST_OVERRIDE_GUARD` and an isolated `Dirs`
/// install for the duration of a test; `Drop` clears the override so
/// later tests see the real roots again, and releases the guard so a
/// parallel override-touching test can proceed.
struct PathsSandbox {
    _guard: std::sync::MutexGuard<'static, ()>,
    _tmp: tempfile::TempDir,
}

impl Drop for PathsSandbox {
    fn drop(&mut self) {
        neenee_persistence::paths::set_test_default(None);
    }
}

/// Sandbox the process-wide XDG roots for the duration of a discovery
/// test. `discover_provider_models` persists a `DiscoveryCache` through
/// `paths::get()` on `changed`, so without an override a test writes its
/// `test-instance` rows into the developer's real
/// `$XDG_CACHE_HOME/neenee/models_discovery.json`. Bind the result to
/// `_sandbox` for the whole test body.
fn sandboxed_paths() -> PathsSandbox {
    let guard = neenee_persistence::paths::TEST_OVERRIDE_GUARD
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = tempfile::tempdir().expect("tempdir");
    let dirs = neenee_persistence::paths::Dirs {
        config_dir: tmp.path().join("config"),
        data_dir: tmp.path().join("data"),
        state_dir: tmp.path().join("state"),
        cache_dir: tmp.path().join("cache"),
        runtime_dir: None,
    };
    neenee_persistence::paths::set_test_default(Some(dirs));
    // SAFETY: the guard locks `TEST_OVERRIDE_GUARD`, a `static` Mutex, so
    // the lock outlives any borrow; the transmute only erases the
    // lifetime so the sandbox can own the guard without a lifetime
    // parameter. It is released exactly once, in Drop.
    PathsSandbox {
        _guard: unsafe {
            std::mem::transmute::<std::sync::MutexGuard<'_, ()>, std::sync::MutexGuard<'static, ()>>(
                guard,
            )
        },
        _tmp: tmp,
    }
}

/// A config with no keys or model overrides set beyond the built-in
/// defaults, so every field resolves predictably.
fn bare_config() -> Config {
    Config::default()
}

#[test]
fn google_channel_surfaces_effort_from_the_model_ladder() {
    // A Gemini 3.x model advertises a `thinkingLevel` ladder
    // (EFFORT_GEMINI_LEVEL), so its picker row must expose an effort —
    // defaulting to `high` (the ladder's top rung) when the channel has
    // no explicit override. This is what gates the per-model settings
    // editor (`e` in the Models picker) for Antigravity channels.
    let gemini37 = Channel {
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

    // The channel's explicit override wins over the ladder default.
    let mut overridden = gemini37.clone();
    if let Transport::Google { effort, .. } = &mut overridden.transport {
        *effort = Some(Effort::Low);
    }
    assert_eq!(
        channel_model_info(&overridden).effort.as_deref(),
        Some("low")
    );

    // A Gemini model with no effort ladder (an unknown id no baseline
    // knows) must stay inert — no effort surfaced, no editor offered.
    let unknown = Channel {
        model: "gemini-9-nano-nonexistent".to_string(),
        ..gemini37
    };
    assert_eq!(channel_model_info(&unknown).effort, None);
}

#[test]
fn empty_config_has_no_provider_instances() {
    let config = bare_config();
    assert!(build_catalog(&config).is_empty());
    assert_eq!(
        build_picker_state(&config, &ProviderUsage::default())
            .rows
            .len(),
        0
    );
    assert!(build_provider_for(&config, default_provider_id(&config)).is_none());
}

#[test]
fn legacy_builtin_key_migrates_to_named_instance() {
    let mut config = bare_config();
    config.default_provider = "openai".to_string();
    config.default_model = Some("gpt-5.4-mini".to_string());
    config.openai_api_key = Some("sk-old".into());

    assert!(migrate_legacy_provider_instances(&mut config));
    assert!(config.openai_api_key.is_none());
    // `default_model` is a live field (the switch handler persists it), so
    // the migration must NOT strip it — only seed the instance's default
    // channel from it.
    assert_eq!(config.default_model.as_deref(), Some("gpt-5.4-mini"));
    assert_eq!(config.default_provider, "openai");

    let entry = build_catalog(&config)
        .into_iter()
        .find(|entry| entry.id == "openai")
        .expect("migrated openai instance");
    assert_eq!(entry.name, "OpenAI");
    assert_eq!(entry.default_channel().unwrap().model, "gpt-5.4-mini");
    assert_eq!(entry.default_channel().unwrap().api_key, "sk-old");
    assert!(!entry.builtin);
}

#[test]
fn migration_strips_legacy_model_slots_but_preserves_default_model() {
    let mut config = bare_config();
    config.default_provider = "kimi-code".to_string();
    config.default_model = Some("k3".to_string());
    config.moonshot_model = Some("k3".to_string());
    // An existing kimi-code instance (created by an earlier migration or
    // the add-provider flow) — the migration has nothing to create, only
    // legacy fields to strip.
    config.providers.push(UserProviderConfig {
        id: "kimi-code".to_string(),
        channels: vec![UserChannelConfig {
            label: "k3".to_string(),
            model: Some("k3".to_string()),
            ..Default::default()
        }],
        ..Default::default()
    });

    assert!(migrate_legacy_provider_instances(&mut config));
    // Legacy per-provider model slots are consumed…
    assert!(config.moonshot_model.is_none());
    // …but the persisted global model pointer survives, so a fresh
    // session lands on the model the user last switched to.
    assert_eq!(config.default_model.as_deref(), Some("k3"));
    assert_eq!(config.default_provider, "kimi-code");
}

#[test]
fn deepseek_channels_migrate_to_the_responses_transport() {
    let mut config = bare_config();
    // An existing deepseek-template instance still on the official
    // chat-completions endpoint, plus a custom-relay deepseek channel and
    // an unrelated provider that must both stay untouched.
    config.providers.push(UserProviderConfig {
        id: "deepseek".to_string(),
        channels: vec![
            UserChannelConfig {
                label: "deepseek-v4-flash".to_string(),
                transport: UserTransport::OpenAi,
                model: Some("deepseek-v4-flash".to_string()),
                base_url: Some("https://api.deepseek.com/v1/chat/completions".to_string()),
                ..Default::default()
            },
            UserChannelConfig {
                label: "deepseek-v4-pro".to_string(),
                transport: UserTransport::OpenAi,
                model: Some("deepseek-v4-pro".to_string()),
                base_url: Some("https://relay.example.com/v1/chat/completions".to_string()),
                ..Default::default()
            },
        ],
        template_id: Some("deepseek".to_string()),
        ..Default::default()
    });

    assert!(migrate_deepseek_channels_to_responses(&mut config));
    let channels = &config.providers[0].channels;
    // The official-endpoint channel flips to the Responses transport + URL.
    assert_eq!(channels[0].transport, UserTransport::OpenAiResponses);
    assert_eq!(
        channels[0].base_url.as_deref(),
        Some("https://api.deepseek.com/v1/responses")
    );
    // A custom relay keeps chat completions — it may not proxy /responses.
    assert_eq!(channels[1].transport, UserTransport::OpenAi);
    assert_eq!(
        channels[1].base_url.as_deref(),
        Some("https://relay.example.com/v1/chat/completions")
    );
    // Idempotent: a second pass changes nothing.
    assert!(!migrate_deepseek_channels_to_responses(&mut config));
}

#[test]
fn deepseek_responses_migration_covers_untracked_official_channels() {
    // Even a pure-custom instance aimed at the official DeepSeek
    // chat-completions URL migrates — the URL unambiguously identifies the
    // official endpoint, which natively speaks Responses.
    let mut config = bare_config();
    config.providers.push(UserProviderConfig {
        id: "my-deepseek".to_string(),
        channels: vec![UserChannelConfig {
            label: "deepseek-v4-pro".to_string(),
            transport: UserTransport::OpenAi,
            model: Some("deepseek-v4-pro".to_string()),
            base_url: Some("https://api.deepseek.com/v1/chat/completions".to_string()),
            ..Default::default()
        }],
        ..Default::default()
    });

    assert!(migrate_deepseek_channels_to_responses(&mut config));
    let channel = &config.providers[0].channels[0];
    assert_eq!(channel.transport, UserTransport::OpenAiResponses);
    assert_eq!(
        channel.base_url.as_deref(),
        Some("https://api.deepseek.com/v1/responses")
    );
}

#[test]
fn api_key_responses_channel_round_trips_its_protocol_label() {
    // The edit form pre-fills `protocol` from the picker row and the save
    // handler maps it back to a transport. An API-key Responses channel
    // must round-trip as "openai-responses" — plain "openai" would
    // silently downgrade the channel to chat completions on save.
    let mut config = bare_config();
    config.providers.push(UserProviderConfig {
        id: "deepseek".to_string(),
        channels: vec![UserChannelConfig {
            label: "deepseek-v4-pro".to_string(),
            transport: UserTransport::OpenAiResponses,
            api_key: Some("sk-test".into()),
            model: Some("deepseek-v4-pro".to_string()),
            base_url: Some("https://api.deepseek.com/v1/responses".to_string()),
            ..Default::default()
        }],
        template_id: Some("deepseek".to_string()),
        ..Default::default()
    });
    let picker = build_picker_state(&config, &ProviderUsage::default());
    let row = picker
        .rows
        .iter()
        .find(|row| row.id == "deepseek")
        .expect("deepseek row");
    assert_eq!(row.protocol, "openai-responses");
    assert_eq!(row.base_url, "https://api.deepseek.com/v1/responses");
    // The per-model info still reports the OpenAI surface so the picker's
    // effort gating treats it like any OpenAI-family channel.
    let info = row
        .model_info
        .iter()
        .find(|info| info.model == "deepseek-v4-pro")
        .expect("model info");
    assert_eq!(info.protocol, "openai");
    assert_eq!(info.effort.as_deref(), Some("high"));
}

/// A provider instance created from a template, pre-stamped with its
/// `template_id`. Used to exercise model reconciliation without depending
/// on the live `PROVIDER_TEMPLATE_SPECS` model lists (which evolve).
fn template_instance(tid: &str, models: &[&str]) -> UserProviderConfig {
    UserProviderConfig {
        id: "test-instance".to_string(),
        name: Some("Test".to_string()),
        channels: models
            .iter()
            .map(|m| UserChannelConfig {
                label: m.to_string(),
                transport: UserTransport::OpenAi,
                api_key_env: None,
                api_key: Some("sk-test".into()),
                model: Some(m.to_string()),
                base_url: Some("https://relay.example.com/v1/chat/completions".to_string()),
                user_agent: None,
                effort: None,
                thinking: None,
                auth: Default::default(),
                remote: None,
            })
            .collect(),
        default_channel: 0,
        template_id: Some(tid.to_string()),
        model_source: Default::default(),
        fitted_models: Default::default(),
    }
}

/// The exact current model ids a known template seeds — read from the live
/// registry so this test tracks template evolution rather than a snapshot.
fn current_template_models(tid: &str) -> Vec<String> {
    provider_template_spec(tid)
        .expect("known template id")
        .models
        .iter()
        .map(|m| m.to_string())
        .collect()
}

#[test]
fn discovery_intersection_keeps_only_supported_models_in_registry_order() {
    let supported = &["model-a", "model-b", "model-c"];
    let available = vec![
        "unknown-cloud-model".to_string(),
        "model-c".to_string(),
        "model-a".to_string(),
    ];

    assert_eq!(
        supported_model_intersection(supported, &available),
        vec!["model-a", "model-c"]
    );
    assert!(supported_model_intersection(supported, &["unknown".to_string()]).is_empty());
}

#[test]
fn template_supported_models_come_from_the_local_table() {
    let openai = supported_models_for_template(provider_template_spec("openai").unwrap());
    assert!(openai.contains(&"gpt-4o"));
    assert!(openai.contains(&"gpt-5.6"));
    assert!(!openai.contains(&"claude-opus-4-8"));

    let anthropic = supported_models_for_template(provider_template_spec("anthropic").unwrap());
    assert!(anthropic.contains(&"claude-opus-4-8"));
    assert!(!anthropic.contains(&"gpt-4o"));
}

#[test]
fn reconcile_noops_when_instance_already_mirrors_template() {
    // An instance whose channels exactly equal the current template models
    // must not be churned (no change reported, channels untouched).
    let models = current_template_models("deepseek");
    let mut config = bare_config();
    config.providers.push(UserProviderConfig {
        id: "relay".to_string(),
        name: Some("Relay".to_string()),
        channels: models
            .iter()
            .map(|m| UserChannelConfig {
                label: m.clone(),
                transport: UserTransport::OpenAi,
                api_key_env: None,
                api_key: Some("sk".into()),
                model: Some(m.clone()),
                base_url: Some("https://relay.example.com".to_string()),
                user_agent: None,
                effort: None,
                thinking: None,
                auth: Default::default(),
                remote: None,
            })
            .collect(),
        default_channel: 0,
        template_id: Some("deepseek".to_string()),
        model_source: Default::default(),
        fitted_models: Default::default(),
    });
    let before_models: Vec<String> = config.providers[0]
        .channels
        .iter()
        .map(|c| c.model.clone().unwrap_or_default())
        .collect();

    assert!(!reconcile_provider_models(&mut config));
    let after_models: Vec<String> = config.providers[0]
        .channels
        .iter()
        .map(|c| c.model.clone().unwrap_or_default())
        .collect();
    assert_eq!(after_models, before_models);
}

#[test]
fn reconcile_drops_models_removed_from_template() {
    // Start with the current template models plus one extra user-added
    // model. After reconcile, the extra is gone — pure-mirror semantics.
    let mut models = current_template_models("deepseek");
    models.push("stale-user-model".to_string());
    let mut config = bare_config();
    config.providers.push(template_instance("deepseek", &{
        let refs: Vec<&str> = models.iter().map(|s| s.as_str()).collect();
        refs
    }));

    assert!(reconcile_provider_models(&mut config));
    let got: Vec<String> = config.providers[0]
        .channels
        .iter()
        .map(|c| c.model.clone().unwrap_or_default())
        .collect();
    assert_eq!(got, current_template_models("deepseek"));
    assert!(
        !got.iter().any(|m| m == "stale-user-model"),
        "extra user model must be dropped on reconcile"
    );
}

#[test]
fn reconcile_adds_new_models_introduced_by_template() {
    // An instance seeded with a strict subset of the template models picks
    // up the missing ones after reconcile — proving template edits propagate
    // forward to existing instances.
    let full = current_template_models("deepseek");
    let subset: Vec<&str> = full.iter().take(1).map(|s| s.as_str()).collect();
    let mut config = bare_config();
    config
        .providers
        .push(template_instance("deepseek", &subset));

    assert!(reconcile_provider_models(&mut config));
    let got: Vec<String> = config.providers[0]
        .channels
        .iter()
        .map(|c| c.model.clone().unwrap_or_default())
        .collect();
    assert_eq!(got, full, "missing template models are added");
    // The shared key configured on the surviving channel is copied onto the
    // newly added channels so the instance keeps working.
    assert!(
        config.providers[0].channels.iter().all(|c| c
            .api_key
            .as_ref()
            .map(SecretString::expose_secret)
            == Some("sk-test")),
        "shared key is preserved across the reseed"
    );
}

#[test]
fn reconcile_api_instance_keeps_last_discovered_supported_subset() {
    let known = supported_models_for_template(provider_template_spec("deepseek").unwrap());
    let subset = [known[1], known[3]];
    let mut instance = template_instance("deepseek", &subset);
    instance.model_source = neenee_persistence::config::ModelSource::Api;
    let mut config = bare_config();
    config.providers.push(instance);

    assert!(
        !reconcile_provider_models(&mut config),
        "startup reconciliation must not expand a persisted Api subset"
    );
    assert_eq!(config.providers[0].channel_models(), subset);
}

#[test]
fn reconcile_api_instance_drops_unsupported_without_expanding_subset() {
    let known = supported_models_for_template(provider_template_spec("deepseek").unwrap());
    let kept = known[2];
    let mut instance = template_instance("deepseek", &[kept, "removed-or-unknown-model"]);
    instance.model_source = neenee_persistence::config::ModelSource::Api;
    let mut config = bare_config();
    config.providers.push(instance);

    assert!(reconcile_provider_models(&mut config));
    assert_eq!(config.providers[0].channel_models(), vec![kept]);
    let channel = &config.providers[0].channels[0];
    assert_eq!(
        channel.api_key.as_ref().map(SecretString::expose_secret),
        Some("sk-test")
    );
    assert_eq!(
        channel.base_url.as_deref(),
        Some("https://relay.example.com/v1/chat/completions")
    );
}

#[test]
fn reconcile_preserves_per_model_reasoning_for_surviving_models() {
    // A model that survives the reseed keeps its effort/thinking knobs; a
    // newly added model starts with reasoning off (ADR-0046).
    let full = current_template_models("anthropic");
    let kept: Vec<&str> = full.iter().take(1).map(|s| s.as_str()).collect();
    let mut config = bare_config();
    let mut inst = template_instance("anthropic", &kept);
    inst.channels[0].transport = UserTransport::Anthropic;
    inst.channels[0].effort = Some("high".to_string());
    inst.channels[0].thinking = Some(true);
    config.providers.push(inst);

    assert!(reconcile_provider_models(&mut config));
    let channels = &config.providers[0].channels;
    let survived = channels
        .iter()
        .find(|c| c.model.as_deref() == Some(kept[0]))
        .expect("surviving model present");
    assert_eq!(survived.effort.as_deref(), Some("high"));
    assert_eq!(survived.thinking, Some(true));
    let added = channels
        .iter()
        .find(|c| c.model.as_deref() != Some(kept[0]))
        .expect("a newly added model exists");
    assert!(added.effort.is_none(), "new model starts with no effort");
    assert!(
        added.thinking.is_none(),
        "new model starts with thinking off"
    );
}

#[test]
fn reconcile_zai_code_fixed_instance_picks_up_newly_added_models() {
    let mut config = bare_config();
    let inst = template_instance("zai-code", &["glm-5.2"]);
    config.providers.push(inst);

    assert!(reconcile_provider_models(&mut config));
    let got = config.providers[0].channel_models();
    assert_eq!(got, vec!["glm-5.3", "glm-5.2"]);
}

#[test]
fn reconcile_upgrades_existing_zai_code_instance_with_api_key_to_glm_5_3() {
    let mut config = bare_config();
    config.providers.push(UserProviderConfig {
        id: "111f".to_string(),
        name: Some("111f".to_string()),
        channels: vec![UserChannelConfig {
            label: "glm-5.2".to_string(),
            transport: UserTransport::OpenAi,
            api_key_env: None,
            api_key: Some("742f4d62404d4f30bc0ed0429f732722.EfnudJ2pfIu4TbRj".into()),
            model: Some("glm-5.2".to_string()),
            base_url: Some("https://api.z.ai/api/coding/paas/v4/chat/completions".to_string()),
            user_agent: Some("opencode/1.17.10".to_string()),
            effort: None,
            thinking: None,
            auth: Default::default(),
            remote: None,
        }],
        default_channel: 0,
        template_id: Some("zai-code".to_string()),
        model_source: ModelSource::Fixed,
        fitted_models: Default::default(),
    });

    assert!(reconcile_provider_models(&mut config));
    assert_eq!(
        config.providers[0].channel_models(),
        vec!["glm-5.3", "glm-5.2"]
    );
    assert_eq!(
        config.providers[0].channels[0]
            .api_key
            .as_ref()
            .map(SecretString::expose_secret),
        Some("742f4d62404d4f30bc0ed0429f732722.EfnudJ2pfIu4TbRj")
    );
    assert_eq!(
        config.providers[0].channels[0].base_url.as_deref(),
        Some("https://open.bigmodel.cn/api/coding/paas/v4/chat/completions")
    );
    assert_eq!(
        config.providers[0].channels[0].user_agent.as_deref(),
        Some(ZCODE_USER_AGENT)
    );
    assert_eq!(
        config.providers[0].channels[1].base_url.as_deref(),
        Some("https://open.bigmodel.cn/api/coding/paas/v4/chat/completions")
    );
    assert_eq!(
        config.providers[0].channels[1].user_agent.as_deref(),
        Some(ZCODE_USER_AGENT)
    );
    let usage = ProviderUsage::default();
    let picker = build_picker_state(&config, &usage);
    let zai_row = picker.rows.iter().find(|r| r.id == "111f").unwrap();
    assert_eq!(zai_row.models, vec!["glm-5.3", "glm-5.2"]);
}

#[test]
fn reconcile_leaves_unknown_template_id_untouched() {
    // A template_id that no longer resolves (template removed from the
    // codebase) must NOT blank out a working instance — the dangling
    // pointer is ignored so the provider keeps serving its models.
    let mut config = bare_config();
    config.providers.push(UserProviderConfig {
        id: "orphan".to_string(),
        name: Some("Orphan".to_string()),
        channels: vec![UserChannelConfig {
            label: "only-model".to_string(),
            transport: UserTransport::OpenAi,
            api_key_env: None,
            api_key: Some("sk".into()),
            model: Some("only-model".to_string()),
            base_url: Some("https://x.example.com".to_string()),
            user_agent: None,
            effort: None,
            thinking: None,
            auth: Default::default(),
            remote: None,
        }],
        default_channel: 0,
        template_id: Some("removed-template".to_string()),
        model_source: Default::default(),
        fitted_models: Default::default(),
    });
    let before_models: Vec<String> = config.providers[0]
        .channels
        .iter()
        .map(|c| c.model.clone().unwrap_or_default())
        .collect();

    assert!(!reconcile_provider_models(&mut config));
    // Channels, model, key, and the (dangling) template_id are all
    // unchanged — a dangling pointer must not blank a working provider.
    let after_models: Vec<String> = config.providers[0]
        .channels
        .iter()
        .map(|c| c.model.clone().unwrap_or_default())
        .collect();
    assert_eq!(after_models, before_models);
    assert_eq!(config.providers[0].channels.len(), 1);
    assert_eq!(
        config.providers[0].template_id.as_deref(),
        Some("removed-template")
    );
}

#[test]
fn reconcile_leaves_pure_custom_instance_untouched() {
    // A pure-custom instance (no template_id) whose model set does NOT match
    // any template is never re-seeded — user customizations are preserved.
    let mut config = bare_config();
    config
        .providers
        .push(template_instance("", &["alpha", "beta"]));
    config.providers[0].template_id = None;
    let before_models: Vec<String> = config.providers[0]
        .channels
        .iter()
        .map(|c| c.model.clone().unwrap_or_default())
        .collect();

    assert!(!reconcile_provider_models(&mut config));
    // The user's custom models and keys are intact; no template_id stamped.
    let after_models: Vec<String> = config.providers[0]
        .channels
        .iter()
        .map(|c| c.model.clone().unwrap_or_default())
        .collect();
    assert_eq!(after_models, before_models);
    assert_eq!(config.providers[0].template_id, None);
}

#[test]
fn reconcile_backfills_template_id_for_legacy_matching_instance() {
    // A pre-template_id instance whose model set exactly equals a current
    // template gets stamped, so it will track future template edits. The
    // stamp itself is the change.
    let models = current_template_models("deepseek");
    let refs: Vec<&str> = models.iter().map(|s| s.as_str()).collect();
    let mut inst = template_instance("", &refs);
    inst.template_id = None;
    let mut config = bare_config();
    config.providers.push(inst);

    assert!(reconcile_provider_models(&mut config));
    assert_eq!(
        config.providers[0].template_id.as_deref(),
        Some("deepseek"),
        "legacy matching instance is stamped"
    );
}

#[test]
#[ignore = "legacy behavior: built-in providers are now user-added templates"]
fn catalog_contains_every_builtin_preset() {
    let entries = build_catalog(&bare_config());
    let ids: Vec<&str> = entries.iter().map(|e| e.id.as_str()).collect();
    assert!(ids.contains(&"kimi-code"), "missing kimi-code: {ids:?}");
    assert!(ids.contains(&"openai"));
    assert!(ids.contains(&"google"), "missing google: {ids:?}");
    assert!(ids.contains(&"deepseek"), "missing deepseek: {ids:?}");
    assert!(ids.contains(&"opencode-go"), "missing opencode-go: {ids:?}");
    assert!(ids.contains(&"anthropic"), "missing anthropic: {ids:?}");
    // Every registry preset is present.
    for spec in OPENAI_PROVIDER_SPECS {
        assert!(
            entries.iter().find(|e| e.id == spec.id).is_some(),
            "registry preset {} missing",
            spec.id
        );
    }
}

#[test]
fn opencode_go_seed_channels_only_include_models_the_relay_serves() {
    let channels = opencode_go_seed_channels("go-key".into());
    let ids: Vec<&str> = channels.iter().filter_map(|c| c.model.as_deref()).collect();
    // Models registered for other providers but not served by the relay
    // must not be seeded: an unserved channel only answers "model not
    // found" (Kimi k3 is kimi-code-only; glm-4.7 is not on go).
    assert!(!ids.contains(&"k3"), "k3 must not be seeded: {ids:?}");
    assert!(!ids.contains(&"glm-4.7"), "glm-4.7 must not be seeded");
    // Served models in the registry are seeded, each with the transport
    // its wire format implies (one provider, two wire formats).
    for (id, is_anthropic) in [
        ("glm-5.2", false),
        ("kimi-k2.7-code", false),
        ("minimax-m3", true),
    ] {
        let channel = channels
            .iter()
            .find(|c| c.model.as_deref() == Some(id))
            .unwrap_or_else(|| panic!("{id} served by opencode-go"));
        let want_anthropic = matches!(channel.transport, UserTransport::Anthropic);
        assert_eq!(want_anthropic, is_anthropic, "{id} transport");
    }
    // The seed set is exactly the served catalogue the registry knows.
    let mut expected: Vec<&str> = OPENCODE_GO_SERVED_MODELS.to_vec();
    expected.sort_unstable();
    let mut got = ids;
    got.sort_unstable();
    assert_eq!(got, expected);
}

#[test]
#[ignore = "legacy behavior: built-in providers are now user-added templates"]
fn opencode_go_hosts_both_wire_formats() {
    let entries = build_catalog(&bare_config());
    let entry = entries
        .iter()
        .find(|e| e.id == "opencode-go")
        .expect("opencode-go entry");
    // Every served model has its own channel.
    assert!(!entry.channels.is_empty());
    // An OpenAI-format model routes through the OpenAi transport.
    let glm = entry
        .channel_for_model("glm-5.2")
        .expect("glm-5.2 served by opencode-go");
    assert!(
        matches!(
            glm.transport,
            neenee_contracts::catalog::Transport::OpenAi { .. }
        ),
        "glm-5.2 must use OpenAi"
    );
    // An Anthropic-format model routes through the Anthropic transport —
    // the load-bearing detail: one provider, two wire formats.
    let mm = entry
        .channel_for_model("minimax-m3")
        .expect("minimax-m3 served by opencode-go");
    assert!(
        matches!(
            mm.transport,
            neenee_contracts::catalog::Transport::Anthropic { .. }
        ),
        "minimax-m3 must use Anthropic /messages"
    );
}

#[test]
#[ignore = "legacy behavior: built-in providers are now user-added templates"]
fn anthropic_relay_hosts_claude_family_over_messages() {
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }
    let entries = build_catalog(&bare_config());
    let entry = entries
        .iter()
        .find(|e| e.id == "anthropic")
        .expect("anthropic entry");
    // Every Claude model is a channel, all on the Anthropic /messages
    // transport pointed at the configured endpoint.
    assert!(!entry.channels.is_empty());
    let opus = entry
        .channel_for_model("claude-opus-4-8")
        .expect("claude-opus-4-8 served");
    match &opus.transport {
        Transport::Anthropic { base_url, .. } => {
            // Default endpoint is Anthropic's official API.
            assert_eq!(base_url, "https://api.anthropic.com/v1/messages");
        }
        other => panic!("anthropic must use the Anthropic transport, got {other:?}"),
    }
}

#[test]
#[ignore = "legacy behavior: built-in providers are now user-added templates"]
fn anthropic_relay_base_url_is_configurable() {
    // A custom relay address (e.g. a self-hosted proxy) flows through config
    // with no code change — the load-bearing requirement for users whose
    // relay URL differs.
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }
    let mut config = bare_config();
    config.anthropic_base_url = Some("https://relay.example.com/v1/messages".to_string());
    let entries = build_catalog(&config);
    let entry = entries.iter().find(|e| e.id == "anthropic").unwrap();
    let channel = entry.default_channel().expect("default channel");
    match &channel.transport {
        Transport::Anthropic { base_url, .. } => {
            assert_eq!(base_url, "https://relay.example.com/v1/messages");
        }
        other => panic!("expected Anthropic transport, got {other:?}"),
    }
}

#[test]
fn custom_anthropic_model_rows_carry_channel_settings() {
    let mut config = bare_config();
    config
        .providers
        .push(neenee_persistence::config::UserProviderConfig {
            id: "example".to_string(),
            name: Some("Example Claude".to_string()),
            channels: vec![neenee_persistence::config::UserChannelConfig {
                label: "claude-sonnet-4-6".to_string(),
                transport: neenee_persistence::config::UserTransport::Anthropic,
                model: Some("claude-sonnet-4-6".to_string()),
                base_url: Some("https://relay.example.com/v1/messages".to_string()),
                effort: Some("high".to_string()),
                thinking: Some(true),
                ..Default::default()
            }],
            default_channel: 0,
            ..Default::default()
        });

    let picker = build_picker_state(&config, &ProviderUsage::default());
    let row = picker.rows.iter().find(|row| row.id == "example").unwrap();
    let info = row
        .model_info
        .iter()
        .find(|info| info.model == "claude-sonnet-4-6")
        .unwrap();
    assert_eq!(info.protocol, "anthropic");
    assert_eq!(info.effort.as_deref(), Some("high"));
    assert_eq!(info.thinking, Some(true));
}

#[test]
fn resolved_model_honors_per_provider_last_used_model() {
    // A multi-model custom provider: with no config `default_model` and no
    // usage telemetry, the active model is the default channel. After a
    // model is recorded as used under that provider, resolving the active
    // model (via usage) lands on it, and the picker row mirrors it — so a
    // provider re-opens on the exact model it was left at.
    use neenee_persistence::config::{UserChannelConfig, UserProviderConfig, UserTransport};
    let mut config = bare_config();
    config.providers.push(UserProviderConfig {
        id: "relay".to_string(),
        name: Some("Relay".to_string()),
        channels: vec![
            UserChannelConfig {
                label: "alpha".to_string(),
                transport: UserTransport::OpenAi,
                model: Some("alpha".to_string()),
                ..Default::default()
            },
            UserChannelConfig {
                label: "beta".to_string(),
                transport: UserTransport::OpenAi,
                model: Some("beta".to_string()),
                ..Default::default()
            },
        ],
        default_channel: 0,
        ..Default::default()
    });
    config.default_provider = "relay".to_string();

    // No usage → default channel model (alpha).
    assert_eq!(
        resolved_model_name_with_usage(&config, "relay", &ProviderUsage::default()).as_deref(),
        Some("alpha")
    );

    // Record `beta` under `relay`: it becomes the resolved active model.
    let mut usage = ProviderUsage::default();
    usage.record_model("relay", "beta");
    assert_eq!(
        resolved_model_name_with_usage(&config, "relay", &usage).as_deref(),
        Some("beta")
    );

    // The picker row's `model` (the displayed active model) mirrors this.
    let picker = build_picker_state(&config, &usage);
    let row = picker.rows.iter().find(|r| r.id == "relay").unwrap();
    assert_eq!(row.model, "beta");
    // And the stage-2 model list carries beta's info row.
    assert!(row.model_info.iter().any(|i| i.model == "beta"));
}

#[test]
fn startup_model_recording_restores_boot_model_on_next_launch() {
    // Regression for "recently-used model not restored on startup". The
    // OAuth GPT (`chatgpt`) provider is multi-model: a user who boots into
    // a non-default model (e.g. selects `gpt-5.6-terra` while the catalog's
    // default channel is `gpt-5.6-sol`) must, on the *next* launch, reopen
    // on that same model. Restoration works only if startup records the
    // boot model via `record_model` — previously startup recorded only the
    // provider, leaving `last_models` empty, so the next launch fell back
    // to the default-channel model.
    //
    // Modeled here with a generic multi-model "relay" provider (two
    // channels, default channel = first = "alpha"), exactly mirroring the
    // `chatgpt` shape from `CHATGPT_BUILTIN_MODELS`.
    use neenee_persistence::config::{UserChannelConfig, UserProviderConfig, UserTransport};
    let mut config = bare_config();
    config.providers.push(UserProviderConfig {
        id: "relay".to_string(),
        name: Some("Relay".to_string()),
        channels: vec![
            UserChannelConfig {
                label: "alpha".to_string(),
                transport: UserTransport::OpenAi,
                model: Some("alpha".to_string()),
                ..Default::default()
            },
            UserChannelConfig {
                label: "beta".to_string(),
                transport: UserTransport::OpenAi,
                model: Some("beta".to_string()),
                ..Default::default()
            },
        ],
        default_channel: 0,
        ..Default::default()
    });
    config.default_provider = "relay".to_string();
    // Boot into the non-default-channel model "beta" (analogous to a
    // session pin or `default_model` selecting gpt-5.6-terra).
    config.default_model = Some("beta".to_string());

    // The model the startup provider is actually built with — same
    // config-only precedence `build_provider_for` uses, and what
    // `SessionDriver::run` now records via `record_model`.
    let boot_model = resolved_model_name(&config, "relay");
    assert_eq!(
        boot_model.as_deref(),
        Some("beta"),
        "boot resolves to the pinned model"
    );

    let mut usage = ProviderUsage::default();
    usage.record("relay");
    usage.record_model("relay", boot_model.as_deref().unwrap());

    // ── Next launch: a fresh session with no `default_model` pin. ──
    // (Session pins live in `SessionData`, not config.toml, so a fresh
    // session sees only the global default — here empty.) Restoration must
    // come from the recorded `last_models` entry, not the default channel.
    let mut next_config = config.clone();
    next_config.default_model = None;
    assert_eq!(
        resolved_model_name_with_usage(&next_config, "relay", &usage).as_deref(),
        Some("beta"),
        "next launch must reopen on the recorded boot model"
    );

    // Counter-assertion: the pre-fix behavior — recording only the
    // provider, never the model — leaves `last_models` empty, so the next
    // launch wrongly reopens on the default-channel model "alpha".
    let provider_only_usage = {
        let mut u = ProviderUsage::default();
        u.record("relay");
        u
    };
    assert_eq!(
        resolved_model_name_with_usage(&next_config, "relay", &provider_only_usage).as_deref(),
        Some("alpha"),
        "without record_model the default-channel model wins (the bug)"
    );
}

#[test]
#[ignore = "legacy behavior: built-in providers are now user-added templates"]
fn built_in_anthropic_applies_per_model_reasoning_overrides() {
    // ADR-0046: reasoning is opt-in per model. A `[model_reasoning]` entry
    // keyed by model id opts that model in; an explicit `thinking = false`
    // keeps it off even with an entry. A sibling model with no entry stays
    // at the default (thinking off, no explicit effort) — it does not
    // reason on its own.
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }
    let mut config = bare_config();
    config
        .model_reasoning
        .for_model_mut("claude-opus-4-8")
        .effort = Some("max".to_string());
    config
        .model_reasoning
        .for_model_mut("claude-opus-4-8")
        .thinking = Some(false);

    let entries = build_catalog(&config);
    let entry = entries.iter().find(|e| e.id == "anthropic").unwrap();
    // The configured model carries max effort + thinking off (explicit).
    let opus = entry.channel_for_model("claude-opus-4-8").unwrap();
    match &opus.transport {
        Transport::Anthropic {
            effort, thinking, ..
        } => {
            assert_eq!(*effort, Some(Effort::Max), "opus per-model effort");
            assert_eq!(
                *thinking,
                Some(ThinkingMode::Off),
                "opus per-model thinking off"
            );
        }
        other => panic!("expected Anthropic transport, got {other:?}"),
    }
    // A sibling model with no entry keeps the opt-in default (effort None,
    // thinking None → off on the wire).
    let sonnet = entry.channel_for_model("claude-sonnet-4-6").unwrap();
    match &sonnet.transport {
        Transport::Anthropic {
            effort, thinking, ..
        } => {
            assert!(effort.is_none(), "sonnet untouched effort");
            assert!(thinking.is_none(), "sonnet untouched thinking");
        }
        other => panic!("expected Anthropic transport, got {other:?}"),
    }
}

#[test]
#[ignore = "legacy behavior: built-in providers are now user-added templates"]
fn per_model_entry_presence_defaults_thinking_on() {
    // ADR-0046 opt-in contract: a `[model_reasoning]` entry's mere presence
    // opts the model in to reasoning — thinking defaults ON unless the
    // entry explicitly sets `thinking = false`. So an entry with only an
    // effort still turns thinking on. This is "写的默认有 think 且为对应
    // effort".
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::remove_var("ANTHROPIC_BASE_URL");
    }
    let mut config = bare_config();
    // Entry with effort only (no thinking key) → thinking defaults on.
    config
        .model_reasoning
        .for_model_mut("claude-opus-4-8")
        .effort = Some("xhigh".to_string());

    let entries = build_catalog(&config);
    let entry = entries.iter().find(|e| e.id == "anthropic").unwrap();
    let opus = entry.channel_for_model("claude-opus-4-8").unwrap();
    match &opus.transport {
        Transport::Anthropic {
            effort, thinking, ..
        } => {
            assert_eq!(*effort, Some(Effort::Xhigh), "effort honored");
            assert_eq!(
                *thinking,
                Some(ThinkingMode::Adaptive),
                "entry presence defaults thinking on"
            );
        }
        other => panic!("expected Anthropic transport, got {other:?}"),
    }
    // A bare entry with NO knobs at all (an empty `[model_reasoning."m"]`)
    // still counts as opted in → thinking on, effort None (model default).
    config.model_reasoning.for_model_mut("claude-sonnet-4-6");
    let entries = build_catalog(&config);
    let entry = entries.iter().find(|e| e.id == "anthropic").unwrap();
    let sonnet = entry.channel_for_model("claude-sonnet-4-6").unwrap();
    match &sonnet.transport {
        Transport::Anthropic {
            effort, thinking, ..
        } => {
            assert!(effort.is_none(), "no effort → model default, omitted");
            assert_eq!(
                *thinking,
                Some(ThinkingMode::Adaptive),
                "bare entry still opts in to thinking"
            );
        }
        other => panic!("expected Anthropic transport, got {other:?}"),
    }
}

#[test]
#[ignore = "legacy behavior: built-in providers are now user-added templates"]
fn anthropic_default_model_selects_its_channel_and_builds() {
    let mut config = bare_config();
    config.default_model = Some("claude-sonnet-4-6".to_string());
    assert_eq!(
        resolved_model_name(&config, "anthropic").as_deref(),
        Some("claude-sonnet-4-6")
    );
    let provider = build_provider_for_model(&config, "anthropic", Some("claude-sonnet-4-6"), None)
        .expect("anthropic provider should build");
    assert_eq!(provider.model(), "claude-sonnet-4-6");
    assert_eq!(provider.provider_id(), "anthropic");
}

#[test]
#[ignore = "legacy behavior: built-in providers are now user-added templates"]
fn opencode_go_default_model_selects_its_channel() {
    let mut config = bare_config();
    config.default_model = Some("minimax-m3".to_string());
    // resolved_model_name honors default_model when the provider serves it.
    assert_eq!(
        resolved_model_name(&config, "opencode-go").as_deref(),
        Some("minimax-m3")
    );
    // models_for_provider lists every served model for the picker.
    let models = models_for_provider(&config, "opencode-go");
    assert!(models.contains(&"glm-5.2".to_string()));
    assert!(models.contains(&"minimax-m3".to_string()));
}

#[test]
#[ignore = "legacy behavior: built-in providers are now user-added templates"]
fn build_provider_for_model_picks_anthropic_transport_for_minimax() {
    // Selecting minimax-m3 under opencode-go must build a provider whose
    // model id is minimax-m3 (the Anthropic /messages path), proving the
    // per-model transport routing reaches construction.
    let config = bare_config();
    let provider = build_provider_for_model(&config, "opencode-go", Some("minimax-m3"), None)
        .expect("opencode-go minimax-m3 channel should build");
    assert_eq!(provider.model(), "minimax-m3");
    assert_eq!(provider.provider_id(), "opencode-go");
}

#[test]
#[ignore = "legacy behavior: built-in providers are now user-added templates"]
fn kimi_code_uses_kimi_code_platform() {
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::remove_var("MOONSHOT_MODEL");
    }
    let config = bare_config();
    let entries = build_catalog(&config);
    let entry = entries
        .iter()
        .find(|e| e.id == "kimi-code")
        .expect("kimi-code entry");
    let channel = entry.default_channel().expect("default channel");
    // The Kimi Code platform pins the model id to k3.
    assert_eq!(channel.model, "k3", "model must be the pinned k3 alias");
    let (base_url, user_agent) = match &channel.transport {
        Transport::OpenAi {
            base_url,
            user_agent,
            ..
        } => (base_url.clone(), user_agent.clone()),
        other => panic!("kimi-code must be OpenAi, got {other:?}"),
    };
    assert_eq!(base_url, "https://api.kimi.com/coding/v1/chat/completions");
    // The preset borrows a recognized coding-agent UA as the zero-risk
    // default (the endpoint tolerates any UA under OAuth, untested for
    // API-key auth).
    assert_eq!(user_agent, OPENCODE_USER_AGENT);
}

#[test]
#[ignore = "legacy behavior: built-in providers are now user-added templates"]
fn google_default_model_selects_its_google_channel() {
    // google is multi-model: default_model picks which Google channel is
    // active; every channel uses the native Google transport. ENV_GUARD is
    // held because the built-in entry reads `GOOGLE_BASE_URL` (and other
    // `GEMINI_*` vars) — a parallel test mutating them must not leak in.
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let mut config = bare_config();
    config.default_model = Some("gemini-2.0-flash".to_string());
    let entries = build_catalog(&config);
    let entry = entries
        .iter()
        .find(|e| e.id == "google")
        .expect("google entry");
    assert_eq!(entry.default_channel().unwrap().model, "gemini-2.0-flash");
    assert!(matches!(
        entry.default_channel().unwrap().transport,
        Transport::Google { .. }
    ));
    // The built-in default base URL resolves to Google's official endpoint.
    if let Transport::Google { base_url, .. } = &entry.default_channel().unwrap().transport {
        assert_eq!(base_url, "https://generativelanguage.googleapis.com/v1beta");
    }
}

#[test]
#[ignore = "legacy behavior: built-in providers are now user-added templates"]
fn deepseek_hosts_flash_and_pro_as_one_provider() {
    // The DeepSeek models are now channels of one `deepseek` provider,
    // all over the Responses transport at the official DeepSeek endpoint.
    let entries = build_catalog(&bare_config());
    let entry = entries
        .iter()
        .find(|e| e.id == "deepseek")
        .expect("deepseek entry");
    assert!(entry.offers_model("deepseek-v4-flash"));
    assert!(entry.offers_model("deepseek-v4-flash-0731"));
    assert!(entry.offers_model("deepseek-v4-pro"));
    assert!(entry.offers_model("deepseek-v4-pro-0813"));
    let flash = entry.channel_for_model("deepseek-v4-flash").unwrap();
    match &flash.transport {
        Transport::OpenAiResponses { base_url, .. } => {
            assert_eq!(base_url, "https://api.deepseek.com/v1/responses");
        }
        other => panic!("deepseek must be OpenAiResponses, got {other:?}"),
    }
}

#[test]
fn resolved_model_name_falls_back_for_unknown_id() {
    assert!(resolved_model_name(&bare_config(), "nope").is_none());
}

#[test]
fn build_provider_for_unknown_id_returns_none() {
    assert!(build_provider_for(&bare_config(), "does-not-exist").is_none());
}

#[test]
fn split_deepseek_ids_no_longer_resolve_as_providers() {
    // The pre-merge provider ids are gone; only the merged `deepseek` id is a
    // provider now, so the old ids no longer resolve.
    assert!(build_provider_for(&bare_config(), "deepseek-v4-flash").is_none());
    assert!(build_provider_for(&bare_config(), "deepseek-v4-pro").is_none());
}

#[test]
#[ignore = "legacy behavior: built-in providers are now user-added templates"]
fn cloud_providers_report_not_ready_without_key() {
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::remove_var("OPENAI_API_KEY");
    }
    let entries = build_catalog(&bare_config());
    let openai = entries
        .iter()
        .find(|e| e.id == "openai")
        .expect("openai entry");
    assert!(
        !openai.key_ready(),
        "openai without a key must not be ready"
    );
}

/// Build a user model override on `google` with two channels.
fn google_two_channel_config() -> Config {
    let mut config = bare_config();
    config.providers = vec![UserProviderConfig {
        id: "google".to_string(),
        name: Some("Gemini (custom)".to_string()),
        channels: vec![
            UserChannelConfig {
                label: "Studio".to_string(),
                transport: UserTransport::Google,
                api_key_env: Some("GEMINI_STUDIO_KEY".to_string()),
                model: Some("gemini-2.5-flash".to_string()),
                base_url: Some("https://relay.example.com/v1beta".to_string()),
                ..Default::default()
            },
            UserChannelConfig {
                label: "Relay".to_string(),
                transport: UserTransport::OpenAi,
                base_url: Some("https://relay.example.com/v1/chat/completions".to_string()),
                api_key: Some("inline-key".into()),
                model: Some("gemini-2.5-flash".to_string()),
                ..Default::default()
            },
        ],
        default_channel: 1,
        ..Default::default()
    }];
    config
}

#[test]
fn user_model_overrides_builtin_by_id() {
    let entries = build_catalog(&google_two_channel_config());
    let google = entries
        .iter()
        .find(|e| e.id == "google")
        .expect("overridden google entry");
    // The user-supplied name wins over the built-in "Gemini 2.5 Flash".
    assert_eq!(google.name, "Gemini (custom)");
    assert!(!google.builtin, "an override is user-owned, not read-only");
    // Two channels, with the user's default index honored.
    assert_eq!(google.channels.len(), 2);
    assert_eq!(google.default_channel, 1);
    assert_eq!(google.default_channel().unwrap().label, "Relay");
}

#[test]
fn user_channel_resolves_env_key_over_inline() {
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::set_var("GEMINI_STUDIO_KEY", "env-key");
    }
    let entries = build_catalog(&google_two_channel_config());
    let entry = entries.iter().find(|e| e.id == "google").unwrap();
    // Studio names an env var → the env value wins.
    let studio = entry.channels.iter().find(|c| c.label == "Studio").unwrap();
    assert_eq!(studio.api_key, "env-key");
    // Relay uses an inline key (no env var named) → inline wins.
    let relay = entry.channels.iter().find(|c| c.label == "Relay").unwrap();
    assert_eq!(relay.api_key, "inline-key");
    unsafe {
        std::env::remove_var("GEMINI_STUDIO_KEY");
    }
}

#[test]
fn openai_reasoning_effort_surfaces_in_picker_and_transport() {
    let mut config = bare_config();
    config.providers = vec![UserProviderConfig {
        id: "openai-relay".to_string(),
        name: Some("OpenAI Relay".to_string()),
        channels: vec![
            UserChannelConfig {
                label: "default".to_string(),
                transport: UserTransport::OpenAi,
                api_key: Some("k".into()),
                model: Some("gpt-5.5".to_string()),
                ..Default::default()
            },
            UserChannelConfig {
                label: "xhigh".to_string(),
                transport: UserTransport::OpenAi,
                api_key: Some("k".into()),
                model: Some("gpt-5.2".to_string()),
                effort: Some("xhigh".to_string()),
                ..Default::default()
            },
        ],
        default_channel: 0,
        ..Default::default()
    }];

    let picker = build_picker_state(&config, &ProviderUsage::default());
    let row = picker
        .rows
        .iter()
        .find(|row| row.id == "openai-relay")
        .expect("openai relay row");
    let gpt55 = row
        .model_info
        .iter()
        .find(|info| info.model == "gpt-5.5")
        .expect("gpt-5.5 info");
    assert_eq!(gpt55.protocol, "openai");
    assert_eq!(gpt55.effort.as_deref(), Some("medium"));
    assert_eq!(gpt55.thinking, None);

    let entries = build_catalog(&config);
    let entry = entries
        .iter()
        .find(|entry| entry.id == "openai-relay")
        .expect("openai relay entry");
    let gpt52 = entry.channel_for_model("gpt-5.2").expect("gpt-5.2");
    match &gpt52.transport {
        Transport::OpenAi { effort, .. } => assert_eq!(*effort, Some(Effort::Xhigh)),
        other => panic!("expected OpenAi, got {other:?}"),
    }
}

#[test]
fn user_google_native_channel_carries_relay_base_url() {
    // A 中转站 wired onto a native-Google channel supplies the versioned
    // base URL; it must land on the transport verbatim (the provider
    // appends the `/models/{id}:generateContent` path itself).
    let entries = build_catalog(&google_two_channel_config());
    let entry = entries.iter().find(|e| e.id == "google").unwrap();
    let studio = entry.channels.iter().find(|c| c.label == "Studio").unwrap();
    match &studio.transport {
        Transport::Google { base_url, .. } => {
            assert_eq!(base_url, "https://relay.example.com/v1beta");
        }
        other => panic!("Studio must be native Google, got {other:?}"),
    }
}

#[test]
fn user_google_native_channel_defaults_base_url_when_unset() {
    // A native-Google channel with no base_url falls back to the localhost
    // relay default (mirrors the OpenAI/Anthropic unset-channel contract),
    // never to Google's official endpoint — only the built-in `google`
    // preset resolves the official default.
    let mut config = bare_config();
    config.providers = vec![UserProviderConfig {
        id: "google".to_string(),
        name: None,
        channels: vec![UserChannelConfig {
            label: "default".to_string(),
            transport: UserTransport::Google,
            api_key: Some("k".into()),
            model: Some("gemini-2.5-flash".to_string()),
            ..Default::default()
        }],
        default_channel: 0,
        ..Default::default()
    }];
    let entries = build_catalog(&config);
    let entry = entries.iter().find(|e| e.id == "google").unwrap();
    match &entry.default_channel().unwrap().transport {
        Transport::Google { base_url, .. } => {
            assert_eq!(base_url, "http://localhost:8080/v1beta");
        }
        other => panic!("expected native Google, got {other:?}"),
    }
}

#[test]
#[ignore = "legacy behavior: built-in providers are now user-added templates"]
fn google_base_url_env_overrides_official_default() {
    // The built-in `google` preset reads GOOGLE_BASE_URL first (falling back
    // to the legacy GEMINI_BASE_URL), then the config slot, falling back to
    // the official endpoint — same contract as the anthropic relay (ADR for
    // the configurable Claude relay).
    let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        std::env::set_var("GOOGLE_BASE_URL", "https://relay.example.com/v1beta");
    }
    let mut config = bare_config();
    config.google_base_url = Some("https://from-config.example.com/v1beta".to_string());
    let entries = build_catalog(&config);
    unsafe {
        std::env::remove_var("GOOGLE_BASE_URL");
    }
    let entry = entries.iter().find(|e| e.id == "google").unwrap();
    match &entry.default_channel().unwrap().transport {
        Transport::Google { base_url, .. } => {
            // env wins over config.
            assert_eq!(base_url, "https://relay.example.com/v1beta");
        }
        other => panic!("google must be native Google, got {other:?}"),
    }
}

#[test]
fn user_model_appends_when_id_is_new() {
    let mut config = bare_config();
    config.providers = vec![UserProviderConfig {
        id: "my-relay".to_string(),
        name: Some("My Relay".to_string()),
        channels: vec![UserChannelConfig {
            label: "default".to_string(),
            transport: UserTransport::OpenAi,
            base_url: Some("https://my.example.com/v1/chat/completions".to_string()),
            api_key: Some("k".into()),
            model: Some("my-model".to_string()),
            ..Default::default()
        }],
        ..Default::default()
    }];
    let entries = build_catalog(&config);
    let relay = entries
        .iter()
        .find(|e| e.id == "my-relay")
        .expect("appended user model");
    assert_eq!(relay.name, "My Relay");
    assert_eq!(relay.default_channel().unwrap().model, "my-model");
}

#[test]
fn default_provider_id_reads_config() {
    let mut config = bare_config();
    config.default_provider = "zai-code".to_string();
    assert_eq!(default_provider_id(&config), "zai-code");
}

#[test]
fn picker_state_reflects_user_default_and_channels() {
    let mut config = google_two_channel_config();
    config.default_provider = "google".to_string();
    let usage = ProviderUsage::default();
    let snapshot = build_picker_state(&config, &usage);
    assert_eq!(snapshot.default_id, "google");
    let google_row = snapshot
        .rows
        .iter()
        .find(|r| r.id == "google")
        .expect("google row present");
    assert!(google_row.key_ready, "Relay channel has an inline key");
    // The picker row is fully self-describing: a user-defined provider shows
    // its display name, served models, active model, and builtin=false — the
    // fields the snapshot-driven TUI renders directly (no static table).
    assert_eq!(google_row.name, "Gemini (custom)");
    assert!(!google_row.builtin, "user-defined provider is not builtin");
    assert_eq!(google_row.models.len(), 2, "both channels' models listed");
    assert!(google_row.models.iter().all(|m| m == "gemini-2.5-flash"));
    assert_eq!(google_row.model, "gemini-2.5-flash");
}

#[test]
#[ignore = "legacy behavior: built-in providers are now user-added templates"]
fn openai_is_a_multi_model_builtin_with_gpt_4o_default() {
    // OpenAI is now a multi-model provider: its picker row lists every
    // OPENAI_BUILTIN_MODELS entry and defaults to gpt-4o.
    let config = bare_config();
    let usage = ProviderUsage::default();
    let snapshot = build_picker_state(&config, &usage);
    let openai = snapshot
        .rows
        .iter()
        .find(|r| r.id == "openai")
        .expect("openai row present");
    assert_eq!(openai.name, "OpenAI");
    assert!(openai.builtin);
    assert!(openai.models.contains(&"gpt-4o".to_string()));
    assert!(openai.models.contains(&"gpt-4o-mini".to_string()));
    assert_eq!(openai.model, "gpt-4o");
    // Llama no longer appears as a built-in provider.
    assert!(snapshot.rows.iter().all(|r| r.id != "llama"));
}

// ── live model discovery (discover_provider_models) ────────────────────

#[test]
fn default_model_source_maps_discovery_flag() {
    // A discovery-enabled template → Api; a fixed-list template → Fixed.
    let openai_spec = provider_template_spec("openai").expect("openai template");
    assert_eq!(
        default_model_source_for_spec(openai_spec),
        neenee_persistence::config::ModelSource::Api
    );
    let opencode_spec = provider_template_spec("opencode-go").expect("opencode-go template");
    assert_eq!(
        default_model_source_for_spec(opencode_spec),
        neenee_persistence::config::ModelSource::Fixed
    );
}

#[tokio::test]
async fn discover_filters_to_supported_intersection_and_keeps_provider_settings() {
    let _sandbox = sandboxed_paths();
    let spec = provider_template_spec("deepseek").unwrap();
    let kept_a = spec.models[1];
    let kept_b = spec.models[spec.models.len() - 1];
    let known_outside_seed = "gpt-4o";
    assert!(!spec.models.contains(&known_outside_seed));
    let advertised = vec![
        "cloud-only-model".to_string(),
        kept_b.to_string(),
        known_outside_seed.to_string(),
        kept_a.to_string(),
    ];
    let expected = supported_model_intersection(&supported_models_for_template(spec), &advertised)
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let mut server = mockito::Server::new_async().await;
    let body = format!(
        r#"{{"data":[{{"id":"cloud-only-model"}},{{"id":"{kept_b}"}},{{"id":"{known_outside_seed}"}},{{"id":"{kept_a}"}}]}}"#
    );
    let _mock = server
        .mock("GET", "/v1/models")
        .match_header("authorization", "Bearer sk-test")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(body)
        .create_async()
        .await;

    let mut instance = template_instance("deepseek", spec.models);
    instance.model_source = neenee_persistence::config::ModelSource::Api;
    let chat_url = format!("{}/v1/chat/completions", server.url());
    for channel in &mut instance.channels {
        channel.base_url = Some(chat_url.clone());
        channel.api_key_env = Some("RELAY_API_KEY".to_string());
        channel.user_agent = Some("relay-client/1.0".to_string());
    }
    let mut config = bare_config();
    config.providers.push(instance);

    assert!(discover_provider_models(&mut config).await.changed);
    assert_eq!(config.providers[0].channel_models(), expected);
    assert!(config.providers[0].channels.iter().all(|channel| {
        channel.api_key.as_ref().map(SecretString::expose_secret) == Some("sk-test")
            && channel.api_key_env.as_deref() == Some("RELAY_API_KEY")
            && channel.base_url.as_deref() == Some(chat_url.as_str())
            && channel.user_agent.as_deref() == Some("relay-client/1.0")
    }));
}

#[tokio::test]
async fn discover_empty_supported_intersection_keeps_previous_provider() {
    let _sandbox = sandboxed_paths();
    let spec = provider_template_spec("deepseek").unwrap();
    let mut server = mockito::Server::new_async().await;
    let _mock = server
        .mock("GET", "/v1/models")
        .match_header("authorization", "Bearer sk-test")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"data":[{"id":"cloud-only-model"}]}"#)
        .create_async()
        .await;

    let mut instance = template_instance("deepseek", spec.models);
    instance.model_source = neenee_persistence::config::ModelSource::Api;
    let chat_url = format!("{}/v1/chat/completions", server.url());
    for channel in &mut instance.channels {
        channel.base_url = Some(chat_url.clone());
    }
    let before = instance
        .channel_models()
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    let mut config = bare_config();
    config.providers.push(instance);

    assert!(!discover_provider_models(&mut config).await.changed);
    assert_eq!(config.providers[0].channel_models(), before);
    assert!(config.providers[0].channels.iter().all(|channel| {
        channel.api_key.as_ref().map(SecretString::expose_secret) == Some("sk-test")
            && channel.base_url.as_deref() == Some(chat_url.as_str())
    }));
}

#[tokio::test]
async fn discover_skips_fixed_instances_without_hitting_network() {
    let _sandbox = sandboxed_paths();
    // A Fixed template-sourced instance must be skipped entirely — the
    // snapshot from reconcile is authoritative. Because discover returns
    // `false` (no change) and never attempts a fetch, this also confirms
    // the gating is correct.
    let mut config = bare_config();
    let mut instance = template_instance("deepseek", DEEPSEEK_BUILTIN_MODELS);
    instance.model_source = neenee_persistence::config::ModelSource::Fixed;
    config.providers.push(instance);
    let before: Vec<String> = config.providers[0]
        .channel_models()
        .iter()
        .map(|s| s.to_string())
        .collect();

    let changed = discover_provider_models(&mut config).await.changed;

    assert!(!changed, "Fixed instance must not be discovered");
    let after: Vec<String> = config.providers[0]
        .channel_models()
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(after, before, "Fixed instance models untouched");
}

#[tokio::test]
async fn discover_falls_back_to_snapshot_when_fetch_fails() {
    let _sandbox = sandboxed_paths();
    // An Api instance whose endpoint is unreachable (relay.example.com does
    // not resolve within the request timeout) must keep the template
    // snapshot — the live fetch only ever improves, never regresses.
    let mut config = bare_config();
    let mut instance = template_instance("deepseek", DEEPSEEK_BUILTIN_MODELS);
    instance.model_source = neenee_persistence::config::ModelSource::Api;
    config.providers.push(instance);
    let before: Vec<String> = config.providers[0]
        .channel_models()
        .iter()
        .map(|s| s.to_string())
        .collect();

    let changed = discover_provider_models(&mut config).await.changed;

    assert!(
        !changed,
        "a failed fetch must not report a change (snapshot kept as-is)"
    );
    let after: Vec<String> = config.providers[0]
        .channel_models()
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(
        after, before,
        "snapshot must be preserved when the live fetch fails"
    );
}

#[tokio::test]
async fn discover_skips_discovery_disabled_template_even_when_api() {
    let _sandbox = sandboxed_paths();
    // opencode-go is discovery=false; even with model_source=Api it must be
    // skipped (the template does not expose a usable /models endpoint).
    let mut config = bare_config();
    let mut instance = template_instance("opencode-go", OPENCODE_GO_MODELS);
    instance.model_source = neenee_persistence::config::ModelSource::Api;
    config.providers.push(instance);
    let before: Vec<String> = config.providers[0]
        .channel_models()
        .iter()
        .map(|s| s.to_string())
        .collect();

    let changed = discover_provider_models(&mut config).await.changed;

    assert!(!changed, "discovery-disabled template must be skipped");
    let after: Vec<String> = config.providers[0]
        .channel_models()
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(after, before);
}

#[tokio::test]
async fn discover_skips_pure_custom_instance() {
    let _sandbox = sandboxed_paths();
    // A pure-custom instance (no template_id) must never be discovered.
    let mut config = bare_config();
    let mut instance = template_instance("deepseek", DEEPSEEK_BUILTIN_MODELS);
    instance.template_id = None;
    instance.model_source = neenee_persistence::config::ModelSource::Api;
    config.providers.push(instance);
    let before: Vec<String> = config.providers[0]
        .channel_models()
        .iter()
        .map(|s| s.to_string())
        .collect();

    let changed = discover_provider_models(&mut config).await.changed;

    assert!(!changed, "pure-custom instance must not be discovered");
    let after: Vec<String> = config.providers[0]
        .channel_models()
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(after, before);
}

#[test]
fn reconcile_backfill_sets_api_model_source_for_discovery_template() {
    // A legacy instance that exactly matches a discovery-enabled template
    // (deepseek) gets stamped AND adopts model_source=Api, so it
    // starts benefiting from live discovery on the next startup.
    let models = current_template_models("deepseek");
    let mut config = bare_config();
    config.providers.push(UserProviderConfig {
        id: "relay".to_string(),
        name: Some("Relay".to_string()),
        channels: models
            .iter()
            .map(|m| UserChannelConfig {
                label: m.clone(),
                transport: UserTransport::OpenAi,
                api_key_env: None,
                api_key: Some("sk".into()),
                model: Some(m.clone()),
                base_url: Some("https://relay.example.com".to_string()),
                user_agent: None,
                effort: None,
                thinking: None,
                auth: Default::default(),
                remote: None,
            })
            .collect(),
        default_channel: 0,
        template_id: None,
        model_source: Default::default(),
        fitted_models: Default::default(),
    });

    assert!(reconcile_provider_models(&mut config));
    assert_eq!(config.providers[0].template_id.as_deref(), Some("deepseek"));
    assert_eq!(
        config.providers[0].model_source,
        neenee_persistence::config::ModelSource::Api,
        "backfilled discovery-template instance adopts Api source"
    );
}

#[test]
fn reconcile_backfill_sets_fixed_model_source_for_nondiscovery_template() {
    // A legacy instance that exactly matches a discovery-disabled template
    // (opencode-go) gets stamped but keeps model_source=Fixed.
    let models = current_template_models("opencode-go");
    let mut config = bare_config();
    config.providers.push(UserProviderConfig {
        id: "go".to_string(),
        name: Some("OpenCode Go".to_string()),
        channels: models
            .iter()
            .map(|m| UserChannelConfig {
                label: m.clone(),
                transport: UserTransport::OpenAi,
                api_key_env: None,
                api_key: Some("sk".into()),
                model: Some(m.clone()),
                base_url: Some("https://opencode.ai/zen/go/v1/chat/completions".to_string()),
                user_agent: None,
                effort: None,
                thinking: None,
                auth: Default::default(),
                remote: None,
            })
            .collect(),
        default_channel: 0,
        template_id: None,
        model_source: Default::default(),
        fitted_models: Default::default(),
    });

    assert!(reconcile_provider_models(&mut config));
    assert_eq!(
        config.providers[0].template_id.as_deref(),
        Some("opencode-go")
    );
    assert_eq!(
        config.providers[0].model_source,
        neenee_persistence::config::ModelSource::Fixed,
        "backfilled nondiscovery-template instance keeps Fixed source"
    );
}

#[test]
fn reconcile_upgrades_fixed_to_api_for_fitting_templates() {
    // kimi-code gained discovery+fitting after existing instances had been
    // stamped Fixed by the backfill — that Fixed was never a deliberate
    // opt-out (the template offered no Api source at the time), so the
    // instance follows the template to Api and starts live discovery.
    let mut config = bare_config();
    let mut instance = template_instance("kimi-code", KIMI_CODE_MODELS);
    instance.model_source = neenee_persistence::config::ModelSource::Fixed;
    config.providers.push(instance);

    assert!(reconcile_provider_models(&mut config));
    assert_eq!(
        config.providers[0].model_source,
        neenee_persistence::config::ModelSource::Api,
        "Fixed instance of a fitting template upgrades to Api"
    );
    // The upgrade itself does not touch the channel set.
    assert_eq!(
        config.providers[0].channel_models(),
        KIMI_CODE_MODELS.to_vec()
    );
}

#[test]
fn reconcile_api_instance_retains_fitted_channels() {
    // An Api instance whose channels came from a previous live fetch keeps
    // its fitted ids across reconciles — intersecting against the static
    // registry alone would drop them and undo the fitting.
    let mut config = bare_config();
    let mut instance = template_instance("kimi-code", &["k3", "kimi-for-coding"]);
    instance.model_source = neenee_persistence::config::ModelSource::Api;
    instance.fitted_models.insert(
        "kimi-for-coding".to_string(),
        FittedModelInfo {
            context_window: 262_144,
            reasoning: true,
            vision: true,
            efforts: Vec::new(),
        },
    );
    config.providers.push(instance);

    // The channel set already equals the retainable set → no-op.
    assert!(!reconcile_provider_models(&mut config));
    assert_eq!(
        config.providers[0].channel_models(),
        vec!["k3".to_string(), "kimi-for-coding".to_string()]
    );
}

#[tokio::test]
async fn discover_fitting_template_materializes_and_fits_advertised_models() {
    let _sandbox = sandboxed_paths();
    // Recorded 2026-07 from GET https://api.kimi.com/coding/v1/models.
    let body = r#"{"data":[
        {"id":"kimi-for-coding","created":1761264000,"created_at":"2025-10-24T00:00:00Z","object":"model","display_name":"kimi-for-coding","type":"model","context_length":262144,"supports_reasoning":true,"supports_image_in":true,"supports_video_in":true,"supports_thinking_type":"only"},
        {"id":"kimi-for-coding-highspeed","created":1761264000,"created_at":"2025-10-24T00:00:00Z","object":"model","display_name":"kimi-for-coding-highspeed","type":"model","context_length":262144,"supports_reasoning":true,"supports_image_in":true,"supports_video_in":true,"supports_thinking_type":"only"},
        {"id":"k3","created":1761264000,"created_at":"2025-10-24T00:00:00Z","object":"model","display_name":"k3","type":"model","context_length":1048576,"supports_reasoning":true,"supports_image_in":true,"supports_video_in":true,"supports_thinking_type":"only","think_efforts":{"support":true,"valid_efforts":["max"],"default_effort":"max"}}
    ],"object":"list","first_id":"kimi-for-coding","last_id":"k3","has_more":false}"#;
    let mut server = mockito::Server::new_async().await;
    let _mock = server
        .mock("GET", "/v1/models")
        .match_header("authorization", "Bearer sk-test")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(body)
        .create_async()
        .await;

    let mut instance = template_instance("kimi-code", KIMI_CODE_MODELS);
    instance.model_source = neenee_persistence::config::ModelSource::Api;
    let chat_url = format!("{}/v1/chat/completions", server.url());
    for channel in &mut instance.channels {
        channel.base_url = Some(chat_url.clone());
    }
    let mut config = bare_config();
    config.providers.push(instance);

    assert!(discover_provider_models(&mut config).await.changed);
    // Every advertised id is materialized (sorted by id), including the
    // platform-native ids the static registry does not know.
    assert_eq!(
        config.providers[0].channel_models(),
        vec![
            "k3".to_string(),
            "kimi-for-coding".to_string(),
            "kimi-for-coding-highspeed".to_string()
        ]
    );
    // Fitted metadata is persisted only for registry-unknown ids — k3 is
    // registered, so its vetted static entry stays authoritative.
    let fitted = &config.providers[0].fitted_models;
    assert!(!fitted.contains_key("k3"));
    let kimi_for_coding = &fitted["kimi-for-coding"];
    assert_eq!(kimi_for_coding.context_window, 262_144);
    assert!(kimi_for_coding.reasoning);
    assert!(kimi_for_coding.vision);
    let highspeed = &fitted["kimi-for-coding-highspeed"];
    assert_eq!(highspeed.context_window, 262_144);
    assert!(highspeed.reasoning);
}

#[tokio::test]
async fn discover_copilot_uses_remote_picker_models_and_persists_routes() {
    let _sandbox = sandboxed_paths();
    let body = r#"{"data":[
        {
            "id":"gpt-5",
            "name":"GPT-5",
            "model_picker_enabled":true,
            "supported_endpoints":["/responses"],
            "capabilities":{
                "type":"chat",
                "family":"gpt-5",
                "limits":{"max_context_window_tokens":200000,"max_output_tokens":16384},
                "supports":{"tool_calls":true,"vision":true,"reasoning_effort":["low","high"]}
            }
        },
        {
            "id":"claude-opus-4.7",
            "name":"Claude Opus 4.7",
            "model_picker_enabled":true,
            "supported_endpoints":["/v1/messages"],
            "capabilities":{
                "type":"chat",
                "family":"claude-opus",
                "limits":{"max_context_window_tokens":144000,"max_output_tokens":64000},
                "supports":{"adaptive_thinking":true,"tool_calls":true,"vision":true}
            }
        },
        {
            "id":"internal-title",
            "name":"Internal title model",
            "model_picker_enabled":false,
            "supported_endpoints":["/responses"],
            "capabilities":{
                "type":"chat",
                "family":"internal",
                "limits":{"max_output_tokens":1024},
                "supports":{"tool_calls":false}
            }
        }
    ]}"#;
    let mut server = mockito::Server::new_async().await;
    let _mock = server
        .mock("GET", "/models")
        .match_header("authorization", "Bearer copilot-token")
        .match_header("copilot-integration-id", "vscode-chat")
        .match_header("x-initiator", "user")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(body)
        .create_async()
        .await;

    let mut instance = template_instance("copilot-oauth", &["gpt-4o-mini"]);
    instance.model_source = ModelSource::Api;
    for channel in &mut instance.channels {
        channel.api_key = Some("copilot-token".into());
        channel.base_url = Some(format!("{}/chat/completions", server.url()));
    }
    let mut config = bare_config();
    config.providers.push(instance);

    let changed = discover_provider_models(&mut config).await.changed;

    assert!(changed);
    assert_eq!(
        config.providers[0].channel_models(),
        vec!["claude-opus-4.7".to_string(), "gpt-5".to_string()]
    );
    let gpt = config.providers[0]
        .channels
        .iter()
        .find(|channel| channel.model.as_deref() == Some("gpt-5"))
        .unwrap();
    assert_eq!(
        gpt.remote.as_ref().and_then(|remote| remote.endpoint),
        Some(RemoteModelEndpoint::Responses)
    );
    let claude = config.providers[0]
        .channels
        .iter()
        .find(|channel| channel.model.as_deref() == Some("claude-opus-4.7"))
        .unwrap();
    assert_eq!(
        claude.remote.as_ref().and_then(|remote| remote.endpoint),
        Some(RemoteModelEndpoint::Messages)
    );
}

#[test]
fn sync_fitted_model_registry_populates_the_resolution_overlay() {
    let mut config = bare_config();
    let mut instance = template_instance("kimi-code", &["k3", "fitted-sync-k9"]);
    instance.fitted_models.insert(
        "fitted-sync-k9".to_string(),
        FittedModelInfo {
            context_window: 512_000,
            reasoning: true,
            vision: true,
            // Unsorted: the overlay stores levels ascending.
            efforts: vec!["high".to_string(), "low".to_string()],
        },
    );
    config.providers.push(instance);

    sync_fitted_model_registry(&config);

    let model = neenee_contracts::model::resolve("fitted-sync-k9");
    assert_eq!(model.family, "kimi-code");
    assert_eq!(model.context_window, 512_000);
    assert!(model.reasoning());
    assert!(model.vision);
    assert_eq!(model.effort_levels, &[Effort::Low, Effort::High]);
}

#[test]
fn copilot_remote_endpoint_selects_the_advertised_transport() {
    use neenee_contracts::{RemoteModelMetadata, ThinkingSupport};

    let base = UserChannelConfig {
        label: "remote-model".to_string(),
        model: Some("remote-model".to_string()),
        auth: neenee_contracts::ChannelAuth::CopilotOAuth,
        remote: Some(RemoteModelMetadata {
            endpoint: Some(RemoteModelEndpoint::Messages),
            max_output_tokens: Some(64_000),
            thinking: Some(ThinkingSupport::AnthropicAdaptive),
            ..Default::default()
        }),
        ..Default::default()
    };
    let messages =
        user_channel_to_channel(&base, "remote-model", "copilot-test", Some("copilot-oauth"));
    assert!(matches!(
        messages.transport,
        Transport::Anthropic { copilot: true, .. }
    ));

    let mut responses = base.clone();
    responses.remote.as_mut().unwrap().endpoint = Some(RemoteModelEndpoint::Responses);
    let responses = user_channel_to_channel(
        &responses,
        "remote-model",
        "copilot-test",
        Some("copilot-oauth"),
    );
    assert!(matches!(
        responses.transport,
        Transport::OpenAiResponses { copilot: true, .. }
    ));

    let mut chat = base;
    chat.remote.as_mut().unwrap().endpoint = Some(RemoteModelEndpoint::ChatCompletions);
    let chat =
        user_channel_to_channel(&chat, "remote-model", "copilot-test", Some("copilot-oauth"));
    assert!(matches!(
        chat.transport,
        Transport::OpenAi { copilot: true, .. }
    ));
}

#[test]
fn trusted_remote_metadata_is_persisted_only_for_picker_models() {
    let mut provider = template_instance("copilot-oauth", &["gpt-5", "internal-title"]);
    let discovered = vec![
        neenee_providers::DiscoveredModel {
            id: "gpt-5".to_string(),
            picker_enabled: Some(true),
            endpoint: Some(RemoteModelEndpoint::Responses),
            family: Some("gpt-5".to_string()),
            context_window: Some(200_000),
            max_output_tokens: Some(16_384),
            reasoning: Some(true),
            thinking: Some(neenee_contracts::ThinkingSupport::ReasoningSummary),
            tool_call: Some(true),
            vision: Some(true),
            effort_levels: Some(vec!["low".to_string(), "high".to_string()]),
        },
        neenee_providers::DiscoveredModel {
            id: "internal-title".to_string(),
            picker_enabled: Some(false),
            endpoint: Some(RemoteModelEndpoint::Responses),
            ..Default::default()
        },
    ];

    assert!(persist_remote_model_metadata(
        &mut provider,
        &discovered,
        true
    ));
    let gpt5 = provider
        .channels
        .iter()
        .find(|channel| channel.model.as_deref() == Some("gpt-5"))
        .unwrap();
    assert_eq!(
        gpt5.remote.as_ref().and_then(|remote| remote.endpoint),
        Some(RemoteModelEndpoint::Responses)
    );
    let internal = provider
        .channels
        .iter()
        .find(|channel| channel.model.as_deref() == Some("internal-title"))
        .unwrap();
    assert!(internal.remote.is_none());
}
