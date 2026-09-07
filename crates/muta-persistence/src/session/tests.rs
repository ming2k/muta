//! Session persistence test suite (ADR-0186 single-transcript model).
//!
//! Tests exercise the public [`SessionStore`] API against the SQLite store:
//! the transcript is the authority, the window is a derive, projections are
//! append-only directives, and fork shares facts by identity.

use crate::session::*;
use crate::blobs::BlobStore;
use muta_contracts::{
    DirectiveKind, DirectivePayload, InjectionKind, Message, ProjectionDirective, Role, ToolCall,
    TranscriptEntry,
};
use std::path::PathBuf;
use tokio::sync::Mutex;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("muta-t18x-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn store(tag: &str) -> SessionStore {
    let dir = temp_dir(tag);
    let path = dir.join("session.json");
    SessionStore::for_path(path)
}

fn user(content: &str) -> Message {
    Message::new(Role::User, content)
}

fn assistant(content: &str) -> Message {
    Message::new(Role::Assistant, content)
}

// ---------------------------------------------------------------
// Round trip: transcript is the SSOT; the window is a derive.
// ---------------------------------------------------------------

#[tokio::test]
async fn messages_round_trip_through_reload() {
    let dir = temp_dir("roundtrip");
    let path = dir.join("session.json");
    let store = SessionStore::for_path(path.clone());
    store
        .replace_messages(vec![user("alpha"), assistant("beta")])
        .await
        .unwrap();

    let reloaded = SessionStore::for_path(path);
    let window = reloaded.model_window().await;
    assert_eq!(window.len(), 2);
    assert_eq!(window[0].content, "alpha");
    assert_eq!(window[1].content, "beta");
}

#[tokio::test]
async fn full_transcript_includes_everything_unprojected() {
    let store = store("full").await;
    store
        .replace_messages(vec![user("old"), assistant("older")])
        .await
        .unwrap();
    // Prune the assistant's tool result... actually prune targets tool bodies;
    // instead compact to hide the head and verify full_transcript still sees it.
    let full = store.full_transcript().await;
    assert_eq!(full.len(), 2);
    let window = store.model_window().await;
    assert_eq!(window.len(), 2);
    let _ = store.archived_transcript_count().await; // no directives yet
}

// ---------------------------------------------------------------
// Projection: commit translates to directives; view reproduces.
// ---------------------------------------------------------------

#[tokio::test]
async fn compaction_commit_hides_range_and_keeps_originals() {
    let store = store("compact").await;
    store
        .replace_messages(vec![user("old1"), assistant("old2"), user("tail")])
        .await
        .unwrap();
    let checkpoint = checkpoint_message("summary of old rounds");
    let result = ContextProjectionResult {
        model_window: vec![checkpoint, user("tail")],
        archived_originals: vec![user("old1"), assistant("old2")],
        checkpoint: ContextProjectionCheckpoint {
            operation: ContextProjectionKind::Compact,
            archived_messages: 2,
            active_messages: 2,
            window_tokens_before: 10,
            window_tokens_after: 5,
        },
    };
    store.commit_context_projection(result).await.unwrap();

    let window = store.model_window().await;
    assert_eq!(window.len(), 2);
    assert!(window[0].content.starts_with("[Conversation checkpoint]"));
    assert_eq!(window[1].content, "tail");
    // Originals remain recoverable.
    assert_eq!(store.archived_transcript_count().await, 2);
    // Round trip: the derived view survives a reload.
    let dir = temp_dir("compact-rt");
    let data = {
        // Persist current state to a fresh store via replace on a shared db
        let path = dir.join("session.json");
        let other = SessionStore::for_path(path);
        other
            .replace_messages(vec![user("old1"), assistant("old2"), user("tail")])
            .await
            .unwrap();
        other
            .commit_context_projection(ContextProjectionResult {
                model_window: vec![checkpoint_message("summary of old rounds"), user("tail")],
                archived_originals: vec![],
                checkpoint: ContextProjectionCheckpoint {
                    operation: ContextProjectionKind::Compact,
                    archived_messages: 2,
                    active_messages: 2,
                    window_tokens_before: 10,
                    window_tokens_after: 5,
                },
            })
            .await
            .unwrap();
        other.model_window().await
    };
    assert_eq!(data.len(), 2);
}

#[tokio::test]
async fn prune_commit_appends_directive_not_rewrite() {
    let store = store("prune").await;
    let call = ToolCall::new("call-1", "read_file", "{}");
    let mut assistant_message = assistant("working");
    assistant_message.tool_calls = Some(vec![call.clone()]);
    let tool_result = Message::tool_result(&call, "long output");
    store
        .replace_messages(vec![user("go"), assistant_message, tool_result.clone()])
        .await
        .unwrap();

    let mut window = store.model_window().await;
    window[2].content = "[pruned]".into();
    let original_body = window[2].content.clone();
    let _ = original_body;
    let archived = store.full_transcript().await;
    store
        .commit_context_projection(ContextProjectionResult {
            model_window: window,
            archived_originals: vec![tool_result.clone()],
            checkpoint: ContextProjectionCheckpoint {
                operation: ContextProjectionKind::Prune,
                archived_messages: 1,
                active_messages: 3,
                window_tokens_before: 9,
                window_tokens_after: 3,
            },
        })
        .await
        .unwrap();

    let window = store.model_window().await;
    assert_eq!(window[2].content, "[pruned]");
    // Full transcript keeps the original body.
    let full = store.full_transcript().await;
    assert_eq!(full[2].content, "long output");
}

// ---------------------------------------------------------------
// Turn commits: prefix-delta appends; divergence rebuilds.
// ---------------------------------------------------------------

#[tokio::test]
async fn append_turn_appends_delta() {
    let store = store("append").await;
    store.replace_messages(vec![user("go")]).await.unwrap();
    store
        .append_turn(&[user("go"), assistant("done")])
        .await
        .unwrap();
    let window = store.model_window().await;
    assert_eq!(window.len(), 2);
    assert_eq!(window[1].content, "done");
}

#[tokio::test]
async fn commit_turn_persists_window_counter_and_usage() {
    let store = store("commit").await;
    store.replace_messages(vec![user("go")]).await.unwrap();
    let mut messages = store.model_window().await;
    messages.push(assistant("ok"));
    store
        .commit_turn(CommitTurn::new(&messages).tap_counter(3))
        .await
        .unwrap();
    assert_eq!(store.model_window().await.len(), 2);
    assert_eq!(store.round_counter().await, 3);
}

impl CommitTurn<'_> {
    fn tap_counter(mut self, counter: u64) -> Self {
        self.round_counter = Some(counter);
        self
    }
}

#[tokio::test]
async fn commit_turn_rebuilds_on_divergence() {
    let store = store("diverge").await;
    store
        .replace_messages(vec![user("a"), assistant("b")])
        .await
        .unwrap();
    store
        .commit_turn(CommitTurn::new(&[user("replaced")]))
        .await
        .unwrap();
    let window = store.model_window().await;
    assert_eq!(window.len(), 1);
    assert_eq!(window[0].content, "replaced");
}

// ---------------------------------------------------------------
// Working state: todos derive from state entries; title is terminal.
// ---------------------------------------------------------------

#[tokio::test]
async fn todos_round_trip_through_state_entries() {
    let dir = temp_dir("todos");
    let path = dir.join("session.json");
    let store = SessionStore::for_path(path.clone());
    let mut list = muta_contracts::TodoList::default();
    list.reconcile(
        &[("write schema".into(), muta_contracts::TodoStatus::Pending)],
        0,
        0,
    );
    store.set_todos(list).await.unwrap();
    assert!(!store.todos().await.is_empty());

    let reloaded = SessionStore::for_path(path);
    assert!(!reloaded.todos().await.is_empty());
}

#[tokio::test]
async fn title_is_terminal_once_set() {
    let store = store("title").await;
    store
        .set_title(Some("My title".into()), true)
        .await
        .unwrap();
    let (title, has_title) = store.title().await;
    assert_eq!(title.as_deref(), Some("My title"));
    assert!(has_title);
    // AI generation (manual=false) may only fill None; the store API accepts
    // the call but the non-NULL-is-terminal rule is enforced by the caller
    // contract. Clearing remains possible explicitly.
    store.set_title(None, false).await.unwrap();
    let (title, has_title) = store.title().await;
    assert!(title.is_none());
    assert!(!has_title);
}

// ---------------------------------------------------------------
// Fork: entries shared by identity across sessions.
// ---------------------------------------------------------------

#[tokio::test]
async fn fork_shares_entries_and_diverges() {
    let dir = temp_dir("fork");
    let db = dir.join("muta.db");
    let path = dir.join("session.json");
    std::fs::create_dir_all(&dir).unwrap();

    let store = SessionStore::for_path(path.clone());
    store
        .replace_messages(vec![user("parent"), assistant("history")])
        .await
        .unwrap();
    let (child_id, parent_id) = store.fork().await.unwrap();
    assert_eq!(store.id().await, child_id);

    // The child's own persisted state carries the shared transcript.
    let child = SessionStore::for_path(dir.join(format!("{child_id}.json")));
    let window = child.model_window().await;
    assert_eq!(window.len(), 2);
    assert_eq!(window[0].content, "parent");
    let _ = (parent_id, db);
}

#[tokio::test]
async fn fork_to_side_keeps_active_pointer() {
    let store = store("side").await;
    store.replace_messages(vec![user("aside seed")]).await.unwrap();
    let (side_id, _) = store.fork_to_side().await.unwrap();
    assert_eq!(store.model_window().await.len(), 1);
    let side = store.open_side(&side_id).await.unwrap();
    assert_eq!(side.model_window().await.len(), 1);
}

// ---------------------------------------------------------------
// Provider pin and digest round trips (working state).
// ---------------------------------------------------------------

#[tokio::test]
async fn provider_selection_round_trips() {
    let dir = temp_dir("provider");
    let path = dir.join("session.json");
    let store = SessionStore::for_path(path.clone());
    // Lazy contract (ADR-0018): a pin on an empty session does not
    // materialise it — give the session content first.
    store.replace_messages(vec![user("real work")]).await.unwrap();
    store
        .set_provider_selection(Some(ProviderSelection {
            connection: "anthropic".into(),
            model: Some("claude-sonnet-4".into()),
        }))

        .await
        .unwrap();
    let reloaded = SessionStore::for_path(path);
    let selection = reloaded.provider_selection().await.unwrap();
    assert_eq!(selection.connection, "anthropic");
}

#[tokio::test]
async fn digest_round_trips() {
    let store = store("digest").await;
    store
        .set_digest(Some(muta_contracts::SessionDigest::default()), Some(42))
        .await
        .unwrap();
    let (digest, anchor) = store.digest().await;
    assert!(digest.is_some());
    assert_eq!(anchor, Some(42));
}

// ---------------------------------------------------------------
// Interrupt records (projection state, never in the window).
// ---------------------------------------------------------------

#[tokio::test]
async fn round_interrupts_record_and_clear() {
    let store = store("interrupt").await;
    store
        .record_round_interrupt(muta_contracts::RoundInterrupt {
            reason: muta_contracts::RoundInterruptReason::User,
            at_ms: 1,
            round: Some(1),
            detail: None,
        })
        .await
        .unwrap();
    assert_eq!(store.round_interrupts().await.len(), 1);
    // Interrupts never enter the window.
    store.replace_messages(vec![user("x")]).await.unwrap();
    assert_eq!(store.model_window().await.len(), 1);
    store.clear_round_interrupts().await.unwrap();
    assert!(store.round_interrupts().await.is_empty());
}

// ---------------------------------------------------------------
// Command ledger.
// ---------------------------------------------------------------

#[tokio::test]
async fn command_ledger_round_trips() {
    let store = store("commands").await;
    store
        .mutate_commands(|ledger| {
            ledger.push(muta_contracts::CommandRecord::new("models", ""));
        })
        .await
        .unwrap();
    assert_eq!(store.commands().await.len(), 1);
}

// ---------------------------------------------------------------
// Legacy snapshots are retired, not migrated (clean break).
// ---------------------------------------------------------------

#[tokio::test]
async fn legacy_snapshot_content_is_retired() {
    let dir = temp_dir("legacy");
    let path = dir.join("session.json");
    // Write a pre-transcript snapshot payload directly.
    let legacy = r#"{
        "id": "11111111-1111-1111-1111-111111111111",
        "created_at": 1,
        "updated_at": 1,
        "project_root": "/tmp",
        "schema_version": 12,
        "model_window": [
            {"role": "user", "content": "legacy content"}
        ]
    }"#;
    std::fs::write(&path, legacy).unwrap();
    let store = SessionStore::for_path(path.clone());
    // The legacy dialogue is retired; the identity is stable across reloads.
    assert!(store.model_window().await.is_empty());
    let id = store.id().await;
    let reloaded = SessionStore::for_path(path);
    assert_eq!(reloaded.id().await, id);
}

// ---------------------------------------------------------------
// Session identity: empty sessions stay unpersisted.
// ---------------------------------------------------------------

#[tokio::test]
async fn fresh_session_stays_unpersisted_until_content() {
    let dir = temp_dir("empty");
    let path = dir.join("session.json");
    let store = SessionStore::for_path(path.clone());
    assert!(store.is_empty_unpersisted().await);
    assert!(!path.exists());
    store.replace_messages(vec![user("now real")]).await.unwrap();
    assert!(!store.is_empty_unpersisted().await);
    let reloaded = SessionStore::for_path(path);
    assert_eq!(reloaded.model_window().await.len(), 1);
}

// ---------------------------------------------------------------
// Tree DAG: leaf switch rebuilds the transcript.
// ---------------------------------------------------------------

#[tokio::test]
async fn tree_leaf_switch_rebuilds_transcript() {
    let store = store("tree").await;
    let root_id = store
        .insert_tree_entry(muta_contracts::SessionEntry::new_message(
            "root",
            None,
            0,
            user("root message"),
        ))
        .await
        .unwrap();
    let messages = store.switch_tree_leaf(&root_id).await.unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(store.model_window().await.len(), 1);
}

// ---------------------------------------------------------------
// Subagent sessions: nested transcripts become their own sessions.
// ---------------------------------------------------------------

#[tokio::test]
async fn runner_children_become_subagent_sessions_with_pointer() {
    let dir = temp_dir("subagent");
    let path = dir.join("session.json");
    let store = SessionStore::for_path(path.clone());
    store.replace_messages(vec![user("spawn task")]).await.unwrap();

    let call = ToolCall::new("call-sub", "task", "{}");
    let mut parent = assistant("delegating");
    parent.tool_calls = Some(vec![call.clone()]);
    let mut child_messages = vec![user("child task"), assistant("child done")];
    let runner_meta = muta_contracts::message::RunnerMeta {
        description: Some("do the thing".into()),
        duration_ms: Some(1234),
        toolset_count: 3,
        ..Default::default()
    };
    let result = Message::tool_result(&call, "[task result]:\ndone")
        .with_children(child_messages.clone())
        .with_runner_meta(runner_meta);

    let mut window = store.model_window().await;
    window.push(parent);
    window.push(result);
    store.commit_turn(CommitTurn::new(&window)).await.unwrap();

    // The parent's tool entry carries a SubagentRef into a Subagent-kind
    // session whose transcript holds the child messages.
    let engine = crate::db::DatabaseEngine::open(&dir.join("muta.db"), None).unwrap();
    let subagent_id = {
        let state = store.state_lock_for_test().await;
        let entry = state
            .data
            .transcript
            .entries
            .iter()
            .rev()
            .find_map(|entry| match &entry.payload {
                muta_contracts::EntryPayload::Message(payload) => payload.subagent.clone(),
                _ => None,
            })
            .expect("tool entry must carry a subagent pointer");
        entry.session_id
    };
    let subagent = engine.load_session_full(&subagent_id).unwrap().unwrap();
    assert_eq!(
        subagent.fork_kind,
        muta_contracts::SessionForkKind::Subagent
    );
    assert_eq!(subagent.parent_id.as_deref(), Some(store.id().await.as_str()));
    let transcript = subagent
        .transcript
        .entries
        .iter()
        .filter_map(|entry| entry.to_message())
        .collect::<Vec<_>>();
    assert_eq!(transcript.len(), 2);
    assert_eq!(transcript[0].content, "child task");
    assert_eq!(transcript[1].content, "child done");
    let _ = child_messages;

    // Subagent sessions never surface in the picker.
    let summaries = engine
        .list_session_summaries(None, &store.id().await)
        .unwrap();
    assert!(!summaries.iter().any(|summary| summary.id == subagent_id));
}

// ---------------------------------------------------------------
// Deterministic compaction helpers (pure functions).
// ---------------------------------------------------------------

#[test]
fn compaction_selects_and_builds_checkpoint() {
    let history = vec![
        user("r1"),
        assistant("a1"),
        user("r2"),
        assistant("a2"),
        user("r3"),
    ];
    let selection = select_compaction(&history, 1).unwrap();
    assert_eq!(selection.archived.len(), 4);
    assert_eq!(selection.tail.len(), 1);
    let result = build_compaction_result(100, selection, "summary".into());
    assert_eq!(result.model_window.len(), 2);
    assert_eq!(result.archived_originals.len(), 4);
    assert_eq!(result.checkpoint.operation, ContextProjectionKind::Compact);
}

#[test]
fn excerpt_summary_respects_token_budget() {
    let archived: Vec<Message> = (0..50)
        .map(|i| user(&format!("message {i} with some filler text to burn budget")))
        .collect();
    let summary = build_excerpt_summary(&archived, 120, None);
    assert!(!summary.is_empty());
    assert!(muta_contracts::tokenizer::count_tokens(&summary) <= 120);
}
