//! Pluggable page-content backends ("readers") for `webfetch` — the depth
//! half of the two-stage research pipeline (websearch = breadth, webfetch =
//! depth; ADR-0117).
//!
//! A reader turns one URL into clean page text. The builtin reader is a
//! direct fetch plus the local naive HTML stripping; the Jina reader
//! delegates to `r.jina.ai`, which renders JavaScript and extracts the main
//! content server-side. Adding a reader is one new module + one match arm in
//! [`build_reader`]; `webfetch` and the other readers never change.
//!
//! SSRF note: readers receive only URLs that already passed
//! [`crate::tools::ssrf::assert_public_url`]. The Jina reader sends the URL to
//! a third party, so it must never be pointed at private addresses — the
//! pre-check in `webfetch` enforces this before any reader runs.

use super::web::html_to_text;
use crate::tools::reader::jina::ReadPage;

pub mod jina;

/// Which page-content backend `webfetch` uses. Unknown config names resolve
/// to [`Reader::Builtin`] rather than erroring at construction time — same
/// philosophy as [`crate::tools::search::build_provider`]: a typo never
/// leaves the tool without a working backend.
pub(crate) enum Reader {
    Builtin,
    Jina(jina::JinaReader),
}

pub(crate) fn build_reader(cfg: &neenee_contracts::WebSearchConfig) -> Reader {
    match cfg.reader.trim() {
        "jina" => Reader::Jina(jina::JinaReader {
            api_key: cfg
                .jina_api_key
                .as_ref()
                .map(|k| k.expose_secret().to_string()),
        }),
        _ => Reader::Builtin,
    }
}

impl Reader {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Reader::Builtin => "builtin",
            Reader::Jina(_) => "jina",
        }
    }

    /// Fetch one URL and return clean page text plus the content type of the
    /// underlying response. `raw` skips text extraction for non-HTML
    /// content.
    ///
    /// Errors are surfaced verbatim to the model/user. The Jina reader
    /// deliberately reports *its own* failure and lets the caller decide
    /// whether to fall back to the builtin path rather than silently
    /// masking a misconfigured third party.
    pub(crate) async fn read(
        &self,
        client: &reqwest::Client,
        url: &str,
        raw: bool,
    ) -> Result<ReaderOutput, String> {
        match self {
            Reader::Builtin => builtin_read(client, url, raw).await,
            Reader::Jina(j) => j.read(client, url).await,
        }
    }
}

/// What a reader produced for one URL.
pub(crate) struct ReaderOutput {
    /// Clean text, ready for the model. For non-HTML content or `raw=true`
    /// this is the body verbatim.
    pub text: String,
    /// Content type reported by the *underlying* fetch (e.g. from Jina's
    /// target response), used by `webfetch` to label the output.
    pub content_type: String,
}

async fn builtin_read(
    client: &reqwest::Client,
    url: &str,
    raw: bool,
) -> Result<ReaderOutput, String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Request failed: {e}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("HTTP {status} for {url}"));
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let body = response
        .text()
        .await
        .map_err(|e| format!("Failed to read body: {e}"))?;
    let text = if raw || !content_type.contains("html") {
        body
    } else {
        html_to_text(&body)
    };
    Ok(ReaderOutput { text, content_type })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_reader_defaults_to_builtin_for_unknown_name() {
        let cfg = neenee_contracts::WebSearchConfig::default();
        assert_eq!(build_reader(&cfg).name(), "builtin");

        let cfg: neenee_contracts::WebSearchConfig =
            toml::from_str("reader = \"totally-bogus\"").expect("reader field parses");
        assert_eq!(build_reader(&cfg).name(), "builtin");
    }

    #[test]
    fn build_reader_selects_jina_by_name() {
        let cfg: neenee_contracts::WebSearchConfig =
            toml::from_str("reader = \"jina\"").expect("reader field parses");
        assert_eq!(build_reader(&cfg).name(), "jina");
    }
}
