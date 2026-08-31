//! Modal/panel/surface navigation: the transient stack, sub-layer pop, chrome application, scroll/follow state, and panel lifecycle.

use super::*;

impl App {
    pub(crate) fn active_modal(&self) -> Modal {
        self.surfaces.modal()
    }

    /// Exact identity of the focused retained panel (ADR-0141: a retained
    /// modal). This deliberately cannot be reconstructed from
    /// [`Self::active_modal`] because Activity and Todos share the same
    /// modal presentation.
    pub(crate) fn active_panel(&self) -> Option<crate::surfaces::PanelId> {
        self.surfaces.active_panel()
    }

    /// The full-screen view the user stands in (ADR-0141) — the terminal is
    /// this view. Single source of truth behind `in_runner_view()` /
    /// `in_side_view()`, replacing the bare `focus_stack` emptiness check
    /// and the bare `in_side_view` boolean.
    pub(crate) fn current_view(&self) -> crate::surfaces::View {
        self.surfaces.active_view()
    }

    /// Navigate to a full-screen view, remembering a scoped view
    /// (`Runner`/`Side`) so closing the destination returns to it.
    pub(crate) fn show_view_surface(&mut self, view: crate::surfaces::View) {
        self.surfaces.show_view(view);
    }

    /// Hard reset to the session view (home): drop the overlay, the
    /// transient chain, and any view-return frames. Session switches and
    /// queue exits use this.
    pub(crate) fn show_chat_surface(&mut self) {
        self.surfaces.show_session_view();
    }

    pub(crate) fn replace_transient_surface(&mut self, modal: Modal) {
        if modal == Modal::None {
            self.show_chat_surface();
        } else {
            self.surfaces.replace_transient(modal);
        }
    }

    /// Push a transient over the current surface, preserving the exact
    /// parent identity and its retained cursor/scroll before the child
    /// borrows shared presentation fields.
    pub(crate) fn push_transient_surface(&mut self, modal: Modal) {
        if let Some(id) = self.active_panel() {
            self.save_panel_state(id);
        }
        self.surfaces.push_transient(modal);
    }

    /// Pop one transient and restore the parent panel's live projection.
    pub(crate) fn pop_transient_surface(&mut self) -> Modal {
        let restored = self.surfaces.pop_transient();
        if let Some(id) = restored.panel() {
            self.restore_panel_state(id);
        }
        restored.modal()
    }

    pub(crate) fn transient_return_modal(&self) -> Modal {
        self.surfaces
            .return_surface()
            .map_or(Modal::None, crate::surfaces::Surface::modal)
    }

    pub(crate) fn transient_return_panel(&self) -> Option<crate::surfaces::PanelId> {
        self.surfaces
            .return_surface()
            .and_then(crate::surfaces::Surface::panel)
    }

    pub(crate) fn can_open_view_switcher(&self) -> bool {
        self.can_accept_navigation_signal() && !self.modal_keymap_open
    }

    /// Whether asynchronous presentation intent may replace the foreground.
    /// Data snapshots are always safe to apply, but navigation waits while a
    /// transient transaction or a parent-owned drill-in has control.
    pub(crate) fn can_accept_navigation_signal(&self) -> bool {
        use crate::surfaces::{Surface, View};
        let root_surface = !matches!(self.surfaces.active(), Surface::Transient(_));
        let active_panel = self.active_panel();
        let view = self.current_view();
        root_surface
            && !(view == View::Dashboard && (self.host_prompting || self.host_preview.is_some()))
            && !(active_panel == Some(crate::surfaces::PanelId::Sessions)
                && self.session_info_detail)
            && !(active_panel == Some(crate::surfaces::PanelId::Telemetry)
                && (self.telemetry_detail || self.telemetry_turn.is_some()))
            && !(view == View::Settings
                && (self.config_custom_editing
                    || self.config_dropdown.is_some()
                    || self.config_focus == crate::overlays::ConfigFocus::Detail))
    }

    #[cfg(test)]
    pub(crate) fn set_active_modal_for_test(&mut self, modal: Modal) {
        use crate::surfaces::{PanelId, View};
        let panel = match modal {
            Modal::Help => Some(PanelId::Help),
            Modal::Activity => Some(if self.activity_tab == ActivityTab::Todos {
                PanelId::Todos
            } else {
                PanelId::Activity
            }),
            Modal::Tools => Some(PanelId::Tools),
            Modal::Mcp => Some(PanelId::Mcp),
            Modal::Skills => Some(PanelId::Skills),
            Modal::Permissions => Some(PanelId::Permissions),
            Modal::UsageStats => Some(PanelId::UsageStats),
            Modal::Telemetry => Some(PanelId::Telemetry),
            Modal::Btw => Some(PanelId::Btw),
            Modal::Config => {
                self.surfaces.show_view(View::Settings);
                return;
            }
            Modal::Models => Some(PanelId::Models),
            Modal::Connections => Some(PanelId::Connections),
            Modal::HistorySearch => Some(PanelId::HistorySearch),
            Modal::Queue => Some(PanelId::Queue),
            Modal::Host => {
                self.surfaces.show_view(View::Dashboard);
                return;
            }
            Modal::Sessions => Some(PanelId::Sessions),
            Modal::Tree => Some(PanelId::Tree),
            _ => None,
        };
        if let Some(id) = panel {
            self.panels.open(id);
            self.surfaces.show_panel(id);
        } else if modal == Modal::None {
            self.surfaces.show_session_view();
        } else {
            self.surfaces.show_transient(modal);
        }
    }

    /// The modal body's scroll offset and (optional) follow-flag that a
    /// `Scroll*` action should mutate, keyed off [`App::active_modal`].
    ///
    /// This is the single source of truth that the `ScrollUp` / `ScrollDown` /
    /// `ScrollPageUp` / `ScrollPageDown` / `ScrollTop` / `ScrollBottom` actions
    /// consult: every scrollable modal resolves to `Some((&mut scroll,
    /// follow_flag))`, so a key press advances the right field without a
    /// per-modal `if/else` chain duplicated across six action arms.
    ///
    /// The follow flag (`Some` only for list-style modals that auto-follow the
    /// ↑/↓ selection) is cleared on any manual scroll so the user can browse a
    /// long list freely until they navigate again — mirroring the established
    /// per-modal behaviour. Returns `None` for modals that don't scroll their
    /// own body (the inline permission sheet drives `permission_scroll` via a
    /// separate action, and the caret-owning text editors have no body scroll).
    pub(crate) fn modal_scroll_field(&mut self) -> Option<(&mut usize, Option<&mut bool>)> {
        let modal = self.active_modal();
        match modal {
            Modal::Help => Some((&mut self.help_scroll, None)),
            Modal::Activity => Some((&mut self.activity_scroll, None)),
            Modal::Permissions => Some((&mut self.permissions_scroll, None)),
            Modal::Config => match self.config_focus {
                crate::overlays::ConfigFocus::Categories => Some((&mut self.config_scroll, None)),
                crate::overlays::ConfigFocus::Detail => {
                    Some((&mut self.config_detail_scroll, None))
                }
            },
            Modal::Telemetry => Some((&mut self.telemetry_scroll, None)),
            Modal::UsageStats => Some((&mut self.usage_stats_scroll, None)),
            Modal::OauthPending => Some((&mut self.oauth_scroll, None)),
            Modal::ProviderPreset => Some((&mut self.preset_scroll, None)),
            Modal::CustomProvider => Some((&mut self.custom_scroll, None)),
            // List-style modals: clear the follow flag so manual scroll wins.
            Modal::Tools | Modal::Mcp | Modal::Skills | Modal::Sessions => Some((
                &mut self.session_scroll,
                Some(&mut self.session_modal_follow),
            )),
            // The dashboard routes body-scroll to the deepest open layer:
            // the session preview when present, else the focused pane (dock
            // selection-scroll or the console read-out scroll).
            Modal::Host => {
                if self.host_preview.is_some() {
                    Some((&mut self.host_preview_scroll, None))
                } else {
                    match self.host_focus {
                        crate::overlays::DashboardFocus::List => {
                            Some((&mut self.host_scroll, Some(&mut self.host_modal_follow)))
                        }
                        crate::overlays::DashboardFocus::Detail => {
                            Some((&mut self.host_detail_scroll, None))
                        }
                    }
                }
            }
            Modal::Queue => Some((&mut self.queue_scroll, Some(&mut self.queue_modal_follow))),
            Modal::Btw => Some((&mut self.btw_scroll, Some(&mut self.btw_modal_follow))),
            Modal::HistorySearch => Some((
                &mut self.history_scroll,
                Some(&mut self.history_modal_follow),
            )),
            Modal::Connections | Modal::Models => {
                Some((&mut self.model_scroll, Some(&mut self.model_modal_follow)))
            }
            Modal::Question => Some((
                &mut self.question_scroll,
                Some(&mut self.question_modal_follow),
            )),
            Modal::Tree => Some((&mut self.tree_scroll, Some(&mut self.tree_modal_follow))),
            // Permission drives its own body via PermissionDetailsUp/Down (and
            // the transcript behind it scrolls when no step is focused); the
            // caret-owning text editors have no body scroll. None => the
            // Scroll* action falls through to the transcript fallback.
            Modal::None | Modal::Permission | Modal::ModelEditor | Modal::InputInjection => None,
            // The quick switcher scrolls its own list through the shared
            // session slot, like the other compact list modals.
            Modal::ViewSwitcher => Some((
                &mut self.session_scroll,
                Some(&mut self.session_modal_follow),
            )),
        }
    }

    /// Reset every piece of composer navigation state that is **scoped to the
    /// viewed session** — the ↑/↓ history cursor and the per-session draft
    /// stash — when the viewed session changes (`/new`, `/session open`,
    /// `/resume`, `/fork`, entering/leaving a `/btw` aside).
    ///
    /// These slots belong to *a conversation's* composer, not the terminal:
    /// carrying a cursor over a session boundary would make the first `↑` in
    /// the new session land on a position clamped against the *old* session's
    /// row count, and a restored draft would leak what the user was typing
    /// into the previous conversation. The composer itself is emptied the
    /// same way the send path empties it, so the new session starts from a
    /// clean slate.
    pub fn on_viewed_session_changed(&mut self) {
        self.history_index = None;
        self.clear_history_draft();
        // Retained view state (ADR-0139) belongs to the conversation being
        // left — a scroll position into Tools/Skills rows or a report page
        // is context about *that* session's data. Forgetting it here is the
        // `close` verb applied wholesale.
        if let Some(sid) = self.queue_exit_session.take() {
            self.resume_queue(&sid);
        }
        self.surfaces.show_session_view();
        self.panels.close_all();
        for id in crate::surfaces::PanelId::ALL {
            self.reset_view_payload(id);
        }
        self.session_context = None;
        self.view_switcher_query.clear();
        // An armed Esc confirmation targets the conversation being left;
        // carrying it across the boundary could fire session A's interrupt
        // against session B. Disarm so the next Esc starts fresh.
        self.esc_armed_until = None;
        // The queue pointer is scoped like the history cursor: its target
        // belongs to the conversation being left, so a carried pointer would
        // dangle into the new session's outbox. Dissolve without restoring
        // (the composer is emptied right below anyway).
        self.queue_pointer = None;
        self.queue_pointer_draft.clear();
        self.queue_pointer_draft_images.clear();
        self.queue_pointer_draft_text_pastes.clear();
        self.input.clear();
        self.pending_images.clear();
        self.pending_text_pastes.clear();
        self.cursor_position = 0;
        self.input_scroll = 0;
        self.input_drag_scroll = None;
        self.suggestion_index = None;
        self.completion_dismissed = true;
        // The backfill belongs to the conversation being left; the next
        // session rebuilds its own from its transcript.
        self.session_history_backfill.clear();
        self.session_history_backfill_cursor = 0;
    }

    /// Focus a browse panel under the ADR-0139/0141 lifecycle. State is
    /// initialized once and restored on later shows. The return value
    /// reports first show for UI defaults only; `enter_panel` refreshes
    /// authoritative data on every show.
    pub(crate) fn open_panel(&mut self, id: crate::surfaces::PanelId) -> bool {
        if let Some(current) = self.active_panel()
            && current != id
        {
            self.deactivate_panel(current);
        }
        let first = self.panels.open(id).is_none();
        self.surfaces.show_panel(id);
        self.restore_panel_state(id);
        self.modal_keymap_open = false;
        if id == crate::surfaces::PanelId::Todos {
            self.activity_tab = crate::modal::ActivityTab::Todos;
        } else if id == crate::surfaces::PanelId::Activity {
            self.activity_tab = crate::modal::ActivityTab::Activity;
        }
        first
    }

    /// Persist current TUI presentation preferences into `$XDG_CONFIG_HOME/mutx/config.toml`.
    pub fn save_tui_config(&self) {
        let mut cfg = crate::config::TuiConfig::load();
        cfg.color_scheme = self.color_scheme.clone();
        cfg.custom_color_scheme = self.custom_color_scheme.clone();
        cfg.click_outside_dismiss = self.click_outside_dismiss;
        cfg.expand_auto_scroll = self.expand_auto_scroll;
        cfg.transcript_layout = self.transcript_layout.as_str().to_string();
        let _ = cfg.save();
    }

    /// Snapshot the *current* field values of a browse view into the
    /// registry — the "save on losing focus" half of the contract. The
    /// inverse of the restore in [`Self::open_panel`].
    pub(crate) fn save_panel_state(&mut self, id: crate::surfaces::PanelId) {
        let scroll = self.panel_scroll(id);
        let follow = self.panel_follow(id);
        let draft = self.panels.states(&id).and_then(|s| s.draft.clone());
        let query = if self.owns_composer_draft(id) {
            self.input.clone()
        } else {
            self.panels
                .states(&id)
                .map(|state| state.query.clone())
                .unwrap_or_default()
        };
        let query_active = match id {
            crate::surfaces::PanelId::Models | crate::surfaces::PanelId::Connections => {
                self.model_search
            }
            crate::surfaces::PanelId::HistorySearch => self.history_search,
            _ => false,
        };
        self.panels.save(
            id,
            crate::surfaces::PanelState {
                index: self.modal_index,
                scroll,
                follow,
                draft,
                query,
                query_active,
            },
        );
    }

    /// Restore the live fields projected by a retained view. Draft-owning
    /// views first park the chat composer, then load their own retained query.
    fn restore_panel_state(&mut self, id: crate::surfaces::PanelId) {
        let state = self.panels.states(&id).cloned().unwrap_or_default();
        self.modal_index = state.index;
        self.apply_panel_scroll(id, state.scroll);
        self.apply_panel_follow(id, state.follow);
        if self.owns_composer_draft(id) {
            if state.draft.is_none() {
                self.park_draft_into(id);
            }
            self.input = state.query;
            self.set_cursor_end();
            self.input_scroll = 0;
            self.input_drag_scroll = None;
            self.suggestion_index = None;
            match id {
                crate::surfaces::PanelId::Models | crate::surfaces::PanelId::Connections => {
                    self.model_search = state.query_active;
                }
                crate::surfaces::PanelId::HistorySearch => {
                    self.history_search = state.query_active;
                }
                _ => {}
            }
        }
    }

    /// Exit hook for a full-screen view (ADR-0141). Mirrors
    /// [`Self::deactivate_panel`]: hide, switch, and close all route here.
    /// Public wrapper for the event loop's `enter_view` transaction.
    pub(crate) fn leave_view_for_navigation(&mut self, view: crate::surfaces::View) {
        self.deactivate_view(view)
    }

    fn deactivate_view(&mut self, view: crate::surfaces::View) {
        match view {
            crate::surfaces::View::Dashboard => {
                self.host_prompting = false;
                self.host_prompt_new = false;
                self.host_preview = None;
                self.host_preview_scroll = 0;
            }
            crate::surfaces::View::Settings => {
                self.config_dropdown = None;
                if self.config_custom_editing {
                    self.theme =
                        Theme::from_color_scheme(&self.color_scheme, &self.custom_color_scheme);
                    self.custom_color_draft = self.custom_color_scheme.clone();
                    self.config_custom_editing = false;
                }
            }
            crate::surfaces::View::Session
            | crate::surfaces::View::Runner
            | crate::surfaces::View::Side => {}
        }
    }

    /// Run the exit hook for one exact panel without choosing the next
    /// surface. Both hide and switch use this path.
    pub(super) fn deactivate_panel(&mut self, id: crate::surfaces::PanelId) {
        self.save_panel_state(id);
        if self.owns_composer_draft(id) {
            self.restore_draft_from(id);
            if id == crate::surfaces::PanelId::HistorySearch {
                self.history_search = false;
                self.history_preview = false;
                self.history_clear_confirm = false;
            } else {
                self.model_search = false;
            }
        }
        if id == crate::surfaces::PanelId::Sessions {
            self.session_info_detail = false;
            self.session_detail = None;
            self.session_info_scroll = 0;
        }
        if id == crate::surfaces::PanelId::Telemetry {
            self.telemetry_tab = crate::modal::TelemetryTab::Overview;
            self.telemetry_detail = false;
            self.telemetry_turn = None;
            self.telemetry_turn_cursor = 0;
        }
        if id == crate::surfaces::PanelId::Queue
            && let Some(sid) = self.queue_exit_session.take()
        {
            self.resume_queue(&sid);
        }
        self.panels.hide(id);
    }

    /// The `hide` verb (ADR-0139/0141): the active panel loses focus with
    /// its state retained, revealing the full-screen view beneath. Returns
    /// `true` when the active surface *was* a panel or a non-session view
    /// (so callers skip their modal-specific close logic).
    pub(crate) fn hide_active_panel(&mut self) -> bool {
        if let Some(id) = self.active_panel() {
            self.deactivate_panel(id);
            self.surfaces.hide_panel();
            self.modal_keymap_open = false;
            true
        } else if self.current_view() != crate::surfaces::View::Session {
            // Esc from a full-screen destination returns to the scoped view
            // it was opened over (runner/side), else home — not a hard reset,
            // which would drop the zoom/side return frames.
            let leaving = self.current_view();
            self.surfaces.back_view();
            // Leaving Runner/Side via the router must also drop their frame
            // data, or `focus_stack`/`in_side_view` would dangle past the
            // surface that gave them meaning.
            if leaving == crate::surfaces::View::Runner {
                self.focus_stack.clear();
                self.reset_view_state();
            }
            if leaving == crate::surfaces::View::Side {
                self.in_side_view = false;
                self.side_session_id = None;
                self.reset_view_state();
            }
            self.deactivate_view(leaving);
            self.modal_keymap_open = false;
            true
        } else {
            false
        }
    }

    /// Explicitly close a retained view, dropping both its navigation state
    /// and its view-owned volatile UI payload. Closing the focused view first
    /// runs the same exit hook as a switch/hide.
    pub(crate) fn close_panel(&mut self, id: crate::surfaces::PanelId) {
        if self.active_panel() == Some(id) {
            self.deactivate_panel(id);
            // Reveal the full-screen view the panel floated over (ADR-0141)
            // — closing a panel never discards the zoom/side context under it.
            self.surfaces.hide_panel();
        }
        self.panels.close(id);
        self.reset_view_payload(id);
        self.modal_keymap_open = false;
    }

    fn reset_view_payload(&mut self, id: crate::surfaces::PanelId) {
        use crate::surfaces::PanelId;
        match id {
            PanelId::Help => self.help_scroll = 0,
            PanelId::Activity | PanelId::Todos => self.activity_scroll = 0,
            PanelId::Tools | PanelId::Mcp => {
                self.session_scroll = 0;
                self.session_modal_follow = true;
            }
            PanelId::Skills => {
                self.session_scroll = 0;
                self.session_modal_follow = true;
                self.skills_expanded = None;
            }
            PanelId::Permissions => self.permissions_scroll = 0,
            PanelId::UsageStats => {
                self.usage_stats = None;
                self.usage_stats_scroll = 0;
            }
            PanelId::Telemetry => {
                self.token_report = None;
                self.telemetry_tab = crate::modal::TelemetryTab::Overview;
                self.telemetry_scroll = 0;
                self.telemetry_detail = false;
                self.telemetry_turn = None;
                self.telemetry_turn_cursor = 0;
            }
            PanelId::Btw => {
                self.btw_list.clear();
                self.btw_scroll = 0;
                self.btw_modal_follow = true;
            }
            PanelId::Models | PanelId::Connections => {
                self.model_search = false;
                self.model_scroll = 0;
                self.model_modal_follow = true;
            }
            PanelId::HistorySearch => {
                self.history_search = false;
                self.history_preview = false;
                self.history_clear_confirm = false;
            }
            PanelId::Queue => {
                self.queue_scroll = 0;
                self.queue_modal_follow = true;
            }
            PanelId::Sessions => {
                self.sessions_overview.clear();
                self.session_info_detail = false;
                self.session_detail = None;
                self.session_info_scroll = 0;
            }
            PanelId::Tree => {
                self.session_tree = muta_contracts::SessionTree::default();
                self.tree_scroll = 0;
                self.tree_modal_follow = true;
            }
        }
    }

    /// Pop the deepest sub-layer of a view (ADR-0139): the single
    /// "one step back" every drill-in routes through — Esc's deepest-first
    /// chain and the outside-click mirror both call this, so the two can
    /// never drift. Returns `true` when a sub-layer was open (the caller
    /// stops: the view itself stays up).
    pub(crate) fn pop_sublayer(&mut self) -> bool {
        match self.active_modal() {
            Modal::Config if self.config_dropdown.is_some() => {
                self.config_dropdown = None;
                true
            }
            Modal::Config if self.config_custom_editing => {
                self.config_custom_editing = false;
                self.theme =
                    Theme::from_color_scheme(&self.color_scheme, &self.custom_color_scheme);
                self.custom_color_draft = self.custom_color_scheme.clone();
                self.input.clear();
                self.set_cursor(0);
                true
            }
            Modal::Config if self.config_focus == crate::overlays::ConfigFocus::Detail => {
                self.config_focus = crate::overlays::ConfigFocus::Categories;
                true
            }
            // Preview is the deepest dashboard layer (painted over the
            // prompting state; the original deepest-first chain popped it
            // first — a preview open while prompting is unreachable in
            // practice, but the order stays explicit here).
            Modal::Host if self.host_preview.is_some() => {
                self.host_preview = None;
                self.host_preview_scroll = 0;
                true
            }
            Modal::Host if self.host_prompting => {
                self.host_prompting = false;
                self.host_prompt_new = false;
                self.input.clear();
                self.set_cursor(0);
                true
            }
            Modal::Telemetry if self.telemetry_turn.is_some() => {
                // Deepest-first: pop the attempt inspector back to the round
                // detail before leaving the round itself.
                self.telemetry_turn = None;
                self.telemetry_scroll = 0;
                true
            }
            Modal::Telemetry if self.telemetry_detail => {
                self.telemetry_detail = false;
                self.telemetry_turn_cursor = 0;
                self.telemetry_scroll = 0;
                true
            }
            Modal::Sessions if self.session_info_detail => {
                self.session_info_detail = false;
                self.session_detail = None;
                self.session_info_scroll = 0;
                true
            }
            _ => false,
        }
    }

    /// The dispatcher-facing dismiss verb (ADR-0139): what Esc /
    /// outside-click / Ctrl+C do to whatever surface is up. The quick
    /// switcher cancels back to the surface it was opened over (it is a
    /// transient chooser, never a view) — and restores that surface's
    /// cursor/scroll from the registry, because the switcher borrowed
    /// `modal_index` and the shared session-scroll slot while it was up.
    /// A retained browse view hides with its state saved.
    /// Returns `true` when either applied, so legacy close paths can skip
    /// their own handling.
    pub(crate) fn dismiss_surface(&mut self) -> bool {
        if self.active_modal() == Modal::ViewSwitcher {
            self.pop_transient_surface();
            self.modal_keymap_open = false;
            return true;
        }
        self.hide_active_panel()
    }

    /// The per-view body-scroll slot, mirroring [`Self::modal_scroll_field`]
    /// for the retained views. Tools/Mcp/Skills share `session_scroll`
    /// exactly as `modal_scroll_field` already routes them.
    ///
    /// Full-screen views are excluded entirely (ADR-0141): their retained
    /// fields (`host_scroll`, `config_*`, …) already persist on `App` for
    /// the app's lifetime, so registry save/restore would be a no-op for
    /// them.
    fn panel_scroll(&self, id: crate::surfaces::PanelId) -> usize {
        match id {
            crate::surfaces::PanelId::Help => self.help_scroll,
            crate::surfaces::PanelId::Activity | crate::surfaces::PanelId::Todos => {
                self.activity_scroll
            }
            crate::surfaces::PanelId::Tools
            | crate::surfaces::PanelId::Mcp
            | crate::surfaces::PanelId::Skills => self.session_scroll,
            crate::surfaces::PanelId::Permissions => self.permissions_scroll,
            crate::surfaces::PanelId::UsageStats => self.usage_stats_scroll,
            crate::surfaces::PanelId::Telemetry => self.telemetry_scroll,
            crate::surfaces::PanelId::Btw => self.btw_scroll,
            crate::surfaces::PanelId::HistorySearch => self.history_scroll,
            crate::surfaces::PanelId::Models | crate::surfaces::PanelId::Connections => {
                self.model_scroll
            }
            crate::surfaces::PanelId::Queue => self.queue_scroll,
            crate::surfaces::PanelId::Sessions => self.session_scroll,
            crate::surfaces::PanelId::Tree => self.tree_scroll,
        }
    }

    fn apply_panel_scroll(&mut self, id: crate::surfaces::PanelId, scroll: usize) {
        match id {
            crate::surfaces::PanelId::Help => self.help_scroll = scroll,
            crate::surfaces::PanelId::Activity | crate::surfaces::PanelId::Todos => {
                self.activity_scroll = scroll;
            }
            crate::surfaces::PanelId::Tools
            | crate::surfaces::PanelId::Mcp
            | crate::surfaces::PanelId::Skills => {
                self.session_scroll = scroll;
            }
            crate::surfaces::PanelId::Permissions => self.permissions_scroll = scroll,
            crate::surfaces::PanelId::UsageStats => self.usage_stats_scroll = scroll,
            crate::surfaces::PanelId::Telemetry => self.telemetry_scroll = scroll,
            crate::surfaces::PanelId::Btw => self.btw_scroll = scroll,
            crate::surfaces::PanelId::HistorySearch => self.history_scroll = scroll,
            crate::surfaces::PanelId::Models | crate::surfaces::PanelId::Connections => {
                self.model_scroll = scroll;
            }
            crate::surfaces::PanelId::Queue => self.queue_scroll = scroll,
            crate::surfaces::PanelId::Sessions => self.session_scroll = scroll,
            crate::surfaces::PanelId::Tree => self.tree_scroll = scroll,
        }
    }

    fn panel_follow(&self, id: crate::surfaces::PanelId) -> bool {
        match id {
            crate::surfaces::PanelId::Tools
            | crate::surfaces::PanelId::Mcp
            | crate::surfaces::PanelId::Skills => self.session_modal_follow,
            crate::surfaces::PanelId::Btw => self.btw_modal_follow,
            crate::surfaces::PanelId::HistorySearch => self.history_modal_follow,
            crate::surfaces::PanelId::Models | crate::surfaces::PanelId::Connections => {
                self.model_modal_follow
            }
            crate::surfaces::PanelId::Queue => self.queue_modal_follow,
            crate::surfaces::PanelId::Sessions => self.session_modal_follow,
            crate::surfaces::PanelId::Tree => self.tree_modal_follow,
            // These surfaces don't track a follow flag (plain scroll bodies).
            _ => true,
        }
    }

    fn apply_panel_follow(&mut self, id: crate::surfaces::PanelId, follow: bool) {
        match id {
            crate::surfaces::PanelId::Tools
            | crate::surfaces::PanelId::Mcp
            | crate::surfaces::PanelId::Skills
            | crate::surfaces::PanelId::Sessions => self.session_modal_follow = follow,
            crate::surfaces::PanelId::Btw => self.btw_modal_follow = follow,
            crate::surfaces::PanelId::HistorySearch => self.history_modal_follow = follow,
            crate::surfaces::PanelId::Models | crate::surfaces::PanelId::Connections => {
                self.model_modal_follow = follow;
            }
            crate::surfaces::PanelId::Queue => self.queue_modal_follow = follow,
            crate::surfaces::PanelId::Tree => self.tree_modal_follow = follow,
            // Plain scroll bodies do not expose a follow flag.
            _ => {}
        }
    }

    /// Reset transient view state (scroll, selection, sticky pinning) when the
    /// focused message slice changes.
    pub(crate) fn reset_view_state(&mut self) {
        self.scroll = 0;
        self.follow_bottom = true;
        self.selection = SelectionState::None;
        self.drag.cancel();
        self.sticky_step = None;
        self.sticky_rect = None;
        self.sticky_summary_line = None;
        self.pin_summary_line = None;
        self.scroll_settle_pending = false;
        self.focused_target = None;
    }

    /// The chrome of whichever session the user is currently viewing: the
    /// focused aside's entry while in the aside view, the primary's
    /// (carried by the legacy `App` fields) otherwise. Renderers must read
    /// activity/round state through this accessor — never the bare fields —
    /// so a view can only ever display its own session's status.
    pub fn viewed_chrome(&self) -> SessionChrome {
        if self.in_side_view
            && let Some(side_id) = self.side_session_id.as_deref()
            && let Some(chrome) = self.session_chrome.get(side_id)
        {
            return chrome.clone();
        }
        SessionChrome {
            phase: self.phase.clone(),
            responding: self.round_started_at.is_some() || self.phase.is_some(),
            round_count: self.round_count,
            current_turn: self.current_turn,
            round_started_at: self.round_started_at,
            can_retry: self.loop_status.is_idle() && self.harness_retry_pending,
            last_turn_performance: self
                .session_chrome
                .get(&self.current_session_id)
                .and_then(|chrome| chrome.last_turn_performance),
        }
    }

    /// Overwrite the display chrome (the `App`-level fields the renderers
    /// read) from a [`SessionChrome`] entry. The single write path for
    /// view swaps; per-event updates during a round go through the
    /// listener's routing instead.
    pub(super) fn apply_chrome(&mut self, chrome: &SessionChrome) {
        self.phase = chrome.phase.clone();
        self.round_started_at = chrome.round_started_at;
        self.round_count = chrome.round_count;
        self.current_turn = chrome.current_turn;
    }
}
