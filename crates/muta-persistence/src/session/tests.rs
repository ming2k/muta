//! Embedded tests for the session store, file format, migration, and the
//! compaction pipeline.

use super::*;
use muta_contracts::async_trait;

/// process env vars) cannot run in parallel. We serialise them through
/// this lock; pure-computation tests skip the guard.
///
/// Note: this lock alone does **not** synchronise with `config`'s tests —
/// both modules independently touched the shared `paths::set_test_default`
/// through their own private locks, which raced. So the macro also acquires
/// the crate-wide `paths::TEST_OVERRIDE_GUARD`, the single lock every
/// override-touching test in the crate funnels through.
static GLOBAL_GUARD: std::sync::LazyLock<tokio::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));

macro_rules! locked {
    ($body:block) => {{
        let _override_guard = $crate::paths::TEST_OVERRIDE_GUARD
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _guard = GLOBAL_GUARD.lock().await;
        $body
    }};
}

struct CompactionProvider;

#[async_trait]
impl Provider for CompactionProvider {
    async fn chat(&self, _request: muta_contracts::ModelRequest) -> Result<muta_contracts::ProviderCompletion, String> {
        Ok(muta_contracts::ProviderCompletion::message(Message::new(
            Role::Assistant,
            "mock AI summary",
        )))
    }

    async fn stream_chat(
        &self,
        _request: muta_contracts::ModelRequest,
    ) -> Result<futures::stream::BoxStream<'static, Result<String, String>>, String> {
        Ok(Box::pin(futures::stream::empty()))
    }
}

#[tokio::test]
async fn session_data_round_trips() {
    let directory = std::env::temp_dir().join(format!("muta-host-test-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    let messages = vec![Message::new(muta_contracts::Role::User, "hello")];
    store.replace_messages(messages.clone()).await.unwrap();

    let data: SessionData = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(data.model_window[0].content, messages[0].content);
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn schema_version_defaults_to_current_and_serialises() {
    let data = SessionData::default();
    assert_eq!(data.schema_version, CURRENT_SCHEMA_VERSION);
    let raw = serde_json::to_string(&data).unwrap();
    assert!(raw.contains("\"schema_version\":"));
}

/// ADR-0125 boot rehost scan: sessions with armed `/schedule` jobs are
/// discoverable across every project bucket, and sessions without jobs
/// are invisible to the scan. Writes two real snapshots through
/// `SessionStore::load_for_project` under a sandboxed data dir (one
/// session with an armed job, one without), then asserts the scan finds
/// exactly the armed one with its project root.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
// The crate-wide `paths::TEST_OVERRIDE_GUARD` (a std Mutex) is held
// across the awaited body intentionally to serialize the process-wide
// paths override. The single-threaded test runtime cannot block a peer.
async fn armed_schedule_scan_finds_only_sessions_with_jobs() {
    locked!({
        let sandbox =
            std::env::temp_dir().join(format!("muta-armed-scan-{}", uuid::Uuid::new_v4()));
        let data_dir = sandbox.join("data");
        let project = sandbox.join("my-project");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(&data_dir).unwrap();
        paths::set_test_default(Some(paths::Dirs::resolve(&paths::PathsOverride {
            data_dir: Some(data_dir.clone()),
            ..Default::default()
        })));

        // One project, two sessions: armed and unarmed. Both stores go
        // through `load_for_project` (the production path), which pins
        // the real project root into the snapshot.
        let armed = SessionStore::load_for_project(project.clone());
        armed
            .set_scheduled_jobs(vec![muta_contracts::ScheduledJob::once(
                "j1".into(),
                chrono::Utc::now() + chrono::Duration::hours(1),
                "later".into(),
                chrono::Utc::now(),
            )])
            .await
            .unwrap();
        // The store skips persisting "empty" sessions, so both sessions
        // need a message to reach disk.
        armed
            .replace_messages(vec![Message::new(muta_contracts::Role::User, "hi")])
            .await
            .unwrap();
        let unarmed = SessionStore::load_for_project(project.clone());
        unarmed
            .replace_messages(vec![Message::new(muta_contracts::Role::User, "hi")])
            .await
            .unwrap();

        let found = sessions_with_armed_schedules();
        assert_eq!(found.len(), 1, "only the armed session: {found:?}");
        assert_eq!(found[0].session_id, armed.id().await);
        assert_eq!(found[0].project_root, project.canonicalize().unwrap());

        let _ = fs::remove_dir_all(sandbox);
        paths::set_test_default(None);
    });
}

#[test]
fn legacy_session_without_schema_version_loads_as_current() {
    let data: SessionData = serde_json::from_str(
        r#"{
                "id": "00000000-0000-0000-0000-000000000001",
                "messages": [],
                "archived_messages": [],
                "loop_checkpoint": null,
                "compaction": null
            }"#,
    )
    .unwrap();
    // Missing fields inherit the serde default, which is the current
    // schema version, so legacy files appear up-to-date.
    assert_eq!(data.schema_version, CURRENT_SCHEMA_VERSION);
}

#[test]
fn schema_migration_bumps_version() {
    let data = SessionData {
        schema_version: 0,
        ..SessionData::default()
    };
    let migrated = migrate_session_data(data);
    assert_eq!(migrated.schema_version, CURRENT_SCHEMA_VERSION);
}

#[test]
fn checksum_computes_and_verifies() {
    let mut data = SessionData::default();
    data.model_window = vec![Message::new(muta_contracts::Role::User, "hello")];
    data.checksum = Some(compute_checksum(&data).unwrap());
    assert!(verify_checksum(&data).is_ok());

    // Tamper with a field: verification must fail.
    data.model_window[0].content = "goodbye".to_string();
    assert!(verify_checksum(&data).is_err());
}

#[test]
fn checksum_is_none_for_legacy_files() {
    let data: SessionData = serde_json::from_str(
        r#"{
                "id": "00000000-0000-0000-0000-000000000001",
                "messages": [],
                "archived_messages": [],
                "schema_version": 1
            }"#,
    )
    .unwrap();
    assert!(data.checksum.is_none());
    assert!(
        verify_checksum(&data).is_ok(),
        "missing checksum is allowed"
    );
}

#[tokio::test]
async fn session_persists_runner_children_round_trip() {
    // End-to-end persistence contract: a session that contains a `task`
    // tool call must round-trip the runner's nested transcript through
    // session.json, so a subsequent `SessionStore::load_for_project` (the
    // production resume path) restores the children intact. Before Phase 3
    // children were silently dropped because `Message::children` did not
    // exist and the harness only persisted the textual summary.
    let directory =
        std::env::temp_dir().join(format!("muta-runner-persist-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());

    let call = muta_contracts::ToolCall {
        id: "call_sub1".to_string(),
        name: "runner".to_string(),
        arguments: r#"{"description":"d","prompt":"p"}"#.to_string(),
    };
    let assistant = Message::new(muta_contracts::Role::Assistant, "")
        .with_attribution("kimi-code", "kimi-k2.7-code");
    let assistant = Message {
        tool_calls: Some(vec![call.clone()]),
        ..assistant
    };
    let runner_transcript = vec![
        Message::new(muta_contracts::Role::User, "find foo"),
        Message::new(muta_contracts::Role::Assistant, "looking..."),
        Message::new(muta_contracts::Role::Assistant, "foo is at src/foo.rs"),
    ];
    let tool = Message::tool_result(&call, "[task result]:\nfoo is at src/foo.rs")
        .with_children(runner_transcript);
    store
        .replace_messages(vec![
            Message::new(muta_contracts::Role::User, "where is foo?"),
            assistant,
            tool,
        ])
        .await
        .unwrap();

    // Reload from disk as production code would.
    let loaded = fs::read_to_string(&path).unwrap();
    let data: SessionData = serde_json::from_str(&loaded).unwrap();
    let tool_msg = data
        .model_window
        .iter()
        .find(|m| m.role == muta_contracts::Role::Tool)
        .expect("tool result message persisted");
    let children = tool_msg.children.as_ref().expect("children persisted");
    assert_eq!(children.len(), 3);
    assert!(children.iter().any(|m| m.content == "find foo"));
    assert!(children.iter().any(|m| m.content.contains("src/foo.rs")));
    let _ = fs::remove_dir_all(directory);
}

#[test]
fn legacy_single_session_snapshot_migrates_with_defaults() {
    let data: SessionData = serde_json::from_str(
        r#"{
                "id": "00000000-0000-0000-0000-000000000001",
                "messages": [],
                "archived_messages": [],
                "loop_checkpoint": null,
                "compaction": null
            }"#,
    )
    .unwrap();

    assert_eq!(data.parent_id, None);
    assert!(data.created_at > 0);
    assert!(data.updated_at > 0);
    // Phase 2: project_root defaults to current cwd for legacy snapshots
    // missing the field.
    assert!(!data.project_root.as_os_str().is_empty());
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
// The crate-wide `paths::TEST_OVERRIDE_GUARD` (a std Mutex) is acquired
// inside `locked!` and held across the awaited body by design, so this
// test serialises against the synchronous `config` override tests. The
// single-threaded tokio test runtime means a held std guard cannot
// deadlock a peer task.
async fn load_for_project_isolates_sessions_per_cwd() {
    locked!({
        let root = std::env::temp_dir().join(format!("muta-proj-iso-{}", uuid::Uuid::new_v4()));
        let dirs = paths::Dirs::resolve(&paths::PathsOverride {
            home: Some(root.clone()),
            data_dir: Some(root.join("data")),
            state_dir: Some(root.join("state")),
            config_dir: Some(root.join("config")),
            cache_dir: Some(root.join("cache")),
        });
        dirs.ensure().unwrap();
        paths::set_test_default(Some(dirs.clone()));
        // Build two stores bound to different project roots.
        let store_a = SessionStore::load_for_project(PathBuf::from("/projects/alpha"));
        let store_b = SessionStore::load_for_project(PathBuf::from("/projects/beta"));

        store_a
            .replace_messages(vec![Message::new(Role::User, "alpha work")])
            .await
            .unwrap();
        store_b
            .replace_messages(vec![Message::new(Role::User, "beta work")])
            .await
            .unwrap();

        let bucket_a = crate::paths::project_bucket_name(&PathBuf::from("/projects/alpha"));
        let bucket_b = crate::paths::project_bucket_name(&PathBuf::from("/projects/beta"));
        assert_ne!(bucket_a, bucket_b);

        // ADR-0018: each instance pins its own `sessions/<id>.json`. There
        // is no longer a project-root `session.json`.
        let id_a = store_a.id().await;
        let id_b = store_b.id().await;
        assert!(
            dirs.project_sessions_dir(&PathBuf::from("/projects/alpha"))
                .join(format!("{id_a}.json"))
                .exists()
        );
        assert!(
            dirs.project_sessions_dir(&PathBuf::from("/projects/beta"))
                .join(format!("{id_b}.json"))
                .exists()
        );

        // Reloading alpha starts fresh but the prior session is resumable,
        // and alpha never sees beta's messages.
        let reloaded_a = SessionStore::load_for_project(PathBuf::from("/projects/alpha"));
        reloaded_a.resume(Some(&id_a)).await.unwrap();
        assert_eq!(reloaded_a.model_window().await[0].content, "alpha work");
        let reloaded_b = SessionStore::load_for_project(PathBuf::from("/projects/beta"));
        reloaded_b.resume(Some(&id_b)).await.unwrap();
        assert_eq!(reloaded_b.model_window().await[0].content, "beta work");

        // list() is scoped per project — alpha only sees its own session.
        let alpha_sessions = reloaded_a.list().await.unwrap();
        assert!(alpha_sessions.iter().all(|s| !s.overview.contains("beta")));
        let beta_sessions = reloaded_b.list().await.unwrap();
        assert!(beta_sessions.iter().all(|s| !s.overview.contains("alpha")));

        paths::set_test_default(None);
        let _ = std::fs::remove_dir_all(root);
    });
}

#[tokio::test]
async fn fork_preserves_both_durable_branches() {
    let directory = std::env::temp_dir().join(format!("muta-host-fork-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    store
        .replace_messages(vec![Message::new(muta_contracts::Role::User, "parent")])
        .await
        .unwrap();
    let parent_id = store.id().await;

    let (fork_id, source_id) = store.fork().await.unwrap();
    assert_eq!(source_id, parent_id);
    assert_eq!(store.parent_id().await.as_deref(), Some(parent_id.as_str()));
    store
        .replace_messages(vec![Message::new(muta_contracts::Role::User, "fork")])
        .await
        .unwrap();

    let sessions = store.list().await.unwrap();
    assert_eq!(sessions.len(), 2);
    assert!(sessions.iter().any(|item| item.id == parent_id));
    assert!(
        sessions
            .iter()
            .any(|item| item.id == fork_id && item.active)
    );

    store.open(&parent_id[..8]).await.unwrap();
    assert_eq!(store.model_window().await[0].content, "parent");
    store.open(&fork_id[..8]).await.unwrap();
    assert_eq!(store.model_window().await[0].content, "fork");
    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn fork_lineage_is_stamped_and_surfaces_through_list() {
    // Lineage contract (dashboard fork surfacing): `fork` stamps
    // `Fork`, `fork_to_side` stamps `Aside`, a fresh session is `Trunk`,
    // and `list()` carries both the parent link and the kind so the
    // dashboard can group trunk + branches without guessing from ids.
    let directory = std::env::temp_dir().join(format!("muta-lineage-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    store
        .replace_messages(vec![Message::new(muta_contracts::Role::User, "parent")])
        .await
        .unwrap();
    let parent_id = store.id().await;

    // Trunk: the fresh parent has no lineage.
    let trunk = store
        .list()
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.id == parent_id)
        .unwrap();
    assert_eq!(trunk.fork_kind, muta_contracts::SessionForkKind::Trunk);
    assert!(trunk.parent_id.is_none());

    // Explicit fork: kind = Fork, parent recorded. Note that `fork`
    // repoints the store at the branch, so the trunk's row is the
    // *original* parent id and the branch's parent link names it.
    let (fork_id, _) = store.fork().await.unwrap();
    store
        .replace_messages(vec![Message::new(muta_contracts::Role::User, "branch")])
        .await
        .unwrap();
    let branch = store
        .list()
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.id == fork_id)
        .unwrap();
    assert_eq!(branch.fork_kind, muta_contracts::SessionForkKind::Fork);
    assert_eq!(branch.parent_id.as_deref(), Some(parent_id.as_str()));

    // Aside: kind = Aside. `fork_to_side` forks from the *current*
    // pointer — which after `fork()` is the branch — so the aside's
    // parent names the branch, not the trunk. That is the real
    // semantics: an aside inherits the conversation you are looking at.
    let (side_id, _) = store.fork_to_side().await.unwrap();
    let aside = store
        .list()
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.id == side_id)
        .unwrap();
    assert_eq!(aside.fork_kind, muta_contracts::SessionForkKind::Aside);
    assert_eq!(aside.parent_id.as_deref(), Some(fork_id.as_str()));

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn fork_to_side_leaves_primary_active_pointer_intact() {
    // ADR-0017: a side fork must NOT repoint the primary's active pointer.
    // The primary keeps its id, history, and (by construction) any in-flight
    // turn; only a self-contained sibling file is written.
    let directory = std::env::temp_dir().join(format!("muta-host-side-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    store
        .replace_messages(vec![Message::new(muta_contracts::Role::User, "parent")])
        .await
        .unwrap();
    let parent_id = store.id().await;

    let (side_id, source_id) = store.fork_to_side().await.unwrap();
    assert_eq!(source_id, parent_id);
    assert_ne!(side_id, parent_id);

    // The primary is untouched: same id, still holds "parent", and has no
    // parent link (it did not become a child).
    assert_eq!(store.id().await, parent_id);
    assert_eq!(store.model_window().await[0].content, "parent");
    assert!(store.parent_id().await.is_none());

    // The side loads into its own store with the inherited history and the
    // parent lineage recorded.
    let side = store.open_side(&side_id).await.unwrap();
    assert_eq!(side.id().await, side_id);
    assert_eq!(side.parent_id().await.as_deref(), Some(parent_id.as_str()));
    assert_eq!(side.model_window().await[0].content, "parent");

    // Writing to the side never reaches the primary.
    side.replace_messages(vec![Message::new(muta_contracts::Role::User, "side")])
        .await
        .unwrap();
    assert_eq!(store.model_window().await[0].content, "parent");

    // The side is independently resumable from disk (self-contained file).
    let reopened = store.open_side(&side_id).await.unwrap();
    assert_eq!(reopened.model_window().await[0].content, "side");

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn list_skips_active_session_when_it_has_no_content() {
    // Regression: a fresh empty active session used to be appended to
    // every `list()` call. Because `updated_at` is bumped on startup,
    // it sorted to the top and permanently showed "(empty session)".
    // The picker should only surface the active session once it has
    // real content (messages, archived messages, a loop checkpoint, or
    // a compaction marker).
    let directory =
        std::env::temp_dir().join(format!("muta-host-list-empty-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());

    // Seed one archived session so the picker has something to show,
    // then keep the active session empty (the default state).
    let archived = SessionData {
        project_root: directory.clone(),
        model_window: vec![Message::new(muta_contracts::Role::User, "archived branch")],
        ..Default::default()
    };
    store.persist_archive(&archived).unwrap();

    let sessions = store.list().await.unwrap();
    assert_eq!(
        sessions.len(),
        1,
        "empty active session must not appear in the list"
    );
    assert_eq!(sessions[0].id, archived.id);
    assert!(!sessions[0].active);

    // Once the active session gets content it should reappear, marked
    // active so the picker can badge it.
    store
        .replace_messages(vec![Message::new(
            muta_contracts::Role::User,
            "live branch",
        )])
        .await
        .unwrap();
    let sessions = store.list().await.unwrap();
    assert_eq!(sessions.len(), 2);
    assert!(sessions.iter().any(|item| item.active));
    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn list_filters_and_prunes_empty_sessions_on_disk() {
    let directory = std::env::temp_dir().join(format!("muta-prune-empty-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());

    // Create an empty session snapshot and event log directly on disk
    let empty_session_path = directory.join("empty-archived.json");
    let empty_session_jsonl = directory.join("empty-archived.jsonl");
    let empty_data = SessionData {
        id: "empty-archived".to_string(),
        project_root: directory.clone(),
        ..Default::default()
    };
    fs::write(
        &empty_session_path,
        serde_json::to_string(&empty_data).unwrap(),
    )
    .unwrap();
    fs::write(&empty_session_jsonl, "").unwrap();

    // And create a substantive session
    let substantive = SessionData {
        id: "real-session".to_string(),
        project_root: directory.clone(),
        model_window: vec![Message::new(muta_contracts::Role::User, "real dialogue")],
        ..Default::default()
    };
    store.persist_archive(&substantive).unwrap();

    let sessions = store.list().await.unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, "real-session");

    // The empty archived file should have been pruned from disk
    assert!(
        !empty_session_path.exists(),
        "empty session file on disk must be pruned"
    );
    assert!(
        !empty_session_jsonl.exists(),
        "empty session jsonl file on disk must be pruned"
    );

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn list_reads_overview_and_count_without_decoding_message_bodies() {
    // `list()` builds the `/sessions` picker rows. It must extract the
    // message count and the first-user-message overview *without* decoding
    // the full message bodies (which carry runner `children`, tool calls,
    // content blobs, …) — otherwise opening the picker or refreshing it
    // after a delete re-allocates the entire transcript of every session on
    // disk. The picker rows defer message bodies via `Box<RawValue>`; this
    // test pins that the deferred view still reports the right count and
    // overview for a heavy session, including a user turn buried under
    // assistant/tool output.
    let directory =
        std::env::temp_dir().join(format!("muta-list-deferred-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());

    // A user turn carrying nested runner children plus a tool result with a
    // large payload — the kind of content that made the old eager parse
    // expensive. The overview is the LAST effective user prompt ("nested
    // runner prompt"), not the System preamble and not the heavy payloads.
    let mut runner_child = Message::new(muta_contracts::Role::User, "nested runner prompt");
    runner_child.children = Some(vec![Message::new(
        muta_contracts::Role::Assistant,
        "runner reply",
    )]);
    let mut heavy_tool = Message::new(muta_contracts::Role::Tool, "x".repeat(50_000));
    heavy_tool.tool_call_id = Some("call_heavy".to_string());
    store
        .replace_messages(vec![
            Message::new(muta_contracts::Role::System, "system preamble"),
            Message::new(muta_contracts::Role::User, "the real first prompt"),
            Message::new(muta_contracts::Role::Assistant, "ack"),
            heavy_tool,
            runner_child,
        ])
        .await
        .unwrap();

    let sessions = store.list().await.unwrap();
    let row = sessions
        .iter()
        .find(|item| item.active)
        .expect("active session is listed");
    // Count comes from the array length, not the decoded bodies.
    assert_eq!(row.message_count, 5);
    // Overview is the LAST effective user prompt, not the System preamble
    // and not any of the heavy payloads.
    assert_eq!(row.overview, "nested runner prompt");

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn list_overview_excludes_command_echoes_and_picks_last_real_prompt() {
    // Regression: the overview is the most recent user turn that is *not* a
    // non-driving command echo (ADR-0050). A session whose final input was a
    // slash command (`/delegate on`) or a shell passthrough must show its
    // last genuine prompt instead — those echoes are agent operations, not
    // AI-conversation turns. This must hold through the deferred header
    // parse (which decodes `origin` as well as role/content).
    let directory = std::env::temp_dir().join(format!("muta-list-echo-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    store
        .replace_messages(vec![
            Message::new(muta_contracts::Role::System, "system preamble"),
            Message::new(muta_contracts::Role::User, "first real prompt"),
            Message::new(muta_contracts::Role::Assistant, "reply"),
            // A genuine later prompt — should win as the freshest.
            Message::new(muta_contracts::Role::User, "second real prompt"),
            Message::new(muta_contracts::Role::Assistant, "reply 2"),
            // Then non-driving echoes that must NOT become the overview
            // even though they are the last user-role messages:
            Message::command_echo("/delegate on"),
            Message::command_echo("/session open abc123"),
        ])
        .await
        .unwrap();

    let sessions = store.list().await.unwrap();
    let row = sessions.iter().find(|item| item.active).unwrap();
    assert_eq!(
        row.overview, "second real prompt",
        "command echoes are excluded; the last real prompt wins"
    );

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn list_uses_a_stored_title_in_preference_to_first_user_message() {
    // ADR-0022: a stored title (manual or AI) wins over the user-prompt
    // fallback. The deferred header parse still reads the top-level `title`
    // field, so this precedence must hold without decoding message bodies.
    let directory = std::env::temp_dir().join(format!("muta-list-title-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());

    store
        .replace_messages(vec![Message::new(
            muta_contracts::Role::User,
            "raw first prompt that should be hidden by the title",
        )])
        .await
        .unwrap();
    store
        .set_title(Some("Custom Title".to_string()), true)
        .await
        .unwrap();

    let sessions = store.list().await.unwrap();
    let row = sessions.iter().find(|item| item.active).unwrap();
    assert_eq!(row.overview, "Custom Title");

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn rename_live_session_sets_and_clears_the_manual_title() {
    // `RenameSession` on the active session delegates to `set_title`:
    // `Some` records a manual title (ADR-0022's lock: AI generation must
    // not overwrite it) that the picker row prefers over the first-prompt
    // preview; `None` clears the manual override so the overview falls
    // back to the AI-title / first-prompt preview.
    let directory = std::env::temp_dir().join(format!("muta-rename-live-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    store
        .replace_messages(vec![Message::new(
            muta_contracts::Role::User,
            "first prompt",
        )])
        .await
        .unwrap();
    let id = store.id().await;

    // Short-id prefix resolution, exactly like `delete`.
    store
        .rename(&id[..8], Some("manual name".to_string()))
        .await
        .unwrap();
    let (title, manual) = store.title().await;
    assert_eq!(title.as_deref(), Some("manual name"));
    assert!(manual, "a rename is a user-set (manual) title");
    let row = store
        .list()
        .await
        .unwrap()
        .into_iter()
        .find(|item| item.active)
        .unwrap();
    assert_eq!(row.overview, "manual name");

    // `None` clears the manual override: the stored title goes away, the
    // manual lock is released (AI generation may title it again), and the
    // picker row falls back to the first-prompt preview.
    store.rename(&id, None).await.unwrap();
    let (title, manual) = store.title().await;
    assert_eq!(title, None);
    assert!(!manual, "clearing releases the manual lock");
    let row = store
        .list()
        .await
        .unwrap()
        .into_iter()
        .find(|item| item.active)
        .unwrap();
    assert_eq!(row.overview, "first prompt");

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn rename_archived_session_persists_without_touching_the_active_one() {
    // `RenameSession` on a non-live session rewrites that session's file
    // in place (load → set title → append `TitleSet` → re-persist), so
    // the rename survives a cold reload — and the active session's own
    // state is never repointed or mutated.
    let directory =
        std::env::temp_dir().join(format!("muta-rename-archived-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    store
        .replace_messages(vec![Message::new(
            muta_contracts::Role::User,
            "live prompt",
        )])
        .await
        .unwrap();
    let live_id = store.id().await;

    let archived = SessionData {
        project_root: directory.clone(),
        model_window: vec![Message::new(muta_contracts::Role::User, "archived prompt")],
        ..Default::default()
    };
    store.persist_archive(&archived).unwrap();
    let archived_path = directory.join(format!("{}.json", archived.id));

    store
        .rename(&archived.id[..8], Some("renamed archived".to_string()))
        .await
        .unwrap();

    // The picker row now shows the manual title.
    let sessions = store.list().await.unwrap();
    let row = sessions.iter().find(|item| item.id == archived.id).unwrap();
    assert_eq!(row.overview, "renamed archived");

    // The snapshot on disk carries the title + manual lock…
    let on_disk: SessionData =
        serde_json::from_str(&fs::read_to_string(&archived_path).unwrap()).unwrap();
    assert_eq!(on_disk.title.as_deref(), Some("renamed archived"));
    assert!(on_disk.title_manual);

    // …and a cold reload (snapshot + event-log replay) restores it.
    let reloaded = SessionStore::for_path(archived_path);
    let (title, manual) = reloaded.title().await;
    assert_eq!(title.as_deref(), Some("renamed archived"));
    assert!(manual);

    // The active session is untouched: same id, no title.
    assert_eq!(store.id().await, live_id);
    let (title, manual) = store.title().await;
    assert_eq!(title, None);
    assert!(!manual);

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn rename_archived_session_clear_restores_the_prompt_fallback() {
    // Clearing an archived session's manual title must release the
    // ADR-0022 lock on disk too: the reloaded snapshot has `title = None`
    // / `manual = false`, so the picker row falls back to the
    // first-prompt preview and AI generation may title it again.
    let directory = std::env::temp_dir().join(format!(
        "muta-rename-archived-clear-{}",
        uuid::Uuid::new_v4()
    ));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    store
        .replace_messages(vec![Message::new(
            muta_contracts::Role::User,
            "live prompt",
        )])
        .await
        .unwrap();

    let archived = SessionData {
        project_root: directory.clone(),
        title: Some("old manual title".to_string()),
        title_manual: true,
        model_window: vec![Message::new(muta_contracts::Role::User, "archived prompt")],
        ..Default::default()
    };
    store.persist_archive(&archived).unwrap();

    store.rename(&archived.id, None).await.unwrap();

    let reloaded = SessionStore::for_path(directory.join(format!("{}.json", archived.id)));
    let (title, manual) = reloaded.title().await;
    assert_eq!(title, None);
    assert!(
        !manual,
        "clearing an archived title releases the manual lock"
    );
    let row = store
        .list()
        .await
        .unwrap()
        .into_iter()
        .find(|item| item.id == archived.id)
        .unwrap();
    assert_eq!(row.overview, "archived prompt");

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn rename_unknown_id_errors_like_delete() {
    // Resolution is shared with `delete`: an unknown prefix surfaces the
    // same "No session matches" error, and nothing is written.
    let directory =
        std::env::temp_dir().join(format!("muta-rename-unknown-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    store
        .replace_messages(vec![Message::new(
            muta_contracts::Role::User,
            "live prompt",
        )])
        .await
        .unwrap();

    let error = store
        .rename("deadbeef", Some("x".to_string()))
        .await
        .unwrap_err();
    assert_eq!(error, "No session matches 'deadbeef'.");

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn detail_returns_full_last_prompt_and_metadata() {
    // The session-info sub-view (`i`) calls `detail()`, which must return the
    // COMPLETE last effective user prompt (unlike the truncated picker
    // preview), plus title/timestamps/message-count — and must exclude
    // non-driving command echoes from the prompt, like `list()` does.
    let directory = std::env::temp_dir().join(format!("muta-detail-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    let long_prompt = "This is a fairly long prompt that exceeds the \
                           sixty-four character picker preview budget, so the \
                           truncated overview would cut it off with an ellipsis.";
    store
        .replace_messages(vec![
            Message::new(muta_contracts::Role::System, "system preamble"),
            Message::new(muta_contracts::Role::User, "earlier real prompt"),
            Message::new(muta_contracts::Role::Assistant, "reply"),
            Message::new(muta_contracts::Role::User, long_prompt),
            Message::new(muta_contracts::Role::Assistant, "reply 2"),
            // A trailing command echo must NOT become the last prompt.
            Message::command_echo("/delegate on"),
        ])
        .await
        .unwrap();
    let id = store.id().await;

    let detail = store.detail(&id).await.unwrap();
    assert_eq!(detail.id, id);
    assert!(detail.active);
    assert_eq!(detail.message_count, 6);
    // The FULL prompt is returned — not truncated.
    assert_eq!(detail.last_prompt.as_deref(), Some(long_prompt));

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn detail_returns_none_prompt_for_echo_only_session() {
    let directory = std::env::temp_dir().join(format!("muta-detail-echo-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    store
        .replace_messages(vec![Message::command_echo("/delegate on")])
        .await
        .unwrap();
    let id = store.id().await;

    let detail = store.detail(&id).await.unwrap();
    assert_eq!(
        detail.last_prompt, None,
        "a session with only command echoes has no real user prompt"
    );

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn todos_round_trip_through_disk() {
    let directory = std::env::temp_dir().join(format!("muta-todos-state-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    assert!(store.todos().await.is_empty());

    // Seed via reconcile and persist.
    let mut list = muta_contracts::TodoList::new();
    list.reconcile(
        &[
            ("Summary".to_string(), muta_contracts::TodoStatus::Pending),
            (
                "Key Changes".to_string(),
                muta_contracts::TodoStatus::Pending,
            ),
            ("Test Plan".to_string(), muta_contracts::TodoStatus::Pending),
        ],
        1000,
        3,
    );
    store.set_todos(list.clone()).await.unwrap();

    // Mutate (mark progress) and persist again — identity must survive.
    list.update("summary", muta_contracts::TodoStatus::Completed, 2000, 4);
    store.set_todos(list.clone()).await.unwrap();

    // Reload from disk via the event log + snapshot and confirm round-trip.
    let reloaded = SessionStore::for_path(path.clone());
    let loaded = reloaded.todos().await;
    assert_eq!(loaded.len(), 3, "all items round-trip through disk");
    assert_eq!(loaded.items[0].content, "Summary");
    assert_eq!(
        loaded.items[0].status,
        muta_contracts::TodoStatus::Completed
    );
    assert_eq!(loaded.updated_at_round, 4);
    // Identity is stable: the first item's id is unchanged after the update.
    assert_eq!(loaded.items[0].id, list.items[0].id);

    // Clearing persists (empty list is the "no active list" state).
    reloaded
        .set_todos(muta_contracts::TodoList::default())
        .await
        .unwrap();
    assert!(reloaded.todos().await.is_empty());

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn scheduled_jobs_round_trip_through_disk() {
    let directory =
        std::env::temp_dir().join(format!("muta-schedule-state-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    assert!(store.scheduled_jobs().await.is_empty());

    // Seed two jobs (one cron, one once) and persist.
    let now = chrono::Utc::now();
    let job_a = muta_contracts::ScheduledJob::cron(
        "aaaa".into(),
        "*/5 * * * *".into(),
        "check the deploy".into(),
        now,
    )
    .unwrap();
    let job_b = muta_contracts::ScheduledJob::cron(
        "bbbb".into(),
        "0 9 * * 1-5".into(),
        "standup".into(),
        now,
    )
    .unwrap();
    let job_c = muta_contracts::ScheduledJob::once(
        "cccc".into(),
        now + chrono::Duration::hours(2),
        "one-shot reminder".into(),
        now,
    );
    store
        .set_scheduled_jobs(vec![job_a.clone(), job_b.clone(), job_c.clone()])
        .await
        .unwrap();

    // Mutate (cancel one) and persist again — snapshot semantics.
    store
        .set_scheduled_jobs(vec![job_b.clone(), job_c.clone()])
        .await
        .unwrap();

    // Reload from disk via the event log + snapshot and confirm round-trip.
    let reloaded = SessionStore::for_path(path.clone());
    let loaded = reloaded.scheduled_jobs().await;
    assert_eq!(loaded.len(), 2, "only the surviving jobs round-trip");
    assert_eq!(loaded[0].id, "bbbb");
    assert_eq!(
        loaded[0].trigger,
        muta_contracts::Schedule::Cron {
            cron: "0 9 * * 1-5".to_string()
        }
    );
    assert_eq!(loaded[1].id, "cccc");
    assert!(loaded[1].trigger.is_once());

    // Clearing persists (empty list is the "no schedule" state).
    reloaded.set_scheduled_jobs(Vec::new()).await.unwrap();
    assert!(reloaded.scheduled_jobs().await.is_empty());

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn legacy_repeat_snapshot_drops_old_jobs() {
    // ADR-0120 policy: the pre-v9 `repeat_jobs` field is not aliased.
    // Jobs are rebuildable scheduler state (the schema-v9 comment's own
    // classification); an old snapshot loads with zero scheduled jobs
    // rather than carrying a compat mapping.
    let directory =
        std::env::temp_dir().join(format!("muta-schedule-legacy-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("session.json");
    let now = chrono::Utc::now();
    let legacy = serde_json::json!({
        "id": "legacy-repeat",
        "parent_id": null,
        "created_at": 0u64,
        "project_root": ".",
        "model_window": [],
        "archived_transcript": [],
        "todos": [],
        "repeat_jobs": [{
            "id": "legacy1",
            "cron": "0 9 * * 1-5",
            "prompt": "standup",
            "created_at": now,
            "next_fire": now,
            "last_fire": null,
        }],
        "schema_version": 8u32,
        "checksum": null,
    });
    fs::write(&path, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();

    let store = SessionStore::for_path(path.clone());
    let jobs = store.scheduled_jobs().await;
    assert!(jobs.is_empty(), "legacy jobs must not load, got {jobs:?}");
    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn commands_round_trip_through_disk() {
    // ADR-0091: the command ledger (invocation + typed result) must survive
    // persist + reload so resume reconstructs every command and its reply.
    let directory = std::env::temp_dir().join(format!("muta-commands-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    assert!(store.commands().await.is_empty());

    store
        .mutate_commands(|commands| {
            commands.push(
                muta_contracts::CommandRecord::new("search", "foo bar").with_result(
                    muta_contracts::CommandResult::Search {
                        query: "foo bar".to_string(),
                        hits: vec![muta_contracts::SearchHit {
                            text: "match".to_string(),
                            score: 0.9,
                        }],
                    },
                ),
            );
            commands.push(
                muta_contracts::CommandRecord::new("permissions", "").with_result(
                    muta_contracts::CommandResult::PermissionList {
                        allowed: vec!["execute_command".to_string()],
                    },
                ),
            );
        })
        .await
        .unwrap();

    // Dialogue materialises the session and its auxiliary command records.
    store
        .replace_messages(vec![Message::new(
            muta_contracts::Role::User,
            "first message",
        )])
        .await
        .unwrap();

    let loaded = SessionStore::for_path(path.clone());
    let commands = loaded.commands().await;
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0].name, "search");
    assert_eq!(commands[0].args, "foo bar");
    assert_eq!(
        commands[0].result.as_ref().unwrap().to_text(),
        "Relevant history (most similar first):\n\n1. [score=0.900]\nmatch"
    );
    assert_eq!(
        commands[1].result.as_ref().unwrap().to_text(),
        "Always-allowed tools:\n- execute_command"
    );

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn mutate_commands_does_not_persist_an_empty_session_snapshot() {
    // Navigational / inspection slash commands (/sessions, /models, /dashboard)
    // on an otherwise-empty session must remain lazy in memory and not materialize
    // an empty session file on disk.
    let directory =
        std::env::temp_dir().join(format!("muta-commands-lazy-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    store
        .mutate_commands(|commands| {
            commands.push(muta_contracts::CommandRecord::new("sessions", ""));
        })
        .await
        .unwrap();

    assert!(store.is_empty_unpersisted().await);
    assert!(
        !path.exists(),
        "commands alone must not persist an empty session"
    );

    // Once real dialogue content arrives, commands are persisted together with the snapshot.
    store
        .replace_messages(vec![Message::new(
            muta_contracts::Role::User,
            "first message",
        )])
        .await
        .unwrap();

    assert!(!store.is_empty_unpersisted().await);
    assert!(
        path.exists(),
        "dialogue content materializes the session with its commands"
    );

    let loaded = SessionStore::for_path(path.clone());
    assert_eq!(loaded.commands().await.len(), 1);
    assert_eq!(loaded.commands().await[0].name, "sessions");
    assert_eq!(loaded.model_window().await.len(), 1);

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn round_interrupts_round_trip_through_persistence() {
    // C11: interrupt records must survive a session close + reopen —
    // they are the durable answer to "why did this round stop".
    let directory =
        std::env::temp_dir().join(format!("muta-round-interrupts-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    // Materialize the session with dialogue first (an interrupt record on
    // a never-persisted empty session stays in memory, like commands).
    store
        .replace_messages(vec![Message::new(muta_contracts::Role::User, "hello")])
        .await
        .unwrap();
    store
        .record_round_interrupt(muta_contracts::RoundInterrupt {
            reason: muta_contracts::RoundInterruptReason::User,
            at_ms: 1_700_000_000_000,
            round: Some(1),
            detail: None,
        })
        .await
        .unwrap();
    store
        .record_round_interrupt(muta_contracts::RoundInterrupt {
            reason: muta_contracts::RoundInterruptReason::Terminated,
            at_ms: 1_700_000_060_000,
            round: Some(2),
            detail: None,
        })
        .await
        .unwrap();
    // Duplicate guard: same round + reason is a no-op.
    store
        .record_round_interrupt(muta_contracts::RoundInterrupt {
            reason: muta_contracts::RoundInterruptReason::Terminated,
            at_ms: 1_700_000_060_001,
            round: Some(2),
            detail: None,
        })
        .await
        .unwrap();

    let records = store.round_interrupts().await;
    assert_eq!(records.len(), 2, "duplicate (round, reason) is dropped");
    assert_eq!(
        records[0].reason,
        muta_contracts::RoundInterruptReason::User
    );
    assert_eq!(records[1].round, Some(2));

    // Reopen: the log is authoritative, so the records must reload.
    let loaded = SessionStore::for_path(path.clone());
    let reloaded = loaded.round_interrupts().await;
    assert_eq!(reloaded, records, "interrupt records survive reload");

    // Clearing removes them and also round-trips.
    loaded.clear_round_interrupts().await.unwrap();
    let cleared = SessionStore::for_path(path.clone());
    assert!(
        cleared.round_interrupts().await.is_empty(),
        "cleared records stay cleared after reload"
    );

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn legacy_snapshot_without_round_interrupts_loads() {
    // C11: a pre-v11 snapshot (no `round_interrupts` key) loads with an
    // empty list — `#[serde(default)]`, no migration needed.
    let directory = std::env::temp_dir().join(format!(
        "muta-round-interrupts-legacy-{}",
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("session.json");
    let legacy = serde_json::json!({
        "id": "legacy-interrupts",
        "parent_id": null,
        "created_at": 0u64,
        "project_root": ".",
        "model_window": [],
        "archived_transcript": [],
        "todos": { "items": [] },
        "scheduled_jobs": [],
        "schema_version": 10u32,
    });
    fs::write(&path, serde_json::to_string(&legacy).unwrap()).unwrap();
    fs::write(
            path.with_extension("jsonl"),
            "{\"seq\":0,\"timestamp\":1,\"type\":\"started\",\"id\":\"legacy-interrupts\",\"parent_id\":null,\"created_at\":0,\"project_root\":\".\",\"schema_version\":10}\n",
        )
        .unwrap();

    let store = SessionStore::for_path(path.clone());
    assert!(store.round_interrupts().await.is_empty());

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn retry_pending_round_trips_through_persistence() {
    // C12: the `/retry` resume point must survive a session close +
    // reopen — it is the durable "this round stopped, finish it" state,
    // and a re-hosted session offers `/retry` off it.
    let directory = std::env::temp_dir().join(format!("muta-retry-point-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    // Materialize the session with dialogue first (the point on a
    // never-persisted empty session stays in memory, like commands).
    store
        .replace_messages(vec![Message::new(muta_contracts::Role::User, "hello")])
        .await
        .unwrap();

    assert!(store.retry_pending().await.is_none(), "fresh: no point");
    let point = muta_contracts::RetryPoint {
        round: 3,
        turns_committed: 2,
        history_watermark: 5,
        paused_ms: 1_200,
        at_ms: 1_700_000_000_000,
    };
    store.arm_retry_pending(point.clone()).await.unwrap();
    assert_eq!(store.retry_pending().await, Some(point.clone()));

    // Reopen: the log is authoritative, so the point must reload intact —
    // including the turn ordinal and history watermark a resume replays.
    let loaded = SessionStore::for_path(path.clone());
    assert_eq!(
        loaded.retry_pending().await,
        Some(point),
        "resume point survives reload"
    );

    // Clearing removes it and also round-trips (a completed round never
    // re-offers /retry after another reload).
    loaded.clear_retry_pending().await.unwrap();
    let cleared = SessionStore::for_path(path.clone());
    assert!(
        cleared.retry_pending().await.is_none(),
        "cleared point stays cleared after reload"
    );

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn legacy_snapshot_without_retry_pending_loads() {
    // C12: a pre-v12 snapshot (no `retry_pending` key) loads with `None`
    // — `#[serde(default)]`, no migration needed. A legacy session simply
    // has no `/retry` affordance until a round stops under the new code.
    let directory =
        std::env::temp_dir().join(format!("muta-retry-point-legacy-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("session.json");
    let legacy = serde_json::json!({
        "id": "legacy-retry",
        "parent_id": null,
        "created_at": 0u64,
        "project_root": ".",
        "model_window": [],
        "archived_transcript": [],
        "todos": { "items": [] },
        "scheduled_jobs": [],
        "schema_version": 11u32,
    });
    fs::write(&path, serde_json::to_string(&legacy).unwrap()).unwrap();
    fs::write(
            path.with_extension("jsonl"),
            "{\"seq\":0,\"timestamp\":1,\"type\":\"started\",\"id\":\"legacy-retry\",\"parent_id\":null,\"created_at\":0,\"project_root\":\".\",\"schema_version\":11}\n",
        )
        .unwrap();

    let store = SessionStore::for_path(path.clone());
    assert!(store.retry_pending().await.is_none());

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn legacy_echo_messages_fold_into_ledger_on_v10_migration() {
    // ADR-0091 schema v10: a pre-v10 session whose message stream carries
    // ADR-0050 `CommandEcho` messages (slash + shell) must fold each into
    // the command ledger (`result: None`) and drop them from the window —
    // the message stream becomes pure dialogue again.
    let directory =
        std::env::temp_dir().join(format!("muta-commands-legacy-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("session.json");
    let legacy = serde_json::json!({
        "id": "legacy-echo",
        "parent_id": null,
        "created_at": 0u64,
        "project_root": ".",
        "model_window": [
            {"role": "User", "content": "/search foo", "hidden": false,
             "origin": {"kind": "command_echo"}},
            {"role": "User", "content": "hello", "hidden": false},
            {"role": "Assistant", "content": "hi", "hidden": false},
            {"role": "User", "content": "!ls -la", "hidden": false,
             "origin": {"kind": "command_echo"}},
        ],
        "archived_transcript": [],
        "todos": [],
        "scheduled_jobs": [],
        "schema_version": 9u32,
        "checksum": null,
    });
    fs::write(&path, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();

    let store = SessionStore::for_path(path.clone());
    let commands = store.commands().await;
    assert_eq!(
        commands.len(),
        2,
        "both echoes fold into the ledger: {:?}",
        commands
    );
    assert_eq!(commands[0].name, "search");
    assert_eq!(commands[0].args, "foo");
    assert!(
        commands[0].result.is_none(),
        "legacy echoes carry no result"
    );
    assert_eq!(commands[1].name, "shell");
    assert_eq!(commands[1].args, "ls -la");

    // The message stream keeps only the real dialogue.
    let window = store.model_window().await;
    let contents: Vec<&str> = window.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(contents, vec!["hello", "hi"]);
    assert!(
        window.iter().all(|m| !m.is_command_echo()),
        "no command echo remains in the message window"
    );

    let _ = fs::remove_dir_all(directory);
}

#[test]
fn commands_survive_snapshot_to_events_full_replay() {
    // ADR-0091: the ledger must survive a full event-log replay (log
    // compaction and legacy import rebuild state from `snapshot_to_events`
    // + `apply_events`, not from the snapshot JSON). The seed must emit a
    // `CommandsReplaced` event and replay must restore it.
    let data = SessionData {
        commands: vec![
            muta_contracts::CommandRecord::new("search", "foo").with_result(
                muta_contracts::CommandResult::Search {
                    query: "foo".to_string(),
                    hits: vec![],
                },
            ),
            muta_contracts::CommandRecord::new("shell", "!ls -la"),
        ],
        ..SessionData::default()
    };
    let events = snapshot_to_events(&data);
    assert!(
        events
            .iter()
            .any(|envelope| matches!(&envelope.event, SessionEvent::CommandsReplaced { .. })),
        "seed must emit CommandsReplaced"
    );

    let mut restored = SessionData::default();
    apply_events(&mut restored, &events);
    assert_eq!(restored.commands.len(), 2);
    assert_eq!(restored.commands[0].name, "search");
    assert_eq!(restored.commands[1].name, "shell");
    // apply_events does not run schema migration; commands are untouched
    // by the message-only replay paths.
    assert_eq!(
        restored.commands[0].result.as_ref().unwrap().to_text(),
        "No relevant history found."
    );
}

#[tokio::test]
async fn provider_selection_round_trips_through_disk() {
    // C6: a session's provider/model pin must survive persist + reload so a
    // reopened session lands on its own provider instead of the global
    // default, independent of every other session.
    //
    // The pin only persists on a session that has real content — an empty
    // session stays unpersisted (no empty-file litter), so seed a message
    // first.
    let directory =
        std::env::temp_dir().join(format!("muta-provider-sel-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    store
        .replace_messages(vec![Message::new(muta_contracts::Role::User, "seed")])
        .await
        .unwrap();
    assert!(store.provider_selection().await.is_none());

    store
        .set_provider_selection(Some(ProviderSelection {
            provider: "anthropic".to_string(),
            model: Some("claude-sonnet-4-6".to_string()),
        }))
        .await
        .unwrap();

    let loaded = SessionStore::for_path(path.clone());
    let sel = loaded.provider_selection().await;
    let sel = sel.expect("provider selection round-trips through disk");
    assert_eq!(sel.provider, "anthropic");
    assert_eq!(sel.model.as_deref(), Some("claude-sonnet-4-6"));

    // Clearing (None) also persists.
    loaded.set_provider_selection(None).await.unwrap();
    let cleared = SessionStore::for_path(path.clone());
    assert!(cleared.provider_selection().await.is_none());

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn set_provider_selection_does_not_persist_an_empty_session_snapshot() {
    // Regression: pinning a provider on a brand-new, message-less session
    // must NOT write the snapshot — otherwise an empty session (e.g. one a
    // user landed in after Ctrl+C at the startup picker) gets surfaced in
    // the sessions picker the moment they open `/models`. The pin lives in
    // memory only; it is dropped when the process exits.
    //
    // (The lazy seed may leave a `.jsonl` with one `Started` event from the
    // store constructor, but that never produces the `.json` snapshot the
    // picker lists, so the empty session stays invisible.)
    let directory =
        std::env::temp_dir().join(format!("muta-provider-empty-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    store
        .set_provider_selection(Some(ProviderSelection {
            provider: "anthropic".to_string(),
            model: Some("claude-sonnet-4-6".to_string()),
        }))
        .await
        .unwrap();
    // In-memory state updated…
    assert_eq!(
        store.provider_selection().await.as_ref().unwrap().provider,
        "anthropic"
    );
    // …but no snapshot on disk → the empty session never appears in the
    // picker (which lists `.json` files).
    assert!(
        !path.exists(),
        "an empty session must not be persisted as a snapshot by a provider pin"
    );

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn empty_session_is_not_persisted_until_real_content() {
    // Core laziness contract (ADR-0018): a session that is opened but never
    // gains real content (a user message OR a command echo) leaves NO
    // record on disk — opening and exiting must not pollute the session
    // history. Metadata-only mutations on a brand-new empty session
    // (title, provider, a no-op empty-window replace) stay in memory; the
    // first real message or command does persist.
    let directory =
        std::env::temp_dir().join(format!("muta-empty-deferred-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());

    // Metadata-only mutations on the empty session → no snapshot on disk.
    store.set_title(Some("t".to_string()), true).await.unwrap();
    store
        .set_provider_selection(Some(ProviderSelection {
            provider: "anthropic".to_string(),
            model: None,
        }))
        .await
        .unwrap();
    store.replace_messages(Vec::new()).await.unwrap(); // a no-op empty-window replace
    assert!(
        !path.exists(),
        "metadata/no-op mutations on an empty session must not persist a snapshot"
    );

    // A real command echo (via mutate_messages) DOES persist — the user
    // acted, so the session is now real content.
    store
        .mutate_messages(|w| w.push(Message::command_echo("/models")))
        .await
        .unwrap();
    assert!(
        path.exists(),
        "a real command echo persists the session (first-content contract)"
    );

    let _ = fs::remove_dir_all(directory);
}

/// The unified `is_user_facing_empty` rule (ADR-0018), pinned end-to-end
/// through the public store: substantive state (todos / scheduled jobs /
/// tool mask / round counter) materialises a fresh session, while the two
/// picker-litter offenders (title, provider selection) never do on their
/// own. This is the single definition every setter consults, so the test
/// guards against the per-setter drift the predicate was extracted to fix.
#[tokio::test]
async fn substantive_state_materialises_but_title_and_provider_do_not() {
    // Title + provider selection + commands alone stay unpersisted.
    let directory =
        std::env::temp_dir().join(format!("muta-unified-guard-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    store.set_title(Some("t".into()), true).await.unwrap();
    store
        .set_provider_selection(Some(ProviderSelection {
            provider: "anthropic".into(),
            model: None,
        }))
        .await
        .unwrap();
    store
        .mutate_commands(|c| c.push(muta_contracts::CommandRecord::new("sessions", "")))
        .await
        .unwrap();
    assert!(store.is_empty_unpersisted().await);
    assert!(
        !path.exists(),
        "title/provider/commands alone never materialise"
    );

    // A substantive todo list materialises the same session.
    let mut todos = muta_contracts::TodoList::new();
    todos.reconcile(
        &[("Task".to_string(), muta_contracts::TodoStatus::Pending)],
        1000,
        1,
    );
    store.set_todos(todos).await.unwrap();
    assert!(!store.is_empty_unpersisted().await);
    assert!(path.exists(), "a substantive todo list materialises");
    let _ = fs::remove_dir_all(directory);

    // A scheduled job likewise materialises a fresh session on its own.
    let directory2 =
        std::env::temp_dir().join(format!("muta-unified-guard2-{}", uuid::Uuid::new_v4()));
    let path2 = directory2.join("session.json");
    let store2 = SessionStore::for_path(path2.clone());
    let now = chrono::Utc::now();
    let job =
        muta_contracts::ScheduledJob::cron("j1".into(), "* * * * *".into(), "ping".into(), now)
            .expect("a valid cron expression yields a job");
    store2.set_scheduled_jobs(vec![job]).await.unwrap();
    assert!(
        path2.exists(),
        "a scheduled job is substantive and materialises the session"
    );
    let _ = fs::remove_dir_all(directory2);
}

#[tokio::test]
async fn request_usage_round_trips_through_disk() {
    let directory =
        std::env::temp_dir().join(format!("muta-request-usage-state-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    let session_id = store.id().await;
    // Materialise the active session with an active round counter
    store.set_round_counter(2).await.unwrap();
    let record = muta_contracts::RequestUsageRecord {
        key: muta_contracts::RequestUsageKey {
            session_id,
            actor_id: "master".to_string(),
            round: 2,
            turn: 1,
            attempt: 1,
        },
        provider: "openai".to_string(),
        model: "gpt".to_string(),
        status: muta_contracts::RequestUsageStatus::Completed,
        source: muta_contracts::RequestUsageSource::Reported,
        projected_prompt_tokens: 900,
        prompt_tokens: 910,
        completion_tokens: 90,
        total_tokens: 1_000,
        ..Default::default()
    };
    store
        .set_request_usage_records(vec![record.clone()])
        .await
        .unwrap();

    let reloaded = SessionStore::for_path(path);
    assert_eq!(reloaded.request_usage_records().await, vec![record]);
    assert!(
        reloaded
            .set_request_usage_records(Vec::new())
            .await
            .is_err(),
        "request attempts cannot be silently removed"
    );
    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn session_runtime_state_round_trips_through_disk() {
    // ADR-0048 Phase 2: the session-scoped runtime state — disabled-tool
    // mask and round counter — must survive persist + reload so a resumed
    // session restores the agent's exact state instead of silently dropping
    // a toggle or resetting the counter.
    let directory =
        std::env::temp_dir().join(format!("muta-runtime-state-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    assert!(store.disabled_tools().await.is_empty());
    assert_eq!(store.round_counter().await, 0);

    let mut disabled = std::collections::HashSet::new();
    disabled.insert("execute_command".to_string());
    disabled.insert("edit_file".to_string());
    store.set_disabled_tools(disabled.clone()).await.unwrap();
    store.set_round_counter(42).await.unwrap();

    let loaded = SessionStore::for_path(path.clone());
    assert_eq!(loaded.disabled_tools().await, disabled);
    assert_eq!(loaded.round_counter().await, 42);

    // Clearing each (0 / empty) persists.
    loaded
        .set_disabled_tools(std::collections::HashSet::new())
        .await
        .unwrap();
    loaded.set_round_counter(0).await.unwrap();
    let cleared = SessionStore::for_path(path.clone());
    assert!(cleared.disabled_tools().await.is_empty());
    assert_eq!(cleared.round_counter().await, 0);

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn delegated_posture_round_trips_through_disk() {
    // ADR-0132: the delegated posture is session-scoped persisted state.
    // A daemon crash mid-unattended-session must reopen unattended — the
    // store, not the process, is the authority.
    let directory = std::env::temp_dir().join(format!("muta-delegated-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    assert!(!store.delegated().await, "fresh sessions start attended");

    // Materialise the session first: a posture toggle alone on an empty
    // session must not materialise a file (is_user_facing_empty excludes
    // delegated on an otherwise-empty session must not materialise it as non-empty;
    store.set_round_counter(1).await.unwrap();
    store.set_delegated(true).await.unwrap();
    assert!(path.exists(), "materialised session persists the toggle");

    let loaded = SessionStore::for_path(path.clone());
    assert!(
        loaded.delegated().await,
        "the posture survives persist + reload"
    );

    // Toggling off persists too.
    loaded.set_delegated(false).await.unwrap();
    let reloaded = SessionStore::for_path(path.clone());
    assert!(!reloaded.delegated().await);

    // A same-value write is a no-op, not an event (log stays quiet).
    let events_before = {
        let state = reloaded.state.lock().await;
        state.event_log.load().map(|e| e.len()).unwrap_or(0)
    };
    reloaded.set_delegated(false).await.unwrap();
    let events_after = {
        let state = reloaded.state.lock().await;
        state.event_log.load().map(|e| e.len()).unwrap_or(0)
    };
    assert_eq!(
        events_before, events_after,
        "idempotent posture writes append no events"
    );

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn delegated_toggle_alone_does_not_materialise_empty_session() {
    // The emptiness rule deliberately excludes the delegated flag: arming
    // delegated on a brand-new session with no dialogue must not create
    // a session file on disk (nothing to resume yet).
    let directory =
        std::env::temp_dir().join(format!("muta-delegated-guard-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    store.set_delegated(true).await.unwrap();
    assert!(
        !path.exists(),
        "a posture toggle alone must not materialise an empty session"
    );
    assert!(
        store.delegated().await,
        "the in-memory value still holds for the live process"
    );
    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn delegated_event_replays_from_log() {
    // Event-sourced authority: a snapshot-less replay of the jsonl log
    // must rebuild the posture (the snapshot is only a cache; the log
    // wins).
    let directory =
        std::env::temp_dir().join(format!("muta-delegated-log-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    store.set_round_counter(1).await.unwrap(); // materialise
    store.set_delegated(true).await.unwrap();

    // Drop the snapshot, keep the event log: reload must replay.
    let _ = std::fs::remove_file(&path);
    let replayed = SessionStore::for_path(path.clone());
    assert!(
        replayed.delegated().await,
        "the posture replays from the event log alone"
    );
    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn delegated_false_is_omitted_from_snapshot_json() {
    // Canonical-JSON compatibility: an attended session serialises
    // without the key so legacy checksums stay byte-identical.
    let json = serde_json::to_string(&SessionData::default()).unwrap();
    assert!(!json.contains("\"delegated\""));
    let json_on = serde_json::to_string(&SessionData {
        delegated: true,
        ..SessionData::default()
    })
    .unwrap();
    assert!(json_on.contains("\"delegated\":true"));
}

#[test]
fn session_snapshot_round_counter_writes_canonical_key_and_rejects_legacy_key() {
    // ADR-0120 policy: the pre-ADR-0047 `turn_counter` key is not
    // aliased. It parses as an unknown field (dropped) and the counter
    // loads at its default — the stale value must not resurface.
    let mut canonical = serde_json::to_value(SessionData {
        round_counter: 11,
        ..SessionData::default()
    })
    .unwrap();
    let object = canonical.as_object_mut().unwrap();
    let counter = object.remove("round_counter").unwrap();
    object.insert("turn_counter".to_string(), counter);

    let loaded = serde_json::from_value::<SessionData>(canonical).unwrap();
    assert_eq!(
        loaded.round_counter, 0,
        "legacy key must not carry its value through"
    );

    let serialized = serde_json::to_string(&SessionData {
        round_counter: 11,
        ..SessionData::default()
    })
    .unwrap();
    assert!(serialized.contains("\"round_counter\":11"));
}

#[tokio::test]
async fn startup_new_session_can_resume_most_recent_cache() {
    let directory = std::env::temp_dir().join(format!("muta-host-resume-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    store
        .replace_messages(vec![Message::new(muta_contracts::Role::User, "previous")])
        .await
        .unwrap();
    let previous_id = store.id().await;

    let new_id = store.reset().await.unwrap();
    assert_ne!(new_id, previous_id);
    assert!(store.model_window().await.is_empty());

    let resumed_id = store.resume(None).await.unwrap();
    assert_eq!(resumed_id, previous_id);
    assert_eq!(store.model_window().await[0].content, "previous");
    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn snapshot_fast_path_replays_only_the_tail_on_lag() {
    // The snapshot fast path (C5): a checksum-valid snapshot with an
    // `applied_seq` watermark is authoritative for its folded range, and
    // only log events *after* the watermark are replayed. The
    // operationally important case is a lagging snapshot — a crash mid-turn
    // left `append_turn`'s `MessagesAppended` event in the log but the
    // snapshot still at the previous round boundary. The tail replay must
    // recover it. This is the "log authoritative for the tail" contract.
    let directory =
        std::env::temp_dir().join(format!("muta-fastpath-lag-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    let first_id = store.id().await;
    store
        .replace_messages(vec![Message::new(muta_contracts::Role::User, "first")])
        .await
        .unwrap();

    // Simulate a mid-turn crash: append an event the snapshot has NOT
    // folded (its watermark still points at the replace above).
    {
        let state = store.state.lock().await;
        state
            .event_log
            .append(SessionEvent::MessagesAppended {
                messages: vec![Message::new(
                    muta_contracts::Role::Assistant,
                    "recovered tail",
                )],
            })
            .unwrap();
        // Deliberately do NOT persist the snapshot, so its watermark lags.
    }

    // Re-open: the snapshot is checksum-valid but lags by one event. The
    // fast path must replay the tail and surface the appended message.
    let reloaded = SessionStore::for_path(path.clone());
    assert_eq!(reloaded.id().await, first_id);
    let messages = reloaded.model_window().await;
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].content, "first");
    assert_eq!(messages[1].content, "recovered tail");

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn checksum_invalid_snapshot_falls_back_to_full_replay() {
    // A snapshot whose stored checksum no longer matches its content (real
    // corruption, not a recompute) must not be trusted: the load falls
    // through to a full replay from the authoritative event log.
    let directory =
        std::env::temp_dir().join(format!("muta-fastpath-corrupt-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    store
        .replace_messages(vec![Message::new(muta_contracts::Role::User, "truth")])
        .await
        .unwrap();

    // Corrupt the snapshot's content WITHOUT recomputing the checksum, so
    // verification fails. Rewrite the raw JSON directly.
    let raw = fs::read_to_string(&path).unwrap();
    let mut tampered = raw.replace("truth", "LIE");
    // Force a checksum mismatch by zeroing the stored checksum field.
    tampered = tampered.replace(
        "\"checksum\":",
        "\"checksum\":999999999,\"_x\":0,\"checksum\":",
    );
    fs::write(&path, tampered).unwrap();

    let reloaded = SessionStore::for_path(path.clone());
    // Full replay from the log restores the true content.
    assert_eq!(reloaded.model_window().await[0].content, "truth");

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn applied_seq_watermark_round_trips_and_enables_empty_tail_load() {
    // A clean close persists the snapshot with its watermark stamped to the
    // log's high-water mark, so the next load replays an empty tail — the
    // O(snapshot) fast path that makes resume cheap.
    let directory =
        std::env::temp_dir().join(format!("muta-fastpath-clean-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    store
        .replace_messages(vec![Message::new(muta_contracts::Role::User, "hi")])
        .await
        .unwrap();
    store
        .set_title(Some("my session".to_string()), false)
        .await
        .unwrap();
    let persisted_id = store.id().await;

    // The on-disk snapshot must carry the watermark at the high-water mark.
    let on_disk: SessionData = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let watermark = on_disk.applied_seq.expect("watermark stamped on persist");
    let high = EventLog::new(path.with_extension("jsonl"))
        .high_seq()
        .expect("log is seeded");
    assert_eq!(
        watermark, high,
        "watermark == log high-water after clean persist"
    );

    // The tail past the watermark is empty.
    let tail = EventLog::new(path.with_extension("jsonl"))
        .load_since(Some(watermark))
        .unwrap();
    assert!(tail.is_empty(), "clean close leaves an empty tail");

    // Reload restores the title (snapshot-folded) and id.
    let reloaded = SessionStore::for_path(path.clone());
    assert_eq!(reloaded.id().await, persisted_id);
    assert_eq!(reloaded.model_window().await[0].content, "hi");

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn legacy_snapshot_without_watermark_falls_back_to_full_replay() {
    // A pre-C5 snapshot has no `applied_seq`. The load must not take the
    // fast path (there is no watermark to gate it); it replays the whole
    // log, then rewrites the snapshot with a watermark so the *next* load
    // is fast. This is the schema-migration path.
    let directory =
        std::env::temp_dir().join(format!("muta-fastpath-legacy-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    store
        .replace_messages(vec![Message::new(
            muta_contracts::Role::User,
            "legacy content",
        )])
        .await
        .unwrap();

    // Strip the watermark from the persisted snapshot, simulating a pre-C5
    // file (checksum is recomputed so the file is internally consistent).
    let mut data: SessionData = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    data.applied_seq = None;
    let test_blobs = BlobStore::new(directory.join("blobs"));
    write_session_file(&path, &data, &test_blobs).unwrap();

    // Reload: no watermark → full replay → rewrite with watermark.
    let reloaded = SessionStore::for_path(path.clone());
    assert_eq!(reloaded.model_window().await[0].content, "legacy content");

    // The snapshot on disk now has a watermark (the reload rewrote it).
    let rewritten: SessionData = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert!(
        rewritten.applied_seq.is_some(),
        "reload backfills the watermark"
    );

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn event_log_compacts_once_past_threshold_and_stays_consistent() {
    // Once the append-only log exceeds LOG_COMPACTION_THRESHOLD events and
    // the snapshot has fully folded it, a persist rewrites the log to a
    // single seed and the snapshot's watermark matches the seed's
    // high-water mark — so a subsequent reload replays an empty tail and
    // sees identical state. No event is lost.
    let directory =
        std::env::temp_dir().join(format!("muta-log-compaction-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());

    // Seed real content so the session is persisted (the empty-session
    // deferral would otherwise skip the title-set writes below).
    store
        .replace_messages(vec![Message::new(muta_contracts::Role::User, "seed")])
        .await
        .unwrap();

    // Push well past the threshold via repeated title sets (cheap events).
    for i in 0..(LOG_COMPACTION_THRESHOLD + 64) as u64 {
        store.set_title(Some(format!("t{i}")), false).await.unwrap();
    }
    let persisted_id = store.id().await;

    // After the final persist, the log should have been compacted back to a
    // small seed (snapshot_to_events emits a handful of lines, not 1k+).
    let log = EventLog::new(path.with_extension("jsonl"));
    let count = log.load().unwrap().len();
    assert!(
        count < LOG_COMPACTION_THRESHOLD,
        "log should be compacted, has {count} events"
    );

    // The on-disk snapshot watermark matches the seed's high-water mark.
    let on_disk: SessionData = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let watermark = on_disk.applied_seq.expect("watermark present");
    let high = log.high_seq().expect("seeded log");
    assert_eq!(watermark, high, "watermark == compacted-seed high-water");

    // Reload is consistent and replays an empty tail.
    let tail = log.load_since(Some(watermark)).unwrap();
    assert!(tail.is_empty());
    let reloaded = SessionStore::for_path(path.clone());
    assert_eq!(reloaded.id().await, persisted_id);
    let (title, _) = reloaded.title().await;
    assert_eq!(title.as_deref(), Some("t1087")); // last set wins

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn append_turn_persists_delta_and_survives_reload() {
    // The mid-round, turn-boundary save point (ADR-0048): `append_turn`
    // writes only the
    // new tail as a `MessagesAppended` event, and a fresh `SessionStore`
    // at the same path must replay it to recover the full history. This
    // is the resume-after-crash contract — the whole point of the feature.
    let directory = std::env::temp_dir().join(format!("muta-append-turn-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());

    // The round opens with one user message, durably written.
    store
        .replace_messages(vec![Message::new(
            muta_contracts::Role::User,
            "user prompt",
        )])
        .await
        .unwrap();

    // Turn 1 adds an assistant response + a tool result. The caller
    // passes the *full* current history; the store appends only the tail.
    let turn1 = vec![
        Message::new(muta_contracts::Role::User, "user prompt"),
        Message::new(muta_contracts::Role::Assistant, "I will run a tool"),
        Message::new(muta_contracts::Role::Tool, "tool output"),
    ];
    store.append_turn(&turn1).await.unwrap();

    // Turn 2 adds more. The snapshot cache is still at the round-open
    // state (one message); only the event log has grown.
    let turn2 = vec![
        Message::new(muta_contracts::Role::User, "user prompt"),
        Message::new(muta_contracts::Role::Assistant, "I will run a tool"),
        Message::new(muta_contracts::Role::Tool, "tool output"),
        Message::new(muta_contracts::Role::Assistant, "done"),
    ];
    store.append_turn(&turn2).await.unwrap();

    // The live in-memory state reflects all appends.
    let live = store.model_window().await;
    assert_eq!(live.len(), 4);
    assert_eq!(live[3].content, "done");

    // A brand-new store replays the event log and recovers everything,
    // including the appended tail the snapshot never recorded.
    let reloaded = SessionStore::for_path(path.clone());
    let recovered = reloaded.model_window().await;
    assert_eq!(recovered.len(), 4, "appended turns survive reload");
    assert_eq!(recovered[2].content, "tool output");
    assert_eq!(recovered[3].content, "done");

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn commit_turn_unifies_messages_counter_and_usage_in_one_event_batch() {
    // `commit_turn` is the single persistence transaction the turn loop
    // issues: message-tail delta + round counter + changed usage records,
    // in one lock acquisition and at most one snapshot write. A fresh store
    // must replay all three event kinds and recover the identical state.
    let directory = std::env::temp_dir().join(format!("muta-commit-turn-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    store
        .replace_messages(vec![Message::new(
            muta_contracts::Role::User,
            "user prompt",
        )])
        .await
        .unwrap();

    let record = muta_contracts::RequestUsageRecord {
        key: muta_contracts::RequestUsageKey {
            session_id: store.id().await,
            round: 1,
            turn: 1,
            ..Default::default()
        },
        status: muta_contracts::RequestUsageStatus::Completed,
        ..Default::default()
    };

    let turn = vec![
        Message::new(muta_contracts::Role::User, "user prompt"),
        Message::new(muta_contracts::Role::Assistant, "done"),
    ];
    store
        .commit_turn(CommitTurn {
            messages: &turn,
            round_counter: Some(3),
            usage_records: std::slice::from_ref(&record),
        })
        .await
        .unwrap();

    // Live state reflects every mutation.
    assert_eq!(store.model_window().await.len(), 2);
    assert_eq!(store.round_counter().await, 3);

    // Reload: the event log must replay to the same state.
    let reloaded = SessionStore::for_path(path.clone());
    assert_eq!(reloaded.model_window().await.len(), 2);
    assert_eq!(reloaded.round_counter().await, 3);

    // Committing the same window + counter + unchanged usage again is a
    // no-op (idempotent re-settlement after a retry).
    store
        .commit_turn(CommitTurn {
            messages: &turn,
            round_counter: Some(3),
            usage_records: std::slice::from_ref(&record),
        })
        .await
        .unwrap();
    assert_eq!(store.model_window().await.len(), 2);
    assert_eq!(store.round_counter().await, 3);

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn commit_turn_replaces_messages_when_same_length_but_content_changed() {
    let directory = std::env::temp_dir().join(format!("muta-commit-edit-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    store
        .replace_messages(vec![Message::new(
            muta_contracts::Role::User,
            "original prompt",
        )])
        .await
        .unwrap();

    let edited = vec![Message::new(
        muta_contracts::Role::User,
        "edited prompt",
    )];

    store
        .commit_turn(CommitTurn {
            messages: &edited,
            round_counter: None,
            usage_records: &[],
        })
        .await
        .unwrap();

    let fresh = SessionStore::for_path(path.clone());
    assert_eq!(fresh.model_window().await[0].content, "edited prompt");

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn append_turn_is_noop_when_nothing_new() {
    // Passing a history no longer than the durable baseline (e.g. right
    // after a compaction rewrote the window via `replace_messages`) must
    // not corrupt anything or write a spurious event.
    let directory = std::env::temp_dir().join(format!("muta-append-noop-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    let messages = vec![Message::new(muta_contracts::Role::User, "hi")];
    store.replace_messages(messages.clone()).await.unwrap();

    // Same length, same content → no-op.
    store.append_turn(&messages).await.unwrap();
    assert_eq!(store.model_window().await.len(), 1);

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn append_turn_falls_back_to_replace_on_divergent_prefix() {
    // If the incoming prefix disagrees with the durable state (a bug or a
    // compaction that bypassed `replace_messages`), `append_turn` must
    // fall back to a full replace rather than splice a corrupt tail.
    let directory =
        std::env::temp_dir().join(format!("muta-append-diverge-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    store
        .replace_messages(vec![Message::new(muta_contracts::Role::User, "original")])
        .await
        .unwrap();

    // Incoming history where the durable prefix was *rewritten* — the
    // first message content differs.
    let divergent = vec![
        Message::new(muta_contracts::Role::User, "rewritten"),
        Message::new(muta_contracts::Role::Assistant, "new"),
    ];
    store.append_turn(&divergent).await.unwrap();

    // The fallback replaced everything with the incoming history.
    let live = store.model_window().await;
    assert_eq!(live.len(), 2);
    assert_eq!(live[0].content, "rewritten");
    assert_eq!(live[1].content, "new");

    // And a reload recovers the replaced state, not a corrupt splice.
    let reloaded = SessionStore::for_path(path.clone());
    let recovered = reloaded.model_window().await;
    assert_eq!(recovered[0].content, "rewritten");

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn projection_snapshot_import_does_not_duplicate_archive() {
    let directory =
        std::env::temp_dir().join(format!("muta-projection-import-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("session.json");
    let blob_store = BlobStore::new(directory.join("blobs"));
    let snapshot = SessionData {
        model_window: vec![Message::new(muta_contracts::Role::User, "live window")],
        archived_transcript: vec![Message::new(muta_contracts::Role::Assistant, "archived")],
        last_projection: Some(ContextProjectionCheckpoint {
            operation: ContextProjectionKind::Compact,
            archived_messages: 1,
            active_messages: 1,
            window_tokens_before: 100,
            window_tokens_after: 20,
        }),
        ..Default::default()
    };
    write_session_file(&path, &snapshot, &blob_store).unwrap();

    let store = SessionStore::for_path(path.clone());
    let transcript = store.full_transcript().await;
    assert_eq!(transcript.len(), 2);
    assert_eq!(transcript[0].content, "archived");
    assert_eq!(transcript[1].content, "live window");
    assert_eq!(
        store.last_projection().await.unwrap().operation,
        ContextProjectionKind::Compact
    );

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn large_message_content_is_offloaded_to_blob_store() {
    let directory =
        std::env::temp_dir().join(format!("muta-blob-session-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    let big = "x".repeat(8_192);
    store
        .replace_messages(vec![Message::new(muta_contracts::Role::User, &big)])
        .await
        .unwrap();

    // Snapshot on disk should reference the blob.
    let raw = fs::read_to_string(&path).unwrap();
    assert!(
        raw.contains("content_blob"),
        "large content should be offloaded"
    );
    assert!(
        !raw.contains(&big),
        "raw content should not appear in snapshot"
    );

    // Replaying the event log rehydrates content from the blob store.
    let reloaded = SessionStore::for_path(path.clone());
    let messages = reloaded.model_window().await;
    assert_eq!(messages[0].content, big);
    assert!(
        messages[0].content_blob.is_none(),
        "memory uses inline content"
    );

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn injection_origin_survives_persist_and_reload() {
    // A harness-injected message's provenance must round-trip through the
    // snapshot cache AND the event-log replay path — the contract that lets
    // a resumed session faithfully reconstruct what was injected and why.
    // This is the end-to-end (store-layer) companion to the message-level
    // round-trip test in muta-contracts.
    use muta_contracts::{HookEventKind, InjectionKind, InjectionOrigin};
    let directory =
        std::env::temp_dir().join(format!("muta-origin-session-{}", uuid::Uuid::new_v4()));
    let path = directory.join("session.json");
    let store = SessionStore::for_path(path.clone());
    let injected = Message::injected(
        muta_contracts::Role::User,
        "setup context",
        InjectionOrigin::new(InjectionKind::Hook(HookEventKind::SessionStart))
            .with_reason("onstart.sh"),
    );
    store.replace_messages(vec![injected]).await.unwrap();

    // The snapshot file carries the origin object.
    let raw = fs::read_to_string(&path).unwrap();
    assert!(
        raw.contains("\"origin\""),
        "snapshot must persist origin: {raw}"
    );
    // HookEventKind serialises in PascalCase (no rename_all), so the wire
    // tag is "SessionStart". Pretty-printed with a space after the colon.
    assert!(
        raw.contains("\"hook\": \"SessionStart\""),
        "snapshot must persist the hook kind: {raw}"
    );

    // Reload via the event-log path (authoritative) rehydrates it intact.
    let reloaded = SessionStore::for_path(path.clone());
    let messages = reloaded.model_window().await;
    assert_eq!(messages.len(), 1);
    let origin = messages[0].origin.as_ref().expect("origin reloaded");
    assert_eq!(
        origin.kind,
        InjectionKind::Hook(HookEventKind::SessionStart)
    );
    assert_eq!(origin.reason.as_deref(), Some("onstart.sh"));

    let _ = fs::remove_dir_all(directory);
}

#[tokio::test]
async fn legacy_snapshot_without_origin_loads_as_none() {
    // A pre-C4 snapshot file (no `origin` key on any message) must load
    // with `origin: None` for every message — the store-layer side of the
    // backward-compat contract. Provenance is simply absent for old data.
    let directory =
        std::env::temp_dir().join(format!("muta-legacy-origin-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&directory).unwrap();
    let path = directory.join("session.json");
    // Minimal pre-C4 snapshot: no origin key, schema_version 3.
    let legacy = serde_json::json!({
        "id": "legacy",
        "parent_id": null,
        "created_at": 1u64,
        "updated_at": 1u64,
        "messages": [
            {"role":"User","content":"old user input","hidden":false},
            {"role":"Assistant","content":"old reply","hidden":false}
        ],
        "archived_messages": [],
        "loop_checkpoint": null,
        "last_relief": null,
        "project_root": ".",
        "todos": [],
        "schema_version": 3,
        "checksum": null,
        "title": null,
        "title_manual": false,
        "pursuit": null
    });
    fs::write(&path, legacy.to_string()).unwrap();

    let store = SessionStore::for_path(path.clone());
    let messages = store.model_window().await;
    // ADR-0120 policy: pre-rename snapshot keys (`messages` /
    // `archived_messages`) are not aliased; the unparseable snapshot
    // loads as a fresh empty session rather than half-migrating.
    assert!(
        messages.iter().all(|m| m.origin.is_none()),
        "any loaded message must lack origin"
    );

    let _ = fs::remove_dir_all(directory);
}

#[test]
fn compaction_keeps_recent_complete_rounds() {
    let messages = vec![
        Message::new(muta_contracts::Role::System, "system"),
        Message::new(muta_contracts::Role::User, "old question"),
        Message::new(muta_contracts::Role::Assistant, "old answer"),
        Message::new(muta_contracts::Role::Tool, "old tool result"),
        Message::new(muta_contracts::Role::User, "recent question"),
        Message::new(muta_contracts::Role::Assistant, "recent answer"),
    ];

    let result = compact_messages(&messages, 10_000, 1).unwrap();

    assert_eq!(result.checkpoint.operation, ContextProjectionKind::Compact);
    assert_eq!(result.model_window[0].role, muta_contracts::Role::User);
    assert!(result.model_window[0].hidden);
    assert_eq!(result.model_window[1].content, "recent question");
    assert_eq!(result.model_window[2].content, "recent answer");
    assert!(
        result
            .archived_originals
            .iter()
            .any(|message| message.content == "old tool result")
    );
    assert!(
        !result
            .archived_originals
            .iter()
            .any(|message| message.role == muta_contracts::Role::System)
    );
}

#[test]
fn compaction_requires_an_older_complete_round() {
    let messages = vec![
        Message::new(muta_contracts::Role::User, "question"),
        Message::new(muta_contracts::Role::Assistant, "answer"),
    ];
    assert!(compact_messages(&messages, 10_000, 1).is_none());
}

#[test]
fn compaction_reduces_preserved_tail_to_honor_working_memory_target() {
    let mut messages = Vec::new();
    for round in 0..4 {
        let body = format!("round-{round} {}", "word ".repeat(200));
        messages.push(Message::new(Role::User, body.clone()));
        messages.push(Message::new(Role::Assistant, body));
    }

    // Three complete rounds are requested, but that would consume almost
    // all of this 800-token target. The selector keeps one recent round so
    // a checkpoint still has room to carry durable task state.
    let selection = select_compaction_for_target(&messages, 3, 800).unwrap();
    assert_eq!(
        selection
            .tail
            .iter()
            .filter(|message| message.role == Role::User)
            .count(),
        1
    );
    assert!(estimate_tokens(&selection.tail) <= 600);
}

#[test]
fn summary_truncation_respects_the_projection_token_unit() {
    let summary = truncate_summary_to_token_budget("中".repeat(400), 100);
    assert!(muta_contracts::tokenizer::count_tokens(&summary) <= 100);
}

#[test]
fn excerpt_summary_keeps_recent_first_under_budget() {
    // A large old tool result and a small recent user message. With a tiny
    // token budget only the recent message (chosen newest-first) survives;
    // the old verbose tool result is omitted instead of crowding it out.
    let archived = vec![
        Message::new(Role::Tool, "X".repeat(3_000)),
        Message::new(Role::User, "recent critical detail"),
    ];

    let summary = build_excerpt_summary(&archived, 20, None);

    assert!(summary.contains("recent critical detail"));
    assert!(!summary.contains('X'));
}

#[test]
fn excerpt_summary_carries_forward_previous_summary() {
    let archived = vec![Message::new(Role::User, "what is 2+2")];
    let summary = build_excerpt_summary(&archived, 4_000, Some("prev anchored facts"));

    assert!(summary.starts_with("[Previous summary]\n"));
    assert!(summary.contains("prev anchored facts"));
    assert!(summary.contains("[Recent history]"));
    assert!(summary.contains("what is 2+2"));
}

#[test]
fn select_compaction_extracts_previous_summary() {
    let prior = checkpoint_message("prev summary body");
    let messages = vec![
        Message::new(Role::System, "system"),
        prior,
        Message::new(Role::User, "q1"),
        Message::new(Role::Assistant, "a1"),
        Message::new(Role::User, "q2"),
        Message::new(Role::Assistant, "a2"),
    ];

    let selection = select_compaction(&messages, 1).unwrap();
    assert_eq!(
        selection.previous_summary.as_deref(),
        Some("prev summary body")
    );
    // The prior checkpoint lands in the archived head, not the tail.
    assert!(
        selection
            .archived
            .iter()
            .any(|message| message.content.starts_with("[Conversation checkpoint]"))
    );
    assert_eq!(selection.tail.last().unwrap().content, "a2");
}

#[tokio::test]
async fn run_compaction_uses_provider_summary() {
    let mut history = vec![
        Message::new(Role::System, "system"),
        Message::new(Role::User, "old question"),
        Message::new(Role::Assistant, "old answer"),
        Message::new(Role::User, "recent question"),
        Message::new(Role::Assistant, "recent answer"),
    ];
    let provider: Arc<dyn Provider> = Arc::new(CompactionProvider);

    let result = run_compaction(&mut history, 10_000, 1, Some(provider), Vec::new())
        .await
        .unwrap()
        .unwrap();

    // The mock provider's canned reply becomes the checkpoint summary.
    assert!(result.model_window[0].content.contains("mock AI"));
    assert_eq!(result.model_window[1].content, "recent question");
    assert!(result.model_window[0].hidden);
}

#[tokio::test]
async fn run_compaction_falls_back_when_provider_errors() {
    // Pass a provider that always errors and assert we still get an
    // excerpt-based checkpoint.
    struct FailingProvider;
    #[async_trait]
    impl Provider for FailingProvider {
        async fn chat(&self, _request: muta_contracts::ModelRequest) -> Result<muta_contracts::ProviderCompletion, String> {
            Err("boom".to_string())
        }
        async fn stream_chat(
            &self,
            _request: muta_contracts::ModelRequest,
        ) -> Result<futures::stream::BoxStream<'static, Result<String, String>>, String> {
            Err("boom".to_string())
        }
    }

    let mut history = vec![
        Message::new(Role::User, "old question"),
        Message::new(Role::Assistant, "old answer"),
        Message::new(Role::User, "recent question"),
        Message::new(Role::Assistant, "recent answer"),
    ];
    let provider: Arc<dyn Provider> = Arc::new(FailingProvider);

    let result = run_compaction(&mut history, 10_000, 1, Some(provider), Vec::new())
        .await
        .unwrap()
        .unwrap();

    // Fallback excerpt summary references the old question.
    assert!(result.model_window[0].content.contains("old question"));
}
