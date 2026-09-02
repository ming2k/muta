//! Jina Reader backend (`https://r.jina.ai/<url>`) — server-side rendering
//! (including JavaScript-heavy SPA pages) plus readability-style main-content
//! extraction, returned as Markdown. Works anonymously with a modest rate
//! limit; an optional `Authorization: Bearer` key raises the quota.
//!
//! Chosen over a local readability port because it needs no DOM crate and
//! handles the SPA case (which `html_to_text` cannot: an unrendered SPA shell
//! strips to near-empty text). The trade — one extra network hop and sending
//! the URL to a third party — is opt-in via `[websearch] reader = "jina"`.
//!
//! Fallback contract: on any transport error or HTTP >= 400 from Jina itself
//! we return `Err`, and the *caller* (`read_url`) retries via the builtin
//! direct fetch. Jina's "Warning: Target URL returned error" lines (it
//! relays the *target's* status) are passed through as content, not errors,
//! because the page text is often still useful.

use super::ReaderOutput;
use async_trait::async_trait;

const JINA_READER_BASE: &str = "https://r.jina.ai/";

pub(crate) struct JinaReader {
    pub api_key: Option<String>,
}

#[async_trait]
pub(crate) trait ReadPage {
    async fn read(&self, client: &reqwest::Client, url: &str) -> Result<ReaderOutput, String>;
}

#[async_trait]
impl ReadPage for JinaReader {
    async fn read(&self, client: &reqwest::Client, url: &str) -> Result<ReaderOutput, String> {
        let reader_url = format!("{JINA_READER_BASE}{url}");
        let mut request = client
            .get(&reader_url)
            .header(reqwest::header::ACCEPT, "text/plain");
        if let Some(key) = self
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            request = request.bearer_auth(key);
        }
        let response = request
            .send()
            .await
            .map_err(|e| format!("Jina reader request failed: {e}"))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(format!(
                "Jina reader returned HTTP {status}: {}",
                body.chars().take(200).collect::<String>()
            ));
        }
        let text = response
            .text()
            .await
            .map_err(|e| format!("Jina reader response read failed: {e}"))?;
        Ok(ReaderOutput {
            text,
            content_type: "text/markdown".to_string(),
        })
    }
}
