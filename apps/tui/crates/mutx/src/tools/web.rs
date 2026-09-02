//! Presenters for `read_url` and `search_web`.

use super::{ToolPresenter, ToolView, truncate};

pub struct WebReaderPresenter;

impl ToolPresenter for WebReaderPresenter {
    fn summary(&self, view: &ToolView) -> String {
        view.str("url")
            .map(|url| format!("Read {}", url))
            .unwrap_or_else(|| "Read URL".to_string())
    }
}

pub struct WebSearchPresenter;

impl ToolPresenter for WebSearchPresenter {
    fn summary(&self, view: &ToolView) -> String {
        view.str("query")
            .map(|query| format!("Web search \"{}\"", truncate(query, 52)))
            .unwrap_or_else(|| "Web search".to_string())
    }
}
