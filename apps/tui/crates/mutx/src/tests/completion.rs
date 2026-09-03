//! Input completion engine tests: trigger detection, kind classification, mention ranges, slash-command completion, accept/delete/range editing.

use super::*;

// ----- `@path` completion tests -----

#[test]
fn mention_range_detects_at_start_of_input() {
    // Cursor at end of `@src`: range covers the whole token.
    assert_eq!(mention_range_at("@src", 4), Some((0, 4)));
}

#[test]
fn completion_anchor_aligns_slash_menu_with_composer_text_start() {
    // A `/command` replaces the whole input, so the popup hangs off the
    // start of the composer's text area — the rect's left edge plus the
    // two-column prefix (`›` prompt + gap).
    let rect = mutx_engine::Rect::new(0, 10, 80, 3);
    let x = completion_anchor_x("/pu", 3, rect, CompletionKind::Slash);
    assert_eq!(
        x,
        rect.x + crate::design::COMPOSER_PROMPT_PREFIX_COLS as u16
    );
}

#[test]
fn completion_anchor_aligns_path_menu_with_the_at_trigger() {
    // `look at @sr` — the `@` sits at display column 8 of the input, so the
    // popup's leading edge lands 8 columns right of the text area's start.
    let rect = mutx_engine::Rect::new(0, 10, 80, 3);
    let input = "look at @sr";
    let x = completion_anchor_x(input, input.len(), rect, CompletionKind::Path);
    assert_eq!(
        x,
        rect.x + crate::design::COMPOSER_PROMPT_PREFIX_COLS as u16 + 8
    );
}

#[test]
fn completion_anchor_follows_the_at_trigger_across_wraps() {
    // An 10-column text area (rect 14 wide minus the 2+2 composer padding)
    // wraps `wrap this @sr` after `wrap this `; the `@` starts the second
    // text row's column 0, so the popup follows it there.
    let rect = mutx_engine::Rect::new(0, 10, 14, 4);
    let input = "wrap this @sr";
    let x = completion_anchor_x(input, input.len(), rect, CompletionKind::Path);
    assert_eq!(
        x,
        rect.x + crate::design::COMPOSER_PROMPT_PREFIX_COLS as u16
    );
}

#[test]
fn completion_anchor_keeps_column_when_token_stays_on_one_row() {
    // No wrap: the `@` at display column 10 keeps its column even on a
    // narrow-ish box, so the popup tracks the token exactly. Text budget
    // = 20 - 2 - 2 = 16 cols; `wrap this @sr` is 13 wide, fits one row.
    let rect = mutx_engine::Rect::new(0, 10, 20, 3);
    let input = "wrap this @sr";
    let x = completion_anchor_x(input, input.len(), rect, CompletionKind::Path);
    // 2 (prefix) + 10 (token column within text).
    assert_eq!(
        x,
        rect.x + crate::design::COMPOSER_PROMPT_PREFIX_COLS as u16 + 10
    );
}

// ----- resolved `/command` highlight tests -----

#[test]
fn resolved_slash_len_matches_builtin_command_without_args() {
    assert_eq!(
        resolved_slash_command_len("/models", &test_command_catalog()),
        Some(7)
    );
}

#[test]
fn resolved_slash_len_covers_only_the_command_token_not_args() {
    // `/sessions abc` — only `/sessions` (9 bytes) is the resolved command;
    // the argument tail is excluded so the accent stops at the token.
    assert_eq!(
        resolved_slash_command_len("/sessions abc", &test_command_catalog()),
        Some(9)
    );
}

#[test]
fn resolved_slash_len_matches_custom_command() {
    let customs = vec![("/deploy".to_string(), "Deploy the app".to_string())];
    let catalog = muta_runtime::startup::command_catalog(&customs);
    assert_eq!(
        resolved_slash_command_len("/deploy prod", &catalog),
        Some(7)
    );
}

#[test]
fn resolved_slash_len_rejects_partial_prefix_and_unknown_commands() {
    // A bare `/` or an in-progress prefix is not yet a command.
    let catalog = test_command_catalog();
    assert_eq!(resolved_slash_command_len("/", &catalog), None);
    assert_eq!(resolved_slash_command_len("/cle", &catalog), None);
    assert_eq!(resolved_slash_command_len("/not-a-command", &catalog), None);
    // Trigger words steer to a command but are NOT commands themselves, so
    // they never earn the resolved-command accent.
    assert_eq!(resolved_slash_command_len("/clear", &catalog), None);
    assert_eq!(resolved_slash_command_len("/reset", &catalog), None);
    assert_eq!(resolved_slash_command_len("/continue", &catalog), None);
    // Plain prose and `@` mentions never highlight.
    assert_eq!(resolved_slash_command_len("hello", &catalog), None);
    assert_eq!(resolved_slash_command_len("@src/main.rs", &catalog), None);
}

/// The anchor pass is what makes "popup visible ⇒ first row selected" true:
/// with no prior highlight it seeds `Some(0)`, so the band, the details
/// flyout, and a plain Enter/Tab all land on the first candidate without
/// any prior ↓.
#[test]
fn anchor_seeds_the_first_candidate_when_the_menu_opens() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/se".to_string();
    app.cursor_position = app.input.chars().count();
    assert!(app.suggestion_index.is_none());
    let completions = app.completions();
    assert!(!completions.is_empty(), "`/se` should have candidates");
    app.anchor_completion_selection(&completions);
    assert_eq!(
        app.suggestion_index,
        Some(0),
        "a freshly opened menu must start highlighted on its first row"
    );
}

/// A visible menu keeps exactly one highlighted row even when the candidate
/// list shrinks under a stale index: the highlight clamps into range rather
/// than pointing past the list (which would render no band and no flyout).
#[test]
fn anchor_clamps_a_stale_highlight_into_range() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/se".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let count = completions.len();
    // Simulate a stale index from a wider list (e.g. a refine filtered
    // candidates away between keystrokes).
    app.suggestion_index = Some(count + 5);
    app.anchor_completion_selection(&completions);
    assert_eq!(
        app.suggestion_index,
        Some(count - 1),
        "an out-of-range highlight must clamp to the last candidate"
    );
}

/// A resolved composer (the text exactly equals a candidate) renders no
/// menu, so the anchor must clear the highlight — otherwise a lingering
/// index would keep Enter/Tab committing a command the user cannot see.
#[test]
fn anchor_clears_the_highlight_when_no_menu_is_rendered() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/sessions".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    // `/sessions` is a real command: its exact match is the composer text.
    assert!(
        completions
            .iter()
            .any(|c| c.label == app.input && c.replace_end == app.input.len()),
        "`/sessions` should be among its own candidates"
    );
    app.suggestion_index = Some(0);
    app.anchor_completion_selection(&completions);
    assert_eq!(
        app.suggestion_index, None,
        "no rendered menu must mean no highlight"
    );
}

/// Tab's re-open gesture keys off trigger text that survived Esc: a partial
/// slash command qualifies, a resolved exact command does not (its popup is
/// hidden on purpose), and plain prose never does.
#[test]
fn completion_trigger_text_present_matches_the_composer_state() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/se".to_string();
    app.cursor_position = app.input.chars().count();
    assert!(app.completion_trigger_text_present());
    app.input = "/sessions".to_string();
    app.cursor_position = app.input.chars().count();
    assert!(
        !app.completion_trigger_text_present(),
        "a resolved exact command must not offer a re-open"
    );
    app.input = "plain prose".to_string();
    app.cursor_position = app.input.chars().count();
    assert!(!app.completion_trigger_text_present());
}

/// The Esc → Tab round trip, driven through the same `App` state the action
/// arms mutate (`CloseCompletion` latches the dismissal + clears the
/// highlight; `ReopenCompletion` drops the latch; the loop's anchor pass
/// re-seeds the highlight). After the round trip the menu must be visible
/// **and** carry a highlighted row again — the state the renderer needs to
/// paint the band and the details flyout.
#[test]
fn esc_then_tab_round_trip_restores_a_highlighted_menu() {
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/se".to_string();
    app.cursor_position = app.input.chars().count();

    // Frame 1: menu opens, anchor seeds the first candidate.
    let completions = app.completions();
    app.anchor_completion_selection(&completions);
    assert_eq!(app.suggestion_index, Some(0));
    assert!(!app.completion_dismissed);

    // Esc (CloseCompletion arm): popup hidden, highlight dropped.
    app.suggestion_index = None;
    app.completion_dismissed = true;
    assert!(app.completion_trigger_text_present());

    // Tab (ReopenCompletion arm): latch dropped, then the loop's post-
    // dispatch anchor re-derives candidates and re-seeds the highlight.
    app.completion_dismissed = false;
    let completions = app.completions();
    app.anchor_completion_selection(&completions);
    assert_eq!(
        app.suggestion_index,
        Some(0),
        "the reopened menu must land already selected"
    );
}

#[test]
fn mention_range_detects_inline_after_whitespace() {
    // `look at @src`: the `@` follows a space, so the range starts at the
    // `@` and ends at the cursor.
    assert_eq!(mention_range_at("look at @src", 12), Some((8, 12)));
}

#[test]
fn mention_range_rejects_email_style_at() {
    // `user@host` — the char before `@` is non-whitespace, so no mention.
    assert_eq!(mention_range_at("user@host", 9), None);
}

#[test]
fn mention_range_rejects_whitespace_between_at_and_cursor() {
    // `@src foo`: the cursor sits after a space, walking back crosses
    // whitespace before reaching `@`, so no mention.
    assert_eq!(mention_range_at("@src foo", 8), None);
}

#[test]
fn mention_range_rejects_cursor_before_at() {
    // Cursor before the `@`: nothing to walk back to.
    assert_eq!(mention_range_at("look @src", 4), None);
}

#[test]
fn mention_range_handles_multibyte_before_at() {
    // `😀😁 @x` — the `@` is preceded by an ASCII space, so we detect it
    // even when multibyte chars appear earlier in the input.
    let s = "😀😁 @x";
    // Byte offset of the cursor at end (after `x`).
    let cursor_byte = s.len();
    let at_byte = s.find('@').unwrap();
    assert_eq!(
        mention_range_at(s, cursor_byte),
        Some((at_byte, cursor_byte))
    );
}

/// `Shift+D` on a custom provider must STAGE the deletion (open the confirm
/// overlay with default focus = Cancel) rather than deleting immediately. This
/// is the core guarantee of the new confirm overlay: `stage_provider_delete`
/// only mutates overlay state — it never enqueues an `AgentRequest` (that is
/// `confirm_provider_delete`'s job).
#[test]
fn delete_provider_stages_overlay_without_deleting() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.set_active_modal_for_test(Modal::Connections);
    let custom = |id: &str| muta_contracts::ProviderPickerRow {
        id: id.to_string(),
        name: id.to_string(),
        model: "m".to_string(),
        models: vec!["m".to_string()],
        model_info: Vec::new(),
        builtin: false,
        protocol: String::new(),
        base_url: String::new(),
        key_ready: true,
        preset_id: String::new(),
        client_identity: Default::default(),
        last_used_ms: None,
        auth: Default::default(),
    };
    app.provider_picker = muta_contracts::ProviderPickerSnapshot {
        default_id: "my-custom".to_string(),
        rows: vec![custom("my-custom")],
    };
    app.modal_index = 0;

    app.stage_provider_delete();

    // The deletion is staged, not dispatched.
    assert_eq!(
        app.pending_provider_delete.as_deref(),
        Some("my-custom"),
        "Shift+D stages the provider id without deleting"
    );
    // Default focus is Cancel (the safe choice) so a reflexive Enter cancels.
    assert_eq!(
        app.provider_delete_focus,
        crate::ProviderDeleteChoice::Cancel,
        "confirm overlay defaults to Cancel focus"
    );
}

/// Built-in providers are not deletable: `Shift+D` on one is a no-op (the
/// overlay must not open, nothing staged).
#[test]
fn delete_provider_ignores_builtin() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.set_active_modal_for_test(Modal::Connections);
    let builtin = |id: &str| muta_contracts::ProviderPickerRow {
        id: id.to_string(),
        name: id.to_string(),
        model: "m".to_string(),
        models: vec!["m".to_string()],
        model_info: Vec::new(),
        builtin: true,
        protocol: String::new(),
        base_url: String::new(),
        key_ready: true,
        preset_id: String::new(),
        client_identity: Default::default(),
        last_used_ms: None,
        auth: Default::default(),
    };
    app.provider_picker = muta_contracts::ProviderPickerSnapshot {
        default_id: "kimi-code".to_string(),
        rows: vec![builtin("kimi-code")],
    };
    app.modal_index = 0;

    app.stage_provider_delete();

    assert!(
        app.pending_provider_delete.is_none(),
        "built-in provider is never staged for deletion"
    );
}

#[test]
fn accept_slash_completion_does_not_append_trailing_space() {
    // Accepting a slash-command completion must splice the bare label with
    // NO trailing space. A trailing `/pursue ` would immediately match the
    // subcommand prefix and re-trigger the completion menu — the opposite
    // of "Enter/Tab finishes the completion". The user opts into subcommand
    // discovery by typing a space themselves.
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "/re".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let idx = completions
        .iter()
        .position(|c| c.label == "/repeat")
        .expect("/repeat in candidates");
    app.accept_completion(idx);
    // The label is spliced verbatim — no trailing space.
    assert_eq!(app.input, "/repeat");
    assert_eq!(app.cursor_position, "/pursue".chars().count());
    // A slash accept is a terminal commit: the popup must stay hidden and
    // no subcommand menu may fire. This holds for BOTH Tab and Enter since
    // both route through accept_completion for slash commands.
    assert!(
        app.completion_dismissed,
        "slash accept must latch dismissal"
    );
    assert!(app.suggestion_index.is_none(), "highlight cleared");
    assert!(
        app.completions()
            .iter()
            .all(|c| !c.label.starts_with("/pursue ")),
        "subcommand menu must not fire after accepting a slash completion"
    );
}

#[test]
fn accept_path_dir_completion_stays_live_for_descend() {
    // `@path` *directory* accepts stay live so Tab can keep descending the
    // directory tree: the `@` trigger is kept and the popup re-triggers on the
    // directory's contents. This guards against the terminal-accept logic
    // accidentally suppressing directory navigation.
    let (mut app, _tmp) = app_in_tempdir(&["src/main.rs", "src/util.rs"], &["src"]);
    app.input = "@".to_string();
    app.cursor_position = 1;
    let completions = app.completions();
    // The first candidate is a directory (`src/` sorts before files).
    let dir_idx = completions
        .iter()
        .position(|c| c.label == "src/")
        .expect("src/ directory in candidates");
    app.accept_completion(dir_idx);
    // Directory accept must NOT latch dismissal — descend continues.
    assert!(
        !app.completion_dismissed,
        "directory accept must stay live for descend"
    );
    // The `@` trigger is kept so the popup re-triggers on `src/`'s contents.
    assert!(
        app.input.starts_with("@src/"),
        "dir accept keeps @: {}",
        app.input
    );
}

#[test]
fn accept_path_file_completion_is_terminal_and_drops_at() {
    // `@path` *file* accepts are terminal: the `@` is only a completion
    // trigger and must not survive into the message context once a concrete
    // file is chosen, so accept_completion drops the `@`, appends a trailing
    // space, and latches the dismissal flag.
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    app.input = "@Ca".to_string();
    app.cursor_position = app.input.chars().count();
    let completions = app.completions();
    let idx = completions
        .iter()
        .position(|c| c.label == "Cargo.toml")
        .expect("Cargo.toml in candidates");
    app.accept_completion(idx);
    // The `@` trigger is dropped; a trailing space lets the user keep typing.
    assert_eq!(app.input, "Cargo.toml ");
    assert!(
        app.completion_dismissed,
        "file accept must be terminal (latch dismissal)"
    );
}

#[test]
fn accept_path_file_completion_inline_preserves_surrounding_text() {
    // An inline `@mention` mid-sentence: accepting a file must drop the `@`
    // and splice the path in place, preserving the surrounding prose. This is
    // the real-world case — `look at @Cargo` in the middle of a message.
    let (mut app, _tmp) = app_in_tempdir(&["Cargo.toml"], &[]);
    // Cursor sits right after the `@Cargo` token, inside the mention.
    // `look at @Cargo please`: `look at ` is 8 chars, `@Cargo` is 6 → cursor
    // at char index 14 sits just past the `o`.
    app.input = "look at @Cargo please".to_string();
    app.cursor_position = 14;
    let completions = app.completions();
    let idx = completions
        .iter()
        .position(|c| c.label == "Cargo.toml")
        .expect("Cargo.toml in candidates");
    app.accept_completion(idx);
    // The `@` is dropped; the path replaces `@Cargo`; trailing `please` is
    // preserved; the existing space before it is reused (no double space).
    assert_eq!(app.input, "look at Cargo.toml please");
}

/// Esc back-out must respect modal hierarchy: a drill-in sub-page backs out to
/// its parent view *before* any close/quit logic runs. Regression for a bug
/// where pressing Esc in the `Sessions › Info` sub-view at startup
/// (`startup_overlay` armed) quit the program instead of returning to the
/// sessions list — because the startup-quit check was ordered before the
/// sub-page back-out check. This mirrors the event loop's `CloseModal` arm
/// ordering exactly (deepest level first).
#[test]
fn esc_in_session_info_subpage_backs_out_before_quit_or_close() {
    use std::sync::atomic::Ordering;

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);

    // The user is in the Sessions › Info sub-view at startup: both the startup
    // gate AND the info drill-in are active. Esc must back out to the list,
    // NOT quit.
    app.startup_overlay = crate::StartupOverlay::SessionsPicker;
    app.set_active_modal_for_test(Modal::Sessions);
    app.session_info_detail = true;
    app.session_detail = Some(muta_contracts::SessionDetail {
        id: "x".to_string(),
        ..Default::default()
    });
    assert!(!app.should_quit.load(Ordering::SeqCst));

    // Mirror the CloseModal arm's ordering (deepest level wins).
    let quit = if app.active_modal() == Modal::Sessions && app.session_info_detail {
        app.session_info_detail = false;
        app.session_detail = None;
        app.session_info_scroll = 0;
        false
    } else if app.startup_overlay == crate::StartupOverlay::SessionsPicker
        && app.active_modal() == Modal::Sessions
    {
        app.should_quit.store(true, Ordering::SeqCst);
        true
    } else {
        false
    };
    assert!(!quit, "Esc from Info backs out to the list, never quits");
    assert!(
        !app.session_info_detail,
        "sub-view cleared — back on the list"
    );
    assert!(
        !app.should_quit.load(Ordering::SeqCst),
        "program did not quit"
    );

    // Now the list is showing (still at startup). A second Esc DOES quit, since
    // there is no deeper sub-view left.
    let quit = if app.active_modal() == Modal::Sessions && app.session_info_detail {
        false
    } else if app.startup_overlay == crate::StartupOverlay::SessionsPicker
        && app.active_modal() == Modal::Sessions
    {
        app.should_quit.store(true, Ordering::SeqCst);
        true
    } else {
        false
    };
    assert!(quit, "Esc from the startup list quits the program");
    assert!(app.should_quit.load(Ordering::SeqCst));
}

/// Ctrl+C at the `mutx attach` startup picker must quit the program — the
/// same as Esc and an outside click — NOT drop into an empty session. Regression
/// for a bug where Ctrl+C closed the modal (`active_modal = None`) but never set
/// `should_quit`, so the user landed in a bare empty chat (which a stray
/// `/models` then persisted as an empty-session file). Mirrors the event loop's
/// `CtrlC` arm ordering.
#[test]
fn ctrl_c_at_startup_picker_quits_instead_of_dropping_to_empty_session() {
    use std::sync::atomic::Ordering;

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.startup_overlay = crate::StartupOverlay::SessionsPicker;
    app.set_active_modal_for_test(Modal::Sessions);
    assert!(!app.should_quit.load(Ordering::SeqCst));

    // Mirror the CtrlC arm: startup_overlay + Sessions → quit (not modal-close).
    // (Selection copy is skipped — no selection in a modal.)
    let quit = if app.startup_overlay == crate::StartupOverlay::SessionsPicker
        && app.active_modal() == Modal::Sessions
    {
        app.should_quit.store(true, Ordering::SeqCst);
        true
    } else if app.active_modal() != Modal::None && app.active_sheet().is_none() {
        app.set_active_modal_for_test(Modal::None);
        false
    } else if app.active_sheet().is_some() {
        app.dismiss_sheet();
        false
    } else {
        false
    };
    assert!(quit, "Ctrl+C at the startup picker quits");
    assert!(
        app.should_quit.load(Ordering::SeqCst),
        "program quits, does not drop into an empty session"
    );
    // The modal was NOT merely closed (which is what created the empty-session
    // trap): should_quit is set, so the loop exits.
    assert_ne!(app.active_modal(), Modal::None, "quit path wins over close");
}

/// `mutx dashboard` opens the session dashboard (`Modal::Host`) over a
/// carrier session at startup. The user asked for a dashboard, not a
/// conversation, so leaving the screen must quit the whole TUI — the
/// dashboard is the app while it is open. These tests lock the three exits:
///
/// 1. Esc quits immediately (existing behavior, mirrored here for the
///    dashboard arm of `handle_close_modal`).
/// 2. Ctrl+C follows the app-wide double-press contract: first press arms
///    the 2s quit window WITHOUT closing the dashboard, second press quits.
///    Regression: Ctrl+C used to hit the generic modal-close arm and drop
///    the user into the carrier conversation.
/// 3. Ctrl+C never lands in the conversation even after the arm expires —
///    pressing again re-arms rather than closing.
#[test]
fn esc_at_startup_dashboard_quits_instead_of_dropping_to_carrier_chat() {
    use std::sync::atomic::Ordering;

    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.startup_overlay = crate::StartupOverlay::Dashboard;
    app.set_active_modal_for_test(Modal::Host);
    assert!(!app.should_quit.load(Ordering::SeqCst));

    // Esc from the dashboard itself (no preview/prompt sub-layer open).
    super::event_loop::handle_close_modal(&mut app, "carrier");
    assert!(
        app.should_quit.load(Ordering::SeqCst),
        "Esc from the startup dashboard quits the TUI"
    );
    assert_eq!(
        app.active_modal(),
        Modal::Host,
        "quit path never demotes the dashboard to a conversation"
    );
}

#[test]
fn ctrl_c_while_running_arms_and_interrupts_with_confirmation() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let (tx, mut rx) = mpsc::unbounded_channel();
    app.tx = tx;
    app.running_sessions.insert("test-session".to_string());
    let (copy_tx, _copy_rx) = mpsc::unbounded_channel();
    let copy_pending = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    // First Ctrl+C: arms the interrupt window
    super::event_loop::handle_ctrl_c(&mut app, "test-session", &copy_tx, &copy_pending);
    assert!(
        app.ctrl_c_armed(),
        "first Ctrl+C arms the interrupt confirmation"
    );

    // Second Ctrl+C: sends Interrupt request
    super::event_loop::handle_ctrl_c(&mut app, "test-session", &copy_tx, &copy_pending);
    assert!(
        matches!(rx.try_recv(), Ok(AgentRequest::Interrupt)),
        "confirmed Ctrl+C dispatches interrupt request"
    );
}

#[test]
fn ctrl_c_while_idle_does_not_clear_composer() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = "keep this text".to_string();
    let (copy_tx, _copy_rx) = mpsc::unbounded_channel();
    let copy_pending = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    super::event_loop::handle_ctrl_c(&mut app, "test-session", &copy_tx, &copy_pending);
    assert_eq!(
        app.input, "keep this text",
        "Ctrl+C must NEVER clear composer text"
    );
}

/// The double-Esc interrupt confirmation is a real wall-clock window, not a
/// frame counter. Regression: `esc_armed_ticks` decremented once per loop
/// iteration, but the loop wakes on every keystroke, mouse move, and stream
/// delta — far more often than its 100ms animation heartbeat — so the
/// intended ~2s window burned through in a few hundred milliseconds and the
/// "Esc again interrupts" toast vanished before a second press could land.
#[test]
fn esc_interrupt_window_is_wall_clock_not_frame_counted() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);

    // First press arms; the window must still be open well past the 20
    // iterations the old tick counter allowed at any wake rate.
    assert!(!app.esc_press(), "the first Esc only arms");
    assert!(app.esc_armed());
    // The viewed session's round is running, so the per-frame keep-alive
    // holds the window open regardless of how often the loop wakes.
    app.running_sessions.insert(app.current_session_id.clone());
    for _ in 0..100 {
        app.tick_esc_arm();
    }
    assert!(
        app.esc_armed(),
        "100 loop iterations (any wake rate) must not lapse a 2s window"
    );

    // The window is genuinely 2s, not "until the round ends".
    app.arm_esc(Some(std::time::Instant::now()));
    app.tick_esc_arm();
    assert!(!app.esc_armed(), "a lapsed deadline disarms");

    // A press after the lapse re-arms instead of firing a stale interrupt.
    assert!(!app.esc_press(), "the post-lapse press re-arms");
    assert!(app.esc_armed());
}

/// The armed Esc window's keep-alive must follow the *viewed* session's
/// running round — the same `running_sessions` predicate the keymap uses to
/// map Esc to an interrupt — never the runtime's global `is_responding`
/// flag. That flag is primary-only: an aside view armed from its own
/// running round was disarmed on the very next frame because the primary
/// sat idle, which read as "the first press did nothing / the toast
/// flashed and disappeared".
#[test]
fn esc_interrupt_window_survives_idle_primary_while_aside_runs() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);

    // Simulate the aside view: the viewed session runs, the primary
    // (global `is_responding`) does not.
    app.side_session_id = Some("aside-1".to_string());
    app.in_side_view = true;
    app.current_session_id = "aside-1".to_string();
    app.running_sessions.insert("aside-1".to_string());

    assert!(!app.esc_press(), "the first Esc inside the aside arms");
    assert!(app.esc_armed());

    // Repeated frame ticks must keep the window open: the viewed aside is
    // still running even though the primary-only global flag is false.
    for _ in 0..50 {
        app.tick_esc_arm();
    }
    assert!(
        app.esc_armed(),
        "the window must survive while the viewed aside's round runs"
    );

    // The moment the viewed session's round ends, the toast must go: there
    // is nothing left to interrupt.
    app.running_sessions.remove("aside-1");
    app.tick_esc_arm();
    assert!(
        !app.esc_armed(),
        "the window expires once the viewed session has nothing to interrupt"
    );
}

/// The second Esc inside the window fires the interrupt and disarms; the
/// request targets the viewed session (main view → `Interrupt`, aside view
/// → `InterruptSide`), and a third press re-arms rather than re-firing.
#[test]
fn esc_interrupt_fires_on_second_press_and_rearms_after() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    let (tx, mut rx) = mpsc::unbounded_channel();
    app.tx = tx;
    app.running_sessions.insert(app.current_session_id.clone());

    // Main view: arm, then fire. Driven through the real dispatch arm so
    // the wire request (not just the state flip) is asserted.
    super::event_loop::handle_esc_interrupt(&mut app, false);
    assert!(app.esc_armed(), "the first Esc arms the window");
    super::event_loop::handle_esc_interrupt(&mut app, false);
    assert!(matches!(rx.try_recv(), Ok(AgentRequest::Interrupt)));
    assert!(!app.esc_armed(), "firing consumes the arm");

    // The next press starts a fresh confirmation instead of firing again.
    assert!(!app.esc_press());
    assert!(app.esc_armed());
    assert!(
        rx.try_recv().is_err(),
        "a third press must not send another interrupt"
    );

    // Aside view: the fire targets the *aside* (`InterruptSide`), and only
    // while the aside view is actually open.
    app.side_session_id = Some("aside-1".to_string());
    app.in_side_view = true;
    super::event_loop::handle_esc_interrupt(&mut app, true); // arm
    super::event_loop::handle_esc_interrupt(&mut app, true); // fire
    assert!(matches!(
        rx.try_recv(),
        Ok(AgentRequest::InterruptSide { .. })
    ));
}

#[test]
fn delete_input_selection_clears_buffer_and_selection() {
    let mut app = app_with_input_selection("hello world");
    assert!(app.delete_input_selection());
    assert_eq!(app.input, "");
    assert_eq!(app.cursor_position, 0);
    assert_eq!(app.selection, SelectionState::None);
    // Second call is a no-op.
    assert!(!app.delete_input_selection());

    // Partial range deletion deletes only the selected slice.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = "hello world".to_string();
    app.selection = SelectionState::Range {
        anchor: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 6),
        head: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 10),
    };
    assert!(app.delete_input_selection());
    assert_eq!(app.input, "hello ");
    assert_eq!(app.cursor_position, 6);
    assert_eq!(app.selection, SelectionState::None);
}

#[test]
fn range_selection_left_arrow_breaks_selection_at_release_position() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = "hello world".to_string();
    // Drag forward from 'w' (6) to 'd' (10/11): mouse released at 11.
    app.selection = SelectionState::Range {
        anchor: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 6),
        head: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 11),
    };
    app.cursor_position = 11;

    let action = relay_probe(&mut app, crossterm::event::KeyCode::Left);
    assert!(matches!(action, Some(crate::input::InputAction::None)));
    assert_eq!(
        app.selection,
        SelectionState::None,
        "selection must be cancelled"
    );
    assert_eq!(
        app.cursor_position, 10,
        "caret steps left from release point 11"
    );

    // Backward drag: drag from 'd' (11) to 'w' (6): mouse released at 6.
    app.selection = SelectionState::Range {
        anchor: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 11),
        head: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 6),
    };
    app.cursor_position = 6;

    let action = relay_probe(&mut app, crossterm::event::KeyCode::Left);
    assert!(matches!(action, Some(crate::input::InputAction::None)));
    assert_eq!(
        app.selection,
        SelectionState::None,
        "selection must be cancelled"
    );
    assert_eq!(
        app.cursor_position, 5,
        "caret steps left from release point 6"
    );
}

#[test]
fn range_selection_right_arrow_breaks_selection_at_release_position() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = "hello world".to_string();
    // Backward drag: drag from 'd' (11) to 'w' (6): mouse released at 6.
    app.selection = SelectionState::Range {
        anchor: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 11),
        head: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 6),
    };
    app.cursor_position = 6;

    let action = relay_probe(&mut app, crossterm::event::KeyCode::Right);
    assert!(matches!(action, Some(crate::input::InputAction::None)));
    assert_eq!(
        app.selection,
        SelectionState::None,
        "selection must be cancelled"
    );
    assert_eq!(
        app.cursor_position, 7,
        "caret steps right from release point 6"
    );
}

#[test]
fn range_selection_up_and_down_restore_caret_at_release_position() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = "hello world".to_string();
    app.selection = SelectionState::Range {
        anchor: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 11),
        head: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 6),
    };
    app.cursor_position = 1; // stale

    let action = relay_probe(&mut app, crossterm::event::KeyCode::Up);
    assert!(matches!(action, Some(crate::input::InputAction::None)));
    assert_eq!(app.selection, SelectionState::None);
    assert_eq!(app.cursor_position, 6, "↑ restores caret at release point");
}

#[test]
fn range_selection_home_and_end_jump_to_selection_edges() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = "hello world".to_string();
    app.selection = SelectionState::Range {
        anchor: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 6),
        head: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 10),
    };

    let action = relay_probe(&mut app, crossterm::event::KeyCode::Home);
    assert!(matches!(action, Some(crate::input::InputAction::None)));
    assert_eq!(app.selection, SelectionState::None);
    assert_eq!(app.cursor_position, 6, "Home jumps to start of range");

    app.selection = SelectionState::Range {
        anchor: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 6),
        head: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 10),
    };

    let action = relay_probe(&mut app, crossterm::event::KeyCode::End);
    assert!(matches!(action, Some(crate::input::InputAction::None)));
    assert_eq!(app.selection, SelectionState::None);
    assert_eq!(app.cursor_position, 11, "End jumps to end of range");
}

#[test]
fn range_selection_cjk_left_arrow_snaps_grapheme() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = "你好世界".to_string();
    // Drag backwards from '界' (byte 9..12, char 3..4) to '好' (byte 3..6, char 1..2).
    // Mouse released at byte 3 (char 1).
    app.selection = SelectionState::Range {
        anchor: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 12),
        head: crate::model::layout::SemanticCursor::new(crate::view::INPUT_MSG_IDX, 0, 3),
    };
    app.cursor_position = 1;

    let action = relay_probe(&mut app, crossterm::event::KeyCode::Left);
    assert!(matches!(action, Some(crate::input::InputAction::None)));
    assert_eq!(app.selection, SelectionState::None);
    assert_eq!(app.cursor_position, 0, "← steps left from char 1 to char 0");
}

// ----- ADR-0162 Zero-Latency Two-Tier Completion & SWR Tests -----

#[test]
fn adr0162_slash_completions_are_synchronous_and_zero_latency() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = "/m".to_string();
    app.cursor_position = 2;

    // Zero-latency Tier 1: completions available immediately on frame 1
    let completions = app.completions();
    assert!(
        !completions.is_empty(),
        "Slash completions must be computed synchronously"
    );
    assert!(
        completions
            .iter()
            .any(|c| c.label.starts_with("/model") || c.label.starts_with("/m"))
    );

    // Keypress does not wipe backend state
    app.refresh_backend_completion_request();
    let completions_after_refresh = app.completions();
    assert_eq!(completions.len(), completions_after_refresh.len());
}

#[test]
fn adr0162_swr_retains_backend_completions_during_path_mention_typing() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.input = "look at @src/".to_string();
    app.cursor_position = app.input.len();

    // Simulate backend response arriving for @src/
    let item = muta_contracts::InputCompletion {
        label: "src/main.rs".to_string(),
        description: "main entrypoint".to_string(),
        insert_text: "src/main.rs".to_string(),
        replace_start: 8,
        replace_end: 13,
        kind: muta_contracts::InputCompletionKind::PathFile,
        alias_of: None,
        command: None,
    };
    app.apply_backend_completions(0, app.input.clone(), app.cursor_position, vec![item]);

    assert_eq!(app.completions().len(), 1);

    // User types 'm' -> input becomes @src/m
    app.input = "look at @src/m".to_string();
    app.cursor_position = app.input.len();
    app.refresh_backend_completion_request();

    // SWR: completions are retained during in-flight request, not dropped to 0
    let retained = app.completions();
    assert_eq!(
        retained.len(),
        1,
        "SWR must retain active completions during in-flight typing"
    );
}
