//! Live MCP runtime state — the single source of truth for which configured
//! servers are connected, their per-server tools, and their connection status.
//!
//! At startup [`McpRuntime::connect_all`] connects every enabled `[mcp.<name>]`
//! server and publishes tools to a [`DynamicToolSink`]. Thereafter three async
//! mutators keep it live:
//!
//! - [`McpRuntime::set_enabled`] — the `/mcp` modal's `Space` toggle: connect or
//!   disconnect one server for the session (config.toml is not rewritten).
//! - [`McpRuntime::reconnect`] — the modal's `r` action: re-establish one
//!   server's connection on demand.
//! - [`McpRuntime::refresh_all`] — the periodic [`crate::McpCatalog`]
//!   loop: reconnect every server.
//!
//! Every mutation publishes a complete snapshot for each server and updates a
//! synchronously-readable status table, so the session-context snapshot can
//! always reflect the current state.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use neenee_core::mcp::{McpConnectionStatus, McpServerConfig};
use neenee_core::{DynamicToolSink, Tool};
use tokio::sync::Mutex;

use crate::{McpServer, connect_server, reconnect_server};

/// One configured server's live state. `server` is `None` while disabled or
/// when the last connect failed; `tools` is the server's current adapters
/// (empty unless connected).
struct McpEntry {
    name: String,
    server: Option<Arc<McpServer>>,
    tools: Vec<Arc<dyn Tool>>,
    status: McpConnectionStatus,
}

pub struct McpRuntime {
    /// All configured servers (`[mcp.*]`), by name — the source of truth for
    /// re-enabling a disabled server, which has no live handle to clone from.
    configs: HashMap<String, McpServerConfig>,
    /// Per-server live state, name-sorted. Behind an async mutex because every
    /// mutator performs network I/O while holding it, which serializes a user
    /// toggle against the background refresh loop.
    entries: Mutex<Vec<McpEntry>>,
    /// Synchronously-readable status table (name → status, name-sorted), kept in
    /// step with `entries`. The session-context snapshot is built from a sync
    /// context, so it reads this rather than the async `entries` mutex.
    statuses: RwLock<Vec<(String, McpConnectionStatus)>>,
    /// Connector-neutral publication port implemented by the consuming agent.
    sink: Arc<dyn DynamicToolSink>,
}

impl McpRuntime {
    /// Connect every enabled configured server and publish their tools.
    /// Disabled servers are recorded as such without a connection.
    ///
    /// Enabled servers are connected **concurrently** (a bounded `join_all`)
    /// rather than serially, so the worst-case startup latency is the slowest
    /// single server's connect timeout, not the sum of all of them.
    pub async fn connect_all(
        configs: HashMap<String, McpServerConfig>,
        sink: Arc<dyn DynamicToolSink>,
    ) -> Self {
        let mut names: Vec<String> = configs.keys().cloned().collect();
        names.sort();

        // Connect every enabled server concurrently. Disabled ones become
        // entries inline (no I/O).
        let connect_futures = names.into_iter().map(|name| {
            let config = configs[&name].clone();
            async move {
                if !config.enabled {
                    return McpEntry {
                        name,
                        server: None,
                        tools: Vec::new(),
                        status: McpConnectionStatus::Disabled,
                    };
                }
                connect_entry(name, &config).await
            }
        });
        let entries: Vec<McpEntry> = futures::future::join_all(connect_futures).await;

        let runtime = Self {
            configs,
            entries: Mutex::new(entries),
            statuses: RwLock::new(Vec::new()),
            sink,
        };
        {
            let entries = runtime.entries.lock().await;
            runtime.publish(&entries);
        }
        runtime
    }

    /// Build a runtime that records every configured server but defers the
    /// actual connections to a background task. Every enabled server starts in
    /// the `Connecting` status (so the UI can show a spinner / "connecting…"
    /// row) with no tools; disabled servers are marked `Disabled` immediately.
    ///
    /// The returned runtime is ready to use the instant this returns. The
    /// caller should spawn [`McpRuntime::refresh_all`] in the background to
    /// perform the real concurrent connections and publish results into the
    /// dynamic tool sink + status table — without blocking the first frame.
    pub fn start_background(
        configs: HashMap<String, McpServerConfig>,
        sink: Arc<dyn DynamicToolSink>,
    ) -> Self {
        let mut names: Vec<String> = configs.keys().cloned().collect();
        names.sort();
        let entries: Vec<McpEntry> = names
            .iter()
            .map(|name| McpEntry {
                server: None,
                tools: Vec::new(),
                status: if configs[name].enabled {
                    McpConnectionStatus::Connecting
                } else {
                    McpConnectionStatus::Disabled
                },
                name: name.clone(),
            })
            .collect();
        // Seed the sync status table directly from the initial entries — no
        // background task has touched it yet, so there is nothing to lock.
        let statuses: Vec<(String, McpConnectionStatus)> = entries
            .iter()
            .map(|e| (e.name.clone(), e.status.clone()))
            .collect();

        let runtime = Self {
            configs,
            entries: Mutex::new(entries),
            statuses: RwLock::new(statuses),
            sink,
        };
        for name in runtime.configs.keys() {
            runtime.sink.replace(&source_id(name), Vec::new());
        }
        runtime
    }

    /// A name-sorted snapshot of every configured server's connection status,
    /// readable synchronously (for the session-context snapshot).
    pub fn statuses_snapshot(&self) -> Vec<(String, McpConnectionStatus)> {
        self.statuses
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// Enable or disable one server for the live session. Enabling connects it
    /// (or is a no-op when already connected); disabling drops its tools and
    /// closes the connection. Returns `Ok(())` once applied, or `Err` when the
    /// name is not configured.
    pub async fn set_enabled(&self, name: &str, enabled: bool) -> Result<(), String> {
        let Some(config) = self.configs.get(name).cloned() else {
            return Err(format!("MCP server '{name}' is not configured."));
        };
        let mut entries = self.entries.lock().await;
        let Some(entry) = entries.iter_mut().find(|e| e.name == name) else {
            return Err(format!("MCP server '{name}' is not configured."));
        };

        if enabled {
            if entry.server.is_some() {
                return Ok(()); // already connected
            }
            *entry = connect_entry(name.to_string(), &config).await;
        } else {
            // Dropping the handle kills the child process (kill_on_drop).
            entry.server = None;
            entry.tools.clear();
            entry.status = McpConnectionStatus::Disabled;
        }
        self.publish(&entries);
        Ok(())
    }

    /// Re-establish one enabled server's connection from config, re-discovering
    /// its tools. A no-op for a disabled server. Returns `Err` when the name is
    /// not configured.
    pub async fn reconnect(&self, name: &str) -> Result<(), String> {
        let Some(config) = self.configs.get(name).cloned() else {
            return Err(format!("MCP server '{name}' is not configured."));
        };
        let mut entries = self.entries.lock().await;
        let Some(entry) = entries.iter_mut().find(|e| e.name == name) else {
            return Err(format!("MCP server '{name}' is not configured."));
        };
        if matches!(entry.status, McpConnectionStatus::Disabled) {
            return Ok(());
        }
        match &entry.server {
            // Connected: reset + re-discover through the existing handle.
            Some(server) => {
                let (tools, status) = reconnect_server(server).await;
                entry.tools = tools;
                entry.status = status;
            }
            // Failed earlier (no live handle): try a fresh connect.
            None => *entry = connect_entry(name.to_string(), &config).await,
        }
        self.publish(&entries);
        Ok(())
    }

    /// Reconnect every enabled server (the periodic catalog refresh, and the
    /// initial background connect). Disabled servers stay disabled.
    ///
    /// The reconnections run **concurrently**: each server's I/O is
    /// independent, so connecting N servers in parallel takes the slowest
    /// single timeout rather than the sum.
    pub async fn refresh_all(&self) {
        // Snapshot the work while holding the lock, then release it so the
        // per-server I/O can run concurrently. Each item carries either a live
        // server handle (to re-discover through) or marks a fresh connect.
        enum Job {
            Reconnect(Arc<McpServer>),
            Connect,
            Skip,
        }
        let jobs: Vec<(usize, String, Job)> = {
            let entries = self.entries.lock().await;
            entries
                .iter()
                .enumerate()
                .map(|(idx, entry)| {
                    if matches!(entry.status, McpConnectionStatus::Disabled) {
                        return (idx, entry.name.clone(), Job::Skip);
                    }
                    match &entry.server {
                        Some(server) => {
                            (idx, entry.name.clone(), Job::Reconnect(Arc::clone(server)))
                        }
                        None => (idx, entry.name.clone(), Job::Connect),
                    }
                })
                .collect()
        };

        // Run each job concurrently, producing a fresh entry per server.
        let refresh_futures = jobs.into_iter().map(|(idx, name, job)| async move {
            match job {
                Job::Skip => (idx, None),
                Job::Reconnect(server) => {
                    let (tools, status) = reconnect_server(&server).await;
                    (
                        idx,
                        Some(McpEntry {
                            name,
                            server: Some(server),
                            tools,
                            status,
                        }),
                    )
                }
                Job::Connect => {
                    let config = self.configs.get(&name);
                    let entry = match config {
                        Some(config) => connect_entry(name.clone(), config).await,
                        None => McpEntry {
                            name,
                            server: None,
                            tools: Vec::new(),
                            status: McpConnectionStatus::Failed("not configured".into()),
                        },
                    };
                    (idx, Some(entry))
                }
            }
        });
        let results: Vec<(usize, Option<McpEntry>)> =
            futures::future::join_all(refresh_futures).await;

        // Write the refreshed entries back and republish.
        let mut entries = self.entries.lock().await;
        for (idx, entry) in results {
            if let Some(entry) = entry {
                entries[idx] = entry;
            }
        }
        self.publish(&entries);
    }

    /// Whether any server is configured at all (the catalog skips its loop when
    /// none are).
    pub fn is_empty(&self) -> bool {
        self.configs.is_empty()
    }

    /// Publish complete per-server tool snapshots and rebuild the synchronous
    /// status table. Called after every mutation.
    fn publish(&self, entries: &[McpEntry]) {
        for entry in entries {
            self.sink
                .replace(&source_id(&entry.name), entry.tools.clone());
        }
        let statuses = entries
            .iter()
            .map(|e| (e.name.clone(), e.status.clone()))
            .collect();
        if let Ok(mut guard) = self.statuses.write() {
            *guard = statuses;
        }
    }
}

impl Drop for McpRuntime {
    fn drop(&mut self) {
        for name in self.configs.keys() {
            self.sink.remove(&source_id(name));
        }
    }
}

fn source_id(server_name: &str) -> String {
    format!("mcp:{server_name}")
}

/// Connect one server from config, returning a fully-populated entry whether it
/// succeeds (`Connected`) or fails (`Failed`, no handle).
async fn connect_entry(name: String, config: &McpServerConfig) -> McpEntry {
    match connect_server(&name, config).await {
        Ok((server, tools)) => {
            let status = McpConnectionStatus::Connected { tools: tools.len() };
            McpEntry {
                name,
                server: Some(server),
                tools,
                status,
            }
        }
        Err(error) => McpEntry {
            name,
            server: None,
            tools: Vec::new(),
            status: McpConnectionStatus::Failed(error),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct RecordingSink {
        sources: RwLock<BTreeMap<String, Vec<Arc<dyn Tool>>>>,
    }

    impl DynamicToolSink for RecordingSink {
        fn replace(&self, source: &str, tools: Vec<Arc<dyn Tool>>) {
            self.sources
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(source.to_string(), tools);
        }

        fn remove(&self, source: &str) {
            self.sources
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .remove(source);
        }
    }

    #[test]
    fn runtime_publishes_and_removes_per_server_sources() {
        let sink = Arc::new(RecordingSink::default());
        let mut configs = HashMap::new();
        configs.insert(
            "filesystem".to_string(),
            McpServerConfig {
                enabled: false,
                ..McpServerConfig::default()
            },
        );

        let runtime = McpRuntime::start_background(configs, sink.clone());
        assert!(
            sink.sources
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key("mcp:filesystem")
        );

        drop(runtime);
        assert!(
            sink.sources
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty()
        );
    }
}
