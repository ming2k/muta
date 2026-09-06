//! Presenters for `edit_text` and `write_file`.
//!
//! `edit_text` renders a red/green line diff (old vs new) in the expanded body.
//! `write_file` renders a full-file insertion diff (all added lines in green,
//! Git/GitHub style) in the expanded body. Both default to expanded and show a
//! line-count suffix in the collapsed summary.

use super::diff::line_diff_counts;
use super::{ResultKind, ToolPresenter, ToolView};
use crate::components::path::PathView;

pub struct EditPresenter;

impl ToolPresenter for EditPresenter {
    fn summary(&self, view: &ToolView) -> String {
        let Some(raw_path) = view.str("path") else {
            return "Edit text".to_string();
        };
        let path = PathView::from_str(raw_path).format_text();
        match (view.str("old_string"), view.str("new_string")) {
            (Some(old), Some(new)) => {
                let (added, removed) = line_diff_counts(old, new);
                format!("Edit {} +{} -{}", path, added, removed)
            }
            _ => format!("Edit {}", path),
        }
    }

    fn result_kind(&self) -> ResultKind {
        ResultKind::Diff
    }

    fn default_expanded(&self) -> bool {
        true
    }
}

pub struct WritePresenter;

impl ToolPresenter for WritePresenter {
    fn summary(&self, view: &ToolView) -> String {
        let Some(raw_path) = view.str("path") else {
            return "Write file".to_string();
        };
        let path = PathView::from_str(raw_path).format_text();
        match view.str("content") {
            Some(content) => {
                let (added, _) = line_diff_counts("", content);
                format!("Write {} +{}", path, added)
            }
            None => format!("Write {}", path),
        }
    }

    fn result_kind(&self) -> ResultKind {
        ResultKind::Diff
    }

    fn default_expanded(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_presenter_defaults_and_diff_kind() {
        let presenter = EditPresenter;
        assert_eq!(presenter.result_kind(), ResultKind::Diff);
        assert!(presenter.default_expanded());
    }

    #[test]
    fn write_presenter_defaults_and_diff_kind() {
        let presenter = WritePresenter;
        assert_eq!(presenter.result_kind(), ResultKind::Diff);
        assert!(presenter.default_expanded());
    }
}
