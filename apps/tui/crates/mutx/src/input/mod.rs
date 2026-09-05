//! Input handling: keyboard and mouse events mapped to semantic actions.

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};

use crate::model::layout::{LayoutMap, SemanticCursor};
use crate::model::selection::SelectionDrag;

#[derive(Default)]
pub struct InputContext {
    pub active_modal: super::Modal,
    /// While the sessions picker is drilled into its info sub-view (`i`), the
    /// list-only keys (delete `d`, new `n`, info `i`) are inert — the sub-view
    /// is a read-only read-out.
    pub session_info_detail: bool,
    /// While the connections picker is drilled into its detail sub-view (Enter),
    /// the list-only keys (delete `D`, preset `a`, custom `c`) are inert — the sub-view
    /// is a read-only read-out.
    pub connection_info_detail: bool,
    pub is_responding: bool,
    /// Target queue mode for the live composer while a round is running.
    pub composer_send_mode: crate::app::ComposerSendMode,
    /// Which completion menu (slash command vs `@path` mention) is active, or
    /// `None` when no menu is shown. Drives Tab/↑/↓ cycling and the
    /// slash-specific Enter auto-accept. Mirrors [`super::CompletionKind`].
    pub completion_kind: super::CompletionKind,
    pub suggestion_count: usize,
    pub has_exact_suggestion: bool,
    pub suggestion_index: Option<usize>,
    /// Whether the completion menu is currently hidden behind the Esc/Enter
    /// dismissal latch (`App::completion_dismissed`). Tab consults it to
    /// re-open a dismissed menu: Esc closes, Tab reopens — the toggle's
    /// other half.
    pub completion_dismissed: bool,
    /// Whether the composer still holds text a completion menu could anchor
    /// to — a partial `/command` or an `@mention` under the caret. Together
    /// with [`Self::completion_dismissed`] it decides whether Tab can bring
    /// a dismissed popup back: re-opening makes sense only when the trigger
    /// text survived.
    pub has_trigger_text: bool,
    pub permission_confirm_always: bool,
    /// Whether the inline permission sheet is expanded to "Details". Drives
    /// whether ↑/↓ in the compose zone scroll the details body or the
    /// transcript behind it.
    pub permission_show_details: bool,
    /// Whether the view is zoomed into an runner task (focus stack non-empty).
    pub in_runner_view: bool,
    /// Whether the view is inside a `/btw` aside view (ADR-0103). Esc
    /// interrupts the viewed aside's round; Ctrl+C detaches to the primary
    /// transcript.
    pub in_side_view: bool,
    /// Whether a transcript step/action target currently holds keyboard focus.
    ///
    /// This is the TUI's only navigation state: there is no separate "browse
    /// mode". When `true`, a step is highlighted in the transcript and the
    /// keys that would otherwise edit/scroll instead act on that step — `↑`/`↓`
    /// (and `Ctrl+↑`/`Ctrl+↓`) cycle the focused step, `Enter` activates it,
    /// and `Esc` clears the focus. When `false` every key has its ordinary
    /// input-box meaning. Mirrors `App::focused_target.is_some()`.
    pub has_focused_target: bool,
    /// Whether the transcript currently holds browse focus (e.g. via mouse click into viewport).
    pub transcript_focused: bool,
    /// Whether the history modal's search sub-layer is active. Only meaningful
    /// while [`Self::active_modal`] is `super::Modal::HistorySearch`: `false`
    /// is browse mode (typing is inert, `/` enters search), `true` borrows the
    /// composer line as the live fuzzy query. Mirrors `App::history_search`.
    pub history_searching: bool,
    /// Whether the model picker's search sub-layer is active. Only meaningful
    /// while [`Self::active_modal`] is `super::Modal::Models` or
    /// `super::Modal::Connections`: `false` is browse mode (typing is inert,
    /// `/` enters search, `*`/`e`/`d`/`D` act on the row), `true` borrows the
    /// composer line as the live fuzzy query. Mirrors `App::model_search`.
    pub model_searching: bool,
    /// Focused text-field index of the provider editor, or `None` when the
    /// modal is closed or an inline selector is focused.
    pub custom_provider_field: Option<u8>,
    /// Focused field of the key editor (`Modal::ModelEditor`): `0` = API key,
    /// `1` = effort selector, `2` = thinking toggle. `None` when that modal is
    /// not open. Drives ←/→ effort cycling (field 1) and Space thinking toggle
    /// (field 2). Mirrors `App::editor_field` while the key editor is open.
    pub editor_field: Option<u8>,
    /// Whether the Question modal's synthetic "Other" free-text row is the
    /// highlighted row. Only meaningful while [`Self::active_modal`] is
    /// `super::Modal::Question`: when `true` the modal owns a text-input
    /// surface, so printable keys (including Space) insert into the "Other"
    /// field instead of toggling an option. Mirrors
    /// `App::question.is_some_and(|q| q.is_other_highlighted())`.
    pub question_other_highlighted: bool,
    /// Whether the `/host` dashboard's inline prompt is open (`p` prompt or
    /// `n` new session). While true, printable keys edit the prompt text and
    /// Enter submits it. Mirrors `App::host_prompting`.
    pub host_prompting: bool,

    /// The AI-initiated sheet occupying the composer slot, if any (ADR-0173
    /// §3). Mutually exclusive with a non-`None` `active_modal`: a sheet is
    /// not a modal, and `active_modal` is `None` while one is up.
    pub active_sheet: Option<crate::sheet::SheetKind>,
    /// ADR-0175: `true` while the PreAttach interstitial surface owns
    /// the terminal. Mirrors `App::pre_attach.is_some()` so `process_event`
    /// can route keyboard events to PreAttach without inspecting `App`.
    pub pre_attach_active: bool,
    /// Which pane of the Settings View currently owns focus. Mirrors `App::config_focus`.
    pub config_focus: crate::overlays::ConfigFocus,
    /// The full-screen view the user stands in (ADR-0141). Surface dispatch
    /// (ADR-0172) keys off this: a key is offered to the current view's
    /// scheme before the shared/modal layers, and "no modal" never silently
    /// means "session view" for Dashboard or Settings.
    pub current_view: crate::surfaces::View,
    /// User remaps of the global chords (`[keybindings]` config, ADR-0172).
    /// Global resolution and the keycap hints both consult it.
    pub key_overrides: crate::keymap::GlobalOverrides,
    /// User remaps of the full-screen-view surface verbs (`session.*` dotted
    /// keys, ADR-0172). The view resolvers consult it; the composer hint row
    /// renders its effective bindings.
    pub surface_overrides: crate::keymap::SurfaceOverrides,
}

impl InputContext {
    /// Whether a provider-editor text field is focused.
    fn custom_text_field_focused(&self) -> bool {
        self.custom_provider_field.is_some()
    }
}

/// Whether `modal` currently treats the composer line as an editable free-text
/// field — the surfaces where printable keys, Backspace, and the readline
/// editing family (Ctrl+A/E/W/U/K, Alt+B/F/D, …) act on the input buffer. The
/// history and model-picker modals only qualify while their search sub-layer is
/// active (`history_searching` / `model_searching`); in browse mode those keys
/// are inert so `/` can open search and stray letters never mutate a buffer the
/// user isn't editing.
/// Whether the permission sheet occupies the composer slot. The one
/// pass-through surface: transcript navigation and scrolling stay live
/// behind it (ADR-0173 §2).
fn permission_sheet_up(context: &InputContext) -> bool {
    context.active_sheet == Some(crate::sheet::SheetKind::Permission)
}

/// Whether no overlay is up at all — no modal, no sheet: the chat surface.
fn bare_chat_surface(context: &InputContext) -> bool {
    context.active_modal == super::Modal::None && context.active_sheet.is_none()
}

/// Whether clicks, drags and hover reach the live transcript: on the bare
/// chat surface, or behind the pass-through permission sheet.
fn transcript_interactive(context: &InputContext) -> bool {
    bare_chat_surface(context) || permission_sheet_up(context)
}

/// Whether the foreground surface (sheet or modal) pages its own body on the
/// scroll keys — the claims-driven mirror of `App::modal_scroll_field`
/// (ADR-0173 §2).
fn foreground_scrolls_own_body(context: &InputContext) -> bool {
    if let Some(kind) = context.active_sheet {
        return kind.keyboard_claims().body_scroll;
    }
    context.active_modal.keyboard_claims().body_scroll
}

fn edits_input_field(context: &InputContext) -> bool {
    if context.has_focused_target || context.transcript_focused {
        return false;
    }
    // Sheet foreground: only the injection sheet borrows the composer line;
    // the permission and question sheets never edit the shared draft.
    if let Some(kind) = context.active_sheet {
        return kind == crate::sheet::SheetKind::InputInjection;
    }
    // The static column comes from the modal's declared `text_entry` claim
    // (modal.rs, ADR-0173 §2); the live gate (which field, which sub-mode)
    // stays here beside the resolver.
    if !context.active_modal.keyboard_claims().text_entry {
        return false;
    }
    match context.active_modal {
        // No modal: the chat surface's own composer is the text field.
        super::Modal::None | super::Modal::ModelEditor => true,
        super::Modal::Models | super::Modal::Connections => context.model_searching,
        super::Modal::HistorySearch => context.history_searching,
        // The provider editor's four basic string fields borrow the composer;
        // Protocol and Client Identity are inline selectors.
        super::Modal::CustomProvider => context.custom_text_field_focused(),
        _ => false,
    }
}

fn question_other_field(context: &InputContext) -> bool {
    context.active_sheet == Some(crate::sheet::SheetKind::Question)
        && context.question_other_highlighted
}

/// Whether the active modal paints its own scrollable body — derived from the
/// modal's declared [`Claims::body_scroll`] (modal.rs, ADR-0173 §2): the
/// scroll keys page a modal body exactly when the modal declares the family,
/// so the key→action mirror of `App::modal_scroll_field` can never drift from
/// the declaration.
fn scrolls_own_body(modal: super::Modal) -> bool {
    modal.keyboard_claims().body_scroll
}

/// Which OAuth pending-sheet field to copy: the device verification code (the
/// value the user pastes at github.com/login/device) or the verification URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OauthCopyTarget {
    UserCode,
    Url,
    Selected,
}

/// Result of processing an input event.
#[derive(Debug, PartialEq)]
pub enum InputAction {
    /// Nothing to do.
    None,
    /// Quit the application.
    Quit,
    /// Send a chat message.
    SendChat(String),
    /// Immediate steering intervention (while running).
    SteerImmediate(String),
    /// Enqueue follow-up prompt into outbox queue (while running).
    QueueFollowUp(String),
    /// Toggle between steer and follow-up queue mode while running.
    ToggleSendMode,
    /// Send a slash command.
    SendSlash(String),
    /// Activate the highlighted row of the **Models** picker: a flat
    /// (provider, model) pair. Falls through to the API-key setup modal when
    /// the target has no key. The Connections list has no activate concept —
    /// it only manages instances (`a`/`e`/`D`), leaving provider switching to
    /// this picker.
    ProviderPickerActivate,
    /// Drill into the connection detail sub-view for the highlighted connection row in Connections modal.
    OpenConnectionDetail,
    /// Open the connection detail modal directly for the active connection (via click or Ctrl+N).
    OpenActiveConnectionDetail,
    /// Toggle the favorite flag on the highlighted Models row (model-level,
    /// ADR-0046). The Connections list has no favorite concept.
    ProviderPickerToggleFavorite,
    /// Open the unified provider editor (`e`): the per-model settings editor
    /// for the highlighted Models row, or the provider editor (key / meta) for
    /// the highlighted Connections row.
    OpenModelEditor,
    /// Submit the unified provider editor: persist the entered key / model-id and
    /// activate the target model.
    SubmitModelEditor,
    /// Cycle focus between the editor's fields (API key ↔ effort).
    ModelEditorNextField,
    /// Cycle the effort selector (←/→) on the Anthropic key editor's effort
    /// field. Carries a delta of ±1; wraps around the effort levels.
    ModelEditorEffortCycle {
        delta: i8,
    },
    /// Jump the effort selector straight to a tier (digit `1`..=`7` on the
    /// ladder, `0`-indexed here) on the effort field — the flat segmented
    /// layout makes direct selection the natural counterpart to ←/→ stepping.
    /// Ignored when the index is past the model's ladder.
    ModelEditorEffortJump {
        index: usize,
    },
    /// Toggle extended thinking on/off (Space) on the Anthropic key editor's
    /// thinking field. Orthogonal to effort.
    ModelEditorThinkingToggle,
    /// Cycle the vision capability override (ADR-0149 layer 1) tri-state:
    /// inherit → force on → force off. Field 3 of the settings editor.
    ModelEditorVisionCycle,
    /// Cycle the tool-call capability override tri-state. Field 4.
    ModelEditorToolCycle,
    /// Submit the custom-provider editor → `AgentRequest::AddProvider`.
    SubmitCustomProvider,
    /// Cancel the custom-provider editor and return to the Connections list.
    CancelCustomProvider,
    /// Move focus to the next / previous field of the custom-provider editor
    /// (`Tab` / `BackTab`), wrapping at the ends.
    CustomProviderNextField,
    CustomProviderPrevField,
    /// Scroll the custom-provider form with `↑` / `↓`. `forward` = down.
    ScrollCustomProvider {
        forward: bool,
    },
    /// Cycle the focused custom provider selector (`Protocol` or
    /// `ClientIdentity`) with `←` / `→`.
    CycleCustomProviderChoice {
        forward: bool,
    },
    /// Move the preset-chooser selection with `↑` / `↓`. `forward` = down.
    MovePresetChoice {
        forward: bool,
    },
    /// Open the provider editor seeded from the highlighted preset (`Enter`).
    SelectPreset,
    /// Select the highlighted OAuth preset with an explicit login method.
    /// The preset chooser exposes `b` for browser PKCE and `d` for device
    /// authorization when the client registration supports them.
    SelectPresetWithOauthMethod {
        method: muta_contracts::LoginMethod,
    },
    /// Cancel the preset chooser and return to the Connections list.
    CancelPresetChooser,
    /// Cancel the "Add preset connection → OAuth" browser flow (`Esc` while
    /// `Modal::OauthPending` is active).
    CancelOauthPending,
    /// Cycle focus between copyable targets (URL and device code) in OAuth pending sheet (`Tab`/`Left`/`Right`).
    CycleOauthSelection,
    /// Copy the OAuth pending sheet's primary content. `user_code` copies the
    /// device-verification code the user must paste at github.com/login/device;
    /// `url` copies the verification URL; `Selected` copies the focused card.
    CopyOauthContent {
        target: OauthCopyTarget,
    },
    /// Delete the entire highlighted custom provider from the Connections list
    /// (`Shift+D`). Built-in providers are ignored by the handler. Opens the
    /// provider-delete confirm overlay rather than deleting immediately.
    DeleteProvider,
    /// Confirm the pending provider-delete: dispatch the staged
    /// `AgentRequest::DeleteProvider` and close the confirm overlay. Only
    /// produced by the confirm overlay's Enter when focus is on Delete.
    DeleteProviderConfirm,
    /// Cancel the provider-delete confirm overlay: drop the staged provider id
    /// and return focus to the Connections list. Produced by Esc / Ctrl+C
    /// / Enter-on-Cancel inside the confirm overlay.
    DeleteProviderCancel,
    /// Interrupt current operation.
    Interrupt,
    /// Open the flat Models picker (`/models`, Ctrl+M) — the daily-driver
    /// model-switch surface.
    OpenSessions,
    OpenModels,
    /// Open the Connections list (`/connections`) — the provider-instance
    /// management surface.
    OpenConnections,
    /// Refresh / rediscover available models for discovery-enabled providers from upstream.
    RefreshProviderModels,
    /// Open the curated preset chooser (`a` in the Connections modal).
    OpenPresetChooser,
    /// Open the standalone custom-connection editor (`c` in Connections).
    OpenCustomConnection,
    /// Open the input-history modal (Ctrl+R). Opens in browse mode — a plain
    /// newest-first list; `/` then enters the search sub-layer.
    OpenHistory,
    /// Open the help / keybindings modal.
    OpenHelp,
    /// Open the queue overview modal (the full outbox list). Reached via `F2`
    /// or by clicking the persistent queue bar. Mirrors clicking the queue bar
    /// — the request is never forwarded, it only opens the overlay.
    OpenQueue,
    /// Open the permissions manager modal: a centered list of cached "always
    /// allow" rules with per-row revoke and clear-all. Reached via the
    /// `/permissions` slash command (intercepted locally, never sent to the
    /// backend). `/permissions clear` still goes to the backend.
    OpenPermissions,
    /// Open the tools manager modal: a centered, selectable list of every
    /// session tool with a `Space` toggle. Reached via the `/tools` slash
    /// command (intercepted locally, never sent to the backend). The request is
    /// never forwarded — it only opens the overlay.
    OpenTools,
    /// Open the usage-statistics overlay (`/usage`, ADR-0122): daily token
    /// totals, per-model breakdown, and the recent request event log, from
    /// the durable cross-session store. Intercepted locally; the handler
    /// issues `AgentRequest::QueryUsageStats` so the overlay populates from
    /// the daemon-side store.
    OpenUsage,
    /// Open the MCP manager modal: a centered, selectable list of every
    /// configured MCP server with `Space` toggle and `r` reconnect. Reached via
    /// the `/mcp` slash command (intercepted locally, never sent to the
    /// backend). The request is never forwarded — it only opens the overlay.
    OpenMcp,
    /// Open the skills modal: a centered, selectable list of every loaded
    /// skill with a per-row detail expansion. Reached via
    /// the `/skills` slash command (intercepted locally, never sent to the
    /// backend; `/skills list` with args still forward).
    /// The request is never forwarded — it only opens the overlay.
    OpenSkills,
    /// Toggle the detail expansion of the selected skill row in the skills
    /// modal. Bound to `Enter`.
    SkillsToggleDetail,
    /// Open the config manager modal: a centered list of configurable
    /// categories (Appearance and Layout). Reached via the `/config` slash command
    /// (intercepted locally, never sent to the backend). `Enter` / `Space`
    /// on a category drills into its sub-page.
    OpenConfig,
    /// Connect/disconnect the selected MCP server in the MCP manager modal.
    /// Bound to `Space`.
    McpToggle,
    /// Reconnect the selected MCP server in the MCP manager modal. Bound to `r`.
    McpReconnect,
    /// Revoke the selected "always allow" rule in the permissions manager
    /// modal. Bound to `Space`.
    PermissionsActivate,
    /// Clear every cached "always allow" rule. Bound to `c` in the
    /// permissions manager modal.
    PermissionsClearAll,
    /// Activate or toggle the selected item in the Settings View. Bound to `Enter` / `Space`.
    ConfigActivate,
    /// Delete the selected custom connection instance in Settings View. Bound to `d` / `D`.
    ConfigDeleteConnection,
    /// Switch to previous tab segment in Settings View (e.g. Web Search tab). Bound to `←` / `1` / `h`.
    ConfigSegmentPrev,
    /// Switch to next tab segment in Settings View (e.g. Web Fetch tab). Bound to `→` / `2` / `l`.
    ConfigSegmentNext,
    /// Toggle focus between Categories and Detail in the Settings View. Bound to `Tab`.
    ConfigFocusToggle,
    /// Return focus to Categories or close the Settings View. Bound to `Esc`.
    ConfigBack,
    /// Move the tool-selection cursor in the session-context dashboard when it
    /// still hosts the tools list, and in the tools manager modal otherwise.
    /// `forward` = down, else up.
    SessionSelect {
        forward: bool,
    },
    /// Toggle the selected tool's enabled flag in the tools manager modal.
    /// Bound to `Space`.
    SessionActivate,
    /// Open the currently-selected session in the sessions picker.
    OpenSelectedSession,
    /// `/host` panel Enter: switch the TUI to drive the selected daemon
    /// session (ADR-0096). Handled by exiting to re-attach.
    HostSwitchSelected,
    /// Dashboard Enter on a dock selection: open the read-only preview modal
    /// for that session (ADR-0097 §3). Selection alone never triggers this —
    /// only an explicit Enter.
    HostPreviewSelected,
    /// Dashboard `Tab`: toggle keyboard focus between the session list and
    /// the detail pane.
    HostFocusToggle,
    /// Dashboard `i`: interrupt the selected session's current round
    /// (control-plane verb, ADR-0096).
    HostInterruptSelected,
    /// Dashboard `k`: kill (tear down) the selected session — a two-press
    /// confirm, since a session's running work dies with it. The receipt
    /// lands in the console log.
    HostKillSelected,
    /// Dashboard `s`: suspend the selected session (park it in memory; the
    /// next attach rebuilds it via lazy resume). Refused while a client is
    /// attached or a round is active.
    HostSuspendSelected,
    /// Dashboard `p`: open the inline prompt to send a task to the selected
    /// session. While open, Enter submits the prompt text.
    HostPromptOpen,
    /// Dashboard `n`: open the inline new-session prompt (create + optional
    /// opening task). While open, Enter creates the session.
    HostNewSession,
    /// Dashboard printable key with no prompt open: open the inline prompt
    /// seeded with the typed char — the console is a command surface, so
    /// typing `@3 …` starts the composer directly instead of requiring a
    /// `p` first.
    HostPromptSeed(char),
    /// Dashboard inline-prompt submit (Enter while `p`/`n` is open).
    HostPromptSubmit,
    /// Drill into the selected round or turn in the Telemetry modal. Bound to `Enter`.
    TelemetryActivate,
    /// Advance to the next tab in the Telemetry modal (Tab / Right).
    TelemetryNextTab,
    /// Return to the previous tab in the Telemetry modal (Shift+Tab / Left).
    TelemetryPrevTab,
    /// Switch directly to a specific tab in the Telemetry modal ('1' / '2').
    TelemetrySetTab(crate::modal::TelemetryTab),
    /// Delete the currently-selected session in the sessions picker.
    DeleteSelectedSession,
    /// Create a brand new session from the sessions picker ('n' / 'N').
    CreateNewSession,
    /// Open the session-info sub-view for the selected session ('i'). Shows the
    /// full last effective prompt, creation time, and message count.
    OpenSessionInfo,
    /// Close any modal.
    CloseModal,
    /// Scroll up.
    ScrollUp,
    /// Scroll down.
    ScrollDown,
    /// Mouse wheel tick at a screen position. Semantically the same intent as
    /// [`InputAction::ScrollUp`]/[`InputAction::ScrollDown`], but carrying the
    /// pointer cell so the handler can route spatially: a tick landing inside
    /// the composer panel scrolls the input's own viewport instead of the
    /// transcript (the panel is a scroll region when the draft outgrows the
    /// box). Keyboard-driven scroll keeps the bare variants.
    Wheel {
        up: bool,
        x: u16,
        y: u16,
    },
    /// Scroll up by one viewport page.
    ScrollPageUp,
    /// Scroll down by one viewport page.
    ScrollPageDown,
    /// Scroll to the very top.
    ScrollTop,
    /// Scroll to the very bottom and re-engage auto-follow.
    ScrollBottom,
    /// Copy current selection.
    CopySelection,
    /// Plain Ctrl+C: copy selection, clear input, or arm quit. It never
    /// interrupts a running turn — only double-Esc does.
    CtrlC,
    /// Open the Todos modal (the agent's live task list). The list is
    /// agent-owned and read-only in the TUI; the modal surfaces it on its own
    /// dedicated overlay, opened with `Ctrl+T`.
    OpenTodos,
    /// Open the unified session telemetry report — the drill-down behind the model
    /// bar's context meter and rate gauge. Keyboard twin of clicking those gauges (`Ctrl+O`).
    OpenTelemetry,
    /// Move keyboard focus to the next activatable target. When no target is
    /// focused yet, focuses the first (oldest) step. Driven by `Alt+↓` and by
    /// `↓` while a step is already focused.
    FocusNextTarget,
    /// Move keyboard focus to the previous activatable target. When no target
    /// is focused yet, focuses the last (nearest-to-prompt) step. Driven by
    /// `Alt+↑`, `Alt+O`, and by `↑` while a step is already focused.
    FocusPrevTarget,
    /// Activate the current keyboard-focused target (`Enter`).
    ActivateFocusedTarget,
    /// Copy the content of the currently focused target (`y` or `c` while a step is focused).
    CopyFocusedTarget,
    /// Clear the keyboard-focused target, returning every key to its ordinary
    /// input-box meaning. Triggered by `Esc` while a step is focused.
    ClearFocusedTarget,
    /// Paste from the system clipboard (image or text). Resolved by the app
    /// loop, which reads the clipboard asynchronously.
    Paste,
    /// Terminal-level bracketed paste. The text payload is already available;
    /// the app loop routes it through the same chip-or-inline logic as
    /// [`InputAction::Paste`].
    BracketedPaste(String),
    /// Input character.
    InsertChar(char),
    /// Delete character before cursor.
    Backspace,
    /// Delete character after the cursor (the `Del` key's forward delete).
    /// The input layer has already spliced the text; the action signals the
    /// event loop to run the same post-edit passes as `Backspace`
    /// (completion latch reset, focus reclaim, attachment reconcile).
    DeleteForward,
    /// Cycle suggestion forward.
    SuggestNext,
    /// Cycle suggestion backward.
    SuggestPrev,
    /// Accept the next/previous completion item by index without closing the
    /// popup. Used by `Tab`, which cycles through candidates one splice at a
    /// time. The popup re-renders against the spliced input so the user can
    /// keep cycling.
    AcceptSuggestion(String),
    /// Re-open a completion menu that Esc dismissed, without accepting
    /// anything. Bound to `Tab` while the composer still holds the trigger
    /// text (a partial `/command` or an `@mention`): Esc closes the popup,
    /// Tab brings it back, so the toggle is symmetric and the user never has
    /// to re-edit the text to recover the menu.
    ReopenCompletion,
    /// Like [`InputAction::AcceptSuggestion`] but the popup is closed
    /// afterwards. Used by `Enter` (both the slash-prefix auto-accept and the
    /// highlighted-item path). The harness latches a `completion_dismissed`
    /// flag so the popup stays hidden until the next `InsertChar` /
    /// `Backspace`, matching the expectation that pressing Enter "finishes"
    /// the current completion.
    CommitSuggestion(String),
    /// Dismiss the completion popup without accepting anything. Used by `Esc`
    /// when a slash/path completion menu is open. Latches the same
    /// `completion_dismissed` flag as [`InputAction::CommitSuggestion`] so the
    /// popup stays hidden until the next edit clears the latch.
    CloseCompletion,
    /// Navigate history up.
    HistoryPrev,
    /// Navigate history down.
    HistoryNext,
    /// Legacy destructive recall (pop the newest queue item into the
    /// composer). Kept for the queue modal's explicit pull-to-composer re-edit,
    /// where removing the item from the list *is* the point.
    RecallQueued,
    /// Re-edit the queue modal's *selected* item (not always the newest):
    /// recall it into the composer and close the modal. Bound to `Enter`
    /// inside the queue modal. The queue is auto-blocked on modal open, so
    /// this is always safe.
    RecallQueuedSelected,
    /// Toggle the user block on the viewed session's outbox. While blocked,
    /// no queued message auto-drains — not even after the round completes.
    /// Reachable from `Ctrl+P` (bar, no modal) and the queue modal's block
    /// control.
    QueueToggleBlock,
    /// Delete the queue modal's selected item. Bound to `D` inside the queue
    /// modal (matching the destructive-delete convention in Connections /
    /// Sessions).
    QueueDelete,
    /// Move the queue modal's selected item one slot. `delta = -1` toward the
    /// front (next to pop), `delta = 1` toward the tail. Bound to `K` / `J`
    /// (vim convention) inside the queue modal.
    QueueMoveItem {
        delta: i32,
    },
    /// Accept the focused entry in the Ctrl+R history modal (Enter, in either
    /// browse or search mode): insert it into the input box and close the modal.
    /// The message is not sent — the user can edit and press Enter again to ship
    /// it.
    HistoryInsert,
    /// Enter the model picker's search sub-layer (`/` in browse mode): start
    /// borrowing the composer line as a live fuzzy query and re-rank the list.
    ModelEnterSearch,
    /// Leave the model picker's search sub-layer (first Esc while searching):
    /// clear the query and return to the full browse list. A second Esc then
    /// closes the modal.
    ModelExitSearch,
    /// Select modal item up.
    ModalUp,
    /// Select modal item down.
    ModalDown,
    /// Submit the selected permission decision.
    PermissionSubmit,
    /// Reject the active permission request.
    PermissionReject,
    /// Return from the always-allow confirmation step.
    PermissionBack,
    /// Scroll the expanded "Details" body of the permission sheet up a row.
    PermissionDetailsUp,
    /// Scroll the expanded "Details" body of the permission sheet down a row.
    PermissionDetailsDown,
    /// Move the selection up inside the question modal.
    QuestionUp,
    /// Move the selection down inside the question modal.
    QuestionDown,
    /// Toggle/select the currently highlighted question option. For
    /// multi-select this flips the highlighted row on/off (Space); for
    /// single-select it is a harmless no-op because the highlight already
    /// *is* the live selection.
    QuestionToggle,
    /// Move selection to previous action in the permission sheet (Left / BackTab).
    PermissionPrevOption,
    /// Move selection to next action in the permission sheet (Right / Tab).
    PermissionNextOption,
    /// Advance to the next question, or submit all answers from the final
    /// question (Enter).
    QuestionSubmit,
    /// Advance to the next question in a multi-question ask_user request (Tab / Right).
    QuestionNext,
    /// Return to the previous question (Shift+Tab / Left).
    QuestionPrevious,
    /// Cancel the question modal.
    QuestionCancel,
    /// ADR-0175: navigation/decision actions on the PreAttach
    /// interstitial surface. The four variants mirror the Question
    /// sheet's, but route to `PreAttachState::apply` instead of the
    /// Question sheet's `QuestionModel::update`, keeping the two
    /// surfaces' dispatch disjoint (no overloading of `Question*`
    /// actions).
    PreAttachUp,
    PreAttachDown,
    PreAttachSubmit,
    PreAttachCancel,
    /// Submit the input-injection panel's typed text (L3.5 β).
    InputSubmit,
    /// Cancel the input-injection panel (run the command non-interactively).
    InputCancel,
    /// Select a question option by its 1-based index.
    QuestionSelect(usize),
    /// Insert a character into the question modal's "Other" free-text field.
    QuestionInsertChar(char),
    /// Delete a character from the question modal's "Other" free-text field.
    QuestionBackspace,
    /// Start selection at screen coordinates.
    SelectionStart {
        x: u16,
        y: u16,
    },
    /// Update selection to screen coordinates.
    SelectionUpdate {
        x: u16,
        y: u16,
    },
    /// End selection.
    SelectionEnd,
    /// Select entire block at coordinates (e.g. triple-click).
    SelectBlock {
        x: u16,
        y: u16,
    },
    /// Right-click at screen coordinates. Opens a context/detail view for the
    /// interactive element under the cursor (e.g. a tool step's full output).
    RightClick {
        x: u16,
        y: u16,
    },
    /// Mouse pointer moved to screen coordinates (hover tracking). Used to
    /// drive hover affordances on clickable elements like reasoning-trace
    /// headers. Suppressed while an overlay modal is open.
    Hover {
        x: u16,
        y: u16,
    },
    /// Leave the current runner view and return to the parent.
    ExitRunner,
    /// Detach from the `/btw` aside view and return to the primary transcript
    /// (ADR-0103). Non-destructive: the aside keeps running. Mapped from
    /// Ctrl+C while the aside view is focused.
    ExitSideView,
    /// Open the `/btw` asides list modal (ADR-0103 §5). Mapped from F5.
    OpenBtwList,
    /// Open the global view quick switcher (ADR-0139, `Ctrl+L`). A transient
    /// chooser over every browse surface: open views first in MRU order,
    /// then the rest as discovery. Esc closes it with nothing changed.
    ViewSwitcherToggle,
    /// Append a character to the switcher's fuzzy filter (phase 5). The
    /// row set narrows live; ↑/↓ walk the filtered rows.
    ViewSwitcherFilter {
        ch: char,
    },
    /// Drop the last character from the switcher's filter (Backspace).
    ViewSwitcherBackspace,
    /// Switch to the view highlighted in the quick switcher (ADR-0139,
    /// Enter). Hides the current browse view (state retained in the
    /// `PanelRegistry`) and focuses the target with its retained
    /// scroll/index restored.
    ViewSwitchActivate,
    /// Explicitly close the selected retained view and discard its UI state.
    /// Delete never removes the underlying session or backend resource.
    ViewCloseSelected,
    /// Jump into the aside highlighted in the asides modal (ADR-0103 §5).
    BtwFocusSelected,
    /// Close + discard the aside highlighted in the asides modal
    /// (`D`, ADR-0103 §5).
    BtwCloseSelected,
    /// Interrupt the viewed aside's in-flight round (Esc inside an aside
    /// view, ADR-0103 §2). Interrupting never closes the aside.
    InterruptSide,
    /// Move to the previous sibling runner task.
    PrevSibling,
    /// Move to the next sibling runner task.
    NextSibling,
    /// Terminal was resized (SIGWINCH). The event loop forces a redraw and
    /// re-emits `EnableMouseCapture` so the crossterm parser's internal state
    /// machine is resynced: a resize frequently splits an in-flight SGR mouse
    /// sequence across `event::read()` boundaries, and crossterm then hands the
    /// leftover bytes back as spurious `KeyCode::Char` events (issue #854/#668).
    /// Re-arming capture is the cleanest way to get both sides back in step.
    TerminalResized,
}

impl InputAction {
    /// Whether this action is a modal-opening command reached by typing a
    /// slash command into the composer (e.g. `/models`) — as opposed to a
    /// keybinding such as Ctrl+R (history) or F1 (help).
    ///
    /// These commands consume the composer text (the typed `/cmd`) the same
    /// way `SendSlash` does, but unlike `SendSlash` they are intercepted
    /// locally and never forwarded to the harness. The text they consumed is
    /// therefore not carried on the action and would be lost for input-history
    /// purposes; the event loop snapshots the composer before dispatch and uses
    /// this predicate to decide whether to record it.
    pub fn is_text_modal_command(&self) -> bool {
        matches!(
            self,
            InputAction::OpenSessions
                | InputAction::OpenModels
                | InputAction::OpenConnections
                | InputAction::OpenPermissions
                | InputAction::OpenTools
                | InputAction::OpenMcp
                | InputAction::OpenSkills
                | InputAction::OpenConfig
        )
    }
}

/// Insert a literal newline at the cursor position, but only in modals that
/// accept free-text input. Used by the Alt+Enter and Ctrl+J multi-line
/// entry bindings (plain Enter sends the message).
fn insert_newline(input: &mut String, cursor_position: &mut usize, active_modal: super::Modal) {
    if matches!(active_modal, super::Modal::None) {
        let byte_pos = normalized_cursor_byte(input, *cursor_position);
        *cursor_position = input[..byte_pos].chars().count();
        input.insert(byte_pos, '\n');
        *cursor_position += 1;
    }
}

/// Convert the application's char-index cursor to a grapheme boundary. The
/// logical model remains a char index for compatibility with selection and
/// word-navigation code, but every visible edit/motion lands only between
/// grapheme clusters.
pub(crate) fn normalized_cursor_byte(input: &str, cursor_position: usize) -> usize {
    let raw = input
        .char_indices()
        .nth(cursor_position.min(input.chars().count()))
        .map(|(byte, _)| byte)
        .unwrap_or(input.len());
    mutx_engine::text::floor_grapheme_boundary(input, raw)
}

pub(crate) fn char_index_at_byte(input: &str, byte: usize) -> usize {
    input[..byte.min(input.len())].chars().count()
}

fn normalize_cursor_char_index(input: &str, cursor_position: usize) -> usize {
    char_index_at_byte(input, normalized_cursor_byte(input, cursor_position))
}

fn previous_grapheme_char_index(input: &str, cursor_position: usize) -> usize {
    let cursor_byte = normalized_cursor_byte(input, cursor_position);
    if cursor_byte == 0 {
        return 0;
    }
    let previous = mutx_engine::text::floor_grapheme_boundary(input, cursor_byte - 1);
    char_index_at_byte(input, previous)
}

fn next_grapheme_char_index(input: &str, cursor_position: usize) -> usize {
    let cursor_byte = normalized_cursor_byte(input, cursor_position);
    let next = mutx_engine::text::inclusive_grapheme_end(input, cursor_byte);
    char_index_at_byte(input, next)
}

fn delete_previous_grapheme(input: &mut String, cursor_position: &mut usize) -> bool {
    let end = normalized_cursor_byte(input, *cursor_position);
    if end == 0 {
        *cursor_position = 0;
        return false;
    }
    let start = mutx_engine::text::floor_grapheme_boundary(input, end - 1);
    input.replace_range(start..end, "");
    *cursor_position = char_index_at_byte(input, start);
    true
}

fn delete_next_grapheme(input: &mut String, cursor_position: &mut usize) -> bool {
    let start = normalized_cursor_byte(input, *cursor_position);
    let end = mutx_engine::text::inclusive_grapheme_end(input, start);
    if end <= start {
        *cursor_position = char_index_at_byte(input, start);
        return false;
    }
    input.replace_range(start..end, "");
    *cursor_position = char_index_at_byte(input, start);
    true
}

/// Move the caret to the start of the current logical line.
///
/// Used by the `Home` key and `Ctrl+A` (readline convention). For a
/// single-line buffer this is the very start; for a multi-line buffer it
/// stops just past the nearest preceding newline. `cursor_position` is a
/// char index, so the newline search is translated back to chars.
fn cursor_line_start(input: &str, cursor_position: &mut usize) {
    let char_count = input.chars().count();
    let char_pos = (*cursor_position).min(char_count);
    let byte_offset = input
        .char_indices()
        .nth(char_pos)
        .map(|(i, _)| i)
        .unwrap_or(input.len());
    let before = &input[..byte_offset];
    if let Some(rel) = before.rfind('\n') {
        let after_newline = rel + '\n'.len_utf8();
        *cursor_position = before[..after_newline].chars().count();
    } else {
        *cursor_position = 0;
    }
}

/// Move the caret to the end of the current logical line.
///
/// Used by the `End` key and `Ctrl+E`. For a multi-line buffer the caret
/// stops just before the next newline rather than at the end of the whole
/// buffer, matching the readline/standard-editor behaviour users expect.
fn cursor_line_end(input: &str, cursor_position: &mut usize) {
    let char_count = input.chars().count();
    let char_pos = (*cursor_position).min(char_count);
    let byte_offset = input
        .char_indices()
        .nth(char_pos)
        .map(|(i, _)| i)
        .unwrap_or(input.len());
    let after = &input[byte_offset..];
    if let Some(rel) = after.find('\n') {
        let end_byte = byte_offset + rel;
        *cursor_position = input[..end_byte].chars().count();
    } else {
        *cursor_position = char_count;
    }
}

/// Find the start char index of the previous whitespace-delimited word.
/// Skips trailing whitespace (including newlines), then removes the
/// contiguous run of non-whitespace before the caret.  Returns 0 when
/// the caret is at the very start of the buffer; otherwise the returned
/// position can cross newline boundaries.
///
/// Matches readline's `unix-word-rubout` (Ctrl+W) and the
/// `backward-word` / `backward-kill-word` motions users expect from
/// shells and editors.
fn prev_word_start(input: &str, cursor_position: usize) -> usize {
    let chars: Vec<char> = input.chars().collect();
    let mut i = cursor_position.min(chars.len());
    // Skip whitespace between caret and the previous word (includes \n).
    while i > 0 && chars[i - 1].is_whitespace() {
        i -= 1;
    }
    // Skip the contiguous run of non-whitespace that forms the word.
    while i > 0 && !chars[i - 1].is_whitespace() {
        i -= 1;
    }
    i
}

/// Find the end char index of the next whitespace-delimited word.
/// Skips leading whitespace (including newlines), then skips the
/// contiguous run of non-whitespace.  Returns `input.len()` when the
/// caret is at the very end; otherwise the returned position can cross
/// newline boundaries.
///
/// Matches readline's `kill-word` (Alt+D) and `forward-word` motions.
fn next_word_end(input: &str, cursor_position: usize) -> usize {
    let chars: Vec<char> = input.chars().collect();
    let mut i = cursor_position.min(chars.len());
    // Skip whitespace between caret and the next word (includes \n).
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    // Skip the contiguous run of non-whitespace that forms the word.
    while i < chars.len() && !chars[i].is_whitespace() {
        i += 1;
    }
    i
}

/// Char index of the start of the current logical line, mirroring
/// [`cursor_line_start`] but operating on a borrowed char slice so the
/// word-boundary helpers can call it without re-allocating.
fn cursor_line_start_char(chars: &[char], cursor_position: usize) -> usize {
    let char_pos = cursor_position.min(chars.len());
    if let Some(rel) = chars[..char_pos].iter().rposition(|&c| c == '\n') {
        rel + 1
    } else {
        0
    }
}

/// Char index of the end of the current logical line, mirroring
/// [`cursor_line_end`] on a borrowed char slice.
fn cursor_line_end_char(chars: &[char], cursor_position: usize) -> usize {
    let char_pos = cursor_position.min(chars.len());
    if let Some(rel) = chars[char_pos..].iter().position(|&c| c == '\n') {
        char_pos + rel
    } else {
        chars.len()
    }
}

/// Try to move the caret up one logical line in a multi-line buffer,
/// preserving the column (char offset within the line) clamped to the
/// previous line's length. Returns `true` and updates `cursor_position`
/// when there is a line above; returns `false` (without moving) when the
/// caret is already on the first line, so the caller can fall through to
/// history navigation.
///
/// This is what lets `↑` walk lines inside a multi-line draft instead of
/// always jumping to the previous history entry — only at the top line
/// does it hand off to input history.
pub(crate) fn cursor_line_up(input: &str, cursor_position: &mut usize) -> bool {
    let chars: Vec<char> = input.chars().collect();
    let pos = (*cursor_position).min(chars.len());
    let line_start = cursor_line_start_char(&chars, pos);
    if line_start == 0 {
        return false;
    }
    let col = pos - line_start;
    // The char just before `line_start` is the newline that ends the
    // previous line; the previous line's text lives in [prev_start, prev_end).
    let prev_end = line_start - 1;
    let prev_start = if let Some(rel) = chars[..prev_end].iter().rposition(|&c| c == '\n') {
        rel + 1
    } else {
        0
    };
    let target = prev_start + col.min(prev_end - prev_start);
    *cursor_position = normalize_cursor_char_index(input, target);
    true
}

/// Try to move the caret down one logical line, mirroring
/// [`cursor_line_up`]. Returns `false` (without moving) when the caret is
/// already on the last line, so `↓` hands off to history navigation there.
pub(crate) fn cursor_line_down(input: &str, cursor_position: &mut usize) -> bool {
    let chars: Vec<char> = input.chars().collect();
    let pos = (*cursor_position).min(chars.len());
    let line_end = cursor_line_end_char(&chars, pos);
    if line_end >= chars.len() {
        return false;
    }
    let line_start = cursor_line_start_char(&chars, pos);
    let col = pos - line_start;
    // `line_end` is the index of the newline; the next line starts after it.
    let next_start = line_end + 1;
    let next_end = if let Some(rel) = chars[next_start..].iter().position(|&c| c == '\n') {
        next_start + rel
    } else {
        chars.len()
    };
    let target = next_start + col.min(next_end - next_start);
    *cursor_position = normalize_cursor_char_index(input, target);
    true
}

/// SGR mouse-sequence leakage guard.
///
/// Background: crossterm sometimes fails to reassemble a mouse report that
/// arrives split across two `event::read()` calls (issue #854/#668). When that
/// happens the bytes of an SGR mouse sequence (`ESC [ < btn ; col ; row M/m`)
/// are handed back as a stream of ordinary `Event::Key` / `KeyCode::Char`
/// events: `Esc`, `[`, `<`, `6`, `5`, `;`, … `M`. Because the composer's
/// `KeyCode::Char` arm inserts every printable char into the input box, the
/// split sequence shows up as garbage text (e.g. `;25M[<35;56;25M…`). This is
/// observed across terminals on resize, fast trackpad scrolling, and inside
/// multiplexers (tmux/screen/xterm.js).
///
/// `SgrLeakGuard` is a tiny state machine fed one event at a time. While it is
/// tracking what looks like a leaked SGR sequence it reports [`Feed::Drop`],
/// swallowing the fragments *before* they reach `process_event` and mutate the
/// input line. The pattern is deliberately narrow so a genuine `Esc` keypress
/// still works: it only enters the suppression state on the `ESC [ <` prefix
/// (the mouse-sequence intro) — a bare `Esc` with nothing following stays a
/// real key.
///
/// The guard is best-effort at the symbol layer; the primary defense is the
/// reader-thread reassembler in `event_loop::InputReader`, which keeps whole
/// sequences intact in the common case so the guard rarely sees anything.
#[derive(Debug, Default, Clone)]
pub struct SgrLeakGuard {
    state: SgrState,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum SgrState {
    /// Idle: no suspicious prefix seen.
    #[default]
    Idle,
    /// Saw `ESC`; waiting to see if `[` follows (start of a CSI).
    SawEsc,
    /// Saw `ESC [`; waiting for `<` (SGR mouse) — anything else aborts.
    SawCsi,
    /// Inside an SGR mouse payload after `ESC [ <`. Swallow digits/`;` and the
    /// terminating `M`/`m`, then return to idle.
    InSgr,
}

/// Outcome of feeding one event to the guard.
pub enum Feed {
    /// The event is not part of a leaked sequence — handle it normally.
    Accept,
    /// The event looks like part of a leaked SGR sequence — drop it silently.
    Drop,
}

impl SgrLeakGuard {
    /// Feed one event. Returns whether the caller should still process it.
    /// Pure: performs no I/O and never mutates the input line.
    pub fn feed(&mut self, event: &Event) -> Feed {
        let Event::Key(key) = event else {
            // A non-key event (Mouse/Resize/Paste/Focus) always resets the
            // tracker: if crossterm *did* manage to parse a whole mouse event
            // we clearly are no longer mid-leak, and a resize is exactly the
            // disruption that starts one, so resync here.
            self.state = SgrState::Idle;
            return Feed::Accept;
        };
        let c = match key.code {
            KeyCode::Char(c) => c,
            // Esc as a control key (not a printable char) — a possible SGR
            // prefix start. Treat it as the intro byte.
            KeyCode::Esc => '\x1b',
            _ => {
                // Any other real key (Backspace, arrows, F-keys, Enter, …)
                // breaks a half-formed sequence.
                self.state = SgrState::Idle;
                return Feed::Accept;
            }
        };

        // The match returns (next_state, is_part_of_sequence). A character is
        // "part of a leaked sequence" — and therefore dropped — only when it is
        // a payload byte of an `ESC [ < …` mouse report (the `[`, `<`, digits,
        // `;`, and the `M`/`m` terminator). A bare `Esc` keypress is *never*
        // dropped: it is a real control key (never inserted as text), it is the
        // double-Esc interrupt path, and it clears focus / closes modals.
        // Dropping it silently — as the first version of this guard did — broke
        // double-Esc interrupt entirely. Instead we *deliver* the Esc (Accept)
        // and merely enter the tracking state, so the `[` that follows a
        // genuine leak still starts suppression without ever swallowing the Esc
        // itself.
        let (next, part) = match (self.state, c) {
            // A bare Esc from idle: deliver it, but arm the tracker so a
            // following `[` still opens a leak window.
            (SgrState::Idle, '\x1b') => (SgrState::SawEsc, false),
            // `ESC [`: the `[` is the first byte that can only be leak noise
            // (a real `[` key arrives as a printable char from idle), so start
            // suppressing here. The leading Esc was already delivered above.
            (SgrState::SawEsc, '[') => (SgrState::SawCsi, true),
            // The SGR mouse intro. Once we see this prefix the rest of the
            // payload is unambiguously a mouse report fragment.
            (SgrState::SawCsi, '<') => (SgrState::InSgr, true),
            // Terminators: the final byte of the report.
            (SgrState::InSgr, 'M') | (SgrState::InSgr, 'm') => (SgrState::Idle, true),
            // Continuation bytes of the payload.
            (SgrState::InSgr, '0'..='9' | ';' | '\u{1b}') => (SgrState::InSgr, true),
            // Aborted sequences: the bytes we tentatively buffered were not an
            // SGR mouse report after all. Hand the *current* char back for
            // normal processing (it is genuine input) and resync to idle.
            (SgrState::InSgr, _) => (SgrState::Idle, false),
            // A second Esc while one is already buffered: this is a genuine
            // double-Esc (the double-Esc interrupt pattern), not a leak — a
            // real SGR sequence has `[` next, never another Esc. Deliver it and
            // stay armed so the next non-`[` char cleanly aborts to idle.
            (SgrState::SawEsc, '\x1b') => (SgrState::SawEsc, false),
            (SgrState::SawEsc | SgrState::SawCsi, _) => (SgrState::Idle, false),
            (SgrState::Idle, _) => (SgrState::Idle, false),
        };
        self.state = next;
        if part { Feed::Drop } else { Feed::Accept }
    }

    /// Reset the tracker. Called after a resize so a fresh, fully-armed mouse
    /// session starts from a known state.
    pub fn reset(&mut self) {
        self.state = SgrState::Idle;
    }

    /// Whether the tracker is currently idle (not mid-sequence). Used by the
    /// reader-thread reassembler to know when a drain has completed.
    pub fn is_idle(&self) -> bool {
        self.state == SgrState::Idle
    }
}

/// Process a crossterm event into a high-level action.
///
/// `input` and `cursor_position` are mutable because some events modify them directly.
pub fn process_event(
    event: Event,
    input: &mut String,
    cursor_position: &mut usize,
    context: InputContext,
    drag: &mut SelectionDrag,
) -> InputAction {
    match event {
        Event::Mouse(mouse) => {
            let x = mouse.column;
            let y = mouse.row;
            match mouse.kind {
                // The wheel is spatially routed by the event loop's Wheel
                // handler: modal bodies still take it while a modal owns the
                // surface, and otherwise a tick inside the composer panel
                // scrolls the input's own viewport, falling back to the
                // transcript everywhere else. The question modal's body
                // scroll stays decoupled from the ↑/↓ highlight so wheeling
                // browses a long option list without moving the selection
                // cursor.
                MouseEventKind::ScrollUp => InputAction::Wheel { up: true, x, y },
                MouseEventKind::ScrollDown => InputAction::Wheel { up: false, x, y },
                MouseEventKind::Down(MouseButton::Left) => {
                    // The permission sheet replaces the composer but leaves the
                    // transcript above fully interactive, so a click there can
                    // still toggle steps, drag-select text, follow links, etc.
                    // The sheet itself has no click targets (its buttons are
                    // keyboard-driven) and covers only the composer/hint slot,
                    // which has no registered transcript region, so a press
                    // landing on it resolves to nothing and stays inert.
                    if transcript_interactive(&context) {
                        drag.start(SemanticCursor::new(0, 0, 0));
                        InputAction::SelectionStart { x, y }
                    } else if context.active_sheet == Some(crate::sheet::SheetKind::Question)
                        || context.active_modal == super::Modal::OauthPending
                    {
                        InputAction::SelectionStart { x, y }
                    } else if context.active_modal.dismissable_by_outside_click() {
                        // A dismissable modal owns this click — forward it as
                        // a SelectionStart without arming a drag; the event
                        // loop's SelectionStart handler closes the modal when
                        // the press lands outside the panel (and consumes it
                        // either way so it never reaches the transcript
                        // behind the backdrop). Entry modals keep swallowing.
                        // Modals whose body is a selectable document (the
                        // `render_selectable_body` family) arm a drag when the
                        // press lands on registered text, so their content is
                        // copyable the same way the transcript is.
                        InputAction::SelectionStart { x, y }
                    } else {
                        InputAction::None
                    }
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    if drag.active
                        && (transcript_interactive(&context)
                            || context.active_modal == super::Modal::OauthPending)
                    {
                        InputAction::SelectionUpdate { x, y }
                    } else if drag.active {
                        // A drag armed inside a selectable modal document
                        // (SelectionStart resolved to a MODAL_DOC region)
                        // keeps updating while the button is held, even under
                        // modals that otherwise swallow mouse events.
                        InputAction::SelectionUpdate { x, y }
                    } else {
                        InputAction::None
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    if drag.active {
                        drag.end();
                        InputAction::SelectionEnd
                    } else {
                        InputAction::None
                    }
                }
                // Triple-click detection would need a timer; for now we map
                // middle click to "select block" as a quick approximation.
                MouseEventKind::Down(MouseButton::Middle) => {
                    if transcript_interactive(&context) {
                        InputAction::SelectBlock { x, y }
                    } else {
                        InputAction::None
                    }
                }
                MouseEventKind::Down(MouseButton::Right) => {
                    // Right-click opens detail/feedback for interactive
                    // transcript elements. Allowed during a permission prompt
                    // because the transcript stays interactive.
                    if transcript_interactive(&context) {
                        InputAction::RightClick { x, y }
                    } else {
                        InputAction::None
                    }
                } // Mouse motion (reported because `EnableMouseCapture` requests
                // mode 1003 "all motion"). Forwarded on the main view and
                // during a permission prompt so hover affordances keep working
                // on the still-interactive transcript; blocked behind other
                // overlay modals.
                MouseEventKind::Moved => {
                    if transcript_interactive(&context) {
                        InputAction::Hover { x, y }
                    } else {
                        InputAction::None
                    }
                }
                _ => InputAction::None,
            }
        }
        Event::Key(key) => {
            // Ignore key release events: Windows Console / ConPTY and enhanced
            // keyboard protocols send both Press and Release events. Treating
            // Release as an input action causes double typing, immediate Ctrl-C
            // quits, and duplicate hotkey triggers.
            if key.kind == KeyEventKind::Release {
                return InputAction::None;
            }

            let physical_key = crate::keymap::Key::from_event(key);

            // ── Stage 5: Global Hard-Bound Shortcuts ──────────────────────────
            // F1 (Help), Ctrl+L (Palette), Ctrl+C (Interrupt), Ctrl+Q (Quit), CopySelection
            if let Some(cmd_id) =
                crate::keymap::resolve_global_key_with(physical_key, &context.key_overrides)
            {
                match cmd_id {
                    crate::keymap::CommandId::Help => return InputAction::OpenHelp,
                    crate::keymap::CommandId::CommandPalette => {
                        // Ctrl+P / Ctrl+L toggle the palette: open it at the
                        // top level, and close it while it is already open.
                        if context.active_modal == super::Modal::ViewSwitcher
                            || context.active_modal == super::Modal::None
                        {
                            return InputAction::ViewSwitcherToggle;
                        }
                        // Inside any other modal the chord is owned by that
                        // surface — Ctrl+P toggles the queue block in the
                        // queue panel. Ctrl+L over a modal is a dispatch-side
                        // no-op (`can_open_view_switcher` is false), and we
                        // swallow it here so it cannot fall through to the
                        // printable-char arm.
                        if physical_key == crate::keymap::Key::CTRL_L {
                            return InputAction::None;
                        }
                    }
                    crate::keymap::CommandId::OpenTelemetry => {
                        // Ctrl+O (model-bar telemetry keycap). Top level only:
                        // the model bar is session chrome, never visible
                        // behind a modal.
                        if context.active_modal == super::Modal::None {
                            return InputAction::OpenTelemetry;
                        }
                    }
                    crate::keymap::CommandId::OpenActiveConnectionDetail => {
                        // Ctrl+N (model-bar connection keycap). Top level only,
                        // matching the telemetry binding above.
                        if context.active_modal == super::Modal::None {
                            return InputAction::OpenActiveConnectionDetail;
                        }
                    }
                    crate::keymap::CommandId::InterruptTask => return InputAction::Interrupt,
                    crate::keymap::CommandId::Quit => {
                        if physical_key == crate::keymap::Key::CTRL_Q {
                            return InputAction::Quit;
                        } else {
                            return InputAction::CtrlC;
                        }
                    }
                    crate::keymap::CommandId::CopySelection => return InputAction::CopySelection,
                    _ => {}
                }
            }

            // ADR-0175: PreAttach interstitial owns the keyboard. The
            // four navigation keys map to dedicated PreAttach actions
            // and everything else is swallowed (no chat composer,
            // modal, or sheet behind the surface). Global chords
            // (Ctrl+C, Ctrl+Q) still resolve above as escape hatches,
            // which is consistent with how SessionsPicker handles
            // them — the operator always has a force-quit path.
            if context.pre_attach_active {
                return match key.code {
                    KeyCode::Up => InputAction::PreAttachUp,
                    KeyCode::Down => InputAction::PreAttachDown,
                    KeyCode::Enter => InputAction::PreAttachSubmit,
                    // Backspace, Delete, all chars, all Fn keys, all
                    // Ctrl+X chords (other than the globals handled
                    // above) and any unrecognized key fall through to
                    // Esc semantics — the PreAttach surface has only
                    // four meaningful inputs.
                    _ => InputAction::PreAttachCancel,
                };
            }

            // While the `/host` dashboard's inline prompt is open, printable
            // keys and Backspace edit the prompt text (the composer buffer is
            // borrowed as the input). Enter submits; Esc falls through to
            // CloseModal, which the event loop turns into a prompt-cancel when
            // `host_prompting` is set. Every other key is swallowed so the
            // prompt owns the keyboard.
            if context.active_modal == super::Modal::Host && context.host_prompting {
                match key.code {
                    KeyCode::Char(c) => {
                        let byte_pos = normalized_cursor_byte(input, *cursor_position);
                        *cursor_position = char_index_at_byte(input, byte_pos);
                        input.insert(byte_pos, c);
                        *cursor_position += 1;
                        return InputAction::InsertChar(c);
                    }
                    KeyCode::Backspace => {
                        delete_previous_grapheme(input, cursor_position);
                        return InputAction::None;
                    }
                    KeyCode::Delete => {
                        // Forward delete in the borrowed prompt line, so the
                        // Del key behaves here exactly as it does in the main
                        // composer (no chip handling — the dashboard prompt
                        // never stages attachments).
                        delete_next_grapheme(input, cursor_position);
                        return InputAction::None;
                    }
                    KeyCode::Left => {
                        *cursor_position = previous_grapheme_char_index(input, *cursor_position);
                        return InputAction::None;
                    }
                    KeyCode::Right => {
                        *cursor_position = next_grapheme_char_index(input, *cursor_position);
                        return InputAction::None;
                    }
                    // Enter submits the prompt (never swallowed by the `_`
                    // arm below); Esc cancels via the normal close path.
                    KeyCode::Enter => return InputAction::HostPromptSubmit,
                    KeyCode::Esc => return InputAction::CloseModal,
                    // Swallow every other key (Tab, arrows, `?`, …) so the
                    // prompt owns the keyboard.
                    _ => return InputAction::None,
                }
            }

            // ── Surface Dispatch (ADR-0172) ─────────────────────────────
            // Each full-screen view owns the keys for its own focus planes
            // while no modal is up: the Session view's chat scheme (and its
            // Runner / Side siblings) resolves them here, before the modal /
            // global arms below. A key the surface does not own falls through
            // to the shared affordance library and the modal arms.
            if bare_chat_surface(&context)
                && let Some(action) = crate::session::resolve_view_key(
                    context.current_view,
                    physical_key,
                    &context,
                    input,
                    cursor_position,
                )
            {
                return action;
            }

            // ── Modal Verb Dispatch (ADR-0172) ──────────────────────────
            // Each modal owns its single-letter verb keys (space/r in the MCP
            // manager, d/n/i in the sessions picker, the dashboard console,
            // …) in its own scheme. A key the modal does not own falls through
            // to the shared affordance library (list nav, readline, paste,
            // scrolling) and text insertion.
            // ── Sheet Verb Dispatch (ADR-0173 §3) ───────────────────────
            // Each interaction sheet owns its single-key verbs in its own
            // scheme; a key the sheet does not own falls through to the
            // modal schemes and the sheet arms below. A mounted sheet takes
            // priority over a coexisting modal: the sheet blocks the agent
            // (safety-critical approval flow), the modal is a browsing aid.
            if let Some(kind) = context.active_sheet
                && let Some(action) = crate::sheet::resolve_sheet_key(kind, physical_key, &context)
            {
                return action;
            }
            if context.active_modal != super::Modal::None
                && let Some(action) = crate::modal_keys::resolve_modal_key(
                    context.active_modal,
                    physical_key,
                    &context,
                )
            {
                return action;
            }

            match key.code {
                KeyCode::Esc => {
                    // Chat-surface Esc (close completion / exit side or runner
                    // / clear step focus / interrupt) is resolved by the
                    // Session view's own scheme (ADR-0172) before this match.
                    // This arm is modal-only.
                    if permission_sheet_up(&context) {
                        if context.permission_confirm_always {
                            InputAction::PermissionBack
                        } else if context.has_focused_target {
                            // A step is focused behind the permission sheet:
                            // Esc clears the focus and returns to the sheet
                            // rather than rejecting outright — a second Esc
                            // decides it.
                            InputAction::ClearFocusedTarget
                        } else {
                            InputAction::PermissionReject
                        }
                    } else if context.active_sheet == Some(crate::sheet::SheetKind::Question) {
                        InputAction::QuestionCancel
                    } else if context.active_modal == super::Modal::ProviderPreset {
                        // Esc cancels the preset chooser back to the provider
                        // picker it was opened from.
                        InputAction::CancelPresetChooser
                    } else if context.active_modal == super::Modal::OauthPending {
                        InputAction::CancelOauthPending
                    } else if context.active_modal == super::Modal::CustomProvider {
                        // Esc cancels the custom-provider editor and returns to the
                        // provider picker it was opened from.
                        InputAction::CancelCustomProvider
                    } else if context.active_sheet == Some(crate::sheet::SheetKind::InputInjection)
                    {
                        InputAction::InputCancel
                    } else if matches!(
                        context.active_modal,
                        super::Modal::Models | super::Modal::Connections
                    ) && context.model_searching
                    {
                        // Same two-stage Esc as the history modal: the first Esc
                        // drops the picker's search sub-layer back to the
                        // browse list; the next Esc (browse mode) closes.
                        InputAction::ModelExitSearch
                    } else if context.active_modal == super::Modal::Config {
                        InputAction::ConfigBack
                    } else if context.active_modal != super::Modal::None {
                        // For every other surface — the retained browse views
                        // and the quick switcher included (ADR-0133) — Esc is
                        // the shared dismiss verb; the dispatcher decides
                        // hide (state saved) vs cancel-to-origin there.
                        InputAction::CloseModal
                    } else {
                        InputAction::None
                    }
                }
                KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    // Ctrl+R (history search) is a chat-surface chord resolved
                    // by the Session view's scheme (ADR-0172); no other
                    // surface claims it.
                    InputAction::None
                }
                // Ctrl+P toggles the queue block inside the Queue modal so the
                // user can resume without closing the list. At the top level
                // Ctrl+P is claimed by the Command Palette (Stage 5 global
                // resolution), so this arm only ever fires while the Queue
                // modal is active. Inside any other modal it is a no-op.
                KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if context.active_modal == super::Modal::Queue {
                        InputAction::QueueToggleBlock
                    } else {
                        InputAction::None
                    }
                }

                // F5 is a declared global binding (registry → OpenBtwList).
                // Inside the asides modal itself it re-queries the list (a
                // refresh) rather than toggling the modal closed; inside any
                // other modal it is a no-op.
                KeyCode::F(5) => {
                    if context.active_modal == super::Modal::Btw {
                        InputAction::OpenBtwList
                    } else {
                        InputAction::None
                    }
                }
                // Ctrl+H opens help only when the Kitty enhanced-keyboard
                // protocol is active (enabled in `run_tui`). In a raw
                // terminal Ctrl+H is byte-identical to Backspace (0x08), so
                // without Kitty disambiguation it lands in the `Backspace`
                // arm and never reaches here. Multiplexers like tmux that
                // don't forward Kitty flags further collapse Ctrl+Backspace
                // and Ctrl+H onto the same 0x08 byte, so both keys open
                // help there. Use F1 or `?` for a portable shortcut. Not in
                // the registry because it needs the Kitty protocol; the Help
                // modal documents it via its description.
                KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if context.active_modal == super::Modal::None {
                        InputAction::OpenHelp
                    } else {
                        InputAction::None
                    }
                }
                // Ctrl+M is a declared global binding (registry →
                // OpenModels). In a raw terminal Ctrl+M is byte-identical
                // to Enter, so the registry arm only fires under the Kitty
                // protocol; without it Ctrl+M arrives as Enter and leaves
                // input behavior untouched — no regression. It only reaches
                // this arm inside a modal, where it is a no-op.
                KeyCode::Char('m') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    InputAction::None
                }
                // Alt+Enter / Ctrl+J: insert a literal newline so the input
                // box supports multi-line drafting. Plain Enter sends the
                // message, so these are the only multi-line entry paths.
                KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
                    insert_newline(input, cursor_position, context.active_modal);
                    InputAction::None
                }
                KeyCode::Enter => {
                    // Sheet submits (ADR-0173 §3): Enter on a sheet commits
                    // its pending decision.
                    if let Some(kind) = context.active_sheet {
                        return match kind {
                            crate::sheet::SheetKind::Permission => InputAction::PermissionSubmit,
                            crate::sheet::SheetKind::Question => InputAction::QuestionSubmit,
                            crate::sheet::SheetKind::InputInjection => InputAction::InputSubmit,
                        };
                    }
                    match context.active_modal {
                        super::Modal::Models => InputAction::ProviderPickerActivate,
                        super::Modal::Connections if context.connection_info_detail => {
                            InputAction::None
                        }
                        super::Modal::Connections => InputAction::OpenConnectionDetail,
                        super::Modal::ModelEditor => InputAction::SubmitModelEditor,
                        super::Modal::ProviderPreset => InputAction::SelectPreset,
                        super::Modal::OauthPending => InputAction::CopyOauthContent {
                            target: OauthCopyTarget::Selected,
                        },
                        super::Modal::CustomProvider => InputAction::SubmitCustomProvider,
                        // HistorySearch Enter is owned by the history modal's
                        // scheme (modal_keys, ADR-0172); this arm is an
                        // exhaustiveness placeholder until the Enter arm is
                        // fully retired.
                        super::Modal::HistorySearch => InputAction::None,
                        super::Modal::Sessions if context.session_info_detail => InputAction::None,
                        super::Modal::Sessions => InputAction::OpenSelectedSession,
                        // With the dashboard's inline prompt open, Enter
                        // submits the task/new-session text; otherwise it
                        // opens the highlighted session's preview. (Attach
                        // moved to `a`; Enter previews, ADR-0097 §3.)
                        super::Modal::Host if context.host_prompting => {
                            InputAction::HostPromptSubmit
                        }
                        super::Modal::Host => InputAction::HostPreviewSelected,
                        super::Modal::Help => InputAction::CloseModal,
                        super::Modal::Tools => InputAction::CloseModal,
                        super::Modal::Mcp => InputAction::CloseModal,
                        super::Modal::Skills => InputAction::SkillsToggleDetail,
                        super::Modal::Permissions => InputAction::CloseModal,
                        super::Modal::Tree => InputAction::CloseModal,
                        super::Modal::Queue => InputAction::RecallQueuedSelected,
                        super::Modal::Btw => InputAction::BtwFocusSelected,
                        // Quick switcher: Enter switches to the highlighted
                        // view (ADR-0133). Btw's Enter is a *jump*, the
                        // switcher's is a *switch* — both close the panel.
                        super::Modal::ViewSwitcher => InputAction::None,
                        super::Modal::Config => InputAction::ConfigActivate,
                        super::Modal::Todos => InputAction::CloseModal,
                        super::Modal::Telemetry => InputAction::TelemetryActivate,
                        super::Modal::UsageStats => InputAction::CloseModal,
                        super::Modal::None => {
                            // Chat-surface Enter (activate focused step /
                            // commit completion / send / queue / slash) is
                            // resolved by the Session view's scheme
                            // (ADR-0172) before this match. No other view
                            // sends the draft.
                            InputAction::None
                        }
                    }
                }
                KeyCode::Tab => {
                    // Chat-surface Tab (commit / reopen a completion) is
                    // resolved by the Session view's scheme (ADR-0173). This
                    // arm is modal-only.
                    if context.active_modal == super::Modal::ModelEditor {
                        InputAction::ModelEditorNextField
                    } else if context.active_modal == super::Modal::CustomProvider {
                        InputAction::CustomProviderNextField
                    } else if context.active_modal == super::Modal::Host {
                        InputAction::HostFocusToggle
                    } else if context.active_modal == super::Modal::Telemetry {
                        InputAction::TelemetryNextTab
                    } else if context.active_modal == super::Modal::OauthPending {
                        InputAction::CycleOauthSelection
                    } else {
                        // HistorySearch Tab is owned by the history modal's
                        // scheme (modal_keys, ADR-0172).
                        InputAction::None
                    }
                }
                KeyCode::BackTab => {
                    // Chat-surface BackTab (return focus from a step) is
                    // resolved by the Session view's scheme (ADR-0172). This
                    // arm is modal-only.
                    if context.active_sheet == Some(crate::sheet::SheetKind::Question) {
                        InputAction::QuestionPrevious
                    } else if context.active_modal == super::Modal::CustomProvider {
                        InputAction::CustomProviderPrevField
                    } else if context.active_modal == super::Modal::Telemetry {
                        InputAction::TelemetryPrevTab
                    } else if context.active_modal == super::Modal::OauthPending {
                        InputAction::CycleOauthSelection
                    } else {
                        InputAction::None
                    }
                }
                // Ctrl+J: alias for Alt+Enter — insert a literal newline.
                KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    insert_newline(input, cursor_position, context.active_modal);
                    InputAction::None
                }
                // Ctrl+V: paste from the system clipboard. Active on the
                // main prompt and in the free-text modals (provider editor,
                // provider picker filter, history search) which borrow the
                // input line as a single-line field. The app loop reads the
                // clipboard asynchronously and either attaches an image,
                // inserts the text at the cursor (main prompt), or splices it
                // inline into the modal field (modals).
                KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if question_other_field(&context) {
                        // The "Other" field owns its own buffer (not
                        // `App::input`), so it can't share `edits_input_field`
                        // with the readline modals. Route through the same
                        // async paste path; the event loop applies the read to
                        // `QuestionModel::other_text`.
                        InputAction::Paste
                    } else if edits_input_field(&context) {
                        InputAction::Paste
                    } else {
                        InputAction::None
                    }
                }
                // Ctrl+B: move the caret back one character (readline
                // `backward-char`). Mirrors Left and sits alongside the
                // Ctrl+A / Ctrl+E line-motion family. Active wherever free text
                // is edited; a no-op elsewhere so it never inserts a literal
                // 'b' or scrolls.
                KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if edits_input_field(&context) && *cursor_position > 0 {
                        *cursor_position = previous_grapheme_char_index(input, *cursor_position);
                    }
                    InputAction::None
                }
                // Ctrl+A: move the caret to the start of the current line
                // (readline convention). Works wherever free text is being
                // edited — the main prompt in Compose zone and the free-text
                // modals. Outside those (Browse zone, read-only modals) it is
                // a no-op so it never inserts a literal 'a' or scrolls.
                KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if edits_input_field(&context) {
                        cursor_line_start(input, cursor_position);
                    }
                    InputAction::None
                }
                // Ctrl+E: move the caret to the end of the current line.
                KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if edits_input_field(&context) {
                        cursor_line_end(input, cursor_position);
                    }
                    InputAction::None
                }
                // Ctrl+W: delete the previous whitespace-delimited word
                // (readline `unix-word-rubout`). Skips trailing whitespace
                // then removes the contiguous run of non-whitespace before
                // the caret, crossing newline boundaries.
                // No-op outside free-text surfaces so it never closes a
                // modal or inserts a literal 'w'.
                KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if edits_input_field(&context) {
                        let start = prev_word_start(input, *cursor_position);
                        if start < *cursor_position {
                            let start_byte = input
                                .char_indices()
                                .nth(start)
                                .map(|(i, _)| i)
                                .unwrap_or(input.len());
                            let end_byte = input
                                .char_indices()
                                .nth(*cursor_position)
                                .map(|(i, _)| i)
                                .unwrap_or(input.len());
                            input.replace_range(start_byte..end_byte, "");
                            *cursor_position = start;
                            return InputAction::Backspace;
                        }
                    }
                    InputAction::None
                }
                // Ctrl+U: delete from the caret to the start of the current
                // logical line (readline `unix-line-discard`). Multi-line
                // drafts only lose the current line; Ctrl+C still clears the
                // whole buffer when the user wants a full wipe.
                KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if edits_input_field(&context) {
                        let mut start = *cursor_position;
                        cursor_line_start(input, &mut start);
                        if start < *cursor_position {
                            let start_byte = input
                                .char_indices()
                                .nth(start)
                                .map(|(i, _)| i)
                                .unwrap_or(input.len());
                            let end_byte = input
                                .char_indices()
                                .nth(*cursor_position)
                                .map(|(i, _)| i)
                                .unwrap_or(input.len());
                            input.replace_range(start_byte..end_byte, "");
                            *cursor_position = start;
                            return InputAction::Backspace;
                        }
                    }
                    InputAction::None
                }
                // Ctrl+K: delete from the caret to the end of the current
                // logical line (readline `kill-line`). If already at the end
                // of the line (before a newline), deletes the newline to join
                // the next line.
                KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if edits_input_field(&context) {
                        let char_count = input.chars().count();
                        if *cursor_position < char_count {
                            let mut end = *cursor_position;
                            cursor_line_end(input, &mut end);
                            if end == *cursor_position {
                                end = *cursor_position + 1;
                            }
                            let start_byte = input
                                .char_indices()
                                .nth(*cursor_position)
                                .map(|(i, _)| i)
                                .unwrap_or(input.len());
                            let end_byte = input
                                .char_indices()
                                .nth(end)
                                .map(|(i, _)| i)
                                .unwrap_or(input.len());
                            input.replace_range(start_byte..end_byte, "");
                            return InputAction::Backspace;
                        }
                    }
                    InputAction::None
                }
                // Alt+B: jump back one word (readline `backward-word`).
                KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::ALT) => {
                    if edits_input_field(&context) {
                        *cursor_position = normalize_cursor_char_index(
                            input,
                            prev_word_start(input, *cursor_position),
                        );
                    }
                    InputAction::None
                }
                // Alt+F: jump forward one word (readline `forward-word`).
                KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::ALT) => {
                    if edits_input_field(&context) {
                        *cursor_position = normalize_cursor_char_index(
                            input,
                            next_word_end(input, *cursor_position),
                        );
                    }
                    InputAction::None
                }
                // Alt+D: delete the next whitespace-delimited word (readline
                // `kill-word`). Symmetric counterpart to Ctrl+W.
                KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::ALT) => {
                    if edits_input_field(&context) {
                        let end = next_word_end(input, *cursor_position);
                        if end > *cursor_position {
                            let start_byte = input
                                .char_indices()
                                .nth(*cursor_position)
                                .map(|(i, _)| i)
                                .unwrap_or(input.len());
                            let end_byte = input
                                .char_indices()
                                .nth(end)
                                .map(|(i, _)| i)
                                .unwrap_or(input.len());
                            input.replace_range(start_byte..end_byte, "");
                            return InputAction::Backspace;
                        }
                    }
                    InputAction::None
                }
                // Alt+S / Alt+P / Alt+N are chat-surface chords (steer now /
                // previous / next prompt history), resolved by the Session
                // view's scheme (ADR-0172) before this match.
                KeyCode::Char(c) => {
                    // The command palette's filter is owned by its scheme
                    // (modal_keys::resolve_view_switcher_key, ADR-0172);
                    // the modal verb keys too. Only shared text insertion
                    // remains here: editing surfaces (chat composer, borrowed
                    // one-line modal filters, the key editor's API-key field)
                    // insert the character, everything else is inert.
                    if edits_input_field(&context)
                        && !(context.active_modal == super::Modal::ModelEditor
                            && matches!(context.editor_field, Some(2..=4)))
                    {
                        // The key editor's thinking field (2) is a toggle, not
                        // a text field — don't let printable chars mutate the
                        // borrowed input line while it's focused.
                        let byte_pos = normalized_cursor_byte(input, *cursor_position);
                        *cursor_position = char_index_at_byte(input, byte_pos);
                        input.insert(byte_pos, c);
                        *cursor_position += 1;
                        // Return InsertChar so the event loop can reset the
                        // completion-dismissal latch and suggestion highlight.
                        // The input mutation already happened above; the event
                        // loop's InsertChar handler treats the char as a signal
                        // only (it does not re-insert).
                        InputAction::InsertChar(c)
                    } else {
                        InputAction::None
                    }
                }
                KeyCode::Backspace => {
                    // The palette's query backspace is owned by its scheme
                    // (modal_keys, ADR-0172).
                    if context.active_sheet == Some(crate::sheet::SheetKind::Question) {
                        InputAction::QuestionBackspace
                    } else if edits_input_field(&context) && *cursor_position > 0 {
                        // Alt+Backspace / Ctrl+Backspace delete the previous
                        // whitespace-delimited word in one stroke, matching
                        // readline's `backward-kill-word`. Plain Backspace
                        // keeps the chip-aware atomic delete below so pasted
                        // attachment placeholders vanish in a single tap.
                        if key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        {
                            let start = prev_word_start(input, *cursor_position);
                            if start < *cursor_position {
                                let start_byte = input
                                    .char_indices()
                                    .nth(start)
                                    .map(|(i, _)| i)
                                    .unwrap_or(input.len());
                                let end_byte = input
                                    .char_indices()
                                    .nth(*cursor_position)
                                    .map(|(i, _)| i)
                                    .unwrap_or(input.len());
                                input.replace_range(start_byte..end_byte, "");
                                *cursor_position = start;
                                return InputAction::Backspace;
                            }
                        }
                        // Chip-aware atomic delete: when the cursor sits
                        // immediately after an attachment placeholder (and
                        // optionally one trailing space the paste path
                        // inserts), one Backspace removes the whole chip in
                        // a single keystroke — mirroring codex / claude-code
                        // / opencode. The event loop runs the reconcile pass
                        // on the returned `Backspace` action, which drops
                        // the orphaned entry from `pending_images` /
                        // `pending_text_pastes` and relabels survivors.
                        let byte_cursor = input
                            .char_indices()
                            .map(|(i, _)| i)
                            .nth(*cursor_position)
                            .unwrap_or(input.len());
                        if let Some((start, end)) =
                            crate::composer_attachments::chip_range_for_backspace(
                                input,
                                byte_cursor,
                            )
                        {
                            let removed_chars = input[start..end].chars().count();
                            input.replace_range(start..end, "");
                            *cursor_position -= removed_chars;
                            return InputAction::Backspace;
                        }
                        delete_previous_grapheme(input, cursor_position);
                        // Return Backspace so the event loop resets the
                        // completion-dismissal latch and suggestion highlight,
                        // matching InsertChar above.
                        InputAction::Backspace
                    } else {
                        InputAction::None
                    }
                }
                // `Del` key: forward delete — remove the character *after*
                // the caret. Gated like Backspace on the same free-text
                // surfaces (`edits_input_field`), so it never disturbs a
                // read-only modal. Chip-aware: a Delete landing on the `[` of
                // an attachment chip removes the whole chip in one
                // keystroke, mirroring the chip-aware Backspace. The caret
                // does not move (forward delete only shortens the text).
                KeyCode::Delete => {
                    // The palette's delete-selected is owned by its scheme
                    // (modal_keys, ADR-0172).
                    if edits_input_field(&context) && *cursor_position < input.chars().count() {
                        let byte_cursor = input
                            .char_indices()
                            .map(|(i, _)| i)
                            .nth(*cursor_position)
                            .unwrap_or(input.len());
                        if let Some((start, end)) =
                            crate::composer_attachments::chip_range_for_delete(input, byte_cursor)
                        {
                            input.replace_range(start..end, "");
                            return InputAction::DeleteForward;
                        }
                        if delete_next_grapheme(input, cursor_position) {
                            return InputAction::DeleteForward;
                        }
                    }
                    InputAction::None
                }
                KeyCode::Left => {
                    if context.active_modal == super::Modal::Telemetry {
                        return InputAction::TelemetryPrevTab;
                    }
                    if context.active_modal == super::Modal::Config
                        && context.config_focus == crate::overlays::ConfigFocus::Detail
                    {
                        return InputAction::ConfigSegmentPrev;
                    }
                    // In the model editor's effort field, ← cycles the effort
                    // level down (wrapping). Only when field 1 is focused.
                    if context.active_modal == super::Modal::ModelEditor
                        && context.editor_field == Some(1)
                    {
                        return InputAction::ModelEditorEffortCycle { delta: -1 };
                    }
                    if context.active_modal == super::Modal::CustomProvider
                        && context.custom_provider_field.is_none()
                    {
                        return InputAction::CycleCustomProviderChoice { forward: false };
                    }
                    // In provider-editor text fields, ←/→ retain ordinary
                    // caret movement.
                    if edits_input_field(&context) && *cursor_position > 0 {
                        // Ctrl+Left (and Alt+Left on terminals that translate
                        // it) jumps back one whitespace-delimited word,
                        // matching readline's `backward-word`.
                        if key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        {
                            *cursor_position = normalize_cursor_char_index(
                                input,
                                prev_word_start(input, *cursor_position),
                            );
                        } else {
                            *cursor_position =
                                previous_grapheme_char_index(input, *cursor_position);
                        }
                    }
                    InputAction::None
                }
                KeyCode::Right => {
                    if context.active_modal == super::Modal::Telemetry {
                        return InputAction::TelemetryNextTab;
                    }
                    if context.active_modal == super::Modal::Config
                        && context.config_focus == crate::overlays::ConfigFocus::Detail
                    {
                        return InputAction::ConfigSegmentNext;
                    }
                    // Effort field: → cycles the level up (wrapping).
                    if context.active_modal == super::Modal::ModelEditor
                        && context.editor_field == Some(1)
                    {
                        return InputAction::ModelEditorEffortCycle { delta: 1 };
                    }
                    if context.active_modal == super::Modal::CustomProvider
                        && context.custom_provider_field.is_none()
                    {
                        return InputAction::CycleCustomProviderChoice { forward: true };
                    }
                    if edits_input_field(&context) && *cursor_position < input.chars().count() {
                        // Ctrl+Right (and Alt+Right) jump forward one word.
                        if key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        {
                            *cursor_position = normalize_cursor_char_index(
                                input,
                                next_word_end(input, *cursor_position),
                            );
                        } else {
                            *cursor_position = next_grapheme_char_index(input, *cursor_position);
                        }
                    }
                    InputAction::None
                }
                // Alt+↑ / Alt+↓ (transcript step focus switching) are
                // chat-surface chords, resolved by the Session view's scheme
                // (ADR-0172) before this match.
                // Ctrl+↑ / Ctrl+↓ inside a modal scroll the modal body by one
                // page — the same gesture a pager or editor binds to a
                // half-page jump. Mirrors PageUp / PageDown so users have both
                // the dedicated keys and the chord (useful on keyboards without
                // Page keys). Routed through the shared `Scroll*` actions.
                KeyCode::Up
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && scrolls_own_body(context.active_modal) =>
                {
                    InputAction::ScrollPageUp
                }
                KeyCode::Down
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && scrolls_own_body(context.active_modal) =>
                {
                    InputAction::ScrollPageDown
                }
                KeyCode::Up => {
                    // Sheet ↑ (ADR-0173 §3): the permission sheet passes
                    // transcript navigation through (claims), the question
                    // sheet walks its own option cursor.
                    if let Some(kind) = context.active_sheet {
                        return match kind {
                            crate::sheet::SheetKind::Permission => {
                                if context.has_focused_target {
                                    InputAction::FocusPrevTarget
                                } else if context.permission_show_details {
                                    InputAction::PermissionDetailsUp
                                } else {
                                    InputAction::ScrollUp
                                }
                            }
                            crate::sheet::SheetKind::Question => InputAction::QuestionUp,
                            crate::sheet::SheetKind::InputInjection => InputAction::None,
                        };
                    }
                    match context.active_modal {
                        super::Modal::Models | super::Modal::Connections => InputAction::ModalUp,
                        // HistorySearch ↑/↓ are owned by the history modal's
                        // scheme (modal_keys, ADR-0172); placeholders keep the
                        // arrow arm exhaustive until it is fully retired.
                        super::Modal::HistorySearch => InputAction::None,
                        super::Modal::Sessions => InputAction::ModalUp,
                        super::Modal::Host => InputAction::ModalUp,
                        super::Modal::Todos => InputAction::ScrollUp,
                        super::Modal::Tools => InputAction::SessionSelect { forward: false },
                        super::Modal::Mcp => InputAction::SessionSelect { forward: false },
                        super::Modal::Skills => InputAction::SessionSelect { forward: false },
                        super::Modal::Queue | super::Modal::Btw => {
                            InputAction::SessionSelect { forward: false }
                        }
                        super::Modal::ViewSwitcher => InputAction::ModalUp,
                        super::Modal::Permissions => InputAction::ModalUp,
                        super::Modal::Config => InputAction::ModalUp,
                        super::Modal::Tree => InputAction::ModalUp,
                        super::Modal::ProviderPreset => {
                            InputAction::MovePresetChoice { forward: false }
                        }
                        super::Modal::OauthPending => InputAction::ScrollUp,
                        super::Modal::CustomProvider => {
                            InputAction::ScrollCustomProvider { forward: false }
                        }
                        super::Modal::ModelEditor => InputAction::None,
                        super::Modal::Help => InputAction::ScrollUp,
                        super::Modal::Telemetry => InputAction::ModalUp,
                        super::Modal::UsageStats => InputAction::ScrollUp,
                        super::Modal::None => {
                            // Chat-surface ↑ (walk focused steps / completion
                            // suggestions / multi-line caret) is resolved by
                            // the Session view's scheme (ADR-0172).
                            InputAction::None
                        }
                    }
                }
                KeyCode::Down => {
                    // Sheet ↓: mirror of the ↑ arm.
                    if let Some(kind) = context.active_sheet {
                        return match kind {
                            crate::sheet::SheetKind::Permission => {
                                if context.has_focused_target {
                                    InputAction::FocusNextTarget
                                } else if context.permission_show_details {
                                    InputAction::PermissionDetailsDown
                                } else {
                                    InputAction::ScrollDown
                                }
                            }
                            crate::sheet::SheetKind::Question => InputAction::QuestionDown,
                            crate::sheet::SheetKind::InputInjection => InputAction::None,
                        };
                    }
                    match context.active_modal {
                        super::Modal::Models | super::Modal::Connections => InputAction::ModalDown,
                        // HistorySearch ↑/↓ are owned by the history modal's
                        // scheme (modal_keys, ADR-0172); placeholder for
                        // exhaustiveness until the arrow arm is retired.
                        super::Modal::HistorySearch => InputAction::None,
                        super::Modal::Sessions => InputAction::ModalDown,
                        super::Modal::Host => InputAction::ModalDown,
                        super::Modal::Todos => InputAction::ScrollDown,
                        super::Modal::Tools => InputAction::SessionSelect { forward: true },
                        super::Modal::Mcp => InputAction::SessionSelect { forward: true },
                        super::Modal::Skills => InputAction::SessionSelect { forward: true },
                        super::Modal::Queue | super::Modal::Btw => {
                            InputAction::SessionSelect { forward: true }
                        }
                        super::Modal::ViewSwitcher => InputAction::ModalDown,
                        super::Modal::Permissions => InputAction::ModalDown,
                        super::Modal::Config => InputAction::ModalDown,
                        super::Modal::Tree => InputAction::ModalDown,
                        super::Modal::ProviderPreset => {
                            InputAction::MovePresetChoice { forward: true }
                        }
                        super::Modal::OauthPending => InputAction::ScrollDown,
                        super::Modal::CustomProvider => {
                            InputAction::ScrollCustomProvider { forward: true }
                        }
                        super::Modal::ModelEditor => InputAction::None,
                        super::Modal::Help => InputAction::ScrollDown,
                        super::Modal::Telemetry => InputAction::ModalDown,
                        super::Modal::UsageStats => InputAction::ScrollDown,
                        super::Modal::None => {
                            // Chat-surface ↓ is resolved by the Session view's
                            // scheme (ADR-0172).
                            InputAction::None
                        }
                    }
                }
                // PageUp / PageDown: Scroll transcript or modal body by one viewport page.
                KeyCode::PageUp => {
                    // Transcript paging on the bare chat surface and behind
                    // the pass-through permission sheet; a body-scrolling
                    // sheet or modal pages itself (claims, ADR-0173 §2).
                    if bare_chat_surface(&context)
                        || permission_sheet_up(&context)
                        || foreground_scrolls_own_body(&context)
                    {
                        InputAction::ScrollPageUp
                    } else {
                        InputAction::None
                    }
                }
                KeyCode::PageDown => {
                    // Transcript paging on the bare chat surface and behind
                    // the pass-through permission sheet; a body-scrolling
                    // sheet or modal pages itself (claims, ADR-0173 §2).
                    if bare_chat_surface(&context)
                        || permission_sheet_up(&context)
                        || foreground_scrolls_own_body(&context)
                    {
                        InputAction::ScrollPageDown
                    } else {
                        InputAction::None
                    }
                }
                KeyCode::Home
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && (bare_chat_surface(&context)
                            || permission_sheet_up(&context)
                            || foreground_scrolls_own_body(&context)) =>
                {
                    InputAction::ScrollTop
                }
                KeyCode::End
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && (bare_chat_surface(&context)
                            || permission_sheet_up(&context)
                            || foreground_scrolls_own_body(&context)) =>
                {
                    InputAction::ScrollBottom
                }
                // Bare Home / End:
                // - When a target or browse focus is active (or in permission sheet): scroll transcript to top / bottom.
                // - When composer or modal input field is active: move caret to line start / line end (readline convention).
                KeyCode::Home => {
                    if permission_sheet_up(&context)
                        || context.has_focused_target
                        || context.transcript_focused
                    {
                        InputAction::ScrollTop
                    } else if edits_input_field(&context) {
                        cursor_line_start(input, cursor_position);
                        InputAction::None
                    } else {
                        InputAction::None
                    }
                }
                KeyCode::End => {
                    if permission_sheet_up(&context)
                        || context.has_focused_target
                        || context.transcript_focused
                    {
                        InputAction::ScrollBottom
                    } else if edits_input_field(&context) {
                        cursor_line_end(input, cursor_position);
                        InputAction::None
                    } else {
                        InputAction::None
                    }
                }
                _ => InputAction::None,
            }
        }
        Event::Paste(text) => {
            // Terminal-level bracketed paste. Route the payload through the
            // same chip-or-inline logic as Ctrl+V on the main prompt, and
            // splice it inline into the focused field in the free-text
            // modals (provider editor, provider picker filter, history
            // search).
            if question_other_field(&context) {
                // The "Other" field owns its own buffer; route the bracketed
                // payload into it via the event loop's paste apply.
                InputAction::BracketedPaste(text)
            } else if edits_input_field(&context) {
                InputAction::BracketedPaste(text)
            } else {
                InputAction::None
            }
        }
        Event::Resize(..) => {
            // The event loop does the real work (redraw + re-arm mouse capture)
            // off this signal; here we just surface that the terminal geometry
            // changed rather than leaving it to the catch-all `None`.
            InputAction::TerminalResized
        }
        _ => InputAction::None,
    }
}

/// Resolve a screen coordinate to the block it belongs to.
pub fn resolve_block(layout_map: &LayoutMap, x: u16, y: u16) -> Option<(usize, usize)> {
    if let Some(r) = layout_map.region_at(x, y) {
        return Some((r.message_idx, r.block_idx));
    }
    if let Some(rect) = layout_map.composer_rect()
        && rect.x <= x
        && x < rect.x + rect.width
        && rect.y <= y
        && y < rect.y + rect.height
    {
        return Some((crate::model::layout::INPUT_MSG_IDX, 0));
    }
    None
}

#[cfg(test)]
mod tests;
