//! Presenters for file discovery, text search, and shallow directory listing.

use super::{ResultKind, ToolPresenter, ToolView, truncate};
use crate::components::path::PathView;

pub struct SearchTextPresenter;

impl ToolPresenter for SearchTextPresenter {
    fn summary(&self, view: &ToolView) -> String {
        let query = view.str("query").unwrap_or("...");
        let path = view.str("path").unwrap_or(".");
        let path_display = if path == "." {
            ".".to_string()
        } else {
            PathView::from_str(path).format_text()
        };
        format!("Search \"{}\" in {}", truncate(query, 48), path_display)
    }

    fn result_kind(&self) -> ResultKind {
        ResultKind::Matches
    }
}

pub struct FindFilesPresenter;

impl ToolPresenter for FindFilesPresenter {
    fn summary(&self, view: &ToolView) -> String {
        let patterns = view
            .args
            .get("patterns")
            .and_then(serde_json::Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let selection = match patterns {
            [] => "files".to_string(),
            [pattern] => pattern
                .as_str()
                .map(|pattern| truncate(pattern, 48))
                .unwrap_or_else(|| "files".to_string()),
            [first, rest @ ..] => first
                .as_str()
                .map(|pattern| format!("{} +{}", truncate(pattern, 36), rest.len()))
                .unwrap_or_else(|| format!("{} patterns", patterns.len())),
        };
        let path = view.str("path").unwrap_or(".");
        if path == "." {
            format!("Find {selection}")
        } else {
            let path_display = PathView::from_str(path).format_text();
            format!("Find {selection} in {path_display}")
        }
    }

    fn result_kind(&self) -> ResultKind {
        ResultKind::Listing
    }
}

pub struct ListDirPresenter;

impl ToolPresenter for ListDirPresenter {
    fn summary(&self, view: &ToolView) -> String {
        view.str("path")
            .map(|path| format!("List {}", PathView::from_str(path).format_text()))
            .unwrap_or_else(|| "List directory".to_string())
    }

    fn result_kind(&self) -> ResultKind {
        ResultKind::Listing
    }
}
