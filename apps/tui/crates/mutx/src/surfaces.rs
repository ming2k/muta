//! TUI surface routing: full-screen views, retained panels, transients.
//!
//! ADR-0141 fixes the vocabulary:
//!
//! - A [`View`] is an **independent full-screen destination** — the user
//!   stands *in* a view and the terminal is the view (`Session`,
//!   `Dashboard`, `Settings`, `Runner`, `Side`).
//! - A [`PanelId`] names a **retained modal** — one of the browse overlays
//!   (help, activity, todos, tools, …) that floats over whatever view is
//!   active. Retention (cursor/scroll/drafts via [`PanelRegistry`]) is
//!   orthogonal to geometry: a panel is still a modal.
//! - A [`Modal`] is only a surface's rendering/input presentation and is
//!   never a usable identity: Activity and Todos intentionally share one
//!   modal.
//!
//! The [`SurfaceRouter`] is the sole owner of the focused surface: the
//! base view, the panel or transient floating over it, and the transient
//! return stack.

use crate::modal::Modal;
use std::collections::HashMap;

/// An independent, full-screen destination (ADR-0141). The set is closed:
/// everything else is an overlay. `Session` is the home view; `Runner` and
/// `Side` are session-scoped contexts whose *frame data* (zoom stack, side
/// session id) still lives on `App` — the router owns only which view is
/// active, so `App::in_runner_view()` / `App::in_side_view()` are derived
/// from the router instead of scattered booleans and stack emptiness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum View {
    /// The live conversation: transcript + composer. The default view and
    /// the destination every Esc eventually falls back to.
    Session,
    /// The session dashboard (`/dashboard`, was `ViewId::Host`).
    Dashboard,
    /// The full-screen settings center (`/config` / `/settings`, was
    /// `ViewId::Config`).
    Settings,
    /// Zoomed into an runner task's transcript (was the bare
    /// `App::focus_stack` side channel).
    Runner,
    /// An aside's transcript (was the bare `App::in_side_view` flag).
    Side,
}

impl View {
    /// The presentation discriminant this view renders as. The full-screen
    /// conversation-like views project to `Modal::None` (they are the
    /// surface itself, not an overlay); the two destination views keep
    /// their existing modal-render arms.
    pub(crate) fn modal(self) -> Modal {
        match self {
            Self::Session | Self::Runner | Self::Side => Modal::None,
            Self::Dashboard => Modal::Host,
            Self::Settings => Modal::Config,
        }
    }

    /// The label shown in the quick switcher and used for fuzzy matching.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Session => "Session",
            Self::Dashboard => "Session dashboard",
            Self::Settings => "Settings",
            Self::Runner => "Runner task",
            Self::Side => "Aside",
        }
    }

    /// The secondary line the switcher shows under the label.
    pub(crate) fn hint(self) -> &'static str {
        match self {
            Self::Session => "Esc  home",
            Self::Dashboard => "/dashboard",
            Self::Settings => "/config  /settings",
            Self::Runner => "zoom an runner task",
            Self::Side => "focus an aside",
        }
    }
}

/// The exact identity of a retained browse panel (ADR-0141: a *retained
/// modal*, not a view). Mapping each panel to its [`Modal`] presentation is
/// total; the inverse intentionally does not exist.
///
/// Not panel ids: the request-driven sheets (Permission, Question,
/// InputInjection — queue-driven lifecycles) and the child editors of the
/// picker chain (ModelEditor, ProviderPreset, OauthPending,
/// CustomProvider — they are *transitions* within the Models/Connections
/// flow, not places to stand: they never appear in the switcher and their
/// Esc pops the router's transient return stack rather than hiding a
/// panel).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum PanelId {
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
    /// The flat model picker (`Ctrl+M` / `/models`). A retained panel whose
    /// open parks the composer draft into its own per-panel slot
    /// (not the global `stashed_input`), so a draft parked for Models can
    /// never be clobbered by one parked for Connections or History.
    Models,
    /// The connections manager (`/connections`). Same per-panel-draft
    /// contract as [`Self::Models`].
    Connections,
    /// Input-history recall (`Ctrl+R`). Same per-panel-draft contract.
    HistorySearch,
    /// The queue overview (`Ctrl+Q` / queue-bar click). Retained;
    /// its enter/exit effects (auto-block / resume of the viewed session's
    /// outbox) are panel enter/exit hooks, not open-ritual resets.
    Queue,
    /// The sessions picker (`/sessions`). Retained.
    Sessions,
    /// The session DAG tree viewer (`/tree`).
    Tree,
}

impl PanelId {
    /// The presentation discriminant this panel renders as.
    pub(crate) fn modal(self) -> Modal {
        match self {
            PanelId::Help => Modal::Help,
            // Todos is the Activity surface pinned to its Todos section —
            // one panel id per *place the user can stand*, matching how the
            // open actions distinguish them (`Ctrl+T` vs the activity bar).
            PanelId::Activity | PanelId::Todos => Modal::Activity,
            PanelId::Tools => Modal::Tools,
            PanelId::Mcp => Modal::Mcp,
            PanelId::Skills => Modal::Skills,
            PanelId::Permissions => Modal::Permissions,
            PanelId::UsageStats => Modal::UsageStats,
            PanelId::TokenReport => Modal::TokenReport,
            PanelId::Btw => Modal::Btw,
            PanelId::Models => Modal::Models,
            PanelId::Connections => Modal::Connections,
            PanelId::HistorySearch => Modal::HistorySearch,
            PanelId::Queue => Modal::Queue,
            PanelId::Sessions => Modal::Sessions,
            PanelId::Tree => Modal::Tree,
        }
    }

    /// Every panel id, in quick-switcher display order: reference surfaces
    /// first (Help, Activity, Todos), then manager lists, then reports,
    /// then the pickers.
    pub(crate) const ALL: [PanelId; 16] = [
        PanelId::Help,
        PanelId::Activity,
        PanelId::Todos,
        PanelId::Tools,
        PanelId::Mcp,
        PanelId::Skills,
        PanelId::Permissions,
        PanelId::UsageStats,
        PanelId::TokenReport,
        PanelId::Btw,
        PanelId::Models,
        PanelId::Connections,
        PanelId::HistorySearch,
        PanelId::Queue,
        PanelId::Sessions,
        PanelId::Tree,
    ];

    /// The label shown in the quick switcher and used for fuzzy matching.
    pub(crate) fn label(self) -> &'static str {
        match self {
            PanelId::Help => "Help / keys",
            PanelId::Activity => "Activity",
            PanelId::Todos => "Todos",
            PanelId::Tools => "Tools",
            PanelId::Mcp => "MCP servers",
            PanelId::Skills => "Skills",
            PanelId::Permissions => "Permissions",
            PanelId::UsageStats => "Usage stats",
            PanelId::TokenReport => "Context report",
            PanelId::Btw => "Asides (/btw)",
            PanelId::Models => "Switch model",
            PanelId::Connections => "Connections",
            PanelId::HistorySearch => "History",
            PanelId::Queue => "Queue (outbox)",
            PanelId::Sessions => "Sessions",
            PanelId::Tree => "Session tree",
        }
    }

    /// The secondary line the switcher shows under the label — where the
    /// surface is normally reached from, so the list doubles as discovery.
    pub(crate) fn hint(self) -> &'static str {
        match self {
            PanelId::Help => "F1 / ?",
            PanelId::Activity => "activity bar",
            PanelId::Todos => "Ctrl+T / todo bar",
            PanelId::Tools => "/tools",
            PanelId::Mcp => "/mcp",
            PanelId::Skills => "/skills",
            PanelId::Permissions => "/permissions",
            PanelId::UsageStats => "/usage",
            PanelId::TokenReport => "context meter",
            PanelId::Btw => "F5 / /btw list",
            PanelId::Models => "Ctrl+M / /models",
            PanelId::Connections => "/connections",
            PanelId::HistorySearch => "Ctrl+R",
            PanelId::Queue => "Ctrl+Q / queue bar",
            PanelId::Sessions => "/sessions",
            PanelId::Tree => "/tree",
        }
    }
}

/// One row of the quick switcher: either a switchable full-screen view or a
/// retained panel. Views sort first (ADR-0141).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum SwitcherTarget {
    View(View),
    Panel(PanelId),
}

impl SwitcherTarget {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::View(v) => v.label(),
            Self::Panel(id) => id.label(),
        }
    }

    pub(crate) fn hint(self) -> &'static str {
        match self {
            Self::View(v) => v.hint(),
            Self::Panel(id) => id.hint(),
        }
    }
}

/// One focused TUI surface. Full-screen views have their own identity;
/// panels are retained modals over the active view; transient surfaces
/// (request sheets, editors and the switcher) retain only their
/// presentation kind and return through [`SurfaceRouter::pop_transient`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Surface {
    View(View),
    Panel(PanelId),
    Transient(Modal),
}

impl Surface {
    pub(crate) fn modal(self) -> Modal {
        match self {
            Self::View(v) => v.modal(),
            Self::Panel(id) => id.modal(),
            Self::Transient(modal) => modal,
        }
    }

    pub(crate) fn panel(self) -> Option<PanelId> {
        match self {
            Self::Panel(id) => Some(id),
            Self::View(_) | Self::Transient(_) => None,
        }
    }
}

/// What floats over the base view.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Overlay {
    None,
    Panel(PanelId),
    Transient(Modal),
}

/// Authoritative foreground surface: the base full-screen view, the panel
/// or transient floating over it, and a bounded transient return stack.
///
/// Only the router may turn an exact [`PanelId`] or [`View`] into its
/// [`Modal`] projection. This makes the Activity/Todos identity non-lossy
/// and gives request sheets, editors and the quick switcher one consistent
/// push/pop contract.
#[derive(Debug)]
pub(crate) struct SurfaceRouter {
    /// The full-screen destination the user stands in (ADR-0141). Panels
    /// and transients float *over* it; hiding them reveals it again.
    view: View,
    /// The panel or transient currently floating over [`Self::view`].
    overlay: Overlay,
    /// Bounded return stack for transient surfaces (sheets over sheets,
    /// the switcher over whatever was up).
    returns: Vec<Surface>,
    /// Where Esc from a scoped view (`Runner`/`Side`) or a destination view
    /// opened *over* a scoped view returns to. Only scoped views are ever
    /// pushed here; draining it lands on `View::Session`.
    view_back: Vec<View>,
}

const SURFACE_STACK_CAP: usize = 16;

impl Default for SurfaceRouter {
    fn default() -> Self {
        Self {
            view: View::Session,
            overlay: Overlay::None,
            returns: Vec::new(),
            view_back: Vec::new(),
        }
    }
}

impl SurfaceRouter {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Boot straight into a panel (the startup sessions picker).
    pub(crate) fn with_panel(id: PanelId) -> Self {
        Self {
            overlay: Overlay::Panel(id),
            ..Self::default()
        }
    }

    pub(crate) fn active(&self) -> Surface {
        match self.overlay {
            Overlay::None => Surface::View(self.view),
            Overlay::Panel(id) => Surface::Panel(id),
            Overlay::Transient(modal) => Surface::Transient(modal),
        }
    }

    /// The full-screen view beneath any overlay. This — not stack
    /// emptiness or a boolean — is what `App::in_runner_view()` /
    /// `App::in_side_view()` derive from (ADR-0141).
    pub(crate) fn active_view(&self) -> View {
        self.view
    }

    pub(crate) fn modal(&self) -> Modal {
        self.active().modal()
    }

    /// The retained panel currently floating over the view, if any.
    pub(crate) fn active_panel(&self) -> Option<PanelId> {
        match self.overlay {
            Overlay::Panel(id) => Some(id),
            _ => None,
        }
    }

    pub(crate) fn return_surface(&self) -> Option<Surface> {
        self.returns.last().copied()
    }

    /// Navigate to a full-screen view, replacing any overlay. Leaving a
    /// scoped view (`Runner`/`Side`) remembers it so closing the
    /// destination returns to it, exactly as the transcript-level state on
    /// `App` used to survive `show_chat` by accident.
    pub(crate) fn show_view(&mut self, view: View) {
        if view != self.view && matches!(self.view, View::Runner | View::Side) {
            self.view_back.push(self.view);
            if self.view_back.len() > SURFACE_STACK_CAP {
                self.view_back.remove(0);
            }
        }
        self.view = view;
        self.overlay = Overlay::None;
        self.returns.clear();
    }

    /// Esc from a full-screen view: return to the scoped view it was
    /// opened over, else the session (home). Also the drain target when an
    /// runner zoom stack or side view exits.
    pub(crate) fn back_view(&mut self) -> View {
        self.view = self.view_back.pop().unwrap_or(View::Session);
        self.overlay = Overlay::None;
        self.returns.clear();
        self.view
    }

    /// Hard reset to the session view (the old `show_chat`): discard the
    /// overlay, the transient chain, and any view-return frames. Session
    /// switches and queue exits use this.
    pub(crate) fn show_session_view(&mut self) {
        self.view = View::Session;
        self.overlay = Overlay::None;
        self.returns.clear();
        self.view_back.clear();
    }

    /// Float a retained panel over the current view. The view beneath is
    /// untouched, so hiding the panel reveals it again — a panel may be
    /// open over any view, including `Runner` and `Side`.
    pub(crate) fn show_panel(&mut self, id: PanelId) {
        self.overlay = Overlay::Panel(id);
        self.returns.clear();
    }

    /// Drop the floating panel, revealing the view beneath.
    pub(crate) fn hide_panel(&mut self) {
        self.overlay = match self.overlay {
            Overlay::Panel(_) => Overlay::None,
            other => other,
        };
    }

    /// Test-only compatibility for constructing isolated transient states.
    #[cfg(test)]
    pub(crate) fn show_transient(&mut self, modal: Modal) {
        debug_assert_ne!(modal, Modal::None);
        self.overlay = Overlay::Transient(modal);
        self.returns.clear();
    }

    /// Replace only the top transient while preserving its parent chain.
    pub(crate) fn replace_transient(&mut self, modal: Modal) {
        debug_assert_ne!(modal, Modal::None);
        self.overlay = Overlay::Transient(modal);
    }

    /// Put a transient surface over the current one.
    pub(crate) fn push_transient(&mut self, modal: Modal) {
        debug_assert_ne!(modal, Modal::None);
        self.returns.push(self.active());
        if self.returns.len() > SURFACE_STACK_CAP {
            self.returns.remove(0);
        }
        self.overlay = Overlay::Transient(modal);
    }

    /// Dismiss a transient and restore its exact parent. The session view
    /// is the safe fallback for an unbalanced pop.
    pub(crate) fn pop_transient(&mut self) -> Surface {
        let surface = self.returns.pop().unwrap_or(Surface::View(View::Session));
        match surface {
            Surface::View(v) => {
                // Restoring a view from beneath a transient is navigation
                // back to it, not a new push: the return stack below it
                // already describes how to leave it again.
                self.view = v;
                self.overlay = Overlay::None;
            }
            Surface::Panel(id) => self.overlay = Overlay::Panel(id),
            Surface::Transient(modal) => self.overlay = Overlay::Transient(modal),
        }
        self.active()
    }
}

/// The retained state of one panel. Deliberately minimal: the fields every
/// browse surface previously reset on open (`modal_index`, scroll, follow).
/// Surfaces whose data must refresh on reopen keep that data on `App` and use
/// an explicit refresh-on-show side effect in `enter_panel` — retention is
/// about *where the user was standing*, never about serving stale data.
#[derive(Debug, Clone)]
pub(crate) struct PanelState {
    /// Selection cursor (`App::modal_index` while this panel is focused).
    pub(crate) index: usize,
    /// Body scroll offset (`App::help_scroll` / `activity_scroll` / …).
    pub(crate) scroll: usize,
    /// Whether the body scroll follows the selection.
    pub(crate) follow: bool,
    /// The composer draft this panel parked when it borrowed the input line
    /// (ADR-0139 per-panel drafts). Only the draft-owning panels use it
    /// (Models, Connections, HistorySearch): parking stores the composer's
    /// text in the *entering* panel's slot, restoring on return, so two
    /// borrowed-line flows can never clobber each other's draft through the
    /// old single global `stashed_input` slot.
    pub(crate) draft: Option<String>,
    /// Text owned by a panel while it borrows the composer as a filter. It is
    /// distinct from `draft`: `draft` is the user's parked chat text, while
    /// `query` is the Models/Connections/History search text to restore when
    /// that panel is focused again.
    pub(crate) query: String,
    /// Whether the panel's query sub-layer is active. An empty query can
    /// still be an intentional focused search field, so this cannot be
    /// inferred from `query`.
    pub(crate) query_active: bool,
}

impl Default for PanelState {
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

/// A MRU-ordered registry of retained panel states (ADR-0139 lifecycle,
/// restated for panels by ADR-0141).
///
/// `open` initialises state exactly once per panel; subsequent opens are
/// pure focus moves that restore the retained scroll/index — the "leave and
/// come back, nothing lost" contract sessions already have. `hide` keeps
/// both state and MRU membership; `close` forgets both outright.
///
/// Full-screen views are deliberately *not* registered: their retained
/// fields (`host_scroll`, `host_focus`, `config_*`, …) already persist on
/// `App` for the app's lifetime, so the registry would be a no-op for them.
#[derive(Debug, Default)]
pub(crate) struct PanelRegistry {
    /// Most-recent-first open order. Drives the quick switcher's MRU list.
    order: Vec<PanelId>,
    /// Retained per-panel state. Entries persist across hide (ADR-0139); an
    /// entry is removed only by an explicit `close`.
    states: HashMap<PanelId, PanelState>,
}

impl PanelRegistry {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Focus a panel: move it to the front of the MRU order, initialising
    /// its state on first open. Returns the retained state to restore
    /// (cursor, scroll, follow) — `None` on the very first open so the
    /// caller can run one-time UI initialization. Authoritative data
    /// refresh is a separate every-show policy.
    pub(crate) fn open(&mut self, id: PanelId) -> Option<PanelState> {
        self.order.retain(|&v| v != id);
        self.order.insert(0, id);
        match self.states.entry(id) {
            std::collections::hash_map::Entry::Occupied(e) => Some(e.get().clone()),
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(PanelState::default());
                None
            }
        }
    }

    /// Record the panel's current state when it loses focus (hide or
    /// switch). Idempotent for not-yet-open panels.
    pub(crate) fn save(&mut self, id: PanelId, state: PanelState) {
        self.states.insert(id, state);
    }

    /// Hide preserves both state and recency. MRU describes recently used
    /// buffers, not which one happens to be painted right now.
    pub(crate) fn hide(&mut self, _id: PanelId) {}

    /// Explicit close: forget the panel's state and MRU membership
    /// entirely. The switcher's `Del` gesture invokes this without touching
    /// backend data.
    pub(crate) fn close(&mut self, id: PanelId) {
        self.order.retain(|&v| v != id);
        self.states.remove(&id);
    }

    /// Forget every retained state (session switch: the scroll position
    /// belonged to the previous conversation's context).
    pub(crate) fn close_all(&mut self) {
        self.order.clear();
        self.states.clear();
    }

    /// Whether the panel has been opened at least once (state retained).
    /// Currently exercised by tests and reserved for the switcher's UI
    /// badges (the badges read `order()` today).
    #[allow(dead_code)] // production reads `order`; tests assert exact lifetime
    pub(crate) fn is_open(&self, id: PanelId) -> bool {
        self.states.contains_key(&id)
    }

    /// The retained state of a panel, if it has been opened before.
    pub(crate) fn states(&self, id: &PanelId) -> Option<&PanelState> {
        self.states.get(id)
    }

    /// Mutable access to a panel's retained state, if it exists.
    pub(crate) fn states_mut(&mut self, id: &PanelId) -> Option<&mut PanelState> {
        self.states.get_mut(id)
    }

    /// The MRU order of open panels, most recent first.
    pub(crate) fn order(&self) -> &[PanelId] {
        &self.order
    }

    /// Quick-switcher rows (ADR-0139/0141): switchable views first —
    /// `Session` is the home you are never more than an Esc away from and
    /// `Runner`/`Side` are session-scoped contexts entered from the
    /// transcript, so none of the three are switcher rows — then open
    /// panels in MRU order, then every other panel in display order (the
    /// not-yet-opened ones are still listed so the switcher doubles as
    /// discovery).
    pub(crate) fn switcher_rows(&self) -> Vec<SwitcherTarget> {
        let mut rows: Vec<SwitcherTarget> = [View::Dashboard, View::Settings]
            .into_iter()
            .map(SwitcherTarget::View)
            .collect();
        for id in self.order.clone() {
            rows.push(SwitcherTarget::Panel(id));
        }
        for id in PanelId::ALL {
            if !self.order.contains(&id) {
                rows.push(SwitcherTarget::Panel(id));
            }
        }
        rows
    }

    /// The switcher's visible rows for a live query: the view / MRU /
    /// discovery row set filtered by a fuzzy match of `query` against each
    /// row's label and hint (case-insensitive subsequence). An empty query
    /// is the unfiltered list.
    pub(crate) fn switcher_rows_filtered(&self, query: &str) -> Vec<SwitcherTarget> {
        let rows = self.switcher_rows();
        if query.trim().is_empty() {
            return rows;
        }
        let q = query.trim();
        rows.into_iter()
            .filter(|target| {
                let label = target.label();
                let hint = target.hint();
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
        let mut reg = PanelRegistry::new();
        assert!(reg.open(PanelId::Help).is_none(), "first open has no state");
        // A second open (reopen after hide) returns the saved state.
        reg.save(
            PanelId::Help,
            PanelState {
                index: 3,
                scroll: 12,
                follow: false,
                draft: None,
                query: String::new(),
                query_active: false,
            },
        );
        let restored = reg.open(PanelId::Help).expect("state retained");
        assert_eq!(
            (restored.index, restored.scroll, restored.follow),
            (3, 12, false)
        );
    }

    #[test]
    fn hide_retains_state_and_mru() {
        let mut reg = PanelRegistry::new();
        reg.open(PanelId::Help);
        reg.save(
            PanelId::Help,
            PanelState {
                index: 7,
                ..Default::default()
            },
        );
        reg.hide(PanelId::Help);
        assert!(reg.is_open(PanelId::Help));
        assert_eq!(reg.order(), &[PanelId::Help]);
        let restored = reg.open(PanelId::Help).expect("reopen restores");
        assert_eq!(restored.index, 7);
    }

    #[test]
    fn close_forgets_state() {
        let mut reg = PanelRegistry::new();
        reg.open(PanelId::UsageStats);
        reg.close(PanelId::UsageStats);
        assert!(reg.open(PanelId::UsageStats).is_none(), "close forgets");
    }

    #[test]
    fn mru_order_tracks_focusing() {
        let mut reg = PanelRegistry::new();
        reg.open(PanelId::Help);
        reg.open(PanelId::Tools);
        reg.open(PanelId::Help);
        assert_eq!(reg.order(), &[PanelId::Help, PanelId::Tools]);
        reg.hide(PanelId::Help);
        assert_eq!(reg.order(), &[PanelId::Help, PanelId::Tools]);
    }

    #[test]
    fn switcher_rows_views_first_then_panels_mru_then_rest() {
        let mut reg = PanelRegistry::new();
        reg.open(PanelId::Skills);
        reg.open(PanelId::Btw);
        let rows = reg.switcher_rows();
        assert_eq!(
            &rows[..2],
            &[
                SwitcherTarget::View(View::Dashboard),
                SwitcherTarget::View(View::Settings)
            ],
            "full-screen views first"
        );
        assert_eq!(
            &rows[2..4],
            &[
                SwitcherTarget::Panel(PanelId::Btw),
                SwitcherTarget::Panel(PanelId::Skills)
            ],
            "MRU panels next"
        );
        // Every panel is listed exactly once (discovery of un-opened ones).
        assert_eq!(rows.len(), 2 + PanelId::ALL.len());
        for id in PanelId::ALL {
            assert_eq!(
                rows.iter()
                    .filter(|&&r| r == SwitcherTarget::Panel(id))
                    .count(),
                1
            );
        }
    }

    #[test]
    fn router_preserves_todos_and_activity_identity() {
        assert_eq!(PanelId::Todos.modal(), Modal::Activity);
        assert_eq!(PanelId::Activity.modal(), Modal::Activity);
        let mut router = SurfaceRouter::with_panel(PanelId::Todos);
        assert_eq!(router.modal(), Modal::Activity);
        assert_eq!(router.active_panel(), Some(PanelId::Todos));
        assert_eq!(router.active_view(), View::Session);
        router.show_panel(PanelId::Activity);
        assert_eq!(router.active_panel(), Some(PanelId::Activity));
    }

    #[test]
    fn transient_stack_restores_exact_panel() {
        let mut router = SurfaceRouter::with_panel(PanelId::Todos);
        router.push_transient(Modal::ViewSwitcher);
        router.push_transient(Modal::Question);
        assert_eq!(router.pop_transient().modal(), Modal::ViewSwitcher);
        assert_eq!(router.pop_transient().panel(), Some(PanelId::Todos));
        // The panel is restored as the active surface.
        assert_eq!(router.active(), Surface::Panel(PanelId::Todos));
        // An unbalanced pop falls back to the session view.
        assert_eq!(router.active(), Surface::Panel(PanelId::Todos));
        router.hide_panel();
        let drained = router.pop_transient();
        assert_eq!(drained, Surface::View(View::Session));
        assert_eq!(router.active(), Surface::View(View::Session));
    }

    #[test]
    fn hiding_a_panel_reveals_the_view_beneath() {
        let mut router = SurfaceRouter::new();
        router.show_view(View::Runner);
        router.show_panel(PanelId::Models);
        assert_eq!(
            router.active_view(),
            View::Runner,
            "view survives the panel"
        );
        router.hide_panel();
        assert_eq!(router.active_view(), View::Runner);
        assert_eq!(router.active_panel(), None);
    }

    #[test]
    fn destination_over_scoped_view_returns_to_it() {
        let mut router = SurfaceRouter::new();
        router.show_view(View::Runner);
        router.show_view(View::Dashboard);
        assert_eq!(router.active_view(), View::Dashboard);
        assert_eq!(
            router.back_view(),
            View::Runner,
            "dashboard hides to the zoom"
        );
        assert_eq!(router.back_view(), View::Session, "zoom drains to home");
    }

    #[test]
    fn scoped_views_project_to_no_modal() {
        assert_eq!(View::Session.modal(), Modal::None);
        assert_eq!(View::Runner.modal(), Modal::None);
        assert_eq!(View::Side.modal(), Modal::None);
        assert_eq!(View::Dashboard.modal(), Modal::Host);
        assert_eq!(View::Settings.modal(), Modal::Config);
    }

    #[test]
    fn session_reset_discards_everything() {
        let mut router = SurfaceRouter::new();
        router.show_view(View::Side);
        router.show_view(View::Dashboard);
        router.push_transient(Modal::Question);
        router.show_session_view();
        assert_eq!(router.active(), Surface::View(View::Session));
        assert_eq!(router.back_view(), View::Session, "no return frames remain");
    }
}
