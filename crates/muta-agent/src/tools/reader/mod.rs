//! Pluggable page-content backends ("readers") for `webfetch` — the depth
//! half of the two-stage research pipeline (websearch = breadth, webfetch =
//! depth; ADR-0117).
//!
//! A reader turns one URL into clean page text. The Jina reader delegates to
//! `r.jina.ai`, which renders JavaScript and extracts the main content
//! server-side as Markdown.
//!
//! SSRF note: readers receive only URLs that already passed
//! [`crate::tools::ssrf::assert_public_url`]. The Jina reader sends the URL to
//! a third party, so it must never be pointed at private addresses — the
//! pre-check in `webfetch` enforces this before any reader runs.

use crate::tools::reader::jina::ReadPage;

pub mod jina;

/// Which page-content backend `webfetch` uses.
pub(crate) enum Reader {
    Jina(jina::JinaReader),
    Disabled,
}

pub(crate) fn build_reader(cfg: &muta_contracts::WebSearchConfig) -> Reader {
    let name = cfg.reader.trim();
    if name == "none" || name == "(none)" || name == "disabled" {
        return Reader::Disabled;
    }
    Reader::Jina(jina::JinaReader {
        api_key: cfg
            .jina_api_key
            .as_ref()
            .map(|k| k.expose_secret().to_string()),
    })
}

impl Reader {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Reader::Jina(_) => "jina",
            Reader::Disabled => "disabled",
        }
    }

    /// Fetch one URL and return clean page text plus the content type of the
    /// underlying response. `raw` skips text extraction for non-HTML
    /// content when applicable.
    ///
    /// Errors are surfaced verbatim to the model/user.
    pub(crate) async fn read(
        &self,
        client: &reqwest::Client,
        url: &str,
        _raw: bool,
    ) -> Result<ReaderOutput, String> {
        match self {
            Reader::Jina(j) => j.read(client, url).await,
            Reader::Disabled => Err("webfetch reader is disabled in configuration".to_string()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_reader_defaults_to_jina_for_unknown_name() {
        let cfg = muta_contracts::WebSearchConfig::default();
        assert_eq!(build_reader(&cfg).name(), "jina");

        let cfg: muta_contracts::WebSearchConfig =
            toml::from_str("reader = \"totally-bogus\"").expect("reader field parses");
        assert_eq!(build_reader(&cfg).name(), "jina");
    }

    #[test]
    fn build_reader_disables_when_configured() {
        let cfg: muta_contracts::WebSearchConfig =
            toml::from_str("reader = \"disabled\"").expect("reader field parses");
        assert_eq!(build_reader(&cfg).name(), "disabled");

        let cfg: muta_contracts::WebSearchConfig =
            toml::from_str("reader = \"none\"").expect("reader field parses");
        assert_eq!(build_reader(&cfg).name(), "disabled");
    }

    #[test]
    fn build_reader_selects_jina_by_name() {
        let cfg: muta_contracts::WebSearchConfig =
            toml::from_str("reader = \"jina\"").expect("reader field parses");
        assert_eq!(build_reader(&cfg).name(), "jina");
    }
}
