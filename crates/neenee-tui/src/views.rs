//! Retained, buffer-like view state (ADR-0133).
//!
//! Sessions already behave like buffers (ADR-0096: closing the TUI detaches,
//! re-attach resumes). Views got the opposite contract — every open reset
//! scroll/index/sub-layer state, so leaving a view destroyed where the user
//! was. This module is the first migration step: a per-view state record
//! that is initialised **once** on first open, survives hide, and a MRU
//! stack that backs the quick switcher (Ctrl+L).
//!
//! Scope of this step (ADR-0133 phase 1–2): the read-only browse surfaces —
//! Help, Activity/Todos, Tools, MCP, Skills, Permissions, UsageStats,
//! TokenReport, Btw. The picker→editor chains and Host/Sessions keep their
//! existing open rituals; they migrate in later phases.
//!
//! Lifecycle vocabulary (ADR-0133 §Decision 3):
//! - **hide** — Esc / outside-click: state retained (the registry keeps it).
//! - **close** — explicit exit: removed from the registry.
//! - **switch** — quick switcher / navigation: pure focus move, no reset.

use crate::modal::Modal;
use std::collections::HashMap;

/// The identity of a retained browse view. One variant per surface that has
/// migrated onto the buffer-like lifecycle; the mapping to the legacy
/// [`Modal`] discriminant is total so open/close arms stay exhaustive.
///
/// Not view ids (ADR-0133): the request-driven sheets (Permission, Question,
/// InputInjection — queue-driven lifecycles), the composer-borrowing editors
/// (Models/Connections pickers with their parked-draft contract, ModelEditor,
/// ProviderTemplate, OauthPending, CustomProvider), and Host/Sessions
/// (first-class full-screen surfaces with their own open rituals). They keep
/// `Modal` semantics until their migration phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ViewId {
    Help,
    Activity,
    Todos,
    Tools,
    Mcp,
    Skills,
    Permissions,
    UsageStats,
    TokenReport,
    Btw,
    Config,
}

impl ViewId {
    /// The legacy discriminant this view renders as.
    pub(crate) fn modal(self) -> Modal {
        match self {
            ViewId::Help => Modal::Help,
            // Todos is the Activity surface pinned to its Todos section —
            // one view id per *place the user can stand*, matching how the
            // open actions distinguish them (`Ctrl+T` vs the activity bar).
            ViewId::Activity | ViewId::Todos => Modal::Activity,
            ViewId::Tools => Modal::Tools,
            ViewId::Mcp => Modal::Mcp,
            ViewId::Skills => Modal::Skills,
            ViewId::Permissions => Modal::Permissions,
            ViewId::UsageStats => Modal::UsageStats,
            ViewId::TokenReport => Modal::TokenReport,
            ViewId::Btw => Modal::Btw,
            ViewId::Config => Modal::Config,
        }
    }

    /// Every view id, in quick-switcher display order: reference surfaces
    /// first (Help, Activity, Todos), then manager lists, then reports.
    pub(crate) const ALL: [ViewId; 11] = [
        ViewId::Help,
        ViewId::Activity,
        ViewId::Todos,
        ViewId::Tools,
        ViewId::Mcp,
        ViewId::Skills,
        ViewId::Permissions,
        ViewId::UsageStats,
        ViewId::TokenReport,
        ViewId::Btw,
        ViewId::Config,
    ];

    /// The label shown in the quick switcher and used for fuzzy matching.
    pub(crate) fn label(self) -> &'static str {
        match self {
            ViewId::Help => "Help / keys",
            ViewId::Activity => "Activity",
            ViewId::Todos => "Todos",
            ViewId::Tools => "Tools",
            ViewId::Mcp => "MCP servers",
            ViewId::Skills => "Skills",
            ViewId::Permissions => "Permissions",
            ViewId::UsageStats => "Usage stats",
            ViewId::TokenReport => "Context report",
            ViewId::Btw => "Asides (/btw)",
            ViewId::Config => "Settings",
        }
    }

    /// The secondary line the switcher shows under the label — where the
    /// surface is normally reached from, so the list doubles as discovery.
    pub(crate) fn hint(self) -> &'static str {
        match self {
            ViewId::Help => "F1 / ?",
            ViewId::Activity => "activity bar",
            ViewId::Todos => "Ctrl+T / todo bar",
            ViewId::Tools => "/tools",
            ViewId::Mcp => "/mcp",
            ViewId::Skills => "/skills",
            ViewId::Permissions => "/permissions",
            ViewId::UsageStats => "/usage",
            ViewId::TokenReport => "context meter",
            ViewId::Btw => "F5 / /btw list",
            ViewId::Config => "/config · /settings",
        }
    }
}

impl TryFrom<Modal> for ViewId {
    type Error = ();

    /// Map an active modal back to its view id. `Err(())` for every modal
    /// that has not migrated (or is not a view — see the type docs).
    fn try_from(modal: Modal) -> Result<Self, Self::Error> {
        match modal {
            Modal::Help => Ok(ViewId::Help),
            Modal::Activity => Ok(ViewId::Activity),
            Modal::Tools => Ok(ViewId::Tools),
            Modal::Mcp => Ok(ViewId::Mcp),
            Modal::Skills => Ok(ViewId::Skills),
            Modal::Permissions => Ok(ViewId::Permissions),
            Modal::UsageStats => Ok(ViewId::UsageStats),
            Modal::TokenReport => Ok(ViewId::TokenReport),
            Modal::Btw => Ok(ViewId::Btw),
            Modal::Config => Ok(ViewId::Config),
            _ => Err(()),
        }
    }
}

/// The retained state of one view. Deliberately minimal: the fields every
/// browse surface was resetting on open (`modal_index`, scroll, follow) plus
/// the sub-layer toggles that phase-1 surfaces own. Surfaces whose data must
/// refresh on reopen (UsageStats' report) keep that data on `App` and use an
/// explicit refresh-on-show side effect at their open arm — retention is
/// about *where the user was standing*, never about serving stale data.
#[derive(Debug, Default, Clone)]
pub(crate) struct ViewState {
    /// Selection cursor (`App::modal_index` while this view is focused).
    pub(crate) index: usize,
    /// Body scroll offset (`App::help_scroll` / `activity_scroll` / …).
    pub(crate) scroll: usize,
    /// Whether the body scroll follows the selection.
    pub(crate) follow: bool,
}

/// A MRU-ordered registry of retained view states (ADR-0133).
///
/// `open` initialises state exactly once per view; subsequent opens are pure
/// focus moves that restore the retained scroll/index — the "leave and come
/// back, nothing lost" contract sessions already have. `hide` removes the
/// view from the MRU order but keeps its state; `close` forgets it outright.
#[derive(Debug, Default)]
pub(crate) struct ViewRegistry {
    /// Most-recent-first open order. Drives the quick switcher's MRU list.
    order: Vec<ViewId>,
    /// Retained per-view state. Entries persist across hide (ADR-0133); an
    /// entry is removed only by an explicit `close`.
    states: HashMap<ViewId, ViewState>,
}

impl ViewRegistry {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Focus a view: move it to the front of the MRU order, initialising its
    /// state on first open. Returns the retained state to restore (cursor,
    /// scroll, follow) — `None` on the very first open so the caller runs
    /// its per-surface, data-side open effects (a fresh QueryUsageStats, a
    /// session-context query, …) that retention must not skip.
    pub(crate) fn open(&mut self, id: ViewId) -> Option<ViewState> {
        self.order.retain(|&v| v != id);
        self.order.insert(0, id);
        match self.states.entry(id) {
            std::collections::hash_map::Entry::Occupied(e) => Some(e.get().clone()),
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(ViewState::default());
                None
            }
        }
    }

    /// Record the view's current state when it loses focus (hide or switch).
    /// Idempotent for not-yet-open views.
    pub(crate) fn save(&mut self, id: ViewId, state: ViewState) {
        self.states.insert(id, state);
    }

    /// Hide a view: it leaves the MRU order (so the switcher reflects what
    /// is conceptually "closed") but its state is retained for the next
    /// open. This is the Esc/outside-click verb.
    pub(crate) fn hide(&mut self, id: ViewId) {
        self.order.retain(|&v| v != id);
    }

    /// Explicit close: forget the view's state entirely. Phase-1 surfaces
    /// have no user-facing close gesture beyond hide (their content is
    /// derived), so this currently fires only on session change, where the
    /// browsing context the state belonged to is gone.
    #[allow(dead_code)] // wired for ADR-0133 later phases (explicit close verbs)
    pub(crate) fn close(&mut self, id: ViewId) {
        self.order.retain(|&v| v != id);
        self.states.remove(&id);
    }

    /// Forget every retained state (session switch: the scroll position
    /// belonged to the previous conversation's context).
    pub(crate) fn close_all(&mut self) {
        self.order.clear();
        self.states.clear();
    }

    /// Whether the view has been opened at least once (state retained).
    /// Currently exercised by tests and reserved for the switcher's UI
    /// badges in later ADR-0133 phases (the badges read `order()` today).
    #[allow(dead_code)]
    pub(crate) fn is_open(&self, id: ViewId) -> bool {
        self.order.contains(&id)
    }

    /// The retained state of a view, if it has been opened before.
    pub(crate) fn states(&self, id: &ViewId) -> Option<&ViewState> {
        self.states.get(id)
    }

    /// Overwrite just the follow flag of a retained view (no-op for a view
    /// with no state yet — `open` creates it).
    pub(crate) fn set_follow(&mut self, id: ViewId, follow: bool) {
        if let Some(state) = self.states.get_mut(&id) {
            state.follow = follow;
        }
    }

    /// The MRU order of open views, most recent first.
    pub(crate) fn order(&self) -> &[ViewId] {
        &self.order
    }

    /// Quick-switcher rows (ADR-0133 §Decision 4): open views first, in MRU
    /// order, then every other view in display order — the not-yet-opened
    /// ones are still listed so the switcher doubles as discovery.
    pub(crate) fn switcher_rows(&self) -> Vec<ViewId> {
        let mut rows: Vec<ViewId> = self.order.clone();
        for id in ViewId::ALL {
            if !rows.contains(&id) {
                rows.push(id);
            }
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_open_initialises_once_and_returns_none() {
        let mut reg = ViewRegistry::new();
        assert!(reg.open(ViewId::Help).is_none(), "first open has no state");
        // A second open (reopen after hide) returns the saved state.
        reg.save(
            ViewId::Help,
            ViewState {
                index: 3,
                scroll: 12,
                follow: false,
            },
        );
        let restored = reg.open(ViewId::Help).expect("state retained");
        assert_eq!(
            (restored.index, restored.scroll, restored.follow),
            (3, 12, false)
        );
    }

    #[test]
    fn hide_retains_state_but_leaves_mru() {
        let mut reg = ViewRegistry::new();
        reg.open(ViewId::Help);
        reg.save(
            ViewId::Help,
            ViewState {
                index: 7,
                ..Default::default()
            },
        );
        reg.hide(ViewId::Help);
        assert!(!reg.is_open(ViewId::Help));
        assert!(reg.order().is_empty());
        let restored = reg.open(ViewId::Help).expect("reopen restores");
        assert_eq!(restored.index, 7);
    }

    #[test]
    fn close_forgets_state() {
        let mut reg = ViewRegistry::new();
        reg.open(ViewId::UsageStats);
        reg.close(ViewId::UsageStats);
        assert!(reg.open(ViewId::UsageStats).is_none(), "close forgets");
    }

    #[test]
    fn mru_order_tracks_focusing() {
        let mut reg = ViewRegistry::new();
        reg.open(ViewId::Help);
        reg.open(ViewId::Tools);
        reg.open(ViewId::Help);
        assert_eq!(reg.order(), &[ViewId::Help, ViewId::Tools]);
        reg.hide(ViewId::Help);
        assert_eq!(reg.order(), &[ViewId::Tools]);
    }

    #[test]
    fn switcher_rows_open_views_mru_first_then_rest() {
        let mut reg = ViewRegistry::new();
        reg.open(ViewId::Skills);
        reg.open(ViewId::Btw);
        let rows = reg.switcher_rows();
        assert_eq!(&rows[..2], &[ViewId::Btw, ViewId::Skills], "MRU first");
        // Every view is listed exactly once (discovery of un-opened ones).
        assert_eq!(rows.len(), ViewId::ALL.len());
        for id in ViewId::ALL {
            assert_eq!(rows.iter().filter(|&&r| r == id).count(), 1);
        }
    }

    #[test]
    fn todos_and_activity_map_to_the_activity_modal() {
        // Two view ids (places to stand) share one modal discriminant; the
        // try-from mapping resolves to the browsing default.
        assert_eq!(ViewId::Todos.modal(), Modal::Activity);
        assert_eq!(ViewId::Activity.modal(), Modal::Activity);
        assert_eq!(ViewId::try_from(Modal::Activity), Ok(ViewId::Activity));
        assert_eq!(ViewId::try_from(Modal::Host), Err(()));
    }
}
