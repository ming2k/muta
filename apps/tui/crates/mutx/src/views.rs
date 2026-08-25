//! TUI surface routing and retained, buffer-like view state.
//!
//! A [`ViewId`] identifies a durable place the user can return to. A
//! [`Modal`] is only that place's rendering/input presentation and is not a
//! usable identity: Activity and Todos intentionally share one modal. The
//! [`SurfaceRouter`] is therefore the sole owner of the focused surface and
//! its transient return stack; [`ViewRegistry`] owns retained state and MRU.

use crate::modal::Modal;
use std::collections::HashMap;

/// The exact identity of a retained browse view. Mapping each view to its
/// [`Modal`] presentation is total; the inverse intentionally does not exist.
///
/// Not view ids (ADR-0139): the request-driven sheets (Permission,
/// Question, InputInjection — queue-driven lifecycles) and the child editors
/// of the picker chain (ModelEditor, ProviderTemplate, OauthPending,
/// CustomProvider — they are *transitions* within the Models/Connections
/// flow, not places to stand: they never appear in the switcher and their
/// Esc pops the router's transient return stack rather than hiding a view).
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
    /// The flat model picker (`Ctrl+M` / `/models`). A retained view whose
    /// open parks the composer draft into its own per-view slot
    /// (not the global `stashed_input`), so a draft parked for Models can
    /// never be clobbered by one parked for Connections or History.
    Models,
    /// The connections manager (`/connections`). Same per-view-draft
    /// contract as [`Self::Models`].
    Connections,
    /// Input-history recall (`Ctrl+R`). Same per-view-draft contract.
    HistorySearch,
    /// The queue overview (`Ctrl+Q` / queue-bar click). Retained;
    /// its enter/exit effects (auto-block / resume of the viewed session's
    /// outbox) are view enter/exit hooks, not open-ritual resets.
    Queue,
    /// The session dashboard (`/dashboard`). Retained — the dock
    /// selection/focus survive hide; the cockpit console log lives for the
    /// view's lifetime (first open clears it) instead of every open.
    Host,
    /// The sessions picker (`/sessions`). Retained.
    Sessions,
    /// The session DAG tree viewer (`/tree`).
    Tree,
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
            ViewId::Models => Modal::Models,
            ViewId::Connections => Modal::Connections,
            ViewId::HistorySearch => Modal::HistorySearch,
            ViewId::Queue => Modal::Queue,
            ViewId::Host => Modal::Host,
            ViewId::Sessions => Modal::Sessions,
            ViewId::Tree => Modal::Tree,
        }
    }

    /// Every view id, in quick-switcher display order: reference surfaces
    /// first (Help, Activity, Todos), then manager lists, then reports,
    /// then the pickers, then the full-screen surfaces.
    pub(crate) const ALL: [ViewId; 18] = [
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
        ViewId::Models,
        ViewId::Connections,
        ViewId::HistorySearch,
        ViewId::Queue,
        ViewId::Host,
        ViewId::Sessions,
        ViewId::Tree,
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
            ViewId::Models => "Switch model",
            ViewId::Connections => "Connections",
            ViewId::HistorySearch => "History",
            ViewId::Queue => "Queue (outbox)",
            ViewId::Host => "Session dashboard",
            ViewId::Sessions => "Sessions",
            ViewId::Tree => "Session tree",
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
            ViewId::Models => "Ctrl+M / /models",
            ViewId::Connections => "/connections",
            ViewId::HistorySearch => "Ctrl+R",
            ViewId::Queue => "Ctrl+Q / queue bar",
            ViewId::Host => "/dashboard",
            ViewId::Sessions => "/sessions",
            ViewId::Tree => "/tree",
        }
    }
}

/// One focused TUI surface. Views have their own exact identity; transient
/// surfaces (request sheets, editors and the switcher) retain only their
/// presentation kind and return through [`SurfaceRouter::pop_transient`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Surface {
    Chat,
    View(ViewId),
    Transient(Modal),
}

impl Surface {
    pub(crate) fn modal(self) -> Modal {
        match self {
            Self::Chat => Modal::None,
            Self::View(id) => id.modal(),
            Self::Transient(modal) => modal,
        }
    }

    pub(crate) fn view(self) -> Option<ViewId> {
        match self {
            Self::View(id) => Some(id),
            Self::Chat | Self::Transient(_) => None,
        }
    }
}

/// Authoritative foreground surface and bounded transient return stack.
///
/// Only the router may turn an exact [`ViewId`] into its legacy [`Modal`]
/// projection. This makes the Activity/Todos identity non-lossy and gives
/// request sheets, editors and the quick switcher one consistent push/pop
/// contract.
#[derive(Debug)]
pub(crate) struct SurfaceRouter {
    active: Surface,
    returns: Vec<Surface>,
}

const SURFACE_STACK_CAP: usize = 16;

impl Default for SurfaceRouter {
    fn default() -> Self {
        Self {
            active: Surface::Chat,
            returns: Vec::new(),
        }
    }
}

impl SurfaceRouter {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn with_view(id: ViewId) -> Self {
        Self {
            active: Surface::View(id),
            returns: Vec::new(),
        }
    }

    pub(crate) fn active(&self) -> Surface {
        self.active
    }

    pub(crate) fn modal(&self) -> Modal {
        self.active.modal()
    }

    pub(crate) fn active_view(&self) -> Option<ViewId> {
        self.active.view()
    }

    pub(crate) fn return_surface(&self) -> Option<Surface> {
        self.returns.last().copied()
    }

    pub(crate) fn show_chat(&mut self) {
        self.active = Surface::Chat;
        self.returns.clear();
    }

    pub(crate) fn show_view(&mut self, id: ViewId) {
        self.active = Surface::View(id);
        self.returns.clear();
    }

    /// Test-only compatibility for constructing isolated transient states.
    #[cfg(test)]
    pub(crate) fn show_transient(&mut self, modal: Modal) {
        debug_assert_ne!(modal, Modal::None);
        self.active = Surface::Transient(modal);
        self.returns.clear();
    }

    /// Replace only the top transient while preserving its parent chain.
    pub(crate) fn replace_transient(&mut self, modal: Modal) {
        debug_assert_ne!(modal, Modal::None);
        self.active = Surface::Transient(modal);
    }

    /// Put a transient surface over the current one.
    pub(crate) fn push_transient(&mut self, modal: Modal) {
        debug_assert_ne!(modal, Modal::None);
        self.returns.push(self.active);
        if self.returns.len() > SURFACE_STACK_CAP {
            self.returns.remove(0);
        }
        self.active = Surface::Transient(modal);
    }

    /// Dismiss a transient and restore its exact parent. Chat is the safe
    /// fallback for an unbalanced pop.
    pub(crate) fn pop_transient(&mut self) -> Surface {
        self.active = self.returns.pop().unwrap_or(Surface::Chat);
        self.active
    }
}

/// The retained state of one view. Deliberately minimal: the fields every
/// browse surface previously reset on open (`modal_index`, scroll, follow).
/// Surfaces whose data must refresh on reopen keep that data on `App` and use
/// an explicit refresh-on-show side effect in `enter_view` — retention is
/// about *where the user was standing*, never about serving stale data.
#[derive(Debug, Clone)]
pub(crate) struct ViewState {
    /// Selection cursor (`App::modal_index` while this view is focused).
    pub(crate) index: usize,
    /// Body scroll offset (`App::help_scroll` / `activity_scroll` / …).
    pub(crate) scroll: usize,
    /// Whether the body scroll follows the selection.
    pub(crate) follow: bool,
    /// The composer draft this view parked when it borrowed the input line
    /// (ADR-0139 per-view drafts). Only the draft-owning views use it
    /// (Models, Connections, HistorySearch): parking stores the composer's
    /// text in the *entering* view's slot, restoring on return, so two
    /// borrowed-line flows can never clobber each other's draft through the
    /// old single global `stashed_input` slot.
    pub(crate) draft: Option<String>,
    /// Text owned by a view while it borrows the composer as a filter. It is
    /// distinct from `draft`: `draft` is the user's parked chat text, while
    /// `query` is the Models/Connections/History search text to restore when
    /// that view is focused again.
    pub(crate) query: String,
    /// Whether the view's query sub-layer is active. An empty query can still
    /// be an intentional focused search field, so this cannot be inferred
    /// from `query`.
    pub(crate) query_active: bool,
}

impl Default for ViewState {
    fn default() -> Self {
        Self {
            index: 0,
            scroll: 0,
            follow: true,
            draft: None,
            query: String::new(),
            query_active: false,
        }
    }
}

/// A MRU-ordered registry of retained view states (ADR-0139).
///
/// `open` initialises state exactly once per view; subsequent opens are pure
/// focus moves that restore the retained scroll/index — the "leave and come
/// back, nothing lost" contract sessions already have. `hide` keeps both
/// state and MRU membership; `close` forgets both outright.
#[derive(Debug, Default)]
pub(crate) struct ViewRegistry {
    /// Most-recent-first open order. Drives the quick switcher's MRU list.
    order: Vec<ViewId>,
    /// Retained per-view state. Entries persist across hide (ADR-0139); an
    /// entry is removed only by an explicit `close`.
    states: HashMap<ViewId, ViewState>,
}

impl ViewRegistry {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Focus a view: move it to the front of the MRU order, initialising its
    /// state on first open. Returns the retained state to restore (cursor,
    /// scroll, follow) — `None` on the very first open so the caller can run
    /// one-time UI initialization. Authoritative data refresh is a separate
    /// every-show policy.
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

    /// Hide preserves both state and recency. MRU describes recently used
    /// buffers, not which one happens to be painted right now.
    pub(crate) fn hide(&mut self, _id: ViewId) {}

    /// Explicit close: forget the view's state and MRU membership entirely.
    /// The switcher's `Del` gesture invokes this without touching backend data.
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
    /// badges (the badges read `order()` today).
    #[allow(dead_code)] // production reads `order`; tests assert exact lifetime
    pub(crate) fn is_open(&self, id: ViewId) -> bool {
        self.states.contains_key(&id)
    }

    /// The retained state of a view, if it has been opened before.
    pub(crate) fn states(&self, id: &ViewId) -> Option<&ViewState> {
        self.states.get(id)
    }

    /// Mutable access to a view's retained state, if it exists.
    pub(crate) fn states_mut(&mut self, id: &ViewId) -> Option<&mut ViewState> {
        self.states.get_mut(id)
    }

    /// The MRU order of open views, most recent first.
    pub(crate) fn order(&self) -> &[ViewId] {
        &self.order
    }

    /// Quick-switcher rows (ADR-0139): open views first, in MRU
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

    /// The switcher's visible rows for a live query: the MRU /
    /// discovery row set filtered by a fuzzy match of `query` against each
    /// view's label and hint (case-insensitive subsequence). An empty query
    /// is the unfiltered list.
    pub(crate) fn switcher_rows_filtered(&self, query: &str) -> Vec<ViewId> {
        let rows = self.switcher_rows();
        if query.trim().is_empty() {
            return rows;
        }
        let q = query.trim();
        rows.into_iter()
            .filter(|id| {
                let label = id.label();
                let hint = id.hint();
                crate::fuzzy::fuzzy_match(label, q).is_some()
                    || crate::fuzzy::fuzzy_match(hint, q).is_some()
            })
            .collect()
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
                draft: None,
                query: String::new(),
                query_active: false,
            },
        );
        let restored = reg.open(ViewId::Help).expect("state retained");
        assert_eq!(
            (restored.index, restored.scroll, restored.follow),
            (3, 12, false)
        );
    }

    #[test]
    fn hide_retains_state_and_mru() {
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
        assert!(reg.is_open(ViewId::Help));
        assert_eq!(reg.order(), &[ViewId::Help]);
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
        assert_eq!(reg.order(), &[ViewId::Help, ViewId::Tools]);
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
    fn router_preserves_todos_and_activity_identity() {
        assert_eq!(ViewId::Todos.modal(), Modal::Activity);
        assert_eq!(ViewId::Activity.modal(), Modal::Activity);
        let mut router = SurfaceRouter::with_view(ViewId::Todos);
        assert_eq!(router.modal(), Modal::Activity);
        assert_eq!(router.active_view(), Some(ViewId::Todos));
        router.show_view(ViewId::Activity);
        assert_eq!(router.active_view(), Some(ViewId::Activity));
    }

    #[test]
    fn transient_stack_restores_exact_view() {
        let mut router = SurfaceRouter::with_view(ViewId::Todos);
        router.push_transient(Modal::ViewSwitcher);
        router.push_transient(Modal::Question);
        assert_eq!(router.pop_transient().modal(), Modal::ViewSwitcher);
        assert_eq!(router.pop_transient().view(), Some(ViewId::Todos));
    }
}
