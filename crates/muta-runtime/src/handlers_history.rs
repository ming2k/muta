//! Input-history and route-settings services for the frontend (ADR-0197):
//! the daemon is the source of truth for the shared SQLite store, and the
//! frontend reaches it only through these wire requests — it never opens the
//! database directly.

use tokio::sync::mpsc::UnboundedSender;

use muta_contracts::events::AgentResponse;
use muta_paths::paths;

/// `AgentRequest::QueryInputHistory`: load the persisted prompt history.
pub fn query_input_history(resp_tx: &UnboundedSender<AgentResponse>) {
    let rows = muta_persistence::db::DatabaseEngine::open(&paths::get().db_file(), None)
        .ok()
        .and_then(|engine| {
            engine
                .load_input_history(muta_contracts::history::HISTORY_CAP)
                .ok()
        })
        .unwrap_or_default();
    let _ = resp_tx.send(AgentResponse::InputHistory(rows));
}

/// `AgentRequest::RecordInputHistory`: lock + merge entries into the shared
/// store.
pub fn record_input_history(entries: Vec<muta_contracts::HistoryEntry>, dedup: bool) {
    let result = muta_persistence::db::DatabaseEngine::open(&paths::get().db_file(), None)
        .map_err(|e| format!("could not open sqlite db: {e}"))
        .and_then(|engine| {
            engine
                .save_input_history(&entries, dedup)
                .map_err(|e| format!("could not save input history to sqlite: {e}"))
        });
    if let Err(error) = result {
        tracing::warn!(%error, "input history record failed");
    }
}

/// `AgentRequest::DeleteInputHistoryEntry`: remove one row by content and
/// timestamp.
pub fn delete_input_history_entry(text: &str, created_at_ms: u64) {
    let result = muta_persistence::db::DatabaseEngine::open(&paths::get().db_file(), None)
        .map_err(|e| format!("could not open sqlite db: {e}"))
        .and_then(|engine| {
            engine
                .delete_input_history_entry(text, created_at_ms)
                .map_err(|e| format!("could not delete input history entry: {e}"))
        });
    if let Err(error) = result {
        tracing::warn!(%error, "input history delete failed");
    }
}

/// `AgentRequest::QueryRouteSettings`: the stored capability overrides for
/// one provider/model route (the model editor's prefill).
pub fn query_route_settings(
    provider_id: &str,
    model: &str,
    resp_tx: &UnboundedSender<AgentResponse>,
) {
    let overrides = muta_persistence::route_settings::RouteSettingsStore::load()
        .settings_for(provider_id, model)
        .and_then(|r| r.capability_overrides.clone());
    let _ = resp_tx.send(AgentResponse::RouteSettings {
        provider_id: provider_id.to_string(),
        model: model.to_string(),
        overrides,
    });
}

/// `AgentRequest::SearchHistory`: BM25 full-text search over every persisted
/// transcript entry in the shared store (ADR-0208). `workspace: None`
/// searches all project buckets — the Archivist's cross-project recall plane.
///
/// Two-stage recall (ADR-0208 Layer 3's deterministic leg): the strict
/// AND-form query runs first; when it recalls nothing, the same words are
/// re-queried OR-joined (`search_history_relaxed`) so a gist whose words
/// never co-occur still surfaces its candidates. Fail-open: any engine error
/// degrades to an empty hit list.
pub fn search_history(
    query: &str,
    workspace: Option<&str>,
    limit: Option<usize>,
    resp_tx: &UnboundedSender<AgentResponse>,
) {
    const DEFAULT_LIMIT: usize = 20;
    const MAX_LIMIT: usize = 100;
    let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let hits = muta_persistence::db::DatabaseEngine::open(&paths::get().db_file(), None)
        .map_err(|e| format!("could not open sqlite db: {e}"))
        .and_then(|engine| {
            let strict = engine
                .search_history(query, workspace, limit)
                .map_err(|e| format!("history search failed: {e}"))?;
            if !strict.is_empty() {
                return Ok(strict);
            }
            engine
                .search_history_relaxed(query, workspace, limit)
                .map_err(|e| format!("history search failed: {e}"))
        })
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "history search failed");
            Vec::new()
        });
    let hits = hits
        .into_iter()
        .map(|hit| muta_contracts::HistorySearchHit {
            entry_id: hit.entry_id,
            session_id: hit.session_id,
            workspace: hit.project_root,
            session_title: hit.session_title,
            role: hit.role,
            snippet: hit.snippet,
            score: hit.score,
        })
        .collect();
    let _ = resp_tx.send(AgentResponse::HistorySearch(hits));
}
