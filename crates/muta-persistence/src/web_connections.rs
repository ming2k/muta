//! Web Connections — decoupled persisted search and reader connections.
//!
//! Stored in `$XDG_STATE_HOME/muta/web_connections.toml`.

use std::path::PathBuf;

use muta_contracts::{
    SecretString, WebReaderConnection, WebSearchConnection,
};
use serde::{Deserialize, Serialize};

use crate::config::Credentials;
use crate::fsutil;
use crate::paths;

/// The persisted set of web connections (`web_connections.toml`).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WebConnections {
    #[serde(default)]
    pub search_connections: Vec<WebSearchConnection>,
    #[serde(default)]
    pub reader_connections: Vec<WebReaderConnection>,
}

impl WebConnections {
    fn path() -> PathBuf {
        paths::get().web_connections_file()
    }

    /// Read the web connections store. If missing, populates defaults.
    pub fn load() -> Self {
        let path = Self::path();
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::with_defaults();
        };
        match toml::from_str::<Self>(&content) {
            Ok(mut conns) => {
                if conns.search_connections.is_empty() {
                    conns.search_connections = Self::with_defaults().search_connections;
                }
                conns
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "could not parse web_connections.toml; using defaults",
                );
                Self::with_defaults()
            }
        }
    }

    /// Construct initial default web connections: only Exa Search is enabled by default.
    pub fn with_defaults() -> Self {
        Self {
            search_connections: vec![WebSearchConnection {
                id: "exa-default".to_string(),
                name: Some("Exa Search (Hosted MCP)".to_string()),
                preset_id: Some("exa".to_string()),
                api_key_env: Some("EXA_API_KEY".to_string()),
                base_url: None,
                custom_headers: None,
                enabled: true,
            }],
            reader_connections: Vec::new(),
        }
    }

    /// Persist atomically to `$XDG_STATE_HOME/muta/web_connections.toml`.
    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        let bytes = toml::to_string_pretty(self)?.into_bytes();
        fsutil::atomic_write_bytes(&Self::path(), &bytes)?;
        Ok(())
    }

    // ── Search Connections ──────────────────────────────────────────────

    pub fn get_search(&self, id: &str) -> Option<&WebSearchConnection> {
        self.search_connections.iter().find(|c| c.id == id)
    }

    pub fn find_search(&self, id_or_preset: &str) -> Option<&WebSearchConnection> {
        let norm = id_or_preset.trim().to_ascii_lowercase();
        self.search_connections
            .iter()
            .find(|c| c.id.eq_ignore_ascii_case(&norm))
            .or_else(|| {
                self.search_connections
                    .iter()
                    .find(|c| c.preset_id.as_deref().unwrap_or("").eq_ignore_ascii_case(&norm))
            })
    }

    pub fn upsert_search(&mut self, connection: WebSearchConnection) {
        if let Some(existing) = self.search_connections.iter_mut().find(|c| c.id == connection.id) {
            *existing = connection;
        } else {
            self.search_connections.push(connection);
        }
    }

    pub fn remove_search(&mut self, id: &str) -> Option<WebSearchConnection> {
        let index = self.search_connections.iter().position(|c| c.id == id)?;
        Some(self.search_connections.remove(index))
    }

    pub fn unique_search_id(&self, name: &str) -> String {
        let base = crate::connections::slug(name);
        if self.search_connections.iter().any(|c| c.id == base) {
            let mut n = 2;
            loop {
                let candidate = format!("{base}-{n}");
                if !self.search_connections.iter().any(|c| c.id == candidate) {
                    return candidate;
                }
                n += 1;
            }
        }
        base
    }

    // ── Reader Connections ──────────────────────────────────────────────

    pub fn get_reader(&self, id: &str) -> Option<&WebReaderConnection> {
        self.reader_connections.iter().find(|c| c.id == id)
    }

    pub fn find_reader(&self, id_or_preset: &str) -> Option<&WebReaderConnection> {
        let norm = id_or_preset.trim().to_ascii_lowercase();
        self.reader_connections
            .iter()
            .find(|c| c.id.eq_ignore_ascii_case(&norm))
            .or_else(|| {
                self.reader_connections
                    .iter()
                    .find(|c| c.preset_id.as_deref().unwrap_or("").eq_ignore_ascii_case(&norm))
            })
    }

    pub fn upsert_reader(&mut self, connection: WebReaderConnection) {
        if let Some(existing) = self.reader_connections.iter_mut().find(|c| c.id == connection.id) {
            *existing = connection;
        } else {
            self.reader_connections.push(connection);
        }
    }

    pub fn remove_reader(&mut self, id: &str) -> Option<WebReaderConnection> {
        let index = self.reader_connections.iter().position(|c| c.id == id)?;
        Some(self.reader_connections.remove(index))
    }

    pub fn unique_reader_id(&self, name: &str) -> String {
        let base = crate::connections::slug(name);
        if self.reader_connections.iter().any(|c| c.id == base) {
            let mut n = 2;
            loop {
                let candidate = format!("{base}-{n}");
                if !self.reader_connections.iter().any(|c| c.id == candidate) {
                    return candidate;
                }
                n += 1;
            }
        }
        base
    }

    // ── Credential Resolution ───────────────────────────────────────────

    pub fn resolve_search_credential(
        conn: &WebSearchConnection,
        creds: &Credentials,
    ) -> Option<SecretString> {
        if let Some(env_name) = &conn.api_key_env
            && let Ok(val) = std::env::var(env_name)
            && !val.trim().is_empty()
        {
            return Some(SecretString::from(val));
        }

        if let Some(secret) = creds.api_key(&conn.id)
            && !secret.expose_secret().trim().is_empty()
        {
            return Some(secret.clone());
        }

        if let Some(preset_id) = conn.preset_id.as_deref() {
            let legacy = match preset_id {
                "exa" => creds.websearch.exa_api_key.clone(),
                "parallel" => creds.websearch.parallel_api_key.clone(),
                "tavily" => creds.websearch.tavily_api_key.clone(),
                "bocha" => creds.websearch.bocha_api_key.clone(),
                _ => None,
            };
            if let Some(secret) = legacy
                && !secret.expose_secret().trim().is_empty()
            {
                return Some(secret);
            }
        }

        None
    }

    pub fn resolve_reader_credential(
        conn: &WebReaderConnection,
        creds: &Credentials,
    ) -> Option<SecretString> {
        if let Some(env_name) = &conn.api_key_env
            && let Ok(val) = std::env::var(env_name)
            && !val.trim().is_empty()
        {
            return Some(SecretString::from(val));
        }

        if let Some(secret) = creds.api_key(&conn.id)
            && !secret.expose_secret().trim().is_empty()
        {
            return Some(secret.clone());
        }

        if let Some(preset_id) = conn.preset_id.as_deref() {
            let legacy = match preset_id {
                "jina" => creds.websearch.jina_api_key.clone(),
                _ => None,
            };
            if let Some(secret) = legacy
                && !secret.expose_secret().trim().is_empty()
            {
                return Some(secret);
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_contain_only_exa_search() {
        let conns = WebConnections::with_defaults();
        assert_eq!(conns.search_connections.len(), 1);
        assert_eq!(conns.search_connections[0].id, "exa-default");
        assert!(conns.reader_connections.is_empty());
    }

    #[test]
    fn adding_and_removing_search_and_reader_connections() {
        let mut conns = WebConnections::with_defaults();
        let custom_reader = WebReaderConnection {
            id: "my-jina".to_string(),
            name: Some("My Jina Reader".to_string()),
            preset_id: Some("jina".to_string()),
            api_key_env: Some("MY_JINA_KEY".to_string()),
            base_url: None,
            custom_headers: None,
            enabled: true,
        };
        conns.upsert_reader(custom_reader);
        assert_eq!(conns.reader_connections.len(), 1);
        assert_eq!(conns.get_reader("my-jina").unwrap().display_name(), "My Jina Reader");

        conns.remove_reader("my-jina");
        assert!(conns.reader_connections.is_empty());
    }
}
