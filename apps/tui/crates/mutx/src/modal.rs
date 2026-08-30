//! Modal identity and surface-recess policy.
//!
//! [`Modal`] is a fieldless discriminant naming *which* overlay presentation
//! is drawn; under ADR-0141 it is a projection of the router's surface (see
//! [`crate::surfaces`), never an identity. It is the seam shared between the
//! render layer (modal geometry via [`crate::primitives::modal_area`],
//! per-modal renderers) and input dispatch. [`Recess`] is the single source
//! of truth for how the live surface recedes behind a modal.

#[derive(PartialEq, Clone, Copy, Debug, Default)]
pub enum Modal {
    #[default]
    None,
    /// Flat model picker (`Ctrl+M` / `/models`) — the daily-driver switch
    /// surface. One selectable row per (provider, model) pair across the whole
    /// snapshot (`App::models_flat_filtered`), grouped into three labeled
    /// sections — **Favorites** (★-marked, ASCII order), **Recent** (usage
    /// history, most-recent-first), **All models** (ASCII order) — with dim
    /// section-label rows the selection cursor skips. Enter activates the
    /// highlighted pair, `e` opens the pair's per-model settings editor
    /// (ADR-0046), and `d` removes the highlighted model when its provider is
    /// user-defined. It mirrors the input-history modal's two-mode design: it
    /// opens in **browse** mode (composer line not borrowed, typing inert) and
    /// `/` drops into a **search** sub-layer that borrows the line as a live
    /// fuzzy query (`App::model_search` distinguishes the two). Esc in search
    /// returns to browse; Esc in browse (or an outside click) closes and
    /// restores the draft.
    Models,
    /// Provider-instance management (`/connections`): the ranked provider list
    /// (`App::providers_filtered`, favorites → last-used → name) with a trailing
    /// "＋ Add connection" row that opens [`Self::ProviderPreset`]. Enter
    /// activates the provider's current model; `*` favorites; `e` edits (built-in
    /// → API-key editor [`Self::ModelEditor`], custom → meta editor
    /// [`Self::CustomProvider`]); `Shift+D` deletes a custom provider behind a
    /// confirm overlay (`App::pending_provider_delete`). Same browse/search
    /// two-mode design as [`Self::Models`]: `/` borrows the composer line as a
    /// fuzzy query, Esc in search returns to browse, Esc in browse (or an
    /// outside click) closes and restores the draft.
    Connections,
    /// Input-history recall (Ctrl+R). A two-mode surface: it opens in **browse**
    /// mode — a plain reverse-chronological list (newest first, top-focused)
    /// where the composer line is not borrowed and typing is inert — and `/`
    /// drops into a **search** sub-layer that borrows the line as a live fuzzy
    /// query (`App::history_search` distinguishes the two). The name is kept for
    /// continuity even though browsing, not searching, is now the default.
    /// Rows come from `App::history_rows`; Enter inserts the focused entry into
    /// the composer for editing (never sends). The first Esc in search returns to
    /// browse, the second (or an outside click) closes and restores the draft.
    HistorySearch,
    Permission,
    Question,
    /// Unified provider editor: edit the API key and model-id
    /// of a catalog entry in one place. Reached via `e` in the Connections or
    /// Models pickers or `Enter` on a no-key model. Replaces the sequential
    /// ApiKey / Endpoint / ModelName modal chain.
    ModelEditor,
    /// Preset chooser: the "Connections / Add connection" child page
    /// of the Connections list. It retains the provider panel footprint and is
    /// reached from the "＋ Add connection" row at the bottom of the
    /// Connections list. `↑/↓` move; `Enter` opens the [`Self::CustomProvider`]
    /// editor seeded from the chosen preset; `Esc` returns to the Connections
    /// list. See `App::preset_choice` and
    /// [`crate::providers::PROVIDER_PRESETS`].
    ProviderPreset,
    /// OAuth-in-progress sheet for SuperGrok. Stays open while browser authorize
    /// + loopback callback run; on success transitions to [`Self::CustomProvider`].
    OauthPending,
    /// Provider editor: a per-preset form (Name, Base URL, Token, and — when
    /// a preset opts in — Model) for defining a user connection without
    /// editing config.toml by hand. The protocol and seeded models come from the
    /// template chosen in [`Self::ProviderPreset`]; `Tab`/`BackTab` cycle the
    /// visible fields, and the focused field borrows the composer line (like
    /// [`Self::ModelEditor`]). `Enter` saves (→ `AgentRequest::AddProvider`) and
    /// activates; `Esc` returns to the Connections list. See `App::custom_field`
    /// and friends.
    CustomProvider,
    Help,
    Sessions,
    /// Presentation of the full-screen dashboard view
    /// ([`View::Dashboard`](crate::surfaces::View::Dashboard), `/dashboard`,
    /// ADR-0096/0141): header, live session list, detail pane, footer
    /// command strip over the whole viewport. Enter attaches to a hosted
    /// session; `i` / `p` / `n` issue control-plane verbs. Data comes from
    /// the monitor stream the TUI maintains client-side.
    Host,
    /// Tools manager modal: a centered, dismissable, selectable list of every
    /// session tool — builtins, `mcp:<server>`, `pursuit`, `plan` — each with a
    /// `Space` toggle to enable/disable it. Opened with the `/tools` slash
    /// command. `App::modal_index` is its selection cursor; data comes from
    /// the session-context snapshot.
    Tools,
    /// MCP manager modal: a centered, dismissable, selectable list of every
    /// configured MCP server with its connection status (connected / disabled /
    /// failed) and tool count. Opened with the `/mcp` slash command. `Space`
    /// toggles a server on/off for the session (connect/disconnect, applied
    /// live without rewriting config.toml); `r` reconnects the selected server.
    /// `App::modal_index` is its selection cursor; data comes from the
    /// session-context snapshot (its `mcp` pane).
    Mcp,
    /// Skills modal: a centered, dismissable, selectable list of every loaded
    /// skill, each with a short hint and its enabled state. Opened with the
    /// `/skills` slash command (intercepted locally, never sent to the
    /// backend). `Enter` toggles a per-row detail expansion (full description,
    /// version, source, tags) tracked in `App::skills_expanded`; `r` reloads
    /// the skill registry by sending `/skills reload` to the backend.
    /// `App::modal_index` is its selection cursor; data comes from the
    /// session-context snapshot (its `skills` pane).
    Skills,
    /// Permissions manager modal: a centered, dismissable overlay listing the
    /// session's cached "always allow" rules with per-row revoke and a
    /// clear-all action. Opened with the `/permissions` slash command. This
    /// is the management surface — distinct from [`Modal::Permission`] (the
    /// inline real-time approval sheet).
    Permissions,
    /// Config manager modal: a centered, dismissable overlay listing the
    /// configurable categories (Appearance). Opened with the
    /// `/config` slash command (intercepted locally, never sent to the
    /// Presentation of the full-screen settings view
    /// ([`View::Settings`](crate::surfaces::View::Settings), `/config` /
    /// `/settings`, ADR-0141): dual-pane configuration center. `Tab`
    /// switches focus between categories and detail; `Esc` closes.
    Config,
    /// Activity overview: the current pursuit (objective + checklist), the live
    /// plan-progress breakdown, and the running round/turn/model/elapsed/
    /// status. Opened by clicking the activity bar. The body scrolls via
    /// `App::activity_scroll`.
    Activity,
    /// Queue overview: the full list of staged outbox messages for the viewed
    /// session, in dispatch order, each with its target modifier, queued time,
    /// and (truncated) text. Opened by clicking the persistent queue bar below
    /// the transcript gap, or with `F2`. `↑` recalls the highlighted item into
    /// the composer for editing; `Esc` closes. The body scrolls via
    /// `App::queue_scroll`.
    Queue,
    /// Session Telemetry modal: unified context usage and request performance
    /// telemetry grouped by user round, with turn drill-down and attempt inspection.
    /// Opened by clicking the context/rate meters in the model bar or via `Ctrl+O`.
    Telemetry,
    /// Usage statistics (`/usage`, ADR-0122): the durable cross-session view
    /// — daily token totals, per-`(provider, model)` breakdown, and the
    /// recent terminal-request event log. Data comes from the
    /// day-partitioned store under `data/usage/` (a sibling of `projects/`),
    /// so it survives session cleanup by design. Fetched on demand via
    /// `AgentRequest::QueryUsageStats`; the body scrolls via
    /// `App::usage_stats_scroll`. Esc / outside-click closes.
    UsageStats,
    /// `/btw` asides list (ADR-0103 §5): one row per live aside conversation,
    /// newest first, with `Enter` = jump back into the aside and `D` =
    /// close-and-discard it. Opened by F5 or `/btw list`. A read-only-style
    /// picker: no text entry, scrolls its body, refreshes in place when the
    /// harness pushes a new list.
    Btw,
    /// Interactive-input injection panel (L3.5 β): shown when a `bash` command
    /// is classified interactive and the agent cannot supply its own input.
    /// Borrows the composer input line (like `Models`/`ModelEditor`) for
    /// free-text entry; masks the typed text when the request is `secret`
    /// (password/passphrase). `Enter` submits (→ `AgentRequest::InputReply`),
    /// `Esc` cancels (→ empty reply → command runs with closed stdin and fails
    /// fast with a non-interactive remedy hint).
    InputInjection,
    /// Session DAG tree viewer (`/tree`).
    Tree,
    /// Global quick switcher (ADR-0139/0141, `Ctrl+L`): a centered picker
    /// over every navigable surface — switchable full-screen views first,
    /// then retained panels, open ones in MRU order, then the rest as
    /// discovery. `Enter` switches (hides the current panel, focus moves,
    /// retained scroll/index restored); `Esc` closes with nothing changed.
    /// Not itself a retained surface: it is a transient chooser over them,
    /// so it stays out of the [`crate::surfaces::PanelRegistry`].
    ViewSwitcher,
}

/// How the live surface recedes while a modal owns the foreground.
///
/// A terminal cannot alpha-blend, so a modal expresses "the background has
/// receded" in one of three ways instead of painting a translucent veil. This
/// is the single source of truth that both the footer-collapse decision
/// (`App`/event loop) and the per-frame recess pass (`paint::recess_backdrop`)
/// consult, so layout and paint can never disagree about what a modal does to
/// the surface beneath it.
#[derive(PartialEq, Eq, Clone, Copy)]
pub enum Recess {
    /// The modal floats on the fully-live surface. No dimming, no occlusion —
    /// used by lightweight overlays that never take over (Question, Permission).
    None,
    /// The surface stays mounted and is darkened in place so the centered modal
    /// reads as the focal layer while context (transcript, input, hint bar,
    /// activity bar) remains visible. The brightness factor comes from
    /// [`Theme::modal_dim_factor`](crate::view::Theme::modal_dim_factor).
    Dim,
    /// Full takeover: the footer collapses to zero height and the surface is
    /// occluded with a solid fill. Reserved for context-switching flows
    /// (session selection) where a clean slate is the intent.
    Takeover,
}

impl Modal {
    /// The recess policy for this modal — the single source of truth that the
    /// footer-collapse flag and the per-frame recess pass both key off.
    pub fn recess(self) -> Recess {
        match self {
            // Float: lightweight overlays that never touch the surface.
            // HistorySearch floats too — its dropdown panel sits above a fully
            // live composer (the composer IS the filter field), so dimming the
            // surface would only darken the very input the user is typing into.
            Modal::None | Modal::Question | Modal::Permission | Modal::HistorySearch => {
                Recess::None
            }
            // Context switch: the surfaces that fully own the screen. The
            // two full-screen views (Host = dashboard, Config = settings —
            // ADR-0141) plus the sessions picker, a context-switch surface.
            Modal::Sessions | Modal::Host | Modal::Config => Recess::Takeover, // Everything else recedes the surface for focus while keeping it
            // visible (transcript, chrome, and all).
            _ => Recess::Dim,
        }
    }

    /// Whether this modal closes when the user clicks outside its rect
    /// (click-outside-to-dismiss). True for the read-only / info overlays
    /// (Help, Session, Sessions, Activity) and for the history
    /// modal and the Connections/Models pickers: their filter query is
    /// ephemeral and the real composer draft is safely parked in
    /// their per-view state, so an outside click closes them and restores the
    /// parked draft (ADR-0139) — exactly like
    /// Esc. Entry modals that
    /// hold precious in-progress input (ModelEditor, Question) and the
    /// permission sheet stay open so an accidental click never discards an API
    /// key or a pending decision.
    ///
    /// This is the single source of truth for *which* modals are
    /// click-dismissable; the event loop records the renderer's actual panel
    /// rect for these modals and leaves every other modal without an
    /// outside-click target.
    pub fn dismissable_by_outside_click(self) -> bool {
        matches!(
            self,
            Modal::Help
                | Modal::Tools
                | Modal::Mcp
                | Modal::Skills
                | Modal::Sessions
                | Modal::Permissions
                | Modal::Activity
                | Modal::Queue
                | Modal::HistorySearch
                | Modal::Models
                | Modal::Connections
                | Modal::Telemetry
                | Modal::UsageStats
                | Modal::Btw
                | Modal::Tree
                | Modal::ViewSwitcher
        )
    }

    /// Whether this modal unconditionally renders its own text caret. Pickers
    /// with browse/search modes are intentionally excluded because their
    /// ownership depends on live state and is resolved by `App::caret_owner`.
    /// Read-only overlays and decision sheets do not own a caret.
    pub fn owns_caret(self) -> bool {
        matches!(self, Modal::CustomProvider | Modal::InputInjection)
    }
}

/// Which section the Activity modal is showing. Each section is opened
/// independently by clicking the corresponding segment on the activity bar,
/// so there is no tab strip or Left/Right cycling — the variant simply
/// controls which content the modal body renders.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
pub enum ActivityTab {
    Activity,
    Todos,
}

impl ActivityTab {
    /// Modal title shown in the header.
    pub fn title(self) -> &'static str {
        match self {
            ActivityTab::Activity => "Activity",
            ActivityTab::Todos => "Todos",
        }
    }
}
