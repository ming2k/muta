//! Public-web intelligence aggregation and durable hyperlink observation.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use neenee_core::Tool;
use neenee_store::cache::CachedResource;
use neenee_tools::{WebFetchTool, WebSearchTool, WebSnapshotResult};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const MAX_ITEMS: usize = 80;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpinionTopic {
    pub id: String,
    pub label: String,
    pub query: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpinionItem {
    pub id: String,
    pub topic_id: String,
    pub topic_label: String,
    pub title: String,
    pub url: String,
    pub summary: String,
    pub score: f32,
    pub collected_at_ms: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkChange {
    #[default]
    Pending,
    New,
    Unchanged,
    Changed,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchedLink {
    pub id: String,
    pub label: String,
    pub url: String,
    pub title: String,
    pub last_hash: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub text_preview: String,
    pub last_checked_ms: Option<u64>,
    pub last_changed_ms: Option<u64>,
    pub change_count: u64,
    pub change: LinkChange,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OpinionState {
    pub topics: Vec<OpinionTopic>,
    pub items: Vec<OpinionItem>,
    pub watched_links: Vec<WatchedLink>,
    pub last_refresh_ms: Option<u64>,
    pub refresh_errors: Vec<String>,
}

impl Default for OpinionState {
    fn default() -> Self {
        Self {
            topics: vec![
                OpinionTopic {
                    id: "macro-markets".to_string(),
                    label: "Macro & policy".to_string(),
                    query: "global markets macro central banks policy latest top news".to_string(),
                    enabled: true,
                },
                OpinionTopic {
                    id: "equity-catalysts".to_string(),
                    label: "Equity catalysts".to_string(),
                    query: "US Hong Kong equities earnings market catalysts latest top news"
                        .to_string(),
                    enabled: true,
                },
                OpinionTopic {
                    id: "technology-cycle".to_string(),
                    label: "Technology cycle".to_string(),
                    query: "AI semiconductors technology markets latest top news".to_string(),
                    enabled: true,
                },
            ],
            items: Vec::new(),
            watched_links: Vec::new(),
            last_refresh_ms: None,
            refresh_errors: Vec::new(),
        }
    }
}

#[async_trait]
pub trait OpinionSearch: Send + Sync {
    async fn search(&self, query: &str) -> Result<String, String>;
}

#[async_trait]
pub trait LinkObserver: Send + Sync {
    async fn observe(
        &self,
        url: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<WebSnapshotResult, String>;
}

struct ToolSearch {
    tool: WebSearchTool,
}

#[async_trait]
impl OpinionSearch for ToolSearch {
    async fn search(&self, query: &str) -> Result<String, String> {
        self.tool
            .call(&serde_json::json!({ "query": query }).to_string())
            .await
    }
}

struct ToolObserver {
    tool: WebFetchTool,
}

#[async_trait]
impl LinkObserver for ToolObserver {
    async fn observe(
        &self,
        url: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<WebSnapshotResult, String> {
        self.tool.snapshot(url, etag, last_modified).await
    }
}

pub struct OpinionHub {
    state: OpinionState,
    cache: CachedResource,
    search: Arc<dyn OpinionSearch>,
    observer: Arc<dyn LinkObserver>,
}

impl OpinionHub {
    pub fn system_default() -> Self {
        let config = neenee_store::config::Config::load();
        let path = neenee_store::paths::get()
            .state_dir
            .join("intelligence")
            .join("opinion.json");
        Self::with_clients(
            path,
            Arc::new(ToolSearch {
                tool: WebSearchTool::with_config(config.websearch.clone()),
            }),
            Arc::new(ToolObserver {
                tool: WebFetchTool::with_config(config.websearch),
            }),
        )
    }

    pub fn with_clients(
        path: PathBuf,
        search: Arc<dyn OpinionSearch>,
        observer: Arc<dyn LinkObserver>,
    ) -> Self {
        let cache = CachedResource::new(path);
        let state = cache.load_json().unwrap_or_default();
        Self {
            state,
            cache,
            search,
            observer,
        }
    }

    pub fn state(&self) -> &OpinionState {
        &self.state
    }

    pub fn add_topic(&mut self, label: &str, query: &str) -> Result<(), String> {
        let label = label.trim();
        let query = query.trim();
        if query.is_empty() {
            return Err("topic query must not be empty".to_string());
        }
        if self
            .state
            .topics
            .iter()
            .any(|topic| topic.query.eq_ignore_ascii_case(query))
        {
            return Err("that opinion topic is already tracked".to_string());
        }
        self.state.topics.push(OpinionTopic {
            id: Uuid::new_v4().to_string(),
            label: if label.is_empty() { query } else { label }.to_string(),
            query: query.to_string(),
            enabled: true,
        });
        self.persist()
    }

    pub fn add_watch(&mut self, label: &str, url: &str) -> Result<(), String> {
        let url = url.trim();
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err("watched URL must start with http:// or https://".to_string());
        }
        if self
            .state
            .watched_links
            .iter()
            .any(|watch| watch.url.eq_ignore_ascii_case(url))
        {
            return Err("that URL is already watched".to_string());
        }
        self.state.watched_links.push(WatchedLink {
            id: Uuid::new_v4().to_string(),
            label: label.trim().to_string(),
            url: url.to_string(),
            title: String::new(),
            last_hash: None,
            etag: None,
            last_modified: None,
            text_preview: String::new(),
            last_checked_ms: None,
            last_changed_ms: None,
            change_count: 0,
            change: LinkChange::Pending,
            last_error: None,
        });
        self.persist()
    }

    pub fn remove_topic(&mut self, id: &str) -> Result<(), String> {
        self.state.topics.retain(|topic| topic.id != id);
        self.state.items.retain(|item| item.topic_id != id);
        self.persist()
    }

    pub fn remove_watch(&mut self, id: &str) -> Result<(), String> {
        self.state.watched_links.retain(|watch| watch.id != id);
        self.persist()
    }

    pub async fn refresh(&mut self) -> Result<&OpinionState, String> {
        self.state.refresh_errors.clear();
        self.refresh_topics().await;
        self.refresh_watches().await;
        self.state.last_refresh_ms = Some(unix_now_ms());
        self.persist()?;
        Ok(&self.state)
    }

    async fn refresh_topics(&mut self) {
        let topics = self
            .state
            .topics
            .iter()
            .filter(|topic| topic.enabled)
            .cloned()
            .collect::<Vec<_>>();
        let mut refreshed = Vec::new();
        let mut successful = HashSet::new();
        for topic in topics {
            match self.search.search(&topic.query).await {
                Ok(raw) => {
                    successful.insert(topic.id.clone());
                    refreshed.extend(parse_search_results(&topic, &raw, unix_now_ms()));
                }
                Err(error) => self
                    .state
                    .refresh_errors
                    .push(format!("{}: {error}", topic.label)),
            }
        }
        self.state
            .items
            .retain(|item| !successful.contains(&item.topic_id));
        self.state.items.extend(refreshed);
        let mut seen = HashSet::new();
        self.state
            .items
            .retain(|item| seen.insert(item.url.clone()));
        self.state
            .items
            .sort_by(|left, right| right.score.total_cmp(&left.score));
        self.state.items.truncate(MAX_ITEMS);
    }

    async fn refresh_watches(&mut self) {
        for index in 0..self.state.watched_links.len() {
            let request = {
                let watch = &self.state.watched_links[index];
                (
                    watch.url.clone(),
                    watch.etag.clone(),
                    watch.last_modified.clone(),
                )
            };
            let result = self
                .observer
                .observe(&request.0, request.1.as_deref(), request.2.as_deref())
                .await;
            let watch = &mut self.state.watched_links[index];
            match result {
                Ok(WebSnapshotResult::NotModified { checked_at_ms }) => {
                    watch.last_checked_ms = Some(checked_at_ms);
                    watch.change = LinkChange::Unchanged;
                    watch.last_error = None;
                }
                Ok(WebSnapshotResult::Modified(snapshot)) => {
                    let previous = watch.last_hash.as_deref();
                    let changed = previous.is_some_and(|hash| hash != snapshot.content_hash);
                    watch.change = if previous.is_none() {
                        LinkChange::New
                    } else if changed {
                        LinkChange::Changed
                    } else {
                        LinkChange::Unchanged
                    };
                    if previous.is_none() || changed {
                        watch.last_changed_ms = Some(snapshot.checked_at_ms);
                    }
                    if changed {
                        watch.change_count = watch.change_count.saturating_add(1);
                    }
                    watch.last_hash = Some(snapshot.content_hash);
                    watch.etag = snapshot.etag;
                    watch.last_modified = snapshot.last_modified;
                    watch.last_checked_ms = Some(snapshot.checked_at_ms);
                    watch.title = snapshot.title;
                    watch.text_preview = snapshot.text_preview;
                    watch.last_error = None;
                }
                Err(error) => {
                    watch.change = LinkChange::Error;
                    watch.last_error = Some(error.clone());
                    self.state
                        .refresh_errors
                        .push(format!("{}: {error}", watch.url));
                }
            }
        }
    }

    fn persist(&self) -> Result<(), String> {
        self.cache.store_json(&self.state)
    }
}

fn parse_search_results(topic: &OpinionTopic, raw: &str, collected_at_ms: u64) -> Vec<OpinionItem> {
    let lines = raw.lines().map(str::trim).collect::<Vec<_>>();
    let mut results = Vec::new();
    let mut seen = HashSet::new();
    let mut pending_title = String::new();

    for (index, line) in lines.iter().enumerate() {
        if line.is_empty() || line.starts_with("Search results for") {
            continue;
        }
        if let Some((title, url)) = markdown_link(line) {
            push_search_item(
                &mut results,
                &mut seen,
                topic,
                title,
                url,
                nearby_summary(&lines, index),
                collected_at_ms,
            );
            continue;
        }
        if let Some(url) = first_url(line) {
            let title = if pending_title.is_empty() {
                url.clone()
            } else {
                pending_title.clone()
            };
            push_search_item(
                &mut results,
                &mut seen,
                topic,
                title,
                url,
                nearby_summary(&lines, index),
                collected_at_ms,
            );
            pending_title.clear();
            continue;
        }
        if looks_like_result_title(line) {
            pending_title = clean_title(line);
        }
    }
    results
}

#[allow(clippy::too_many_arguments)]
fn push_search_item(
    results: &mut Vec<OpinionItem>,
    seen: &mut HashSet<String>,
    topic: &OpinionTopic,
    title: String,
    url: String,
    summary: String,
    collected_at_ms: u64,
) {
    if !seen.insert(url.clone()) {
        return;
    }
    let rank = results.len();
    results.push(OpinionItem {
        id: format!("{}:{}", topic.id, rank),
        topic_id: topic.id.clone(),
        topic_label: topic.label.clone(),
        title: clean_title(&title),
        url,
        summary,
        score: (100.0 - rank as f32 * 6.0).max(10.0),
        collected_at_ms,
    });
}

fn markdown_link(line: &str) -> Option<(String, String)> {
    let close = line.find("](")?;
    let open = line[..close].rfind('[')?;
    let rest = &line[close + 2..];
    let end = rest.find(')')?;
    let url = clean_url(&rest[..end])?;
    Some((line[open + 1..close].trim().to_string(), url))
}

fn first_url(line: &str) -> Option<String> {
    let start = line.find("https://").or_else(|| line.find("http://"))?;
    let tail = &line[start..];
    let end = tail
        .find(|character: char| character.is_whitespace() || matches!(character, ')' | ']' | '>'))
        .unwrap_or(tail.len());
    clean_url(&tail[..end])
}

fn clean_url(raw: &str) -> Option<String> {
    let url = raw
        .trim_matches(|character: char| {
            matches!(
                character,
                '<' | '>' | '(' | ')' | '[' | ']' | '"' | '\'' | ',' | '.'
            )
        })
        .to_string();
    (url.starts_with("https://") || url.starts_with("http://")).then_some(url)
}

fn clean_title(line: &str) -> String {
    let trimmed = line
        .trim()
        .trim_start_matches(|character: char| character.is_ascii_digit())
        .trim_start_matches(['.', ')', '-', '*', '#', ' ']);
    if trimmed.is_empty() {
        "Untitled source".to_string()
    } else {
        trimmed.chars().take(180).collect()
    }
}

fn looks_like_result_title(line: &str) -> bool {
    line.chars()
        .next()
        .is_some_and(|character| character.is_ascii_digit() || matches!(character, '-' | '*' | '#'))
        || line.to_ascii_lowercase().starts_with("title:")
}

fn nearby_summary(lines: &[&str], index: usize) -> String {
    lines
        .iter()
        .skip(index + 1)
        .find(|line| !line.is_empty() && first_url(line).is_none())
        .map(|line| clean_title(line).chars().take(320).collect())
        .unwrap_or_default()
}

fn unix_now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use neenee_tools::{WebPageSnapshot, WebSnapshotResult};
    use std::sync::Mutex;

    struct FakeSearch;

    #[async_trait]
    impl OpinionSearch for FakeSearch {
        async fn search(&self, _query: &str) -> Result<String, String> {
            Ok("Search results\n\n1. First catalyst\n   https://example.com/one\n   Important development\n\n2. [Second catalyst](https://example.com/two)".to_string())
        }
    }

    struct FakeObserver {
        hash: Mutex<String>,
    }

    #[async_trait]
    impl LinkObserver for FakeObserver {
        async fn observe(
            &self,
            url: &str,
            _etag: Option<&str>,
            _last_modified: Option<&str>,
        ) -> Result<WebSnapshotResult, String> {
            let hash = self
                .hash
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            Ok(WebSnapshotResult::Modified(WebPageSnapshot {
                requested_url: url.to_string(),
                final_url: url.to_string(),
                title: "Tracked page".to_string(),
                text_preview: "preview".to_string(),
                content_hash: hash,
                content_type: "text/html".to_string(),
                etag: None,
                last_modified: None,
                body_bytes: 7,
                checked_at_ms: 10,
            }))
        }
    }

    fn hub(observer: Arc<FakeObserver>) -> (tempfile::TempDir, OpinionHub) {
        let directory = tempfile::tempdir().expect("tempdir");
        let hub = OpinionHub::with_clients(
            directory.path().join("opinion.json"),
            Arc::new(FakeSearch),
            observer,
        );
        (directory, hub)
    }

    #[tokio::test]
    async fn refresh_ranks_search_results_and_detects_link_changes() {
        let observer = Arc::new(FakeObserver {
            hash: Mutex::new("v1".to_string()),
        });
        let (_directory, mut hub) = hub(Arc::clone(&observer));
        hub.add_watch("Release", "https://example.com/release")
            .expect("watch");

        hub.refresh().await.expect("first refresh");
        assert!(!hub.state().items.is_empty());
        assert_eq!(hub.state().watched_links[0].change, LinkChange::New);

        *observer
            .hash
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = "v2".to_string();
        hub.refresh().await.expect("second refresh");
        assert_eq!(hub.state().watched_links[0].change, LinkChange::Changed);
        assert_eq!(hub.state().watched_links[0].change_count, 1);
    }

    #[test]
    fn parser_accepts_numbered_and_markdown_search_shapes() {
        let topic = OpinionState::default().topics.remove(0);
        let items = parse_search_results(
            &topic,
            "1. Alpha\n https://a.example/x\n summary\n2. [Beta](https://b.example/y)",
            1,
        );
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].title, "Alpha");
        assert!(items[0].score > items[1].score);
    }
}
