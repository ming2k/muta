//! Per-modal keybinding schemes (ADR-0172).
//!
//! Each modal owns the single-letter *verb* keys that act on its rows and
//! sub-layers — `space`/`r` in the MCP manager, `d`/`n`/`i` in the sessions
//! picker, the dashboard's `a`/`i`/`k`/`s`/`p`/`n` console verbs, and so on.
//! Generic cross-modal affordances (↑/↓ list navigation, Enter select, Esc
//! close, Tab focus, readline editing, paste) stay in the shared layer of the
//! router; this module owns only what is specific to a modal.
//!
//! `resolve_modal_key` returns `Some(action)` when the modal consumes the key
//! and `None` to fall through to the shared affordance library / text
//! insertion. It is consulted by `crate::input::process_event` whenever a
//! modal is active, before the shared arms.

use crossterm::event::{KeyCode, KeyModifiers};

use crate::input::{InputAction, InputContext, OauthCopyTarget};
use crate::keymap::{HintSide, LiveHint};

/// Resolve a key a modal owns. `None` falls through to the shared layer
/// (which handles list navigation, text insertion into the borrowed composer
/// line, readline editing, paste, scrolling).
pub(crate) fn resolve_modal_key(
    modal: crate::Modal,
    key: crate::keymap::Key,
    ctx: &InputContext,
) -> Option<InputAction> {
    // These two modals own families that span key codes (not just printables).
    match modal {
        crate::Modal::HistorySearch => return resolve_history_search_key(key),
        crate::Modal::ViewSwitcher => return resolve_view_switcher_key(key),
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
        crate::Modal::Config => resolve_config_key(c, ctx),
        crate::Modal::Models => resolve_picker_key(c, true, ctx),
        crate::Modal::Connections if !ctx.connection_info_detail => {
            resolve_picker_key(c, false, ctx)
        }
        crate::Modal::Sessions if !ctx.session_info_detail => match c {
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
        crate::Modal::ModelEditor => resolve_model_editor_key(c, ctx),
        _ => None,
    }
}

/// The history modal (Ctrl+R) owns its family across key codes: `Esc` closes
/// (restoring the stashed draft), `Enter`/`Tab` insert the focused entry into
/// the composer and close, `↑`/`↓` walk the list. While the search sub-layer
/// is active, printable keys and Backspace edit the borrowed composer line via
/// the shared editing layer (`edits_input_field`).
fn resolve_history_search_key(key: crate::keymap::Key) -> Option<InputAction> {
    match key.code {
        KeyCode::Esc => Some(InputAction::CloseModal),
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
pub(crate) fn live_history_hints() -> &'static [LiveHint] {
    &[
        LiveHint {
            key: crate::keymap::Key::ESC,
            label: "close",
            side: HintSide::Nav,
        },
        LiveHint {
            key: crate::keymap::Key::TAB,
            label: "insert",
            side: HintSide::Action,
        },
        LiveHint {
            key: crate::keymap::Key::ENTER,
            label: "insert",
            side: HintSide::Action,
        },
    ]
}

/// Question sheet: `space` toggles the selection (unless the free-text
/// "Other" row is highlighted), `1..9` picks an option, anything else types
/// into the focused field.
/// Settings modal: `space` activates the row; in the Detail pane `1`/`h` and
/// `2`/`l` step segments; `d`/`D` deletes the selected connection.
fn resolve_config_key(c: char, ctx: &InputContext) -> Option<InputAction> {
    if c == ' ' {
        return Some(InputAction::ConfigActivate);
    }
    if ctx.config_focus == crate::overlays::ConfigFocus::Detail {
        if c == '1' || c == 'h' {
            return Some(InputAction::ConfigSegmentPrev);
        }
        if c == '2' || c == 'l' {
            return Some(InputAction::ConfigSegmentNext);
        }
    }
    if c == 'd' || c == 'D' {
        return Some(InputAction::ConfigDeleteConnection);
    }
    None
}

/// Models / Connections picker browse-mode verbs. While the search sub-layer
/// is active every char is a query and the modal owns nothing here.
fn resolve_picker_key(c: char, is_models: bool, ctx: &InputContext) -> Option<InputAction> {
    if ctx.model_searching {
        return None;
    }
    if (is_models || !is_models) && c == '/' {
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

/// Key editor (ModelEditor): `space` cycles the non-text fields (thinking
/// toggle / capability overrides), a digit on the effort field jumps to that
/// ladder rung; everything else edits the borrowed input line (shared layer).
fn resolve_model_editor_key(c: char, ctx: &InputContext) -> Option<InputAction> {
    if c == ' ' && matches!(ctx.editor_field, Some(2..=4)) {
        Some(match ctx.editor_field {
            Some(3) => InputAction::ModelEditorVisionCycle,
            Some(4) => InputAction::ModelEditorToolCycle,
            _ => InputAction::ModelEditorThinkingToggle,
        })
    } else if c.is_ascii_digit() && c != '0' && ctx.editor_field == Some(1) {
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
    use crate::input::InputContext;
    use crate::surfaces::View;

    fn ctx(modal: crate::Modal, tune: impl FnOnce(&mut InputContext)) -> InputContext {
        let mut c = InputContext {
            active_modal: modal,
            ..Default::default()
        };
        c.current_view = View::Session;
        tune(&mut c);
        c
    }

    fn sheet_ctx(
        kind: crate::sheet::SheetKind,
        tune: impl FnOnce(&mut InputContext),
    ) -> InputContext {
        let mut c = InputContext {
            active_sheet: Some(kind),
            ..Default::default()
        };
        c.current_view = View::Session;
        tune(&mut c);
        c
    }

    fn key(c: char) -> crate::keymap::Key {
        crate::keymap::Key {
            modifiers: crossterm::event::KeyModifiers::NONE,
            code: KeyCode::Char(c),
        }
    }

    #[test]
    fn mcp_owns_space_and_r() {
        let c = ctx(crate::Modal::Mcp, |_| {});
        assert_eq!(
            resolve_modal_key(crate::Modal::Mcp, key(' '), &c),
            Some(InputAction::McpToggle)
        );
        assert_eq!(
            resolve_modal_key(crate::Modal::Mcp, key('r'), &c),
            Some(InputAction::McpReconnect)
        );
        assert_eq!(resolve_modal_key(crate::Modal::Mcp, key('z'), &c), None);
    }

    #[test]
    fn queue_owns_delete_and_reorder() {
        let c = ctx(crate::Modal::Queue, |_| {});
        assert_eq!(
            resolve_modal_key(crate::Modal::Queue, key('D'), &c),
            Some(InputAction::QueueDelete)
        );
        assert_eq!(
            resolve_modal_key(crate::Modal::Queue, key('K'), &c),
            Some(InputAction::QueueMoveItem { delta: -1 })
        );
        assert_eq!(
            resolve_modal_key(crate::Modal::Queue, key('J'), &c),
            Some(InputAction::QueueMoveItem { delta: 1 })
        );
    }

    #[test]
    fn picker_search_layer_surrenders_query_chars() {
        // In the search sub-layer every printable char is a query — the modal
        // owns nothing and the shared layer inserts it.
        let c = ctx(crate::Modal::Models, |c| c.model_searching = true);
        assert_eq!(resolve_modal_key(crate::Modal::Models, key('/'), &c), None);
        assert_eq!(resolve_modal_key(crate::Modal::Models, key('*'), &c), None);
        // Browse mode owns the verbs.
        let c = ctx(crate::Modal::Models, |_| {});
        assert_eq!(
            resolve_modal_key(crate::Modal::Models, key('*'), &c),
            Some(InputAction::ProviderPickerToggleFavorite)
        );
        assert_eq!(
            resolve_modal_key(crate::Modal::Models, key('/'), &c),
            Some(InputAction::ModelEnterSearch)
        );
    }

    #[test]
    fn connections_detail_readout_is_inert() {
        let c = ctx(crate::Modal::Connections, |c| {
            c.connection_info_detail = true
        });
        assert_eq!(
            resolve_modal_key(crate::Modal::Connections, key('a'), &c),
            None
        );
        assert_eq!(
            resolve_modal_key(crate::Modal::Connections, key('c'), &c),
            None
        );
        assert_eq!(
            resolve_modal_key(crate::Modal::Connections, key('D'), &c),
            None
        );
    }

    #[test]
    fn dashboard_chars_are_always_actions() {
        let c = ctx(crate::Modal::Host, |_| {});
        assert_eq!(
            resolve_modal_key(crate::Modal::Host, key('a'), &c),
            Some(InputAction::HostSwitchSelected)
        );
        assert_eq!(
            resolve_modal_key(crate::Modal::Host, key('i'), &c),
            Some(InputAction::HostInterruptSelected)
        );
        assert_eq!(
            resolve_modal_key(crate::Modal::Host, key('q'), &c),
            Some(InputAction::HostPromptSeed('q'))
        );
    }

    #[test]
    fn question_space_digit_and_text() {
        use crate::sheet::{SheetKind, resolve_sheet_key};
        let c = sheet_ctx(SheetKind::Question, |_| {});
        assert_eq!(
            resolve_sheet_key(SheetKind::Question, key(' '), &c),
            Some(InputAction::QuestionToggle)
        );
        assert_eq!(
            resolve_sheet_key(SheetKind::Question, key('3'), &c),
            Some(InputAction::QuestionSelect(3))
        );
        // With the "Other" field highlighted, space types into it.
        let c = sheet_ctx(SheetKind::Question, |c| c.question_other_highlighted = true);
        assert_eq!(
            resolve_sheet_key(SheetKind::Question, key(' '), &c),
            Some(InputAction::QuestionInsertChar(' '))
        );
    }

    #[test]
    fn model_editor_space_and_digits() {
        let c = ctx(crate::Modal::ModelEditor, |c| c.editor_field = Some(2));
        assert_eq!(
            resolve_modal_key(crate::Modal::ModelEditor, key(' '), &c),
            Some(InputAction::ModelEditorThinkingToggle)
        );
        let c = ctx(crate::Modal::ModelEditor, |c| c.editor_field = Some(1));
        assert_eq!(
            resolve_modal_key(crate::Modal::ModelEditor, key('5'), &c),
            Some(InputAction::ModelEditorEffortJump { index: 4 })
        );
        // A letter on the API-key field is a query char for the shared layer.
        assert_eq!(
            resolve_modal_key(crate::Modal::ModelEditor, key('x'), &c),
            None
        );
    }

    #[test]
    fn non_printable_and_unowned_modals_fall_through() {
        let c = ctx(crate::Modal::Mcp, |_| {});
        let esc = crate::keymap::Key::ESC;
        assert_eq!(resolve_modal_key(crate::Modal::Mcp, esc, &c), None);
        let c = ctx(crate::Modal::HistorySearch, |_| {});
        assert_eq!(
            resolve_modal_key(crate::Modal::HistorySearch, key('q'), &c),
            None
        );
        // InputInjection is a pure text surface: every key edits via the
        // shared layer, so the sheet scheme owns nothing.
        use crate::sheet::{SheetKind, resolve_sheet_key};
        let c = sheet_ctx(SheetKind::InputInjection, |_| {});
        assert_eq!(
            resolve_sheet_key(SheetKind::InputInjection, key('q'), &c),
            None
        );
    }

    #[test]
    fn history_modal_owns_insert_and_close_family() {
        use crate::keymap::Key;
        let c = ctx(crate::Modal::HistorySearch, |_| {});
        assert_eq!(
            resolve_modal_key(crate::Modal::HistorySearch, Key::ESC, &c),
            Some(InputAction::CloseModal)
        );
        assert_eq!(
            resolve_modal_key(crate::Modal::HistorySearch, Key::ENTER, &c),
            Some(InputAction::HistoryInsert)
        );
        assert_eq!(
            resolve_modal_key(crate::Modal::HistorySearch, Key::TAB, &c),
            Some(InputAction::HistoryInsert)
        );
        assert_eq!(
            resolve_modal_key(crate::Modal::HistorySearch, Key::UP, &c),
            Some(InputAction::ModalUp)
        );
        assert_eq!(
            resolve_modal_key(crate::Modal::HistorySearch, Key::DOWN, &c),
            Some(InputAction::ModalDown)
        );
        // Query chars are not history verbs — they edit via the shared layer.
        assert_eq!(
            resolve_modal_key(crate::Modal::HistorySearch, key('q'), &c),
            None
        );
    }

    #[test]
    fn history_hints_are_all_resolvable() {
        let c = ctx(crate::Modal::HistorySearch, |_| {});
        for h in live_history_hints() {
            assert!(
                resolve_modal_key(crate::Modal::HistorySearch, h.key, &c).is_some(),
                "advertised history chord {h:?} is not handled"
            );
        }
    }

    #[test]
    fn palette_owns_filter_and_delete_family() {
        use crate::keymap::Key;
        let c = ctx(crate::Modal::ViewSwitcher, |_| {});
        assert_eq!(
            resolve_modal_key(crate::Modal::ViewSwitcher, key('q'), &c),
            Some(InputAction::ViewSwitcherFilter { ch: 'q' })
        );
        let backspace = Key {
            modifiers: KeyModifiers::NONE,
            code: KeyCode::Backspace,
        };
        assert_eq!(
            resolve_modal_key(crate::Modal::ViewSwitcher, backspace, &c),
            Some(InputAction::ViewSwitcherBackspace)
        );
        let delete = Key {
            modifiers: KeyModifiers::NONE,
            code: KeyCode::Delete,
        };
        assert_eq!(
            resolve_modal_key(crate::Modal::ViewSwitcher, delete, &c),
            Some(InputAction::ViewCloseSelected)
        );
        assert_eq!(
            resolve_modal_key(crate::Modal::ViewSwitcher, Key::ENTER, &c),
            Some(InputAction::ViewSwitchActivate)
        );
        // ↑/↓ list walking and Esc-close stay in the shared affordance layer.
        assert_eq!(
            resolve_modal_key(crate::Modal::ViewSwitcher, Key::UP, &c),
            None
        );
        assert_eq!(
            resolve_modal_key(crate::Modal::ViewSwitcher, Key::ESC, &c),
            None
        );
    }
}
