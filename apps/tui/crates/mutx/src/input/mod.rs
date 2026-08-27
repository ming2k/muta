//! Input handling: keyboard and mouse events mapped to semantic actions.

use crossterm::event::{Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind};

use crate::model::layout::{LayoutMap, SemanticCursor};
use crate::model::selection::SelectionDrag;

#[derive(Default)]
pub struct InputContext {
    pub active_modal: super::Modal,
    /// While the sessions picker is drilled into its info sub-view (`i`), the
    /// list-only keys (delete `d`, new `n`, info `i`) are inert — the sub-view
    /// is a read-only read-out.
    pub session_info_detail: bool,
    pub is_responding: bool,
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
    /// Whether the send queue holds at least one staged user message. While
    /// true, `↑` walks the queue pointer (before input history).
    pub has_queued: bool,
    /// Whether the queue pointer ([`crate::app::App::queue_pointer`]) is
    /// armed — the composer is currently a projection of a queue item. While
    /// true, `↓` steps the pointer back toward the newest item (and past it
    /// dissolves the pointer, restoring the draft) before any history role.
    pub queue_pointer_armed: bool,
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
    /// Whether the active modal is showing its in-modal keybindings page
    /// (`App::modal_keymap_open`). When true, `?` / Esc toggle or dismiss the
    /// page instead of acting on the underlying list, and Enter is inert.
    pub modal_keymap_open: bool,
    /// Focused field index of the provider editor, or `None` when that modal is
    /// not open. Every visible field borrows the composer line (Name / Base URL /
    /// Token as plain text, Model as a live filter), so printable keys always edit
    /// it. Mirrors `App::custom_field` while [`Self::active_modal`] is
    /// `super::Modal::CustomProvider`.
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
    /// Whether the Ctrl+R history modal is awaiting an explicit clear
    /// confirmation (`Ctrl+X` armed it). While true, every key either
    /// confirms (`y` / Enter) or cancels (anything else / Esc) — the modal
    /// owns the keyboard until the decision is made. Mirrors
    /// `App::history_clear_confirm`.
    pub history_clear_confirm: bool,
    /// Whether the `/host` dashboard's inline prompt is open (`p` prompt or
    /// `n` new session). While true, printable keys edit the prompt text and
    /// Enter submits it. Mirrors `App::host_prompting`.
    pub host_prompting: bool,
    /// Whether the custom color scheme hex editor in Settings is actively editing.
    pub config_custom_editing: bool,
    /// Whether a Web Search settings field (SearXNG URL / API key) in
    /// Settings is actively editing. Mirrors `App::websearch_editing`.
    pub config_websearch_editing: bool,
}

impl InputContext {
    /// Whether any provider-editor field is focused. Every visible field borrows
    /// the composer line (Name / Base URL / Token as plain text, Model as a live
    /// filter), so all are text fields now (ADR-0046 removed the Thinking toggle).
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
fn edits_input_field(context: &InputContext) -> bool {
    match context.active_modal {
        super::Modal::None | super::Modal::ModelEditor | super::Modal::InputInjection => true,
        super::Modal::Config => context.config_custom_editing || context.config_websearch_editing,
        super::Modal::Models | super::Modal::Connections => context.model_searching,
        super::Modal::HistorySearch => context.history_searching,
        // The provider editor edits the composer line on every visible field
        // (Name / Base URL / Token / Model all borrow it).
        super::Modal::CustomProvider => context.custom_text_field_focused(),
        _ => false,
    }
}

/// Whether the question modal's "Other" free-text row is the active editing
/// surface. Unlike the modals covered by [`edits_input_field`], the question
/// field does NOT borrow `App::input` (it owns `QuestionModel::other_text`),
/// so it must not be lumped into `edits_input_field` — that would also enable
/// the readline ops (Ctrl+A/E/W/U/K, Home/End, word-delete Backspace) which
/// all mutate `App::input` and would corrupt an unrelated buffer. Only the
/// paste paths (Ctrl+V, bracketed paste) route into this field.
fn question_other_field(context: &InputContext) -> bool {
    context.active_modal == super::Modal::Question && context.question_other_highlighted
}

/// Whether the active modal supports the in-modal keybindings page (`?`). These
/// are the centered list/info modals whose footer may collapse under width
/// pressure. Entry modals holding precious input (editor, custom provider,
/// oauth), decision modals (question, permission), and Help (already a
/// keybindings surface) are excluded.
fn supports_keymap_page(modal: super::Modal) -> bool {
    matches!(
        modal,
        super::Modal::Models
            | super::Modal::Connections
            | super::Modal::Sessions
            | super::Modal::HistorySearch
            | super::Modal::Tools
            | super::Modal::Mcp
            | super::Modal::Skills
            | super::Modal::Permissions
            | super::Modal::Config
            | super::Modal::Activity
            | super::Modal::Queue
            | super::Modal::TokenReport
            | super::Modal::UsageStats
            | super::Modal::Host
            | super::Modal::Btw
            | super::Modal::Tree
    )
}

/// Whether the active modal paints its own scrollable body — i.e. whether
/// `PageUp` / `PageDown` / `Ctrl+Up` / `Ctrl+Down` should scroll the modal
/// body (true) rather than fall through to transcript / caret handling
/// (false). This is the key→action mirror of `App::modal_scroll_field`: the
/// exact set of modals whose body scroll offset the event loop advances on a
/// `Scroll*` action. Kept in sync with that helper so a page key never routes
/// to a modal the loop can't scroll.
///
/// The inline permission sheet, the caret-owning text editors, and the
/// no-modal baseline are excluded: `PageUp`/`PageDown` there either scroll the
/// transcript behind the sheet or move the input caret, never a modal body.
fn scrolls_own_body(modal: super::Modal) -> bool {
    matches!(
        modal,
        super::Modal::Help
            | super::Modal::Activity
            | super::Modal::Permissions
            | super::Modal::Config
            | super::Modal::TokenReport
            | super::Modal::UsageStats
            | super::Modal::OauthPending
            | super::Modal::ProviderPreset
            | super::Modal::CustomProvider
            | super::Modal::Tools
            | super::Modal::Mcp
            | super::Modal::Skills
            | super::Modal::Sessions
            | super::Modal::Queue
            | super::Modal::Btw
            | super::Modal::HistorySearch
            | super::Modal::Connections
            | super::Modal::Models
            | super::Modal::Question
            | super::Modal::Tree
            | super::Modal::ViewSwitcher
    )
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
    /// Toggle live composer queue target mode (Steer ↔ FollowUp) while a round runs.
    ToggleComposerSendMode,
    /// Send a slash command.
    SendSlash(String),
    /// Activate the highlighted row of the **Models** picker: a flat
    /// (provider, model) pair. Falls through to the API-key setup modal when
    /// the target has no key. The Connections list has no activate concept —
    /// it only manages instances (`a`/`e`/`D`), leaving provider switching to
    /// this picker.
    ProviderPickerActivate,
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
    /// Move the suggestion highlight in the provider editor's Model filter field
    /// with `↑` / `↓`. `forward` = down.
    MoveCustomSuggestion {
        forward: bool,
    },
    /// Move the preset-chooser selection with `↑` / `↓`. `forward` = down.
    MovePresetChoice {
        forward: bool,
    },
    /// Open the provider editor seeded from the highlighted preset (`Enter`).
    SelectPreset,
    /// Cancel the preset chooser and return to the Connections list.
    CancelPresetChooser,
    /// Cancel the "+ Add provider → OAuth" browser flow (`Esc` while
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
    OpenModels,
    /// Open the Connections list (`/connections`) — the provider-instance
    /// management surface.
    OpenConnections,
    /// Refresh / rediscover available models for discovery-enabled providers from upstream.
    RefreshProviderModels,
    /// Open the add-connection preset chooser (`a` in the Connections modal) —
    /// the first step of adding a new provider connection.
    OpenPresetChooser,
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
    /// skill with a per-row detail expansion and an `r` reload. Reached via
    /// the `/skills` slash command (intercepted locally, never sent to the
    /// backend; `/skills list` / `/skills reload` with args still forward).
    /// The request is never forwarded — it only opens the overlay.
    OpenSkills,
    /// Toggle the detail expansion of the selected skill row in the skills
    /// modal. Bound to `Enter`.
    SkillsToggleDetail,
    /// Reload the skill registry from the skills modal by forwarding
    /// `/skills reload` to the backend. Bound to `r`.
    SkillsReload,
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
    /// Drill into the selected turn's model-round usage. Bound to `Enter`.
    TokenReportActivate,
    /// Delete the currently-selected session in the sessions picker.
    DeleteSelectedSession,
    /// Create a brand new session from the sessions picker ('n' / 'N').
    CreateNewSession,
    /// Open the session-info sub-view for the selected session ('i'). Shows the
    /// full last effective prompt, creation time, and message count.
    OpenSessionInfo,
    /// Close any modal.
    CloseModal,
    /// Toggle the in-modal keybindings page (`?` while a collapsible modal is
    /// open). Not a nested modal — the same `active_modal` stays open and the
    /// body is swapped for the full keymap. Esc / a second `?` closes it.
    ToggleModalKeymap,
    /// Scroll up.
    ScrollUp,
    /// Scroll down.
    ScrollDown,
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
    /// Move keyboard focus to the next activatable target. When no target is
    /// focused yet, focuses the first (oldest) step. Driven by `Ctrl+↓` and by
    /// `↓` while a step is already focused.
    FocusNextTarget,
    /// Move keyboard focus to the previous activatable target. When no target
    /// is focused yet, focuses the last (nearest-to-prompt) step. Driven by
    /// `Ctrl+↑` and by `↑` while a step is already focused.
    FocusPrevTarget,
    /// Activate the current keyboard-focused target (`Enter`).
    ActivateFocusedTarget,
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
    /// composer). Superseded at the top level by the non-destructive
    /// [`InputAction::QueuePointerPrev`] / [`InputAction::QueuePointerNext`]
    /// pair; kept for the queue modal's explicit pull-to-composer re-edit,
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
    /// `↑` at the top level with a non-empty outbox: arm/step the queue
    /// pointer toward older items (newest first). Non-destructive — the queue
    /// is untouched; the composer becomes an editable projection of the
    /// pointed-at item.
    QueuePointerPrev,
    /// `↓` while the queue pointer is armed: step toward newer items and,
    /// past the newest, dissolve the pointer (restoring the stashed draft).
    QueuePointerNext,
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
    /// Toggle the "full prompt" preview of the selected history entry inside
    /// the Ctrl+R modal. In preview mode the body shows the entry's complete
    /// (possibly multi-line) text; ↑/↓ re-renders the newly focused entry.
    HistoryTogglePreview,
    /// Arm the Ctrl+R modal's "clear all history" confirmation (`Ctrl+X`).
    /// The next `y` wipes the entire input history; any other key / Esc
    /// cancels. Kept out of the top level so a stray `Ctrl+X` can never wipe
    /// history while composing.
    HistoryClearAll,
    /// Confirm the armed clear-history action (`y` / Enter while
    /// `history_clear_confirm` is latched): wipe the entire input history.
    HistoryClearConfirm,
    /// Cancel the armed clear-history action (any other key / Esc).
    HistoryClearCancel,
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
    /// Advance to the next question, or submit all answers from the final
    /// question (Enter).
    QuestionSubmit,
    /// Return to the previous question (Shift+Tab).
    QuestionPrevious,
    /// Cancel the question modal.
    QuestionCancel,
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
            InputAction::OpenModels
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
fn normalized_cursor_byte(input: &str, cursor_position: usize) -> usize {
    let raw = input
        .char_indices()
        .nth(cursor_position.min(input.chars().count()))
        .map(|(byte, _)| byte)
        .unwrap_or(input.len());
    mutx_engine::text::floor_grapheme_boundary(input, raw)
}

fn char_index_at_byte(input: &str, byte: usize) -> usize {
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
fn cursor_line_up(input: &str, cursor_position: &mut usize) -> bool {
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
fn cursor_line_down(input: &str, cursor_position: &mut usize) -> bool {
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
                // The wheel always scrolls the body of whatever modal owns the
                // surface (or the transcript when none does). The event loop's
                // ScrollUp/ScrollDown handlers translate it per-modal — including
                // the question modal, whose body scroll is decoupled from the ↑/↓
                // highlight so wheeling browses a long option list without moving
                // the selection cursor.
                MouseEventKind::ScrollUp => InputAction::ScrollUp,
                MouseEventKind::ScrollDown => InputAction::ScrollDown,
                MouseEventKind::Down(MouseButton::Left) => {
                    // The permission sheet replaces the composer but leaves the
                    // transcript above fully interactive, so a click there can
                    // still toggle steps, drag-select text, follow links, etc.
                    // The sheet itself has no click targets (its buttons are
                    // keyboard-driven) and covers only the composer/hint slot,
                    // which has no registered transcript region, so a press
                    // landing on it resolves to nothing and stays inert.
                    if matches!(
                        context.active_modal,
                        super::Modal::None | super::Modal::Permission
                    ) {
                        drag.start(SemanticCursor::new(0, 0, 0));
                        InputAction::SelectionStart { x, y }
                    } else if context.active_modal == super::Modal::Question
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
                        && matches!(
                            context.active_modal,
                            super::Modal::None
                                | super::Modal::Permission
                                | super::Modal::OauthPending
                        )
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
                    if matches!(
                        context.active_modal,
                        super::Modal::None | super::Modal::Permission
                    ) {
                        InputAction::SelectBlock { x, y }
                    } else {
                        InputAction::None
                    }
                }
                MouseEventKind::Down(MouseButton::Right) => {
                    // Right-click opens detail/feedback for interactive
                    // transcript elements. Allowed during a permission prompt
                    // because the transcript stays interactive.
                    if matches!(
                        context.active_modal,
                        super::Modal::None | super::Modal::Permission
                    ) {
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
                    if matches!(
                        context.active_modal,
                        super::Modal::None | super::Modal::Permission
                    ) {
                        InputAction::Hover { x, y }
                    } else {
                        InputAction::None
                    }
                }
                _ => InputAction::None,
            }
        }
        Event::Key(key) => {
            // While the Ctrl+R clear-history confirmation is armed, the modal
            // owns EVERY key — `y` / Enter confirm the wipe, anything else —
            // Esc and even global shortcuts like Ctrl+C included — cancels it.
            // This must run before the global-binding registry below, whose
            // `Gate::Always` entries (Ctrl+C → copy/clear) would otherwise
            // fire and let a stray keystroke escape the question.
            if context.history_clear_confirm {
                return match key.code {
                    KeyCode::Char('y') => InputAction::HistoryClearConfirm,
                    KeyCode::Enter => InputAction::HistoryClearConfirm,
                    _ => InputAction::HistoryClearCancel,
                };
            }

            // Global shortcuts are resolved through the unified keybinding
            // registry (`tui::keymap`), the single source of truth shared with
            // the Help modal. Anything resolved here wins over the contextual
            // match arms below. Keys not declared globally (text editing,
            // modal-internal selection, Esc's modal hierarchy, …) return `None`
            // and fall through to the contextual handling.
            if let Some(global) = super::keymap::Registry::new().resolve(key, context.active_modal)
            {
                return global;
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

            match key.code {
                KeyCode::Esc => {
                    // In-modal keymap page: Esc closes the page first, never
                    // the underlying modal. A second Esc then follows the
                    // normal modal dismiss path.
                    if context.modal_keymap_open
                        && context.active_modal != super::Modal::None
                        && context.active_modal != super::Modal::Help
                    {
                        return InputAction::ToggleModalKeymap;
                    }
                    if context.active_modal == super::Modal::Permission {
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
                    } else if context.active_modal == super::Modal::Question {
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
                    } else if context.active_modal == super::Modal::InputInjection {
                        InputAction::InputCancel
                    } else if context.active_modal == super::Modal::HistorySearch {
                        // The history panel floats above a live composer that is
                        // permanently the filter field, so there is no
                        // browse/search distinction to step out of: a single Esc
                        // closes the panel and restores the stashed draft (the
                        // query is discarded, since it was only ever a filter).
                        InputAction::CloseModal
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
                    } else if context.in_side_view {
                        // `/btw` aside view (ADR-0103 §2): Esc interrupts the
                        // *viewed aside's* round — the same armed
                        // press-twice-to-confirm contract as the main view —
                        // and never leaves the view. Leaving is Ctrl+C's one
                        // job. Takes priority over focus clearing and
                        // completion so the interrupt intent is unambiguous.
                        InputAction::InterruptSide
                    } else if context.in_runner_view {
                        // Runner zoom: Esc returns to the parent view.
                        // Takes priority over focus clearing so one Esc
                        // always exits the zoom, even if a step inside the
                        // runner is keyboard-focused.
                        InputAction::ExitRunner
                    } else if context.has_focused_target {
                        // A transcript step is focused: Esc clears the focus
                        // and hands every key back to the input box.
                        InputAction::ClearFocusedTarget
                    } else if context.completion_kind != super::CompletionKind::None
                        && context.suggestion_count > 0
                    {
                        // A completion popup (slash command or `@path`) is
                        // open: Esc dismisses it without touching the input
                        // text. The popup stays hidden until the next edit
                        // clears the dismissal latch, so Esc then ↑/↓ walks
                        // history instead of suggestions.
                        InputAction::CloseCompletion
                    } else if context.is_responding {
                        InputAction::Interrupt
                    } else {
                        InputAction::None
                    }
                }
                KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    // Ctrl+R is a declared global binding (registry →
                    // OpenHistory). It only reaches this arm when a modal is
                    // open (the gate blocks it at the top level), so it is a
                    // no-op here.
                    InputAction::None
                }
                // Ctrl+X inside the Ctrl+R panel arms the clear-history
                // confirmation (next `y` wipes the whole history; any other
                // key cancels). Nowhere else does Ctrl+X mean anything, so it
                // stays a no-op at the top level and inside other modals —
                // a stray Ctrl+X while composing can never wipe history.
                KeyCode::Char('x') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if context.active_modal == super::Modal::HistorySearch {
                        InputAction::HistoryClearAll
                    } else {
                        InputAction::None
                    }
                }
                // F1 is a declared global binding (registry → OpenHelp) and
                // only reaches this arm inside a modal, where it is a no-op.
                KeyCode::F(1) => InputAction::None,
                // Ctrl+P is a declared global binding (registry →
                // ToggleQueueBlock) at the top level. Inside the Queue modal
                // it also toggles the block so the user can resume without
                // closing the list. Inside any other modal it is a no-op.
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
                    // While the in-modal keymap page is open, Enter is inert
                    // (Esc / `?` close it) — never activate the hidden list row.
                    if context.modal_keymap_open && supports_keymap_page(context.active_modal) {
                        return InputAction::ToggleModalKeymap;
                    }
                    match context.active_modal {
                        super::Modal::Models => InputAction::ProviderPickerActivate,
                        // Connections is a pure management surface: Enter is
                        // inert here (no activate concept — switching the
                        // active provider is the Models picker's job). `a`/`e`/
                        // `D` are the management shortcuts, handled as printable
                        // chars below.
                        super::Modal::Connections => InputAction::None,
                        super::Modal::ModelEditor => InputAction::SubmitModelEditor,
                        super::Modal::ProviderPreset => InputAction::SelectPreset,
                        super::Modal::OauthPending => InputAction::CopyOauthContent {
                            target: OauthCopyTarget::Selected,
                        },
                        super::Modal::CustomProvider => InputAction::SubmitCustomProvider,
                        super::Modal::HistorySearch => InputAction::HistoryInsert,
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
                        super::Modal::Permission => InputAction::PermissionSubmit,
                        super::Modal::Question => InputAction::QuestionSubmit,
                        super::Modal::InputInjection => InputAction::InputSubmit,
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
                        super::Modal::ViewSwitcher => InputAction::ViewSwitchActivate,
                        super::Modal::Config => InputAction::ConfigActivate,
                        super::Modal::Activity => InputAction::CloseModal,
                        super::Modal::TokenReport => InputAction::TokenReportActivate,
                        super::Modal::UsageStats => InputAction::CloseModal,
                        super::Modal::None => {
                            if context.has_focused_target {
                                return InputAction::ActivateFocusedTarget;
                            }
                            // Slash-only: pressing Enter on a unique prefix
                            // auto-accepts the first suggestion rather than
                            // sending `/go` as a (rejected) command. Path
                            // mentions skip this so Enter still sends the message.
                            if context.completion_kind == super::CompletionKind::Slash
                                && context.suggestion_count > 0
                                && context.suggestion_index.is_none()
                                && !context.has_exact_suggestion
                            {
                                return InputAction::CommitSuggestion("0".to_string());
                            }
                            // If a completion menu is open and the user has
                            // highlighted an item (via ↑/↓ or Tab cycling),
                            // Enter accepts that item rather than sending the
                            // partial input. Applies to both slash commands and
                            // `@path` mentions. An explicit highlight is a
                            // stronger signal than the raw text in the box, so
                            // this wins over the exact-match slash fast path
                            // below.
                            if let Some(i) = context.suggestion_index
                                && context.completion_kind != super::CompletionKind::None
                            {
                                return InputAction::CommitSuggestion(i.to_string());
                            }
                            let text = std::mem::take(input);
                            *cursor_position = 0;
                            if text.starts_with('/') {
                                // Match on the trimmed text so a slash command
                                // typed with a trailing space (e.g. the user
                                // typed `/models ` themselves) still hits the
                                // exact-match arm instead of silently no-op'ing.
                                match text.trim() {
                                    "/models" => InputAction::OpenModels,
                                    "/connections" => InputAction::OpenConnections,
                                    "/permissions" => InputAction::OpenPermissions,
                                    "/tools" => InputAction::OpenTools,
                                    "/usage" => InputAction::OpenUsage,
                                    "/mcp" => InputAction::OpenMcp,
                                    "/skills" => InputAction::OpenSkills,
                                    // Bare `/settings` (or `/config`) opens the manager modal
                                    // locally; `/settings reload` (and any other
                                    // argument form) is a backend command —
                                    // it falls through to SendSlash like
                                    // `/skills reload` does.
                                    "/settings" | "/config" => InputAction::OpenConfig,
                                    "/exit" => InputAction::Quit,
                                    _ => InputAction::SendSlash(text),
                                }
                            } else if !text.is_empty() {
                                InputAction::SendChat(text)
                            } else {
                                InputAction::None
                            }
                        }
                    }
                }
                KeyCode::Tab => {
                    if context.active_modal == super::Modal::None
                        && context.completion_kind != super::CompletionKind::None
                        && context.suggestion_count > 0
                        // A fully-typed command is resolved: its completion popup
                        // is hidden and its keys return to their ordinary roles, so
                        // Tab must not invisibly cycle sibling candidates (e.g.
                        // `/session` → `/sessions`). Type the target command to
                        // reach a sibling.
                        && !context.has_exact_suggestion
                        && !context.completion_dismissed
                    {
                        // A slash/path suggestion menu is open with a row
                        // highlighted: Tab commits it — the same gesture as
                        // Enter, down to sharing the terminal-accept
                        // semantics in `accept_completion`. (The highlight
                        // is now anchored to the first candidate whenever
                        // the popup is visible, so this fires on a plain
                        // Tab with no prior navigation.)
                        let idx = context.suggestion_index.unwrap_or(0);
                        InputAction::CommitSuggestion(idx.to_string())
                    } else if context.active_modal == super::Modal::None
                        && context.completion_kind != super::CompletionKind::None
                        && context.completion_dismissed
                        && context.has_trigger_text
                    {
                        // Esc closed the popup but the composer still holds a
                        // completion trigger (a partial `/command` or an
                        // `@mention`): Tab re-opens it without accepting
                        // anything. Tab is the toggle's other half — Esc
                        // closes, Tab reopens — so the user can always get
                        // the menu back without re-editing the text.
                        InputAction::ReopenCompletion
                    } else if context.active_modal == super::Modal::ModelEditor {
                        // Tab cycles focus between the editor's API-key and
                        // model-id fields.
                        InputAction::ModelEditorNextField
                    } else if context.active_modal == super::Modal::Config {
                        InputAction::ConfigFocusToggle
                    } else if context.active_modal == super::Modal::CustomProvider {
                        // Tab advances through the editor's visible fields.
                        InputAction::CustomProviderNextField
                    } else if context.active_modal == super::Modal::HistorySearch {
                        // Tab toggles the full-prompt preview of the selected
                        // entry. The fuzzy filter is a free-text field, so an
                        // alpha key would clash; Tab is the unambiguous gesture.
                        InputAction::HistoryTogglePreview
                    } else if context.active_modal == super::Modal::Host {
                        // The dashboard has two panes: Tab moves focus between
                        // the session list and the detail read-out.
                        InputAction::HostFocusToggle
                    } else if context.active_modal == super::Modal::OauthPending {
                        InputAction::CycleOauthSelection
                    } else if context.is_responding && context.active_modal == super::Modal::None {
                        // While a round runs, Tab toggles between Steer (turn-boundary injection)
                        // and FollowUp (queued next-round dispatch).
                        InputAction::ToggleComposerSendMode
                    } else {
                        // No completion open (or it was dismissed without a
                        // trigger left in the text) and no modal field to
                        // cycle: Tab is a no-op. (There is no zone switching:
                        // focus is toggled with Ctrl+Up/Ctrl-Down, never Tab.)
                        InputAction::None
                    }
                }
                KeyCode::BackTab => {
                    // Shift+Tab steps backward through multi-field / multi-page
                    // modal state; elsewhere it is a no-op (transcript focus
                    // uses Ctrl+Up/Ctrl-Down, not Tab).
                    if context.active_modal == super::Modal::CustomProvider {
                        InputAction::CustomProviderPrevField
                    } else if context.active_modal == super::Modal::Config {
                        InputAction::ConfigFocusToggle
                    } else if context.active_modal == super::Modal::Question {
                        InputAction::QuestionPrevious
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
                // logical line (readline `kill-line`). Stops at the next
                // newline so multi-line drafts keep their other lines.
                KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if edits_input_field(&context) {
                        let mut end = *cursor_position;
                        cursor_line_end(input, &mut end);
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
                KeyCode::Char(c) => {
                    // The quick switcher's filter owns every printable key
                    // while it is up (ADR-0133 phase 5) — before any other
                    // consumer, so `?`-help and list-action keys never
                    // steal filter characters.
                    if context.active_modal == super::Modal::ViewSwitcher {
                        return InputAction::ViewSwitcherFilter { ch: c };
                    }
                    // `?` opens help from the top level when the input box is
                    // empty — mirrors the conventional help key without ever
                    // swallowing a `?` the user is typing. Like Ctrl+H it is
                    // a fallback for terminals/multiplexers that lose the
                    // Kitty protocol (see the Ctrl+H note above).
                    if context.active_modal == super::Modal::None && c == '?' && input.is_empty() {
                        return InputAction::OpenHelp;
                    }
                    // In-modal keymap page: `?` toggles the page on collapsible
                    // list modals (Provider / Sessions / History / …). Not
                    // offered while a free-text search field is active, so a
                    // typed `?` in a filter still inserts. Help itself has no
                    // nested keymap. While the page is open, `?` closes it.
                    if c == '?'
                        && context.active_modal != super::Modal::None
                        && supports_keymap_page(context.active_modal)
                        && !edits_input_field(&context)
                    {
                        return InputAction::ToggleModalKeymap;
                    }
                    // While the keymap page is open, swallow other list-action
                    // keys so they do not act on the hidden list.
                    if context.modal_keymap_open && supports_keymap_page(context.active_modal) {
                        return InputAction::None;
                    }
                    // Sibling runner navigation works in both zones (it is a
                    // runner view feature, not a typing-navigation thing)
                    // but only when no text is being composed.
                    if context.active_modal == super::Modal::None
                        && context.in_runner_view
                        && input.is_empty()
                    {
                        match c {
                            '[' => return InputAction::PrevSibling,
                            ']' => return InputAction::NextSibling,
                            _ => {}
                        }
                    }
                    if context.active_modal == super::Modal::Question
                        && c == ' '
                        && !context.question_other_highlighted
                    {
                        return InputAction::QuestionToggle;
                    }
                    // Space inside the tools manager toggles the selected
                    // tool's enabled flag.
                    if context.active_modal == super::Modal::Tools && c == ' ' {
                        return InputAction::SessionActivate;
                    }
                    // Space toggles the selected server in the MCP manager;
                    // `r` reconnects it.
                    if context.active_modal == super::Modal::Mcp && c == ' ' {
                        return InputAction::McpToggle;
                    }
                    if context.active_modal == super::Modal::Mcp && c == 'r' {
                        return InputAction::McpReconnect;
                    }
                    // The OAuth pending sheet copies its primary content: `c`
                    // copies the device code (the value to paste at
                    // github.com/login/device), `u` copies the verification URL.
                    // Mouse drag-select does not reach modal body text (muta
                    // captures mouse events), so these keys are the copy path.
                    if context.active_modal == super::Modal::OauthPending && c == 'c' {
                        return InputAction::CopyOauthContent {
                            target: OauthCopyTarget::UserCode,
                        };
                    }
                    if context.active_modal == super::Modal::OauthPending && c == 'u' {
                        return InputAction::CopyOauthContent {
                            target: OauthCopyTarget::Url,
                        };
                    }
                    if context.active_modal == super::Modal::OauthPending && (c == ' ' || c == 'y')
                    {
                        return InputAction::CopyOauthContent {
                            target: OauthCopyTarget::Selected,
                        };
                    }
                    // `r` in the skills modal reloads the skill registry.
                    if context.active_modal == super::Modal::Skills && c == 'r' {
                        return InputAction::SkillsReload;
                    }
                    // Space inside the permissions manager revokes the
                    // selected rule.
                    if context.active_modal == super::Modal::Permissions && c == ' ' {
                        return InputAction::PermissionsActivate;
                    }
                    if context.active_modal == super::Modal::Config
                        && !context.config_custom_editing
                        && c == ' '
                    {
                        return InputAction::ConfigActivate;
                    }
                    if context.active_modal == super::Modal::Question
                        && let Some(d) = c.to_digit(10)
                        && (1..=9).contains(&d)
                    {
                        return InputAction::QuestionSelect(d as usize);
                    }
                    // A focused transcript step does not capture typing: with
                    // no separate browse mode, printable characters always fall
                    // through to the input box below (the focus highlight stays
                    // until Esc / Enter). `Enter` activates the focused step;
                    // `Space` just inserts a space.
                    if matches!(
                        context.active_modal,
                        super::Modal::Models | super::Modal::Connections
                    ) && !context.model_searching
                        && c == '/'
                    {
                        // Browse mode: `/` opens the search sub-layer rather than
                        // inserting a literal slash — mirrors the history modal.
                        InputAction::ModelEnterSearch
                    } else if context.active_modal == super::Modal::Models
                        && !context.model_searching
                        && c == '*'
                    {
                        // Models browse mode only: star the highlighted MODEL as
                        // a favorite (favorite is model-level, ADR-0046). In the
                        // search sub-layer `*` is a query char; the Connections
                        // list has no favorite concept.
                        InputAction::ProviderPickerToggleFavorite
                    } else if matches!(
                        context.active_modal,
                        super::Modal::Connections | super::Modal::Models
                    ) && !context.model_searching
                        && c == 'a'
                    {
                        // Connections / Models browse mode: `a` opens the add-provider
                        // preset chooser (the first step of adding a
                        // connection). In the search sub-layer `a` is a query
                        // char.
                        InputAction::OpenPresetChooser
                    } else if matches!(
                        context.active_modal,
                        super::Modal::Models | super::Modal::Connections
                    ) && !context.model_searching
                        && c == 'e'
                    {
                        // Connections: edit the highlighted provider. Models:
                        // edit the highlighted model's per-model settings.
                        InputAction::OpenModelEditor
                    } else if matches!(
                        context.active_modal,
                        super::Modal::Models | super::Modal::Connections
                    ) && !context.model_searching
                        && (c == 'r' || c == 'R')
                    {
                        // Refresh / rediscover models from upstream.
                        InputAction::RefreshProviderModels
                    } else if context.active_modal == super::Modal::Connections
                        && !context.model_searching
                        && c == 'D'
                    {
                        // Connections browse mode: `Shift+D` deletes the entire
                        // highlighted custom provider (ignored for built-ins by
                        // the handler).
                        InputAction::DeleteProvider
                    } else if context.active_modal == super::Modal::Sessions
                        && !context.session_info_detail
                        && c == 'd'
                    {
                        InputAction::DeleteSelectedSession
                    } else if context.active_modal == super::Modal::Sessions
                        && !context.session_info_detail
                        && (c == 'n' || c == 'N')
                    {
                        InputAction::CreateNewSession
                    } else if context.active_modal == super::Modal::Sessions
                        && !context.session_info_detail
                        && c == 'i'
                    {
                        InputAction::OpenSessionInfo
                    } else if context.active_modal == super::Modal::Host && c == 'a' {
                        // Dashboard: attach to the selected session (detach +
                        // re-attach). Enter only previews; `a` is the attach.
                        #[allow(clippy::needless_return)]
                        return InputAction::HostSwitchSelected;
                    } else if context.active_modal == super::Modal::Host && c == 'i' {
                        // Dashboard: interrupt the selected session's round.
                        // Early `return` so this keypress is never re-inserted
                        // as prompt text by the arms below (the dashboard keys
                        // are actions, never literal input).
                        #[allow(clippy::needless_return)]
                        return InputAction::HostInterruptSelected;
                    } else if context.active_modal == super::Modal::Host
                        && c == 'k'
                        && !context.host_prompting
                    {
                        // Dashboard dock: kill the selection. Two-press
                        // confirm — the first press arms, the second fires.
                        // Inert while the inline prompt is open (`k` is then
                        // literal text; `/kill` is the prompt's spelling).
                        #[allow(clippy::needless_return)]
                        return InputAction::HostKillSelected;
                    } else if context.active_modal == super::Modal::Host
                        && c == 's'
                        && !context.host_prompting
                    {
                        // Dashboard dock: suspend the selection (park it in
                        // memory; the next attach resumes it). Same
                        // prompt-open exclusion as `k`.
                        #[allow(clippy::needless_return)]
                        return InputAction::HostSuspendSelected;
                    } else if context.active_modal == super::Modal::Host && c == 'p' {
                        // Dashboard: open the inline prompt-to-session field.
                        #[allow(clippy::needless_return)]
                        return InputAction::HostPromptOpen;
                    } else if context.active_modal == super::Modal::Host && c == 'n' {
                        // Dashboard: open the inline new-session field.
                        #[allow(clippy::needless_return)]
                        return InputAction::HostNewSession;
                    } else if context.active_modal == super::Modal::Host && !context.host_prompting
                    {
                        // Dashboard: any other printable key opens the console
                        // composer seeded with that char — the surface is a
                        // command line, so typing `@3 …` or `/help` begins
                        // directly without a `p` first. (`p`/`n` seeds
                        // nothing: they are the explicit openers above, and
                        // their role defaults differ.)
                        #[allow(clippy::needless_return)]
                        return InputAction::HostPromptSeed(c);
                    } else if context.active_modal == super::Modal::Queue && c == 'D' {
                        // Queue modal: `Shift+D` deletes the highlighted item
                        // outright (the queue is auto-blocked on open, so a
                        // mid-delete auto-drain can't race the user). No
                        // confirm step — recall from history recovers the
                        // text, and the queue is a staging surface, not
                        // permanent storage.
                        InputAction::QueueDelete
                    } else if context.active_modal == super::Modal::Btw && c == 'D' {
                        // Asides modal (ADR-0103 §5): `Shift+D` closes and
                        // discards the highlighted aside — cancels its round,
                        // drops it from the list, and deletes its session
                        // files. Deliberate destruction stays a two-surface
                        // gesture (uppercase, like the queue's delete) so a
                        // stray keypress never loses a background aside.
                        InputAction::BtwCloseSelected
                    } else if context.active_modal == super::Modal::Queue && c == 'K' {
                        // Queue modal: `K` moves the highlighted item toward
                        // the front (next to pop). Vim convention.
                        InputAction::QueueMoveItem { delta: -1 }
                    } else if context.active_modal == super::Modal::Queue && c == 'J' {
                        // Queue modal: `J` moves the highlighted item toward
                        // the tail. Vim convention.
                        InputAction::QueueMoveItem { delta: 1 }
                    } else if context.active_modal == super::Modal::Permissions && c == 'c' {
                        InputAction::PermissionsClearAll
                    } else if c == ' '
                        && context.active_modal == super::Modal::ModelEditor
                        && matches!(context.editor_field, Some(2 | 3 | 4))
                    {
                        // Space on the key editor's non-text fields instead
                        // of inserting a space. Field 2 (thinking) is a binary
                        // toggle; fields 3/4 (capability overrides, ADR-0149)
                        // are tri-state: inherit → force on → force off.
                        match context.editor_field {
                            Some(3) => InputAction::ModelEditorVisionCycle,
                            Some(4) => InputAction::ModelEditorToolCycle,
                            _ => InputAction::ModelEditorThinkingToggle,
                        }
                    } else if c.is_ascii_digit()
                        && c != '0'
                        && context.active_modal == super::Modal::ModelEditor
                        && context.editor_field == Some(1)
                    {
                        // A digit on the effort field jumps straight to that
                        // ladder rung (`1` = shallowest … `7` = deepest) instead
                        // of inserting into the borrowed input line — the flat
                        // segmented selector makes direct selection the natural
                        // gesture. `0` is not a tier.
                        let index = c as usize - '1' as usize;
                        InputAction::ModelEditorEffortJump { index }
                    } else if context.active_modal == super::Modal::Question {
                        InputAction::QuestionInsertChar(c)
                    } else if context.active_modal == super::Modal::Config
                        && context.config_custom_editing
                        && c != '#'
                        && !c.is_ascii_hexdigit()
                    {
                        // Custom colors are strict hex fields. Ignore other
                        // printable input instead of letting the user build an
                        // impossible value that can never be saved.
                        InputAction::None
                    } else if edits_input_field(&context)
                        && !(context.active_modal == super::Modal::ModelEditor
                            && matches!(context.editor_field, Some(2 | 3 | 4)))
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
                    if context.active_modal == super::Modal::ViewSwitcher {
                        // The switcher's own filter query (phase 5) — never
                        // the composer.
                        InputAction::ViewSwitcherBackspace
                    } else if context.active_modal == super::Modal::Question {
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
                    if context.active_modal == super::Modal::ViewSwitcher {
                        return InputAction::ViewCloseSelected;
                    } else if edits_input_field(&context)
                        && *cursor_position < input.chars().count()
                    {
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
                    if context.active_modal == super::Modal::Permission {
                        return InputAction::ModalUp;
                    }
                    // In the model editor's effort field, ← cycles the effort
                    // level down (wrapping). Only when field 1 is focused.
                    if context.active_modal == super::Modal::ModelEditor
                        && context.editor_field == Some(1)
                    {
                        return InputAction::ModelEditorEffortCycle { delta: -1 };
                    }
                    // In the provider editor every field borrows the composer
                    // line, so ←/→ move the caret within the focused field.
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
                    if context.active_modal == super::Modal::Permission {
                        return InputAction::ModalDown;
                    }
                    // Effort field: → cycles the level up (wrapping).
                    if context.active_modal == super::Modal::ModelEditor
                        && context.editor_field == Some(1)
                    {
                        return InputAction::ModelEditorEffortCycle { delta: 1 };
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
                // Ctrl+↑ / Ctrl+↓: the gesture that drives transcript item
                // focus. From the input box it focuses the step closest to the
                // prompt (the last interactive target → `FocusPrevTarget` lands
                // on the last entry when nothing is focused yet); once a step is
                // focused it cycles like the bare arrows. This keeps the bare
                // ↑/↓ free for history / caret motion until a step is focused.
                // No-op while a modal owns focus.
                KeyCode::Up
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && context.active_modal == super::Modal::None =>
                {
                    InputAction::FocusPrevTarget
                }
                KeyCode::Down
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && context.active_modal == super::Modal::None =>
                {
                    InputAction::FocusNextTarget
                }
                // Ctrl+↑ / Ctrl+↓ inside a modal scroll the modal body by one
                // page — the same gesture a pager or editor binds to a
                // half-page jump. Mirrors PageUp / PageDown so users have both
                // the dedicated keys and the chord (useful on keyboards without
                // Page keys, and consistent with the transcript's Ctrl+↑/↓
                // focus gesture on the no-modal baseline). Routed through the
                // shared `Scroll*` actions so the same per-modal field advances
                // as every other scroll input. Must precede the bare ↑/↓ arms
                // because those match any modifier.
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
                    // While the in-modal keymap page is open, ↑/↓ scroll it.
                    if context.modal_keymap_open && supports_keymap_page(context.active_modal) {
                        return InputAction::ScrollUp;
                    }
                    match context.active_modal {
                        super::Modal::Models | super::Modal::Connections => InputAction::ModalUp,
                        super::Modal::HistorySearch => InputAction::ModalUp,
                        super::Modal::Sessions => InputAction::ModalUp,
                        super::Modal::Host => InputAction::ModalUp,
                        super::Modal::Question => InputAction::QuestionUp,
                        super::Modal::Permission => {
                            // Browse zone: walk transcript targets. Compose zone:
                            // scroll the expanded details, otherwise fall through
                            // to a transcript scroll so the history stays readable
                            // even while a prompt is pending.
                            if context.has_focused_target {
                                InputAction::FocusPrevTarget
                            } else if context.permission_show_details {
                                InputAction::PermissionDetailsUp
                            } else {
                                InputAction::ScrollUp
                            }
                        }
                        super::Modal::Activity => InputAction::ScrollUp,
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
                            InputAction::MoveCustomSuggestion { forward: false }
                        }
                        super::Modal::ModelEditor | super::Modal::InputInjection => {
                            InputAction::None
                        }
                        super::Modal::Help => InputAction::ScrollUp,
                        super::Modal::TokenReport => InputAction::ModalUp,
                        super::Modal::UsageStats => InputAction::ScrollUp,
                        super::Modal::None => {
                            if context.has_focused_target {
                                InputAction::FocusPrevTarget
                            } else if context.completion_kind != super::CompletionKind::None
                                && context.suggestion_count > 0
                                // A fully-typed known `/command` is a *resolved*
                                // state — the composer paints it in bold + accent
                                // and the completion popup has nothing left to
                                // navigate (its exact match is the text already in
                                // the box). In that state ↑ keeps its ordinary
                                // history role instead of being captured as a
                                // no-op suggestion move, so switching to a command
                                // never deadens the arrow keys.
                                && !context.has_exact_suggestion
                            {
                                InputAction::SuggestPrev
                            } else if context.has_queued || context.queue_pointer_armed {
                                // The outbox holds next-round items: ↑ walks a
                                // non-destructive **pointer** over them (newest
                                // first), projecting each item into the composer
                                // for editing. Enter writes the edit back into
                                // the pointed-at item in place — the queue's
                                // length and order are untouched. Only an
                                // exhausted queue hands ↑ on to input history.
                                // (`queue_pointer_armed` keeps the gesture alive
                                // even if the queue momentarily reads empty —
                                // e.g. the target vanished mid-edit.)
                                InputAction::QueuePointerPrev
                            } else if cursor_line_up(input, cursor_position) {
                                // Multi-line draft: ↑ first walks the caret to the
                                // previous line (preserving the column). Only when
                                // the caret is already on the first line does ↑
                                // hand off to input-history navigation below.
                                InputAction::None
                            } else {
                                InputAction::HistoryPrev
                            }
                        }
                    }
                }
                KeyCode::Down => {
                    if context.modal_keymap_open && supports_keymap_page(context.active_modal) {
                        return InputAction::ScrollDown;
                    }
                    match context.active_modal {
                        super::Modal::Models | super::Modal::Connections => InputAction::ModalDown,
                        super::Modal::HistorySearch => InputAction::ModalDown,
                        super::Modal::Sessions => InputAction::ModalDown,
                        super::Modal::Host => InputAction::ModalDown,
                        super::Modal::Question => InputAction::QuestionDown,
                        super::Modal::Permission => {
                            if context.has_focused_target {
                                InputAction::FocusNextTarget
                            } else if context.permission_show_details {
                                InputAction::PermissionDetailsDown
                            } else {
                                InputAction::ScrollDown
                            }
                        }
                        super::Modal::Activity => InputAction::ScrollDown,
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
                            InputAction::MoveCustomSuggestion { forward: true }
                        }
                        super::Modal::ModelEditor | super::Modal::InputInjection => {
                            InputAction::None
                        }
                        super::Modal::Help => InputAction::ScrollDown,
                        super::Modal::TokenReport => InputAction::ModalDown,
                        super::Modal::UsageStats => InputAction::ScrollDown,
                        super::Modal::None => {
                            if context.has_focused_target {
                                InputAction::FocusNextTarget
                            } else if context.completion_kind != super::CompletionKind::None
                                && context.suggestion_count > 0
                                // Mirror of the ↑ arm: an exact-match command is
                                // resolved, so ↓ walks history forward rather than
                                // cycling a single-candidate popup that cannot move.
                                && !context.has_exact_suggestion
                            {
                                InputAction::SuggestNext
                            } else if context.queue_pointer_armed {
                                // The composer is a projection of a queue item:
                                // ↓ steps the queue pointer toward newer items
                                // (dissolving it — restoring the draft — past the
                                // newest) instead of touching history.
                                InputAction::QueuePointerNext
                            } else if cursor_line_down(input, cursor_position) {
                                // Multi-line draft: ↓ first walks the caret to the
                                // next line (preserving the column). Only when the
                                // caret is already on the last line does ↓ hand
                                // off to input-history navigation below.
                                InputAction::None
                            } else {
                                InputAction::HistoryNext
                            }
                        }
                    }
                }
                // PageUp / PageDown scroll by one viewport page. On the
                // no-modal baseline and the inline permission sheet this means
                // the transcript behind the prompt; for every modal that paints
                // its own scrollable body it means that body. Modals that
                // neither scroll the transcript nor own a body (the caret-
                // owning text editors) fall through to caret / no-op handling
                // via the `_` arm below.
                KeyCode::PageUp
                    if context.active_modal == super::Modal::None
                        || context.active_modal == super::Modal::Permission
                        || scrolls_own_body(context.active_modal) =>
                {
                    InputAction::ScrollPageUp
                }
                KeyCode::PageDown
                    if context.active_modal == super::Modal::None
                        || context.active_modal == super::Modal::Permission
                        || scrolls_own_body(context.active_modal) =>
                {
                    InputAction::ScrollPageDown
                }
                KeyCode::Home => {
                    // A focused step disambiguates Home from caret motion, so it
                    // no longer clashes with conversation scrolling:
                    //   - Permission modal / a step is focused: scroll to top.
                    //   - Otherwise (free text): move the input caret to the
                    //     start of the current line.
                    if context.active_modal == super::Modal::Permission
                        || (context.active_modal == super::Modal::None
                            && context.has_focused_target)
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
                    if context.active_modal == super::Modal::Permission
                        || (context.active_modal == super::Modal::None
                            && context.has_focused_target)
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
    layout_map
        .region_at(x, y)
        .map(|r| (r.message_idx, r.block_idx))
}

#[cfg(test)]
mod tests;
