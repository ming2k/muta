//! Presenters for orchestration / meta tools that act on session state rather
//! than the filesystem: `todo`, `envoy`, `use_skill`.

use super::{ToolPresenter, ToolView, truncate};

pub struct TodoPresenter;

impl ToolPresenter for TodoPresenter {
    fn summary(&self, _view: &ToolView) -> String {
        "Update todo list".to_string()
    }
}

pub struct EnvoyPresenter;

impl ToolPresenter for EnvoyPresenter {
    fn summary(&self, view: &ToolView) -> String {
        // The role badge `[explore]` / `[plan]` is drawn by the renderer in
        // front of this summary, so the summary itself carries only the task
        // description — repeating the role here would double it up.
        view.str("description")
            .map(|desc| truncate(desc, 56).to_string())
            .unwrap_or_else(|| "Run envoy".to_string())
    }
}

pub struct UseSkillPresenter;

impl ToolPresenter for UseSkillPresenter {
    fn summary(&self, view: &ToolView) -> String {
        view.str("name")
            .map(|name| format!("Use skill {}", name))
            .unwrap_or_else(|| "Use skill".to_string())
    }
}
