//! ADR-0133 retained-view-state tests: browse views, surface router, per-view drafts, sub-layer pop, switcher filter.

use super::*;

#[test]
fn resolve_focused_mut_indexes_root_when_unfocused() {
    let mut messages = conversation_with_runners();
    let focus: Vec<crate::app::ZoomFrame> = Vec::new();
    let resolved = event_loop::resolve_focused_mut(&mut messages, &focus, 2);
    assert_eq!(resolved.map(|m| m.raw.clone()).as_deref(), Some("ok"));
}

#[test]
fn resolve_focused_mut_indexes_children_when_focused() {
    let mut messages = conversation_with_runners();
    let focus = vec![crate::app::ZoomFrame {
        call_id: "task_b".to_string(),
        saved_scroll: crate::app::ScrollSnapshot::default(),
    }];
    // Index 0 inside task_b's children => "child B1".
    let resolved = event_loop::resolve_focused_mut(&mut messages, &focus, 0);
    assert_eq!(resolved.map(|m| m.raw.clone()).as_deref(), Some("child B1"));
    // Indexing task_a's children via task_b focus returns none / out of range.
    assert!(event_loop::resolve_focused_mut(&mut messages, &focus, 5).is_none());
}

#[test]
fn composer_paste_still_chips_large_text_on_main_prompt() {
    // The main-prompt path is unchanged: a large paste collapses into a
    // `[Pasted text #N +M lines]` chip and stages the full text, so the
    // modal-aware branching did not regress the composer behaviour.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.set_active_modal_for_test(Modal::None);
    app.input = String::new();
    app.cursor_position = 0;
    let big = format!("line\n{}", "x".repeat(2048));

    clipboard_ops::apply_clipboard_paste(
        &mut app,
        crate::clipboard::ClipboardRead::Text(big.clone()),
    );

    assert!(
        app.input.contains("Pasted text #1"),
        "large paste on the main prompt should produce a chip"
    );
    assert_eq!(app.pending_text_pastes.len(), 1);
    assert_eq!(app.pending_text_pastes[0], big);
}

#[test]
fn composer_image_paste_rejected_when_model_lacks_vision() {
    // When the current model doesn't support vision, pasting an image on
    // the main prompt should show a failure toast and leave no attachment.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.set_active_modal_for_test(Modal::None);
    app.current_model = "glm-5.2".to_string(); // vision: false
    app.input = String::new();
    app.cursor_position = 0;

    clipboard_ops::apply_clipboard_paste(
        &mut app,
        crate::clipboard::ClipboardRead::Image {
            data: vec![0x89, 0x50, 0x4e, 0x47],
            mime: "image/png".to_string(),
        },
    );

    assert!(
        app.pending_images.is_empty(),
        "non-vision model must not stage image attachments"
    );
    assert!(
        app.copy_toast_failed,
        "non-vision model should toast a failure on image paste"
    );
    assert!(
        app.copy_toast_message.contains("does not support images"),
        "toast should say the model doesn't support images, got: {}",
        app.copy_toast_message,
    );
    assert!(app.copy_toast_until.is_some());
}

#[test]
fn composer_image_paste_accepted_when_model_has_vision() {
    // When the current model supports vision, pasting an image on the main
    // prompt should stage the attachment and insert an `[Image #N]` chip.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.set_active_modal_for_test(Modal::None);
    app.current_model = "gpt-4o".to_string(); // vision: true
    app.input = String::new();
    app.cursor_position = 0;

    clipboard_ops::apply_clipboard_paste(
        &mut app,
        crate::clipboard::ClipboardRead::Image {
            data: vec![0x89, 0x50, 0x4e, 0x47],
            mime: "image/png".to_string(),
        },
    );

    assert_eq!(
        app.pending_images.len(),
        1,
        "vision-capable model should stage the image attachment"
    );
    assert!(
        app.input.contains("Image #1"),
        "image chip should be inserted into the input, got: {}",
        app.input,
    );
    assert!(
        !app.copy_toast_failed,
        "vision-capable model should show a success toast"
    );
    assert!(app.copy_toast_until.is_some());
}

#[test]
fn composer_text_paste_of_image_file_path_stages_attachment() {
    // Ctrl+Shift+V (terminal bracketed paste) can only deliver text, so a
    // copied image file arrives as its path. The composer upgrades an
    // all-file-references payload containing an image to the same
    // attachment pipeline Ctrl+V uses — in both the bare-path and
    // `file://` URI forms a terminal's text flavor produces.
    let (mut app, tmp) = app_in_tempdir(&[], &[]);
    app.set_active_modal_for_test(Modal::None);
    app.current_model = "gpt-4o".to_string(); // vision: true
    app.input = String::new();
    app.cursor_position = 0;
    let png = tmp.path().join("shot.png");
    std::fs::write(&png, [0x89, 0x50, 0x4e, 0x47]).expect("write png");

    clipboard_ops::apply_clipboard_paste(
        &mut app,
        crate::clipboard::ClipboardRead::Text(png.to_str().unwrap().to_string()),
    );

    assert_eq!(
        app.pending_images.len(),
        1,
        "bare image path paste should stage an attachment"
    );
    assert!(
        app.input.contains("Image #1"),
        "image chip should be inserted, got: {}",
        app.input
    );

    // `file://` URI form, with the trailing newline uri-lists carry.
    app.input.clear();
    app.cursor_position = 0;
    clipboard_ops::apply_clipboard_paste(
        &mut app,
        crate::clipboard::ClipboardRead::Text(format!("file://{}\n", png.display())),
    );
    assert_eq!(
        app.pending_images.len(),
        2,
        "file:// URI paste should stage another attachment"
    );
    assert!(app.input.contains("Image #2"), "got: {}", app.input);
}

#[test]
fn composer_text_paste_of_non_image_or_prose_stays_verbatim() {
    // A path to an existing non-image file is a legitimate text reference,
    // and prose around a path is just text: neither may be hijacked into
    // the attachment pipeline.
    let (mut app, tmp) = app_in_tempdir(&[], &[]);
    app.set_active_modal_for_test(Modal::None);
    app.input = String::new();
    app.cursor_position = 0;
    let source = tmp.path().join("main.rs");
    std::fs::write(&source, b"fn main() {}").expect("write file");

    clipboard_ops::apply_clipboard_paste(
        &mut app,
        crate::clipboard::ClipboardRead::Text(source.to_str().unwrap().to_string()),
    );

    assert!(app.pending_images.is_empty(), "no image, no attachment");
    assert_eq!(app.input, source.to_str().unwrap());

    let prose = format!("look at {} please", source.display());
    clipboard_ops::apply_clipboard_paste(
        &mut app,
        crate::clipboard::ClipboardRead::Text(prose.clone()),
    );
    assert!(app.pending_images.is_empty(), "prose stays prose");
    assert!(app.input.contains(&prose), "got: {}", app.input);
}

/// The view reset that follows a focus change (runner zoom enter/exit) must
/// drop a pending settle: the staged frame it was computed for belongs to a
/// transcript slice that is no longer displayed.
#[test]
fn view_reset_clears_pending_scroll_settle() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.scroll_settle_pending = true;
    app.reset_view_state();
    assert!(
        !app.scroll_settle_pending,
        "reset_view_state must clear a pending settle"
    );
}

// ---------------------------------------------------------------------------
// ADR-0133: retained, buffer-like view state.
// ---------------------------------------------------------------------------

#[test]
fn browse_view_reopen_restores_scroll_and_selection() {
    // The core ADR-0133 contract: hiding a browse view (Esc) and reopening
    // it returns to the exact scroll/index the user left. Before the
    // refactor every open reset them to 0.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    assert!(app.open_panel(crate::surfaces::PanelId::Help));
    assert_eq!(app.active_modal(), Modal::Help);
    assert_eq!(app.modal_index, 0);

    // The user scrolls and selects, then hides (Esc → dismiss_surface).
    app.help_scroll = 42;
    app.modal_index = 3;
    assert!(app.dismiss_surface());
    assert_eq!(app.active_modal(), Modal::None);

    // Reopen: first-open returned false and the retained state is back.
    assert!(!app.open_panel(crate::surfaces::PanelId::Help));
    assert_eq!(app.modal_index, 3, "selection retained across hide");
    assert_eq!(app.help_scroll, 42, "scroll retained across hide");
}

#[test]
fn browse_view_state_is_per_view() {
    // Two views keep independent retained state — the buffer analogy: each
    // buffer remembers its own cursor.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_panel(crate::surfaces::PanelId::Permissions);
    app.modal_index = 2;
    app.permissions_scroll = 7;
    assert!(app.dismiss_surface());

    app.open_panel(crate::surfaces::PanelId::UsageStats);
    app.modal_index = 1;
    app.usage_stats_scroll = 9;
    assert!(app.dismiss_surface());

    app.open_panel(crate::surfaces::PanelId::Permissions);
    assert_eq!((app.modal_index, app.permissions_scroll), (2, 7));
    app.open_panel(crate::surfaces::PanelId::UsageStats);
    assert_eq!((app.modal_index, app.usage_stats_scroll), (1, 9));
}

#[test]
fn view_follow_mode_is_restored_per_view() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_panel(crate::surfaces::PanelId::Tools);
    app.session_modal_follow = false;

    app.open_panel(crate::surfaces::PanelId::Mcp);
    app.session_modal_follow = true;

    app.open_panel(crate::surfaces::PanelId::Tools);
    assert!(
        !app.session_modal_follow,
        "shared live fields must restore the selected view's retained mode"
    );
}

#[test]
fn view_state_is_forgotten_on_session_change() {
    // `close_all` fires on viewed-session change: retained state belongs to
    // the conversation, not the terminal (ADR-0133 close verb).
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_panel(crate::surfaces::PanelId::Help);
    app.help_scroll = 5;
    app.modal_index = 1;
    app.on_viewed_session_changed();
    assert!(
        app.open_panel(crate::surfaces::PanelId::Help),
        "state forgotten"
    );
    assert_eq!(app.help_scroll, 0);
    assert_eq!(app.modal_index, 0);
}

#[test]
fn view_switcher_restore_roundtrip() {
    // The Ctrl+L switcher's verbs: open over a browse view, Esc cancels
    // back to it (state intact); Enter on another view hides the origin
    // and focuses the target with its own retained state.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    app.open_panel(crate::surfaces::PanelId::Help);
    app.modal_index = 4;

    // Open the transient switcher over Help; the router preserves the exact
    // parent and the push snapshots its shared cursor/scroll projection.
    app.push_transient_surface(Modal::ViewSwitcher);
    app.modal_index = 0;

    // Esc (the shared dismiss verb) cancels back to Help — and restores
    // Help's own cursor from the registry (the switcher's row cursor must
    // not leak into the restored surface).
    assert!(app.dismiss_surface());
    assert_eq!(app.active_modal(), Modal::Help);
    assert_eq!(
        app.modal_index, 4,
        "Help's selection restored, not the switcher's row cursor"
    );

    // Help's retained state survived the switcher round-trip.
    app.open_panel(crate::surfaces::PanelId::Activity);
    assert!(!app.open_panel(crate::surfaces::PanelId::Help));
    assert_eq!(app.modal_index, 4, "retained selection intact");
}

#[test]
fn per_view_drafts_do_not_clobber_each_other() {
    // The phase-3 reason per-view drafts exist: parking for Models used to
    // overwrite a draft parked for History through the one global slot.
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    // Park a draft on Models.
    app.input = "models draft".to_string();
    app.open_panel(crate::surfaces::PanelId::Models);
    assert!(app.input.is_empty(), "composer borrowed");
    // Esc hands the draft back.
    assert!(app.dismiss_surface());
    assert_eq!(app.input, "models draft");

    // Now the same for HistorySearch — its slot is independent.
    app.input = "history draft".to_string();
    app.open_panel(crate::surfaces::PanelId::HistorySearch);
    assert!(app.input.is_empty());
    assert!(app.dismiss_surface());
    assert_eq!(app.input, "history draft");
}

#[test]
fn switcher_enter_hides_origin_and_restores_target_state() {
    let (mut app, _tmp) = app_in_tempdir(&[], &[]);
    // Target retains state.
    app.open_panel(crate::surfaces::PanelId::Help);
    app.modal_index = 2;
    assert!(app.dismiss_surface());

    // Origin: Tools.
    app.open_panel(crate::surfaces::PanelId::Tools);
    app.modal_index = 1;

    // Open the switcher over Tools (the Toggle arm's push + borrow).
    app.push_transient_surface(crate::Modal::ViewSwitcher);
    app.modal_index = 0;

    // The switcher's rows put open views first; with only Tools open the
    // first row is Tools itself. Pick Help (find its row).
    let rows = app.panels.switcher_rows();
    let help_row = rows
        .iter()
        .position(|r| *r == crate::surfaces::SwitcherTarget::Panel(crate::surfaces::PanelId::Help))
        .unwrap();
    app.modal_index = help_row;

    // Enter (the Activate arm's core, minus the async runtime plumbing).
    let target = rows[help_row];
    app.modal_index = 0;
    app.pop_transient_surface();
    let crate::surfaces::SwitcherTarget::Panel(target) = target else {
        panic!("expected a panel row");
    };
    let first = app.open_panel(target);
    assert!(!first, "Help was opened before — not a first open");
    assert_eq!(app.active_modal(), crate::Modal::Help);
    assert_eq!(app.modal_index, 2, "Help's retained selection restored");
    assert!(
        app.panels.is_open(crate::surfaces::PanelId::Tools),
        "hidden origin remains an initialized MRU buffer"
    );
}

#[test]
fn switcher_filter_narrows_rows_and_matches_labels_and_hints() {
    // Phase 5: the switcher's own fuzzy query against label + hint.
    let mut reg = crate::surfaces::PanelRegistry::new();
    reg.open(crate::surfaces::PanelId::Help);
    reg.open(crate::surfaces::PanelId::Btw);

    // "mcp" matches the MCP label.
    let rows = reg.switcher_rows_filtered("mcp");
    assert_eq!(
        rows,
        vec![crate::surfaces::SwitcherTarget::Panel(
            crate::surfaces::PanelId::Mcp
        ),]
    );

    // "dash" matches the Dashboard label (a switchable full-screen view).
    let rows = reg.switcher_rows_filtered("dash");
    assert_eq!(
        rows,
        vec![crate::surfaces::SwitcherTarget::View(
            crate::surfaces::View::Dashboard
        )]
    );

    // A query matching nothing yields an empty list (rendered as the
    // placeholder), never a fallback-to-all.
    assert!(reg.switcher_rows_filtered("zzz").is_empty());

    // Empty query = views first, then the MRU panels.
    let rows = reg.switcher_rows_filtered("");
    assert_eq!(
        &rows[..4],
        &[
            crate::surfaces::SwitcherTarget::View(crate::surfaces::View::Dashboard),
            crate::surfaces::SwitcherTarget::View(crate::surfaces::View::Settings),
            crate::surfaces::SwitcherTarget::Panel(crate::surfaces::PanelId::Btw),
            crate::surfaces::SwitcherTarget::Panel(crate::surfaces::PanelId::Help),
        ]
    );
}

#[test]
fn surface_router_with_view_boots_directly_into_target_view() {
    let router = crate::surfaces::SurfaceRouter::with_view(crate::surfaces::View::Settings);
    assert_eq!(router.active_view(), crate::surfaces::View::Settings);
    assert_eq!(router.modal(), Modal::Config);
    assert_eq!(router.active_panel(), None);
}

#[test]
fn startup_overlay_env_resolution_accepts_settings_and_nav() {
    // Helper to run with temporary environment overrides
    let test_env = |view: Option<&str>, nav: Option<&str>| -> Option<crate::StartupOverlay> {
        // We test the parsing logic directly by setting/unsetting env vars
        unsafe {
            if let Some(v) = view {
                std::env::set_var("MUTX_STARTUP_VIEW", v);
            } else {
                std::env::remove_var("MUTX_STARTUP_VIEW");
                std::env::remove_var("MUTX_VIEW");
            }
            if let Some(n) = nav {
                std::env::set_var("MUTX_SETTINGS_NAV", n);
            } else {
                std::env::remove_var("MUTX_SETTINGS_NAV");
                std::env::remove_var("MUTX_SETTINGS_CATEGORY");
            }
        }
        let res = crate::StartupOverlay::resolve_from_env();
        unsafe {
            std::env::remove_var("MUTX_STARTUP_VIEW");
            std::env::remove_var("MUTX_VIEW");
            std::env::remove_var("MUTX_SETTINGS_NAV");
            std::env::remove_var("MUTX_SETTINGS_CATEGORY");
        }
        res
    };

    assert_eq!(
        test_env(Some("settings"), None),
        Some(crate::StartupOverlay::Settings { category: None })
    );
    assert_eq!(
        test_env(Some("settings:web"), None),
        Some(crate::StartupOverlay::Settings { category: Some(3) })
    );
    assert_eq!(
        test_env(Some("settings:transcript"), None),
        Some(crate::StartupOverlay::Settings { category: Some(1) })
    );
    assert_eq!(
        test_env(Some("settings:2"), None),
        Some(crate::StartupOverlay::Settings { category: Some(2) })
    );
    assert_eq!(
        test_env(Some("settings"), Some("system")),
        Some(crate::StartupOverlay::Settings { category: Some(4) })
    );
    assert_eq!(
        test_env(None, Some("behavior")),
        Some(crate::StartupOverlay::Settings { category: Some(2) })
    );
    assert_eq!(
        test_env(Some("dashboard"), None),
        Some(crate::StartupOverlay::Dashboard)
    );
    assert_eq!(
        test_env(Some("sessions"), None),
        Some(crate::StartupOverlay::SessionsPicker)
    );
    assert_eq!(test_env(None, None), None);
}
