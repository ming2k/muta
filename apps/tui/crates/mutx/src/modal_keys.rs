//! Per-modal keybinding schemes (ADR-0172).
//!
//! Each modal owns the single-letter *verb* keys that act on its rows and
//! sub-layers — `space`/`r` in the MCP manager, `d`/`n`/`i` in the sessions
//! picker, the dashboard's `a`/`i`/`k`/`s`/`p`/`n` console verbs, and so on —
//! plus its Enter/arrow/Tab verb family, moved here from the router's
//! fallback arms so each surface owns its full key vocabulary. Generic
//! cross-modal affordances (Esc close, readline editing, paste, scrolling)
//! stay in the shared layer of the router.
//!
//! `resolve_modal_key` returns `Some(action)` when the modal consumes the key
//! and `None` to fall through to the shared affordance library / text
//! insertion. It is consulted by `crate::input::route_event` whenever a
//! modal is active, before the shared arms.

use crossterm::event::{KeyCode, KeyModifiers};

use crate::input::readline::{
    char_index_at_byte, delete_next_grapheme, delete_previous_grapheme, next_grapheme_char_index,
    normalized_cursor_byte, previous_grapheme_char_index,
};
use crate::input::{InputAction, OauthCopyTarget};
use crate::keymap::LiveHint;

/// The modal handlers' own sub-state (ADR-0197 M2): which modal sub-layer is
/// live, which field is focused, which pane owns focus. Built once per event
/// by the caller; every read below is modal-local, so nothing else leaks in.
#[derive(Debug, Default, Clone)]
pub struct ModalKeys {
    /// Whether the model picker's search sub-layer is active. Only meaningful
    /// while the foreground modal is `crate::Modal::Models` or
    /// `crate::Modal::Connections`: `false` is browse mode (typing is inert,
    /// `/` enters search, `*`/`e`/`d`/`D` act on the row), `true` borrows the
    /// composer line as the live fuzzy query. Mirrors `App::model_search`.
    pub model_searching: bool,
    /// Whether the history modal's search sub-layer is active. Only meaningful
    /// while the foreground modal is `crate::Modal::HistorySearch`: `false`
    /// is browse mode (typing is inert, `/` enters search), `true` borrows the
    /// composer line as the live fuzzy query. Mirrors `App::history_search`.
    pub history_searching: bool,
    /// Focused text-field index of the provider editor, or `None` when the
    /// modal is closed or an inline selector is focused.
    pub custom_provider_field: Option<u8>,
    /// Focused field of the key editor (`Modal::ModelEditor`): `0` = API key,
    /// `1` = effort selector, `2` = thinking toggle. `None` when that modal is
    /// not open. Drives ←/→ effort cycling (field 1) and Space thinking toggle
    /// (field 2). Mirrors `App::editor_field` while the key editor is open.
    pub editor_field: Option<u8>,
    /// Which pane of the Settings View currently owns focus. Mirrors `App::config_focus`.
    pub config_focus: crate::overlays::ConfigFocus,
    /// While the sessions picker is drilled into its info sub-view (`i`), the
    /// list-only keys (delete `d`, new `n`, info `i`) are inert — the sub-view
    /// is a read-only read-out.
    pub session_info_detail: bool,
    /// While the connections picker is drilled into its detail sub-view (Enter),
    /// the list-only keys (delete `D`, preset `a`, custom `c`) are inert — the sub-view
    /// is a read-only read-out.
    pub connection_info_detail: bool,
    /// Whether the `/host` dashboard's inline prompt is open (`p` prompt or
    /// `n` new session). While true, printable keys edit the prompt text and
    /// Enter submits it. Mirrors `App::host_prompting`.
    pub host_prompting: bool,
}

/// Whether `modal` currently treats the composer line as an editable free-text
/// field — the surfaces where printable keys, Backspace, and the readline
/// editing family (Ctrl+A/E/W/U/K, Alt+B/F/D, …) act on the input buffer. The
/// history and model-picker modals only qualify while their search sub-layer is
/// active (`history_searching` / `model_searching`); in browse mode those keys
/// are inert so `/` can open search and stray letters never mutate a buffer the
/// user isn't editing.
pub fn modal_claims_composer_line(modal: crate::Modal, keys: &ModalKeys) -> bool {
    match modal {
        // No modal: the chat surface's own composer is the text field.
        crate::Modal::None | crate::Modal::ModelEditor => true,
        crate::Modal::Models | crate::Modal::Connections => keys.model_searching,
        crate::Modal::HistorySearch => keys.history_searching,
        // The provider editor's four basic string fields borrow the composer;
        // Protocol and Client Identity are inline selectors.
        crate::Modal::CustomProvider => keys.custom_provider_field.is_some(),
        _ => false,
    }
}

/// Whether the ModelEditor's toggle fields (effort ladder aside) currently
/// swallow printable characters: the key editor's thinking/vision/tool fields
/// (`2`..=`4`) are toggles, not text fields — printable chars must not mutate
/// the borrowed input line while one is focused. Every other modal (and every
/// other ModelEditor field) leaves printable handling to the shared layer.
pub fn modal_swallows_printable(modal: crate::Modal, keys: &ModalKeys) -> bool {
    modal == crate::Modal::ModelEditor && matches!(keys.editor_field, Some(2..=4))
}

/// Resolve a key a modal owns. `None` falls through to the shared layer
/// (which handles Esc, readline editing, paste, scrolling). `input` and
/// `cursor_position` are the borrowed composer line — the dashboard's inline
/// prompt edits it in place.
pub(crate) fn resolve_modal_key(
    modal: crate::Modal,
    key: crate::keymap::Key,
    keys: &ModalKeys,
    input: &mut String,
    cursor_position: &mut usize,
) -> Option<InputAction> {
    // These two modals own families that span key codes (not just printables).
    match modal {
        crate::Modal::HistorySearch => return resolve_history_search_key(key),
        crate::Modal::ViewSwitcher => return resolve_view_switcher_key(key),
        _ => {}
    }
    // The dashboard's inline prompt borrows the composer line and owns the
    // whole keyboard while it is open: printable keys and Backspace edit the
    // prompt text, Enter submits, Esc falls through to CloseModal (the event
    // loop turns that into a prompt-cancel when `host_prompting` is set),
    // and every other key is swallowed. This branch must precede the verb
    // families below — a prompt open means no row navigation is live.
    if modal == crate::Modal::Host && keys.host_prompting {
        return Some(resolve_host_prompt_key(key, input, cursor_position));
    }
    // Enter / arrow / Tab verb families moved from the router's fallback
    // arms (ADR-0172): each modal owns its own activation, list-walk and
    // focus keys here, ahead of the shared affordance layer. Guards mirror
    // the router's arm ordering: Alt+Enter stays the multi-line newline
    // chord, and Ctrl+↑/↓ keep paging a body-scrolling modal (the shared
    // layer's pager) ahead of the plain list walk.
    match key.code {
        KeyCode::Enter if !key.modifiers.contains(KeyModifiers::ALT) => {
            return Some(match modal {
                crate::Modal::Models => InputAction::ProviderPickerActivate,
                crate::Modal::Connections if keys.connection_info_detail => return None,
                crate::Modal::Connections => InputAction::OpenConnectionDetail,
                crate::Modal::ModelEditor => InputAction::SubmitModelEditor,
                crate::Modal::ProviderPreset => InputAction::SelectPreset,
                crate::Modal::OauthPending => InputAction::CopyOauthContent {
                    target: OauthCopyTarget::Selected,
                },
                crate::Modal::CustomProvider => InputAction::SubmitCustomProvider,
                crate::Modal::Sessions if keys.session_info_detail => return None,
                crate::Modal::Sessions => InputAction::OpenSelectedSession,
                // Without the inline prompt open, Enter previews the
                // highlighted session. (Attach moved to `a`; Enter previews,
                // ADR-0097 §3.)
                crate::Modal::Host => InputAction::HostPreviewSelected,
                crate::Modal::Help
                | crate::Modal::Tools
                | crate::Modal::Mcp
                | crate::Modal::Permissions
                | crate::Modal::Tree
                | crate::Modal::UsageStats => InputAction::CloseModal,
                crate::Modal::Skills => InputAction::SkillsToggleDetail,
                crate::Modal::Queue => InputAction::RecallQueuedSelected,
                crate::Modal::Btw => InputAction::BtwFocusSelected,
                crate::Modal::Config => InputAction::ConfigActivate,
                crate::Modal::Telemetry => InputAction::TelemetryActivate,
                // HistorySearch and ViewSwitcher own their Enter verbs in
                // their schemes above; ModelEditor and the rest fall through
                // to the shared layer.
                _ => return None,
            });
        }
        KeyCode::Up
            if !(key.modifiers.contains(KeyModifiers::CONTROL)
                && modal.keyboard_claims().body_scroll) =>
        {
            return Some(match modal {
                crate::Modal::Models
                | crate::Modal::Connections
                | crate::Modal::Sessions
                | crate::Modal::Host
                | crate::Modal::Permissions
                | crate::Modal::Config
                | crate::Modal::Tree
                | crate::Modal::Telemetry => InputAction::ModalUp,
                crate::Modal::Tools
                | crate::Modal::Mcp
                | crate::Modal::Skills
                | crate::Modal::Queue
                | crate::Modal::Btw => InputAction::SessionSelect { forward: false },
                crate::Modal::ProviderPreset => InputAction::MovePresetChoice { forward: false },
                crate::Modal::OauthPending => InputAction::ScrollUp,
                crate::Modal::CustomProvider => {
                    InputAction::ScrollCustomProvider { forward: false }
                }
                crate::Modal::Help | crate::Modal::UsageStats => InputAction::ScrollUp,
                // ModelEditor and the rest fall through to the shared layer.
                _ => return None,
            });
        }
        KeyCode::Down
            if !(key.modifiers.contains(KeyModifiers::CONTROL)
                && modal.keyboard_claims().body_scroll) =>
        {
            return Some(match modal {
                crate::Modal::Models
                | crate::Modal::Connections
                | crate::Modal::Sessions
                | crate::Modal::Host
                | crate::Modal::Permissions
                | crate::Modal::Config
                | crate::Modal::Tree
                | crate::Modal::Telemetry => InputAction::ModalDown,
                crate::Modal::Tools
                | crate::Modal::Mcp
                | crate::Modal::Skills
                | crate::Modal::Queue
                | crate::Modal::Btw => InputAction::SessionSelect { forward: true },
                crate::Modal::ProviderPreset => InputAction::MovePresetChoice { forward: true },
                crate::Modal::OauthPending => InputAction::ScrollDown,
                crate::Modal::CustomProvider => InputAction::ScrollCustomProvider { forward: true },
                crate::Modal::Help | crate::Modal::UsageStats => InputAction::ScrollDown,
                _ => return None,
            });
        }
        KeyCode::Left => match modal {
            crate::Modal::Telemetry => return Some(InputAction::TelemetryPrevTab),
            crate::Modal::Config if keys.config_focus == crate::overlays::ConfigFocus::Detail => {
                return Some(InputAction::ConfigSegmentPrev);
            }
            // In the model editor's effort field, ← cycles the effort level
            // down (wrapping). Only when field 1 is focused.
            crate::Modal::ModelEditor if keys.editor_field == Some(1) => {
                return Some(InputAction::ModelEditorEffortCycle { delta: -1 });
            }
            crate::Modal::CustomProvider if keys.custom_provider_field.is_none() => {
                return Some(InputAction::CycleCustomProviderChoice { forward: false });
            }
            _ => {}
        },
        KeyCode::Right => match modal {
            crate::Modal::Telemetry => return Some(InputAction::TelemetryNextTab),
            crate::Modal::Config if keys.config_focus == crate::overlays::ConfigFocus::Detail => {
                return Some(InputAction::ConfigSegmentNext);
            }
            // Effort field: → cycles the level up (wrapping).
            crate::Modal::ModelEditor if keys.editor_field == Some(1) => {
                return Some(InputAction::ModelEditorEffortCycle { delta: 1 });
            }
            crate::Modal::CustomProvider if keys.custom_provider_field.is_none() => {
                return Some(InputAction::CycleCustomProviderChoice { forward: true });
            }
            _ => {}
        },
        KeyCode::Tab => {
            return Some(match modal {
                crate::Modal::ModelEditor => InputAction::ModelEditorNextField,
                crate::Modal::CustomProvider => InputAction::CustomProviderNextField,
                crate::Modal::Host => InputAction::HostFocusToggle,
                crate::Modal::Telemetry => InputAction::TelemetryNextTab,
                crate::Modal::OauthPending => InputAction::CycleOauthSelection,
                _ => return None,
            });
        }
        KeyCode::BackTab => {
            return Some(match modal {
                crate::Modal::CustomProvider => InputAction::CustomProviderPrevField,
                crate::Modal::Telemetry => InputAction::TelemetryPrevTab,
                crate::Modal::OauthPending => InputAction::CycleOauthSelection,
                _ => return None,
            });
        }
        _ => {}
    }
    let KeyCode::Char(c) = key.code else {
        return None;
    };
    // Modal verb keys are unmodified (or Shift-capitalized) printables. Every
    // Control/Alt/Super chord is a shared command chord — readline editing,
    // paste, scrolling — owned by the router, not by any modal.
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
    {
        return None;
    }
    match modal {
        crate::Modal::Tools if c == ' ' => Some(InputAction::SessionActivate),
        crate::Modal::Mcp => match c {
            // Space toggles the selected server; `r` reconnects it.
            ' ' => Some(InputAction::McpToggle),
            'r' => Some(InputAction::McpReconnect),
            _ => None,
        },
        crate::Modal::OauthPending => match c {
            // The OAuth pending sheet copies its primary content: `c` copies
            // the device code, `u` the verification URL, `space`/`y` the
            // selected target. Mouse drag-select never reaches modal body
            // text, so these keys are the copy path.
            'c' => Some(InputAction::CopyOauthContent {
                target: OauthCopyTarget::UserCode,
            }),
            'u' => Some(InputAction::CopyOauthContent {
                target: OauthCopyTarget::Url,
            }),
            ' ' | 'y' => Some(InputAction::CopyOauthContent {
                target: OauthCopyTarget::Selected,
            }),
            _ => None,
        },
        crate::Modal::ProviderPreset => match c {
            'b' => Some(InputAction::SelectPresetWithOauthMethod {
                method: muta_contracts::LoginMethod::Browser,
            }),
            'd' => Some(InputAction::SelectPresetWithOauthMethod {
                method: muta_contracts::LoginMethod::Device,
            }),
            _ => None,
        },
        crate::Modal::Permissions => match c {
            ' ' => Some(InputAction::PermissionsActivate),
            'c' => Some(InputAction::PermissionsClearAll),
            _ => None,
        },
        crate::Modal::Telemetry => match c {
            '1' => Some(InputAction::TelemetrySetTab(
                crate::modal::TelemetryTab::Overview,
            )),
            '2' => Some(InputAction::TelemetrySetTab(
                crate::modal::TelemetryTab::Activity,
            )),
            '[' | 'h' => Some(InputAction::TelemetryPrevTab),
            ']' | 'l' => Some(InputAction::TelemetryNextTab),
            _ => None,
        },
        crate::Modal::Config => resolve_config_key(c, keys),
        crate::Modal::Models => resolve_picker_key(c, true, keys),
        crate::Modal::Connections if !keys.connection_info_detail => {
            resolve_picker_key(c, false, keys)
        }
        crate::Modal::Sessions if !keys.session_info_detail => match c {
            'd' => Some(InputAction::DeleteSelectedSession),
            'n' | 'N' => Some(InputAction::CreateNewSession),
            'i' => Some(InputAction::OpenSessionInfo),
            _ => None,
        },
        crate::Modal::Host => resolve_host_key(c),
        crate::Modal::Queue => match c {
            // `Shift+D` deletes the highlighted item outright (the queue is
            // auto-blocked on open, so a mid-delete auto-drain can't race);
            // `K`/`J` reorder toward the front / tail (vim convention).
            'D' => Some(InputAction::QueueDelete),
            'K' => Some(InputAction::QueueMoveItem { delta: -1 }),
            'J' => Some(InputAction::QueueMoveItem { delta: 1 }),
            _ => None,
        },
        crate::Modal::Btw if c == 'D' => Some(InputAction::BtwCloseSelected),
        crate::Modal::ModelEditor => resolve_model_editor_key(c, keys),
        _ => None,
    }
}

/// The history modal (Ctrl+R) owns its family across key codes: `Esc` closes
/// (restoring the stashed draft), `Enter`/`Tab` insert the focused entry into
/// the composer and close, `↑`/`↓` walk the list. While the search sub-layer
/// is active, printable keys and Backspace edit the borrowed composer line via
/// the shared editing layer (`edits_input_field`).
pub(crate) fn resolve_history_search_key(key: crate::keymap::Key) -> Option<InputAction> {
    match key.code {
        KeyCode::Esc => Some(InputAction::CloseModal),
        KeyCode::Delete if key.modifiers.contains(KeyModifiers::SHIFT) => {
            Some(InputAction::HistoryDeleteSelected)
        }
        KeyCode::Enter if !key.modifiers.contains(KeyModifiers::ALT) => {
            Some(InputAction::HistoryInsert)
        }
        KeyCode::Tab => Some(InputAction::HistoryInsert),
        KeyCode::Up
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            Some(InputAction::ModalUp)
        }
        KeyCode::Down
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            Some(InputAction::ModalDown)
        }
        _ => None,
    }
}

/// The command palette (Ctrl+P / Ctrl+L) owns its filter family: every
/// printable key types into the palette's own query (never the composer),
/// `Backspace` trims the query, `Delete` drops the selected entry, and `Enter`
/// executes the highlighted command. List walking (↑/↓) and Esc-close stay in
/// the shared affordance layer — they are cross-modal verbs.
fn resolve_view_switcher_key(key: crate::keymap::Key) -> Option<InputAction> {
    match key.code {
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER) =>
        {
            Some(InputAction::ViewSwitcherFilter { ch: c })
        }
        KeyCode::Backspace => Some(InputAction::ViewSwitcherBackspace),
        KeyCode::Delete => Some(InputAction::ViewCloseSelected),
        KeyCode::Enter if !key.modifiers.contains(KeyModifiers::ALT) => {
            Some(InputAction::ViewSwitchActivate)
        }
        _ => None,
    }
}

/// The history modal's hint row (single origin for the composer's history
/// hint): every chord advertised here is handled by
/// [`resolve_history_search_key`].
const HISTORY_HINTS: &[LiveHint] = &[
    LiveHint::nav(crate::keymap::Key::ESC, "close"),
    LiveHint::nav_glyph(
        crate::keymap::Key::UP,
        crate::keymap::keyvocab::ARROWS_UD,
        "navigate",
    ),
    LiveHint::nav(crate::keymap::Key::SHIFT_DELETE, "delete"),
    LiveHint::action(crate::keymap::Key::TAB, "insert"),
    LiveHint::action(crate::keymap::Key::ENTER, "insert"),
];

pub(crate) fn live_history_hints() -> &'static [LiveHint] {
    HISTORY_HINTS
}

/// Question sheet: `space` toggles the selection (unless the free-text
/// "Other" row is highlighted), `1..9` picks an option, anything else types
/// into the focused field.
/// Settings modal: `space` activates the row; in the Detail pane `1`/`h` and
/// `2`/`l` step segments.
fn resolve_config_key(c: char, keys: &ModalKeys) -> Option<InputAction> {
    if c == ' ' {
        return Some(InputAction::ConfigActivate);
    }
    if keys.config_focus == crate::overlays::ConfigFocus::Detail {
        if c == '1' || c == 'h' {
            return Some(InputAction::ConfigSegmentPrev);
        }
        if c == '2' || c == 'l' {
            return Some(InputAction::ConfigSegmentNext);
        }
    }
    None
}

/// Models / Connections picker browse-mode verbs. While the search sub-layer
/// is active every char is a query and the modal owns nothing here.
fn resolve_picker_key(c: char, is_models: bool, keys: &ModalKeys) -> Option<InputAction> {
    if keys.model_searching {
        return None;
    }
    if c == '/' {
        // Browse mode: `/` opens the search sub-layer rather than inserting
        // a literal slash — mirrors the history modal.
        return Some(InputAction::ModelEnterSearch);
    }
    if is_models && c == '*' {
        // Models browse mode only: star the highlighted MODEL as a favorite.
        return Some(InputAction::ProviderPickerToggleFavorite);
    }
    if !is_models && c == 'a' {
        // Connections browse mode: `a` opens the curated preset branch.
        return Some(InputAction::OpenPresetChooser);
    }
    if !is_models && c == 'c' {
        // Custom connections are a sibling of the preset branch.
        return Some(InputAction::OpenCustomConnection);
    }
    if c == 'e' {
        // Connections: edit the highlighted provider. Models: edit the
        // highlighted model's per-model settings.
        return Some(InputAction::OpenModelEditor);
    }
    if c == 'r' || c == 'R' {
        return Some(InputAction::RefreshProviderModels);
    }
    if !is_models && c == 'D' {
        // Connections browse mode: `Shift+D` deletes the highlighted custom
        // provider (ignored for built-ins by the handler).
        return Some(InputAction::DeleteProvider);
    }
    None
}

/// Dashboard (Host) console verbs. Every printable key is an action here —
/// never literal input — with `a` attach, `i` interrupt, `k` kill, `s`
/// suspend, `p`/`n` opening the inline prompt / new-session field, and any
/// other char seeding the console composer.
fn resolve_host_key(c: char) -> Option<InputAction> {
    match c {
        'a' => Some(InputAction::HostSwitchSelected),
        'i' => Some(InputAction::HostInterruptSelected),
        'k' => Some(InputAction::HostKillSelected),
        's' => Some(InputAction::HostSuspendSelected),
        'p' => Some(InputAction::HostPromptOpen),
        'n' => Some(InputAction::HostNewSession),
        _ => Some(InputAction::HostPromptSeed(c)),
    }
}

/// The dashboard's inline prompt (moved verbatim from the router's
/// inline-prompt stage): printable keys and Backspace edit the borrowed
/// prompt line, Delete forward-deletes (no chip handling — the dashboard
/// prompt never stages attachments), ←/→ move the caret, Enter submits, Esc
/// closes (the event loop cancels the prompt on CloseModal while
/// `host_prompting` is set), and every other key is swallowed so the prompt
/// owns the keyboard.
fn resolve_host_prompt_key(
    key: crate::keymap::Key,
    input: &mut String,
    cursor_position: &mut usize,
) -> InputAction {
    match key.code {
        KeyCode::Char(c) => {
            let byte_pos = normalized_cursor_byte(input, *cursor_position);
            *cursor_position = char_index_at_byte(input, byte_pos);
            input.insert(byte_pos, c);
            *cursor_position += 1;
            InputAction::InsertChar(c)
        }
        KeyCode::Backspace => {
            delete_previous_grapheme(input, cursor_position);
            InputAction::None
        }
        KeyCode::Delete => {
            delete_next_grapheme(input, cursor_position);
            InputAction::None
        }
        KeyCode::Left => {
            *cursor_position = previous_grapheme_char_index(input, *cursor_position);
            InputAction::None
        }
        KeyCode::Right => {
            *cursor_position = next_grapheme_char_index(input, *cursor_position);
            InputAction::None
        }
        KeyCode::Enter => InputAction::HostPromptSubmit,
        KeyCode::Esc => InputAction::CloseModal,
        _ => InputAction::None,
    }
}

/// Key editor (ModelEditor): `space` cycles the non-text fields (thinking
/// toggle / capability overrides), a digit on the effort field jumps to that
/// ladder rung; everything else edits the borrowed input line (shared layer).
fn resolve_model_editor_key(c: char, keys: &ModalKeys) -> Option<InputAction> {
    if c == ' ' && matches!(keys.editor_field, Some(2..=4)) {
        Some(match keys.editor_field {
            Some(3) => InputAction::ModelEditorVisionCycle,
            Some(4) => InputAction::ModelEditorToolCycle,
            _ => InputAction::ModelEditorThinkingToggle,
        })
    } else if c.is_ascii_digit() && c != '0' && keys.editor_field == Some(1) {
        // A digit on the effort field jumps straight to that ladder rung
        // (`1` = shallowest … `7` = deepest) instead of inserting into the
        // borrowed input line. `0` is not a tier.
        let index = c as usize - '1' as usize;
        Some(InputAction::ModelEditorEffortJump { index })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheet::SheetKeys;

    fn keys(tune: impl FnOnce(&mut ModalKeys)) -> ModalKeys {
        let mut k = ModalKeys::default();
        tune(&mut k);
        k
    }

    fn sheet_keys(tune: impl FnOnce(&mut SheetKeys)) -> SheetKeys {
        let mut k = SheetKeys::default();
        tune(&mut k);
        k
    }

    fn key(c: char) -> crate::keymap::Key {
        crate::keymap::Key {
            modifiers: crossterm::event::KeyModifiers::NONE,
            code: KeyCode::Char(c),
        }
    }

    /// Test shim: the prompt-editing buffers are only observed by the Host
    /// inline prompt; every other surface ignores them.
    fn resolve(
        modal: crate::Modal,
        k: crate::keymap::Key,
        keys: &ModalKeys,
    ) -> Option<InputAction> {
        resolve_modal_key(modal, k, keys, &mut String::new(), &mut 0)
    }

    #[test]
    fn mcp_owns_space_and_r() {
        let c = keys(|_| {});
        assert_eq!(
            resolve(crate::Modal::Mcp, key(' '), &c),
            Some(InputAction::McpToggle)
        );
        assert_eq!(
            resolve(crate::Modal::Mcp, key('r'), &c),
            Some(InputAction::McpReconnect)
        );
        assert_eq!(resolve(crate::Modal::Mcp, key('z'), &c), None);
    }

    #[test]
    fn queue_owns_delete_and_reorder() {
        let c = keys(|_| {});
        assert_eq!(
            resolve(crate::Modal::Queue, key('D'), &c),
            Some(InputAction::QueueDelete)
        );
        assert_eq!(
            resolve(crate::Modal::Queue, key('K'), &c),
            Some(InputAction::QueueMoveItem { delta: -1 })
        );
        assert_eq!(
            resolve(crate::Modal::Queue, key('J'), &c),
            Some(InputAction::QueueMoveItem { delta: 1 })
        );
    }

    #[test]
    fn picker_search_layer_surrenders_query_chars() {
        // In the search sub-layer every printable char is a query — the modal
        // owns nothing and the shared layer inserts it.
        let c = keys(|k| k.model_searching = true);
        assert_eq!(resolve(crate::Modal::Models, key('/'), &c), None);
        assert_eq!(resolve(crate::Modal::Models, key('*'), &c), None);
        // Browse mode owns the verbs.
        let c = keys(|_| {});
        assert_eq!(
            resolve(crate::Modal::Models, key('*'), &c),
            Some(InputAction::ProviderPickerToggleFavorite)
        );
        assert_eq!(
            resolve(crate::Modal::Models, key('/'), &c),
            Some(InputAction::ModelEnterSearch)
        );
    }

    #[test]
    fn connections_detail_readout_is_inert() {
        let c = keys(|k| k.connection_info_detail = true);
        assert_eq!(resolve(crate::Modal::Connections, key('a'), &c), None);
        assert_eq!(resolve(crate::Modal::Connections, key('c'), &c), None);
        assert_eq!(resolve(crate::Modal::Connections, key('D'), &c), None);
    }

    #[test]
    fn dashboard_chars_are_always_actions() {
        let c = keys(|_| {});
        assert_eq!(
            resolve(crate::Modal::Host, key('a'), &c),
            Some(InputAction::HostSwitchSelected)
        );
        assert_eq!(
            resolve(crate::Modal::Host, key('i'), &c),
            Some(InputAction::HostInterruptSelected)
        );
        assert_eq!(
            resolve(crate::Modal::Host, key('q'), &c),
            Some(InputAction::HostPromptSeed('q'))
        );
    }

    #[test]
    fn question_space_digit_and_text() {
        use crate::sheet::{SheetKind, resolve_sheet_key};
        let c = sheet_keys(|_| {});
        assert_eq!(
            resolve_sheet_key(SheetKind::Question, key(' '), &c),
            Some(InputAction::QuestionToggle)
        );
        assert_eq!(
            resolve_sheet_key(SheetKind::Question, key('3'), &c),
            Some(InputAction::QuestionSelect(3))
        );
        // With the "Other" field highlighted, space types into it.
        let c = sheet_keys(|k| k.question_other_highlighted = true);
        assert_eq!(
            resolve_sheet_key(SheetKind::Question, key(' '), &c),
            Some(InputAction::QuestionInsertChar(' '))
        );
    }

    #[test]
    fn model_editor_space_and_digits() {
        let c = keys(|k| k.editor_field = Some(2));
        assert_eq!(
            resolve(crate::Modal::ModelEditor, key(' '), &c),
            Some(InputAction::ModelEditorThinkingToggle)
        );
        let c = keys(|k| k.editor_field = Some(1));
        assert_eq!(
            resolve(crate::Modal::ModelEditor, key('5'), &c),
            Some(InputAction::ModelEditorEffortJump { index: 4 })
        );
        // A letter on the API-key field is a query char for the shared layer.
        assert_eq!(resolve(crate::Modal::ModelEditor, key('x'), &c), None);
    }

    #[test]
    fn non_printable_and_unowned_modals_fall_through() {
        let c = keys(|_| {});
        let esc = crate::keymap::Key::ESC;
        assert_eq!(resolve(crate::Modal::Mcp, esc, &c), None);
        let c = keys(|_| {});
        assert_eq!(resolve(crate::Modal::HistorySearch, key('q'), &c), None);
        // InputInjection is a pure text surface: every key edits via the
        // shared layer, so the sheet scheme owns nothing.
        use crate::sheet::{SheetKind, resolve_sheet_key};
        let c = sheet_keys(|_| {});
        assert_eq!(
            resolve_sheet_key(SheetKind::InputInjection, key('q'), &c),
            None
        );
    }

    #[test]
    fn history_modal_owns_insert_and_close_family() {
        use crate::keymap::Key;
        let c = keys(|_| {});
        assert_eq!(
            resolve(crate::Modal::HistorySearch, Key::ESC, &c),
            Some(InputAction::CloseModal)
        );
        assert_eq!(
            resolve(crate::Modal::HistorySearch, Key::ENTER, &c),
            Some(InputAction::HistoryInsert)
        );
        assert_eq!(
            resolve(crate::Modal::HistorySearch, Key::TAB, &c),
            Some(InputAction::HistoryInsert)
        );
        assert_eq!(
            resolve(crate::Modal::HistorySearch, Key::UP, &c),
            Some(InputAction::ModalUp)
        );
        assert_eq!(
            resolve(crate::Modal::HistorySearch, Key::DOWN, &c),
            Some(InputAction::ModalDown)
        );
        // Query chars are not history verbs — they edit via the shared layer.
        assert_eq!(resolve(crate::Modal::HistorySearch, key('q'), &c), None);
    }

    #[test]
    fn history_hints_are_all_resolvable() {
        let c = keys(|_| {});
        for h in live_history_hints() {
            assert!(
                resolve(crate::Modal::HistorySearch, h.key, &c).is_some(),
                "advertised history chord {h:?} is not handled"
            );
        }
    }

    #[test]
    fn palette_owns_filter_and_delete_family() {
        use crate::keymap::Key;
        let c = keys(|_| {});
        assert_eq!(
            resolve(crate::Modal::ViewSwitcher, key('q'), &c),
            Some(InputAction::ViewSwitcherFilter { ch: 'q' })
        );
        let backspace = Key {
            modifiers: KeyModifiers::NONE,
            code: KeyCode::Backspace,
        };
        assert_eq!(
            resolve(crate::Modal::ViewSwitcher, backspace, &c),
            Some(InputAction::ViewSwitcherBackspace)
        );
        let delete = Key {
            modifiers: KeyModifiers::NONE,
            code: KeyCode::Delete,
        };
        assert_eq!(
            resolve(crate::Modal::ViewSwitcher, delete, &c),
            Some(InputAction::ViewCloseSelected)
        );
        assert_eq!(
            resolve(crate::Modal::ViewSwitcher, Key::ENTER, &c),
            Some(InputAction::ViewSwitchActivate)
        );
        // ↑/↓ list walking and Esc-close stay in the shared affordance layer.
        assert_eq!(resolve(crate::Modal::ViewSwitcher, Key::UP, &c), None);
        assert_eq!(resolve(crate::Modal::ViewSwitcher, Key::ESC, &c), None);
    }
}
