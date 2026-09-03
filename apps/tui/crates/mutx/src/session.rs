//! The Session view's self-owned keyboard scheme and resolver
//! (ADR-0172, plane-less per ADR-0173).
//!
//! The Session view — and its chat siblings, the zoomed Runner and the `/btw`
//! Side aside — owns the keys that act on its surface across its run states.
//! The keyboard is **plane-less** (ADR-0173): there is no composer/transcript
//! mode to enter or leave. Every chord has one meaning; the focused step is a
//! transient selection driven by the `FocusPrevTarget`/`ClearFocusedTarget`
//! verbs (canonical `Alt+↑`/`Alt+↓`), typing always bounces to the draft, and
//! transcript scrolling (PgUp/PgDn/Home/End) is handled unconditionally by the
//! router. Before this module those keys were
//! scattered through the central `input` match as bare `active_modal == None`
//! branches that silently applied to every view. They now live here as the
//! surface's own keybinding scheme: an executable resolver,
//! [`resolve_chat_surface_key`], whose advertised hints
//! ([`live_chat_hints`]) share its single semantic origin.
//!
//! ## Layer contract
//!
//! The input router (`crate::input::process_event`) offers the key to this
//! resolver **only** while the chat surface owns the keyboard (no modal, view
//! is Session / Runner / Side). A `Some(action)` means the chat surface
//! consumed the key. A `None` falls through to the shared affordance library
//! (readline editing, caret motion, paste, scrolling) and the modal arms,
//! which stay central until each modal owns its own scheme.

use crossterm::event::{KeyCode, KeyModifiers};

use crate::input::{InputAction, InputContext};
use crate::keymap::{HintSide, LiveHint};

/// Run states the composer hint row advertises. (HistorySearch is a modal and
/// stays out of the chat scheme until that modal owns its own.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HintState {
    Idle,
    Command,
    Running,
    Completion,
}

/// The chat surface's live chords for a run state — the single origin for the
/// composer hint row. Every chord returned here is resolvable by
/// [`resolve_chat_surface_key`] in that state (asserted by tests).
///
/// `steer_key` is the *effective* chord of the `steer` verb (ADR-0172): the
/// hint advertises exactly the binding that fires, so a user remap is shown
/// (canonical `Alt+S` when unremapped).
pub(crate) fn live_chat_hints(state: HintState, steer_key: crate::keymap::Key) -> Vec<LiveHint> {
    use crate::keymap::Key;
    let hints: &[LiveHint] = match state {
        HintState::Idle | HintState::Command => &[LiveHint {
            key: Key::ENTER,
            label: "send",
            side: HintSide::Action,
        }],
        HintState::Running => &[
            LiveHint {
                key: steer_key,
                label: "steer now",
                side: HintSide::Nav,
            },
            LiveHint {
                key: Key::ENTER,
                label: "queue follow-up",
                side: HintSide::Action,
            },
        ],
        HintState::Completion => &[
            LiveHint {
                key: Key::ESC,
                label: "dismiss",
                side: HintSide::Nav,
            },
            LiveHint {
                key: Key::TAB,
                label: "select",
                side: HintSide::Action,
            },
            LiveHint {
                key: Key::ENTER,
                label: "select",
                side: HintSide::Action,
            },
        ],
    };
    hints.to_vec()
}

/// Resolve a key pressed while the chat surface owns the keyboard. Returns
/// `Some(action)` when the chat surface consumes the key, `None` to fall
/// through to the shared affordance library.
///
/// The surface *verb* chords (history recall, history walk, steer, step-focus
/// enter/clear, focused-step scroll, runner siblings) resolve override-first
/// (ADR-0172 step 9): each verb's canonical chord is replaced by the user's
/// `session.*` binding when one is configured, and the canonical chord goes
/// inactive. The interaction grammar (Enter/Tab/BackTab/Esc/↑/↓/text) is not
/// remappable.
pub(crate) fn resolve_chat_surface_key(
    key: crate::keymap::Key,
    ctx: &InputContext,
    input: &mut String,
    cursor_position: &mut usize,
) -> Option<InputAction> {
    use crate::keymap::SurfaceVerb;
    let ov = &ctx.surface_overrides;
    if ov.matches(key, SurfaceVerb::OpenHistory) {
        return Some(InputAction::OpenHistory);
    }
    if ov.matches(key, SurfaceVerb::HistoryPrev) {
        return Some(InputAction::HistoryPrev);
    }
    if ov.matches(key, SurfaceVerb::HistoryNext) {
        return Some(InputAction::HistoryNext);
    }
    if ov.matches(key, SurfaceVerb::Steer) {
        return resolve_steer(ctx, input, cursor_position);
    }
    if ov.matches(key, SurfaceVerb::FocusPrevTarget) {
        return Some(InputAction::FocusPrevTarget);
    }
    if ov.matches(key, SurfaceVerb::ClearFocusedTarget) && ctx.has_focused_target {
        return Some(InputAction::ClearFocusedTarget);
    }
    // Home / End unconditionally scroll the transcript (ADR-0173): reading
    // the transcript never requires entering a state first.
    if ov.matches(key, SurfaceVerb::ScrollTop) {
        return Some(InputAction::ScrollTop);
    }
    if ov.matches(key, SurfaceVerb::ScrollBottom) {
        return Some(InputAction::ScrollBottom);
    }

    match key.code {
        KeyCode::Enter if !key.modifiers.contains(KeyModifiers::ALT) => {
            resolve_enter(ctx, input, cursor_position)
        }
        KeyCode::Tab => resolve_tab(ctx),
        KeyCode::Esc => resolve_esc(ctx),
        KeyCode::Up => resolve_up(ctx, input, cursor_position),
        KeyCode::Down => resolve_down(ctx, input, cursor_position),
        // Only unmodified (or Shift-capitalized) characters are owned as
        // text; every Control/Alt/Super chord is a shared command chord
        // (readline editing, paste, …) handled by the router.
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER) =>
        {
            resolve_printable(ctx, c, input, cursor_position)
        }
        _ => None,
    }
}

/// The Runner view's own scheme (ADR-0172): the zoom owns its exit (`Esc`)
/// and sibling navigation (`[`/`]`, a remappable `session.prev_sibling` /
/// `session.next_sibling` verb), and delegates every other key to the shared
/// chat core for step-focus walking.
pub(crate) fn resolve_runner_key(
    key: crate::keymap::Key,
    ctx: &InputContext,
    input: &mut String,
    cursor_position: &mut usize,
) -> Option<InputAction> {
    use crate::keymap::SurfaceVerb;
    let ov = &ctx.surface_overrides;
    // Sibling navigation rides the verb's effective chord only while no text
    // is composed and no step is focused (a focused step bounces the key to
    // the composer).
    if ov.matches(key, SurfaceVerb::PrevSibling) && !ctx.has_focused_target && input.is_empty() {
        return Some(InputAction::PrevSibling);
    }
    if ov.matches(key, SurfaceVerb::NextSibling) && !ctx.has_focused_target && input.is_empty() {
        return Some(InputAction::NextSibling);
    }
    match key.code {
        // Runner zoom: Esc returns to the parent view, priority over focus
        // clearing — unless a completion popup is up, which is dismissed
        // first (mirrors the pre-ADR-0172 arm order).
        KeyCode::Esc if ctx.completion_kind == crate::completion::CompletionKind::None => {
            Some(InputAction::ExitRunner)
        }
        _ => resolve_chat_surface_key(key, ctx, input, cursor_position),
    }
}

/// The Side view's own scheme (ADR-0172): the aside owns its exit (`Esc`
/// returns to the main session), and every other key is the full chat scheme
/// — an aside is a normal transcript + composer.
pub(crate) fn resolve_side_key(
    key: crate::keymap::Key,
    ctx: &InputContext,
    input: &mut String,
    cursor_position: &mut usize,
) -> Option<InputAction> {
    match key.code {
        // Esc in an aside returns to the primary transcript (ADR-0103),
        // unless a completion popup is up (dismissed first).
        KeyCode::Esc if ctx.completion_kind == crate::completion::CompletionKind::None => {
            Some(InputAction::ExitSideView)
        }
        _ => resolve_chat_surface_key(key, ctx, input, cursor_position),
    }
}

/// Route a key to the full-screen view's own scheme (ADR-0172). Dashboard and
/// Settings do not own keyboard state yet and return `None`, so the router
/// falls through to the shared affordance library.
pub(crate) fn resolve_view_key(
    view: crate::surfaces::View,
    key: crate::keymap::Key,
    ctx: &InputContext,
    input: &mut String,
    cursor_position: &mut usize,
) -> Option<InputAction> {
    match view {
        crate::surfaces::View::Session => {
            resolve_chat_surface_key(key, ctx, input, cursor_position)
        }
        crate::surfaces::View::Runner => resolve_runner_key(key, ctx, input, cursor_position),
        crate::surfaces::View::Side => resolve_side_key(key, ctx, input, cursor_position),
        _ => None,
    }
}

/// Enter on the chat surface. Mode-sensitive:
/// a focused step activates, a highlighted completion commits, a unique slash
/// prefix auto-accepts, otherwise the draft is sent — or queued while running.
fn resolve_enter(
    ctx: &InputContext,
    input: &mut String,
    cursor_position: &mut usize,
) -> Option<InputAction> {
    if ctx.has_focused_target {
        return Some(InputAction::ActivateFocusedTarget);
    }
    // Slash-only: Enter on a unique prefix auto-accepts the first suggestion
    // rather than sending `/go` as a (rejected) command. Path mentions skip
    // this so Enter still sends the message.
    if ctx.completion_kind == crate::completion::CompletionKind::Slash
        && ctx.suggestion_count > 0
        && ctx.suggestion_index.is_none()
        && !ctx.has_exact_suggestion
    {
        return Some(InputAction::CommitSuggestion("0".to_string()));
    }
    // An explicit highlight (via ↑/↓ or Tab) wins over the exact-match slash
    // fast path below.
    if let Some(i) = ctx.suggestion_index
        && ctx.completion_kind != crate::completion::CompletionKind::None
    {
        return Some(InputAction::CommitSuggestion(i.to_string()));
    }
    let text = std::mem::take(input);
    *cursor_position = 0;
    if text.starts_with('/') {
        // Match on the trimmed text so a slash command typed with a trailing
        // space (e.g. `/models `) still hits the exact-match arm.
        let action = match text.trim() {
            "/models" => InputAction::OpenModels,
            "/connections" => InputAction::OpenConnections,
            "/permissions" => InputAction::OpenPermissions,
            "/tools" => InputAction::OpenTools,
            "/usage" => InputAction::OpenUsage,
            "/mcp" => InputAction::OpenMcp,
            "/skills" => InputAction::OpenSkills,
            // Bare `/settings` (or `/config`) opens the manager modal locally;
            // any argument form is a backend command and falls through to
            // SendSlash.
            "/settings" | "/config" => InputAction::OpenConfig,
            "/exit" => InputAction::Quit,
            _ => InputAction::SendSlash(text),
        };
        Some(action)
    } else if !text.is_empty() {
        if ctx.is_responding {
            Some(InputAction::QueueFollowUp(text))
        } else {
            Some(InputAction::SendChat(text))
        }
    } else {
        None
    }
}

/// Tab on the chat surface: commit a live completion or re-open a dismissed
/// menu. Tab owns no navigation duty (ADR-0173): with no completion up it is
/// inert and falls through to the shared affordance layer.
fn resolve_tab(ctx: &InputContext) -> Option<InputAction> {
    if ctx.completion_kind != crate::completion::CompletionKind::None
        && ctx.suggestion_count > 0
        && !ctx.has_exact_suggestion
        && !ctx.completion_dismissed
    {
        let idx = ctx.suggestion_index.unwrap_or(0);
        Some(InputAction::CommitSuggestion(idx.to_string()))
    } else if ctx.completion_kind != crate::completion::CompletionKind::None
        && ctx.completion_dismissed
        && ctx.has_trigger_text
        && !ctx.is_responding
    {
        Some(InputAction::ReopenCompletion)
    } else {
        None
    }
}

/// Esc on the Session view. Priority order mirrors the pre-ADR-0172 central
/// arm: dismiss an open completion first, then clear step focus, then
/// interrupt a running round. (Runner and Side own their own Esc exits in
/// [`resolve_runner_key`] / [`resolve_side_key`].)
fn resolve_esc(ctx: &InputContext) -> Option<InputAction> {
    if ctx.completion_kind != crate::completion::CompletionKind::None && !ctx.completion_dismissed {
        Some(InputAction::CloseCompletion)
    } else if ctx.has_focused_target {
        Some(InputAction::ClearFocusedTarget)
    } else if ctx.completion_kind != crate::completion::CompletionKind::None
        && ctx.suggestion_count > 0
        && !ctx.completion_dismissed
    {
        Some(InputAction::CloseCompletion)
    } else if ctx.is_responding {
        Some(InputAction::Interrupt)
    } else {
        None
    }
}

/// Alt+S: take the draft and steer it into the running round.
fn resolve_steer(
    ctx: &InputContext,
    input: &mut String,
    cursor_position: &mut usize,
) -> Option<InputAction> {
    if ctx.is_responding {
        let text = std::mem::take(input);
        *cursor_position = 0;
        Some(InputAction::SteerImmediate(text))
    } else {
        None
    }
}

/// ↑ on the chat surface: walk completion suggestions, move the caret up
/// through a multi-line draft, then — at the top line of the draft (ADR-0174
/// revision of ADR-0173's arrow table) — hand off to inline history recall
/// toward older entries. Transcript step walking stays owned by the
/// remappable `FocusPrevTarget` verb (canonical `Alt+↑`, ADR-0173).
fn resolve_up(
    ctx: &InputContext,
    input: &str,
    cursor_position: &mut usize,
) -> Option<InputAction> {
    if ctx.completion_kind != crate::completion::CompletionKind::None
        && ctx.suggestion_count > 0
        && !ctx.has_exact_suggestion
    {
        Some(InputAction::SuggestPrev)
    } else if crate::input::cursor_line_up(input, cursor_position) {
        // Multi-line draft: ↑ walks the caret to the previous line.
        Some(InputAction::None)
    } else {
        // Top line: readline-style edge hand-off to inline history recall.
        Some(InputAction::HistoryPrev)
    }
}

/// ↓ on the chat surface: mirror of [`resolve_up`], handing off to history
/// recall toward newer entries at the draft's last line.
fn resolve_down(
    ctx: &InputContext,
    input: &str,
    cursor_position: &mut usize,
) -> Option<InputAction> {
    if ctx.completion_kind != crate::completion::CompletionKind::None
        && ctx.suggestion_count > 0
        && !ctx.has_exact_suggestion
    {
        Some(InputAction::SuggestNext)
    } else if crate::input::cursor_line_down(input, cursor_position) {
        Some(InputAction::None)
    } else {
        // Last line: walk history forward (or restore the stashed draft once
        // the newest entry is passed).
        Some(InputAction::HistoryNext)
    }
}

/// A printable character on the chat surface. A focused transcript step does
/// not capture typing: the character is inserted into the composer and focus
/// bounces back to it (`ClearFocusedTarget`). Runner sibling navigation
/// (`[`/`]`) is owned by the Runner view's resolver.
fn resolve_printable(
    ctx: &InputContext,
    c: char,
    input: &mut String,
    cursor_position: &mut usize,
) -> Option<InputAction> {
    if ctx.has_focused_target {
        let byte_pos = crate::input::normalized_cursor_byte(input, *cursor_position);
        *cursor_position = crate::input::char_index_at_byte(input, byte_pos);
        input.insert(byte_pos, c);
        *cursor_position += 1;
        return Some(InputAction::ClearFocusedTarget);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::process_event;
    use crate::surfaces::View;
    use crossterm::event::{Event, KeyEvent, KeyEventKind, KeyEventState};

    /// Run states used to build a context for resolver tests. Test-local so
    /// the shipped module carries no dead types (ADR-0172: the resolver is
    /// the scheme; the mode table is a test concern).
    #[derive(Clone, Copy, Debug)]
    enum Mode {
        Idle,
        Running,
        FocusedTarget,
        Completion,
        Runner,
        Side,
    }

    /// A mode-appropriate chat-surface context for direct resolver tests.
    fn ctx(mode: Mode, tune: impl FnOnce(&mut InputContext)) -> InputContext {
        let mut c = InputContext::default();
        match mode {
            Mode::Runner => c.current_view = View::Runner,
            Mode::Side => c.current_view = View::Side,
            _ => c.current_view = View::Session,
        }
        match mode {
            Mode::Idle => {}
            Mode::Running => c.is_responding = true,
            Mode::FocusedTarget => c.has_focused_target = true,
            Mode::Completion => {
                c.completion_kind = crate::completion::CompletionKind::Slash;
                c.suggestion_count = 3;
                c.suggestion_index = Some(1);
            }
            Mode::Runner => c.in_runner_view = true,
            Mode::Side => c.in_side_view = true,
        }
        tune(&mut c);
        c
    }

    /// Route a chord through the real `process_event` pipeline to confirm the
    /// resolver is reached for the chat surface (ADR-0172 layer wiring).
    fn process(view: View, code: KeyCode, modifiers: KeyModifiers, mode: Mode) -> InputAction {
        let mut input = String::new();
        let mut cursor = 0;
        let mut drag = crate::model::selection::SelectionDrag::default();
        let mut context = ctx(mode, |_| {});
        context.current_view = view;
        process_event(
            Event::Key(KeyEvent {
                code,
                modifiers,
                kind: KeyEventKind::Press,
                state: KeyEventState::NONE,
            }),
            &mut input,
            &mut cursor,
            context,
            &mut drag,
        )
    }

    #[test]
    fn every_owned_chord_resolves_in_its_mode() {
        // The chat surface's owned chords, paired with the run state they act
        // in. Each must resolve (ADR-0172: an owned chord is never dead).
        let owned: &[(crate::keymap::Key, Mode)] = &[
            (crate::keymap::Key::ENTER, Mode::Idle),
            (crate::keymap::Key::ENTER, Mode::Running),
            (crate::keymap::Key::ENTER, Mode::FocusedTarget),
            (crate::keymap::Key::ESC, Mode::Running),
            (crate::keymap::Key::ESC, Mode::FocusedTarget),
            (crate::keymap::Key::ESC, Mode::Side),
            (crate::keymap::Key::ESC, Mode::Runner),
            (crate::keymap::Key::ALT_S, Mode::Running),
            (crate::keymap::Key::CTRL_R, Mode::Idle),
            (crate::keymap::Key::ALT_P, Mode::Idle),
            (crate::keymap::Key::ALT_N, Mode::Idle),
            (crate::keymap::Key::ALT_UP, Mode::Idle),
            (crate::keymap::Key::ALT_DOWN, Mode::FocusedTarget),
        ];
        for (key, mode) in owned {
            let c = ctx(*mode, |_| {});
            let mut input = String::from("hi");
            let mut cursor = input.chars().count();
            let resolved = resolve_view_key(c.current_view, *key, &c, &mut input, &mut cursor);
            assert!(
                resolved.is_some(),
                "owned chord {key:?} did not resolve in {mode:?}"
            );
        }
    }

    #[test]
    fn global_and_modal_keys_are_not_session_owned() {
        // Readline/editing and navigation chords are shared, not owned by the
        // chat surface — the resolver must leave them for the router.
        for key in [
            crate::keymap::Key::CTRL_A,
            crate::keymap::Key::CTRL_G,
            crate::keymap::Key::CTRL_W,
            crate::keymap::Key::CTRL_L,
            crate::keymap::Key::CTRL_O,
            crate::keymap::Key::CTRL_N,
            crate::keymap::Key::PAGE_UP,
            crate::keymap::Key::PAGE_DOWN,
        ] {
            let c = ctx(Mode::Idle, |_| {});
            let mut input = String::from("x");
            let mut cursor = 1;
            assert!(
                resolve_chat_surface_key(key, &c, &mut input, &mut cursor).is_none(),
                "{key:?} must not be owned by the chat surface"
            );
        }
    }

    #[test]
    fn enter_multi_mode_reactions() {
        // Idle + text → send.
        let action = {
            let mut input = String::from("hello");
            let mut cursor = 5;
            resolve_chat_surface_key(
                crate::keymap::Key::ENTER,
                &ctx(Mode::Idle, |_| {}),
                &mut input,
                &mut cursor,
            )
        };
        assert_eq!(action, Some(InputAction::SendChat("hello".into())));

        // Running + text → queue follow-up.
        let action = {
            let mut input = String::from("next");
            let mut cursor = 4;
            resolve_chat_surface_key(
                crate::keymap::Key::ENTER,
                &ctx(Mode::Running, |_| {}),
                &mut input,
                &mut cursor,
            )
        };
        assert_eq!(action, Some(InputAction::QueueFollowUp("next".into())));

        // Focused step → activate, ignoring the draft.
        let action = {
            let mut input = String::from("draft");
            let mut cursor = 5;
            resolve_chat_surface_key(
                crate::keymap::Key::ENTER,
                &ctx(Mode::FocusedTarget, |_| {}),
                &mut input,
                &mut cursor,
            )
        };
        assert_eq!(action, Some(InputAction::ActivateFocusedTarget));

        // Highlighted completion → commit the highlighted item.
        let action = resolve_chat_surface_key(
            crate::keymap::Key::ENTER,
            &ctx(Mode::Completion, |_| {}),
            &mut String::new(),
            &mut 0,
        );
        assert_eq!(action, Some(InputAction::CommitSuggestion("1".into())));
    }

    #[test]
    fn focus_navigation_and_interrupt_react_to_mode() {
        // Esc interrupts only while running.
        assert_eq!(
            resolve_chat_surface_key(
                crate::keymap::Key::ESC,
                &ctx(Mode::Running, |_| {}),
                &mut String::new(),
                &mut 0,
            ),
            Some(InputAction::Interrupt)
        );
        // Esc clears focus while a step is focused, even when running.
        assert_eq!(
            resolve_chat_surface_key(
                crate::keymap::Key::ESC,
                &ctx(Mode::FocusedTarget, |c| c.is_responding = true),
                &mut String::new(),
                &mut 0,
            ),
            Some(InputAction::ClearFocusedTarget)
        );
        // Tab without completion is inert (ADR-0173: completion-only).
        assert_eq!(
            resolve_chat_surface_key(
                crate::keymap::Key::TAB,
                &ctx(Mode::Idle, |_| {}),
                &mut String::new(),
                &mut 0,
            ),
            None
        );
        // Bare ↑ with no completion and a single-line draft hands off to
        // inline history recall (ADR-0174 edge hand-off revision).
        assert_eq!(
            resolve_chat_surface_key(
                crate::keymap::Key::UP,
                &ctx(Mode::FocusedTarget, |_| {}),
                &mut String::new(),
                &mut 0,
            ),
            Some(InputAction::HistoryPrev)
        );
        // Alt+S steers only while running.
        assert!(
            resolve_chat_surface_key(
                crate::keymap::Key::ALT_S,
                &ctx(Mode::Idle, |_| {}),
                &mut String::from("steer"),
                &mut 5,
            )
            .is_none()
        );
    }

    #[test]
    fn typed_character_bounces_focus_to_composer() {
        let mut input = String::from("");
        let mut cursor = 0;
        let action = resolve_chat_surface_key(
            crate::keymap::Key {
                modifiers: KeyModifiers::NONE,
                code: KeyCode::Char('x'),
            },
            &ctx(Mode::FocusedTarget, |_| {}),
            &mut input,
            &mut cursor,
        );
        assert_eq!(action, Some(InputAction::ClearFocusedTarget));
        assert_eq!(input, "x", "typed char must land in the composer");
    }

    #[test]
    fn router_offers_chat_keys_only_on_chat_surfaces() {
        use crate::input::InputAction;
        // Tab on the Session view without a completion is inert (ADR-0173:
        // the chord belongs to completion, not to plane switching).
        assert_eq!(
            process(View::Session, KeyCode::Tab, KeyModifiers::NONE, Mode::Idle),
            InputAction::None
        );
        // On the Settings view the resolver is never consulted: Tab is inert.
        assert_eq!(
            process(View::Settings, KeyCode::Tab, KeyModifiers::NONE, Mode::Idle),
            InputAction::None
        );
        // A runner step's Enter still activates through the shared path.
        assert_eq!(
            process(
                View::Runner,
                KeyCode::Enter,
                KeyModifiers::NONE,
                Mode::FocusedTarget
            ),
            InputAction::ActivateFocusedTarget
        );
    }

    /// Read the current key-handling region's Enter behavior as a canary:
    /// after the extraction the central match must no longer send the draft.
    #[test]
    fn advertised_hints_are_resolvable_in_their_state() {
        // Every chord `live_chat_hints` advertises for a run state must be
        // consumable by the resolver in that state (ADR-0172: hints and
        // dispatch share a semantic origin).
        use crate::keymap::Key;

        // Idle.
        let c = ctx(Mode::Idle, |_| {});
        let mut input = String::from("hi");
        assert!(resolve_chat_surface_key(Key::ENTER, &c, &mut input, &mut 2).is_some());

        // Running.
        let c = ctx(Mode::Running, |_| {});
        let mut input = String::from("next");
        assert!(resolve_chat_surface_key(Key::ENTER, &c, &mut input, &mut 4).is_some());
        let mut input = String::from("steer");
        assert!(resolve_chat_surface_key(Key::ALT_S, &c, &mut input, &mut 5).is_some());

        // Completion.
        let c = ctx(Mode::Completion, |_| {});
        assert!(resolve_chat_surface_key(Key::ESC, &c, &mut String::new(), &mut 0).is_some());
        assert!(resolve_chat_surface_key(Key::TAB, &c, &mut String::new(), &mut 0).is_some());
        assert!(resolve_chat_surface_key(Key::ENTER, &c, &mut String::new(), &mut 0).is_some());
    }

    #[test]
    fn surface_overrides_remap_verbs_and_kill_canonical_chords() {
        use crate::keymap::{Key, SurfaceOverrides};

        let mut map = std::collections::HashMap::new();
        map.insert("open_history".to_string(), "ctrl+shift+r".to_string());
        map.insert("steer".to_string(), "alt+enter".to_string());
        map.insert("prev_sibling".to_string(), "alt+[".to_string());
        let ov = SurfaceOverrides::from_config(&map);
        let c = ctx(Mode::Idle, |c| c.surface_overrides = ov.clone());

        // The assigned chord fires; the canonical chord is dead.
        assert_eq!(
            resolve_chat_surface_key(
                crate::keymap::Key::CTRL_SHIFT_R,
                &c,
                &mut String::new(),
                &mut 0
            ),
            Some(InputAction::OpenHistory)
        );
        assert_eq!(
            resolve_chat_surface_key(crate::keymap::Key::CTRL_R, &c, &mut String::new(), &mut 0),
            None,
            "the canonical Ctrl+R must go inactive once open_history is remapped"
        );

        // Steer's guard still applies on the remapped chord: fires only while
        // running.
        assert_eq!(
            resolve_chat_surface_key(
                crate::keymap::Key::ALT_ENTER,
                &ctx(Mode::Running, |c| c.surface_overrides = ov.clone()),
                &mut String::from("steer"),
                &mut 5
            ),
            Some(InputAction::SteerImmediate("steer".into()))
        );
        assert_eq!(
            resolve_chat_surface_key(
                crate::keymap::Key::ALT_ENTER,
                &c,
                &mut String::from("steer"),
                &mut 5
            ),
            None,
            "steer on a remapped chord must still require a running round"
        );

        // Runner sibling nav follows the remapped chord, same guards.
        let rc = ctx(Mode::Runner, |c| c.surface_overrides = ov);
        assert_eq!(
            resolve_view_key(
                View::Runner,
                Key::ALT_BRACKET_LEFT,
                &rc,
                &mut String::new(),
                &mut 0
            ),
            Some(InputAction::PrevSibling)
        );
        assert_eq!(
            resolve_view_key(
                View::Runner,
                Key::BRACKET_LEFT,
                &rc,
                &mut String::new(),
                &mut 0
            ),
            None,
            "the canonical `[` must go inactive once prev_sibling is remapped"
        );
    }

    #[test]
    fn runner_and_side_own_their_esc_and_navigation() {
        use crate::keymap::Key;

        // Runner: Esc exits the zoom — even while a step is focused.
        assert_eq!(
            resolve_view_key(
                View::Runner,
                Key::ESC,
                &ctx(Mode::Runner, |_| {}),
                &mut String::new(),
                &mut 0
            ),
            Some(InputAction::ExitRunner)
        );
        assert_eq!(
            resolve_view_key(
                View::Runner,
                Key::ESC,
                &ctx(Mode::Runner, |c| c.has_focused_target = true),
                &mut String::new(),
                &mut 0
            ),
            Some(InputAction::ExitRunner),
            "runner Esc exits even with a focused step"
        );
        // `[` / `]` walk siblings while the composer is empty and no step is
        // focused; a focused step bounces the key to the composer instead.
        let bracket = Key {
            modifiers: KeyModifiers::NONE,
            code: KeyCode::Char('['),
        };
        assert_eq!(
            resolve_view_key(
                View::Runner,
                bracket,
                &ctx(Mode::Runner, |_| {}),
                &mut String::new(),
                &mut 0
            ),
            Some(InputAction::PrevSibling)
        );
        assert_eq!(
            resolve_view_key(
                View::Runner,
                bracket,
                &ctx(Mode::Runner, |c| c.has_focused_target = true),
                &mut String::new(),
                &mut 0
            ),
            Some(InputAction::ClearFocusedTarget),
            "focused step owns `[` via the bounce path"
        );

        // Side: Esc returns to the main session, unless a completion is up.
        assert_eq!(
            resolve_view_key(
                View::Side,
                Key::ESC,
                &ctx(Mode::Side, |_| {}),
                &mut String::new(),
                &mut 0
            ),
            Some(InputAction::ExitSideView)
        );
        assert_eq!(
            resolve_view_key(
                View::Side,
                Key::ESC,
                &ctx(Mode::Side, |c| {
                    c.completion_kind = crate::completion::CompletionKind::Slash;
                    c.completion_dismissed = false;
                }),
                &mut String::new(),
                &mut 0
            ),
            Some(InputAction::CloseCompletion),
            "side Esc dismisses a completion before returning"
        );

        // The Session view never emits the runner/side exits.
        let c = ctx(Mode::FocusedTarget, |_| {});
        let action = resolve_view_key(View::Session, Key::ESC, &c, &mut String::new(), &mut 0);
        assert_ne!(action, Some(InputAction::ExitRunner));
        assert_ne!(action, Some(InputAction::ExitSideView));
    }

    /// ADR-0174: the readline-style edge hand-off. On a single-line draft ↑
    /// resolves to `HistoryPrev` and ↓ to `HistoryNext` once completion and
    /// caret motion have had their chance; a multi-line draft only hands off
    /// from its true first/last line.
    #[test]
    fn arrow_edge_hands_off_to_history_recall() {
        // Single-line draft: both edges hand off immediately.
        let mut input = String::from("hello");
        let mut cursor = 5;
        assert_eq!(
            resolve_chat_surface_key(
                crate::keymap::Key::UP,
                &ctx(Mode::Idle, |_| {}),
                &mut input,
                &mut cursor
            ),
            Some(InputAction::HistoryPrev)
        );
        assert_eq!(
            resolve_chat_surface_key(
                crate::keymap::Key::DOWN,
                &ctx(Mode::Idle, |_| {}),
                &mut input,
                &mut cursor
            ),
            Some(InputAction::HistoryNext)
        );

        // Multi-line draft, caret on the middle line: no hand-off.
        let mut input = String::from("one\ntwo\nthree");
        let mut cursor = 5; // on "two"
        assert_eq!(
            resolve_chat_surface_key(
                crate::keymap::Key::UP,
                &ctx(Mode::Idle, |_| {}),
                &mut input,
                &mut cursor
            ),
            Some(InputAction::None)
        );
        assert_eq!(
            resolve_chat_surface_key(
                crate::keymap::Key::DOWN,
                &ctx(Mode::Idle, |_| {}),
                &mut input,
                &mut cursor
            ),
            Some(InputAction::None)
        );

        // Caret on the first line of a multi-line draft: ↑ hands off without
        // disturbing the draft; ↓ stays a caret motion.
        let mut input = String::from("one\ntwo");
        let mut cursor = 2;
        assert_eq!(
            resolve_chat_surface_key(
                crate::keymap::Key::UP,
                &ctx(Mode::Idle, |_| {}),
                &mut input,
                &mut cursor
            ),
            Some(InputAction::HistoryPrev)
        );
        assert_eq!(input, "one\ntwo", "the hand-off must not mutate the draft");
        let mut cursor = 0;
        assert_eq!(
            resolve_chat_surface_key(
                crate::keymap::Key::DOWN,
                &ctx(Mode::Idle, |_| {}),
                &mut input,
                &mut cursor
            ),
            Some(InputAction::None)
        );
        assert_eq!(cursor, 4, "↓ moved the caret to the next line (column kept)");
    }

    #[test]
    fn central_match_no_longer_owns_chat_enter_on_settings() {
        // Enter on Settings (non-chat) with a draft must NOT send it — the
        // send path is chat-surface-owned.
        let mut input = String::from("draft");
        let mut cursor = 5;
        let mut drag = crate::model::selection::SelectionDrag::default();
        let action = process_event(
            Event::Key(KeyEvent {
                code: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                kind: KeyEventKind::Press,
                state: KeyEventState::NONE,
            }),
            &mut input,
            &mut cursor,
            InputContext {
                current_view: View::Settings,
                ..Default::default()
            },
            &mut drag,
        );
        assert_eq!(action, InputAction::None);
    }
}
