//! The per-frame draw routine for the TUI event loop, extracted from
//! `run_app_loop`'s `if needs_draw` stage (it was a ~1000-line closure).

use crate::completion::{CompletionKind, completion_anchor_x, resolved_slash_command_len};
use crate::model::document::{MessageKind, TranscriptMessage};
use crate::model::layout::LayoutMap;
use crate::view;
use crate::{App, Modal, ProviderDeleteChoice, Recess};

use super::{display_status, effective_reasoning_effort};

/// Loop stage: the per-frame draw. Paints the chrome (startup picker,
/// transcript, hint bar, composer, completion popup, effort-ignition
/// overlay), recesses the live surface for the open modal, then draws the
/// active modal panel, persisting per-frame layout state back onto `app`.
/// Invoked through `Terminal::stage` (bottom-follow measurement pass) or
/// `Terminal::draw`; extracted verbatim from the `render_frame` closure.
pub(super) fn render_frame(
    app: &mut App,
    f: &mut neenee_tui_engine::Frame<'_>,
    viewed_session_id: &str,
) {
    let mut layout_map = LayoutMap::new();

    if app.startup_overlay == crate::StartupOverlay::SessionsPicker
        && app.active_modal == Modal::Sessions
    {
        // `neenee resume` (no id): initial launch opens ONLY the sessions picker
        // on a clean background. Do not open/render the chat interface, empty state,
        // composer input box, status bar, or header components until a session is selected.
        f.render_widget(
            neenee_tui_engine::widgets::Block::default()
                .style(neenee_tui_engine::Style::default().bg(app.theme.app_bg)),
            f.area(),
        );

        let spinner_phase = (app.spinner_epoch.elapsed().as_millis() / 100) as usize;
        let drawn_modal_rect = view::draw_sessions_modal(
            f,
            &app.sessions_overview,
            app.modal_index
                .min(app.sessions_overview.len().saturating_sub(1)),
            app.modal_keymap_open,
            &mut app.session_scroll,
            app.session_modal_follow,
            &app.theme,
            app.startup_overlay == crate::StartupOverlay::SessionsPicker,
            spinner_phase,
            app.session_info_detail,
            app.session_detail.as_ref(),
            &mut app.session_info_scroll,
        );

        app.layout_map = layout_map;
        app.modal_body_height =
            drawn_modal_rect
                .height
                .saturating_sub(crate::primitives::modal_chrome_rows(
                    crate::primitives::ModalSpec {
                        width_percent: 0,
                        header: true,
                        footer: true,
                    },
                ));
        app.modal_rect = if app.active_modal.dismissable_by_outside_click() {
            Some(drawn_modal_rect)
        } else {
            None
        };
        return;
    }

    app.modal_hit_map.clear();
    // Borrow the height cache out of `app` for the duration of the draw:
    // `view_messages` borrows `app` immutably below, so the cache cannot
    // also be reached through `app` at the same time. It is restored once
    // `view_messages` is no longer borrowed (see below).
    let mut height_cache = std::mem::take(&mut app.layout_height_cache);
    // View-scoped chrome: render the activity text of whichever session the
    // user is viewing — the focused aside's own entry inside `/btw`, the
    // primary's otherwise. This is the aside-view activity-bar fix: the
    // displayed bar tracks the *viewed* session, never a global blend.
    let viewed_chrome = app.viewed_chrome();
    let activity_for_display = viewed_chrome.activity.as_str();
    let status = display_status(
        app.loop_status,
        activity_for_display,
        app.pending_permission.is_some(),
    );

    // Compute the displayed input text first so the transcript layout can
    // reserve the right height for a wrapping, growing input box.
    let masked_input = if app.active_modal == Modal::ModelEditor && app.editor_field == 0 {
        // Mask the API key everywhere it could be rendered (the editor
        // field itself, and any layout pass that inspects the input).
        "•".repeat(app.input.chars().count())
    } else {
        app.input.clone()
    };

    // Modal recess policy (single source of truth: `Modal::recess`).
    // A terminal cannot alpha-blend, so a modal either floats, darkens
    // the live surface in place, or fully occludes it:
    // - Takeover (Sessions): the footer collapses to zero height and
    //   the surface is occluded — opening a different session is a full
    //   context switch, so a clean slate is the intent.
    // - Dim (every other centered modal): the footer keeps its height
    //   so layout is stable, and the whole surface is darkened in place
    //   by the recess pass just before the modal is drawn. Context
    //   (transcript, input, hint bar, activity bar, state bar) stays visible for
    //   focus while the centered panel reads as the focal layer.
    // - None (Question / Permission): floats on the fully-live surface.
    // Provider / ModelEditor / HistorySearch borrow the input line as
    // their own field, so the composer is suppressed for them (its rect
    // stays as recessed surface) — no duplicate field, and no
    // masked-cursor panic in the editor.
    let recess = app.active_modal.recess();
    let chrome_hidden = recess == Recess::Takeover;

    // When zoomed into an Envoy, render its child messages and
    // show a contextual first-row header; otherwise render the
    // root conversation.
    let view_messages = app.focused_messages();
    // `/btw` aside page-header context (ADR-0017/0103): shown only while the
    // aside view is active. Envoy zoom and the aside view are mutually
    // exclusive, so the two modes never coexist.
    let side_banner = app.in_side_view.then_some(view::BtwHead {
        parent: app.parent_status,
    });
    // Row-2 affordance legend inputs (ADR-0103 §3): the aside chip is
    // offered on the main view only (inside an aside, `F5 asides` is a bare
    // pair without the count); interruptibility follows whether the viewed
    // page's session has a live round. The running count is derived from
    // `running_sessions` (maintained per session id by the HarnessState
    // outbox signal), not the list snapshot — so a background aside's
    // round finishing flips the chip on the very next frame without a list
    // refetch.
    let viewed_running = app.running_sessions.contains(viewed_session_id);
    let aside_running = app
        .btw_list
        .iter()
        .filter(|row| app.running_sessions.contains(row.id.as_str()))
        .count();
    let page_hints = view::PageHints {
        kind: if side_banner.is_some() {
            view::PageKind::Btw
        } else if app.in_envoy_view() {
            view::PageKind::Envoy
        } else {
            view::PageKind::Main
        },
        asides: (!app.in_side_view && !app.btw_list.is_empty()).then_some(view::AsidesChip {
            total: app.btw_list.len(),
            running: aside_running,
        }),
        interruptible: viewed_running,
        parent_note: "",
    };
    let envoy_bar = app.focus_stack.last().and_then(|current| {
        let tasks: Vec<&TranscriptMessage> = app
            .messages
            .iter()
            .filter(|message| message.is_envoy_task())
            .collect();
        let idx = tasks
            .iter()
            .position(|message| message.tool_step_call_id() == Some(current.call_id.as_str()))?;
        Some(view::EnvoyBarInfo {
            role: tasks.get(idx)?.envoy_role(),
            label: tasks.get(idx)?.envoy_description(),
            index: idx + 1,
            total: tasks.len(),
        })
    });

    // Empty-state guidance policy (ADR-0057/0104): the app shell picks the
    // variant, the view paints it. A setup blocker beats the tour — nothing
    // rotates until a keyed provider exists. The blocker reads from
    // `provider_picker` rows (a row exists ⇒ the provider is configured;
    // `key_status` refines key readiness), mirroring what `/connections`
    // manages, so the nudge clears the moment the user fixes the real thing.
    // An empty snapshot means "not synced yet" — the daemon's startup
    // snapshot arrives within the first loop iterations — so the tour
    // renders in that window rather than flashing a false no-provider
    // warning at an already-configured user. A genuinely provider-less
    // install is indistinguishable until its snapshot lands; the cost is a
    // few tour frames before the blocker appears, never a false warning.
    let has_keyed_provider = app
        .provider_picker
        .rows
        .iter()
        .any(|row| row.key_ready || app.key_status.get(&row.id).copied().unwrap_or(true))
        || app.provider_picker.rows.is_empty();
    let guidance = if has_keyed_provider {
        view::EmptyStateGuidance::Tour
    } else {
        view::EmptyStateGuidance::NeedsProvider
    };

    // Suppress the hover affordance whenever a full-overlay modal is
    // open so no stale highlight bleeds through. The permission sheet
    // keeps the transcript interactive, so it is exempted.
    let chrome_interactive = matches!(app.active_modal, Modal::None | Modal::Permission);

    // Project the viewed session's outbox into the small view the
    // persistent queue bar renders. Dispatch order (front pops
    // first) is preserved, so the bar previews the genuine next
    // item to ship. The items are owned snapshots so the bar/modal
    // do not borrow `app` (which is mutated again right after the
    // draw closure).
    //
    // Every outbox item is a next-round item now: a live mid-round
    // insert (`Ctrl+O`) is transcript-owned and never passes through
    // the outbox (ADR-0126), so there is no `steering` slice to
    // exclude from the modal either.
    let queue_items: Vec<view::QueueItemView> = app
        .pending_dispatch
        .iter()
        .filter(|item| item.session_id == viewed_session_id)
        .map(|item| view::QueueItemView {
            queued_at_ms: item.queued_at_ms,
            text: item.text.clone(),
            steering: false,
        })
        .collect();
    let queue_modal_items: Vec<view::QueueItemView> = queue_items.clone();

    let transcript_render = view::draw_transcript(
        f,
        &mut layout_map,
        view::TranscriptView {
            messages: view_messages,
            scroll: app.scroll,
            selection: &app.selection,
            cell_selection: app.drag.cell_info.as_ref(),
            activity: &status,
            // A pending permission request forces the activity bar
            // on (and tints it warning) so it stays the visible
            // anchor above the permission sheet even if the loop
            // has gone idle.
            awaiting_permission: app.pending_permission.is_some(),
            // ~100ms per phase keeps one breathing cycle near 1.2s
            // (SPINNER_PHASES steps); `breathing_color` wraps modulo.
            spinner_phase: (app.spinner_epoch.elapsed().as_millis() / 100) as usize,
            input: &masked_input,
            byte_cursor: app.byte_cursor(),
            chrome_hidden,
            queue_bar: view::QueueBarView {
                items: &queue_items,
                paused: app.pending_count(viewed_session_id) > 0
                    && app.idle_sessions.contains(viewed_session_id)
                    && !app.naturally_completed_sessions.contains(viewed_session_id),
                blocked: app.pending_count(viewed_session_id) > 0
                    && app.is_queue_blocked(viewed_session_id),
            },
            envoy_bar,
            side_banner,
            page_hints: Some(page_hints),
            session_head: Some(view::SessionHead {
                session_id: viewed_session_id,
                workspace: &app.current_workspace,
                autopilot: app.autopilot,
            }),
            todos: app.todos.as_ref(),
            // View-scoped: the elapsed-timer origin belongs to the viewed
            // session's round (an aside view times the aside's round, not
            // the primary's).
            round_started_at: viewed_chrome.round_started_at,
            hovered_step: chrome_interactive.then_some(app.hovered_step).flatten(),
            focused_target: chrome_interactive.then_some(app.focused_target).flatten(),
            logo: app.logo.as_deref(),
            guidance,
            carousel_index: crate::empty_state::carousel_page_for(
                app.carousel_epoch.elapsed().as_millis(),
            ),
            theme: &app.theme,
            layout: app.transcript_layout,
            height_cache: Some(&mut height_cache),
        },
    );
    let input_rect = transcript_render.input_rect;
    let hint_rect = transcript_render.hint_rect;
    let content_lines = transcript_render.content_lines;
    let view_height = transcript_render.view_height;
    let sticky = transcript_render.sticky;

    // The input-action hint bar (with model/context metadata on
    // the right) lives directly below the input box. It is drawn
    // before the composer because it borrows `view_messages` (an
    // immutable borrow of `app`) while `draw_composer` needs a
    // mutable borrow of `app.input_scroll`.
    // The permission sheet takes over the hint line as well as the
    // input box, so suppress the hint bar while it is open.
    if !chrome_hidden && hint_rect.height > 0 && app.active_modal != Modal::Permission {
        // Resolve the active model's effective reasoning effort for
        // the hint bar's `◆ {effort}` tag. Reads the same per-model
        // channel info the `/models` picker uses
        // (`ProviderModelInfo { effort, thinking }`), then applies
        // the ADR-0046 per-protocol gating: Anthropic effort shows
        // only while thinking is opted in; OpenAI effort (a
        // standalone knob with no separate thinking field) shows
        // whenever the model exposes one; Google never. `None`
        // otherwise — non-reasoning models keep the bar quiet.
        let active_provider_row = app
            .provider_picker
            .rows
            .iter()
            .find(|row| row.id == app.current_provider);
        // The `@<instance>` suffix after the model name — the
        // instance's display name, so identical models served by
        // different instances stay attributable.
        let hint_instance = active_provider_row.map(|row| row.name.as_str());
        let hint_reasoning = effective_reasoning_effort(app);
        let model_available = active_provider_row
            .is_none_or(|row| row.models.iter().any(|m| m == &app.current_model));
        let busy = app.running_sessions.contains(viewed_session_id);
        let can_retry = !busy
            && view_messages
                .iter()
                .rev()
                .find(|m| !matches!(m.kind, MessageKind::CommandResult { .. }))
                .is_some_and(|m| m.is_error_notice());
        app.hint_context_rect = view::draw_hint_bar(
            f,
            hint_rect,
            view::HintBarView {
                current_model: &app.current_model,
                model_available,
                provider_name: hint_instance,
                messages: view_messages,
                reasoning_effort: hint_reasoning,
                busy,
                can_retry,
                context_tokens: app.context_tokens.map(|snapshot| snapshot.tokens),
                ignition_elapsed_ms: app
                    .effort_ignition_epoch
                    .map(|epoch| epoch.elapsed().as_millis()),
            },
            &app.theme,
        );
    } else {
        app.hint_context_rect = None;
    }

    // The input box is only shown when no overlay modal is open. The
    // `focused` flag drops the panel to its dim "blurred" palette and
    // hides the caret whenever keyboard focus is on the conversation
    // stream (Browse zone), so the user can see at a glance which
    // surface the next keypress will land on. A pending permission
    // request replaces the composer with the inline permission sheet.
    if !chrome_hidden {
        if app.active_modal == Modal::Permission {
            if let Some(request) = app.pending_permission.as_ref() {
                // Extend the slot down by the composer/hint gap plus
                // the hint-line height so the sheet also covers
                // (replaces) the bar below the input.
                let permission_rect = neenee_tui_engine::Rect::new(
                    input_rect.x,
                    input_rect.y,
                    input_rect.width,
                    input_rect.height + crate::design::COMPOSER_HINT_GAP_ROWS + hint_rect.height,
                );
                let max_scroll = view::draw_permission_sheet(
                    f,
                    &mut app.modal_hit_map,
                    request,
                    app.modal_index,
                    app.permission_confirm_always,
                    app.permission_show_details,
                    app.permission_scroll,
                    permission_rect,
                    &app.theme,
                );
                app.permission_max_scroll = max_scroll;
                app.permission_scroll = app.permission_scroll.min(app.permission_max_scroll);
            }
        } else if matches!(
            app.active_modal,
            Modal::Connections | Modal::Models | Modal::ModelEditor | Modal::CustomProvider
        ) {
            // These modals borrow the input line as their own field
            // (filter / key+model / history-query), so the composer
            // underneath would only duplicate the same `app.input` the
            // modal already shows — and, since both are bound to the
            // one buffer, would read as a second live input field
            // accepting the same keystrokes. Its rect stays mounted
            // (so the footer layout is stable) but is left as recessed
            // surface — the dim pass darkens it like the rest of the
            // background. For the editor's key field the composer would
            // also panic: the masked key's byte cursor is computed
            // against the unmasked string.
        } else if !app.in_envoy_view() {
            // The composer stays mounted for the dim-recess modals
            // (Help / Session /
            // Activity) so the footer layout doesn't shift when the
            // overlay opens or closes; the recess pass darkens it in
            // place with the rest of the surface. When a transcript
            // step carries keyboard focus (Ctrl+↑/↓), the composer drops
            // to its dim "blurred" palette and hides the caret so the
            // user can see at a glance that the next keypress targets
            // the step, not the input box. Typing into the box clears
            // the focus and re-brightens it immediately.
            //
            // `show_caret` comes straight from the single source of
            // truth (`App::caret_visible`): in this branch the composer
            // is the only possible caret surface (the caret-owning
            // modals are handled by the `skip` branch above, and envoy
            // zoom is excluded by the `!in_envoy_view` gate), so
            // `caret_visible` reduces to "no step focus, no selection"
            // — exactly the old hand-rolled condition, without the risk
            // of drifting from the hide/show state machine.
            let step_focused = app.focused_target.is_some();
            let show_caret = app.caret_visible();
            // A fully-typed known `/command` is painted in bold +
            // accent color so it reads as a resolved command
            // rather than prose; an unmatched `/`-prefix keeps
            // the normal text color.
            let slash_len = resolved_slash_command_len(&app.input, &app.custom_commands);
            // Effort-ignition prompt tint: a color-only accent on
            // the `›` prompt while the wave runs (the glyph never
            // changes). `None` once the animation has finished.
            let prompt_accent = app
                .effort_ignition_epoch
                .map(|epoch| (true, Some(epoch.elapsed().as_millis())));
            match slash_len {
                Some(len) => view::draw_composer_highlighted(
                    f,
                    input_rect,
                    &app.input,
                    app.byte_cursor(),
                    !step_focused,
                    show_caret,
                    &app.theme,
                    &mut layout_map,
                    true,
                    &mut app.input_scroll,
                    &app.selection,
                    len,
                    app.pending_images.len(),
                    app.pending_text_pastes.len(),
                ),
                None => match prompt_accent {
                    Some(accent) => view::draw_composer_igniting(
                        f,
                        input_rect,
                        &app.input,
                        app.byte_cursor(),
                        !step_focused,
                        show_caret,
                        &app.theme,
                        &mut layout_map,
                        true,
                        &mut app.input_scroll,
                        &app.selection,
                        app.pending_images.len(),
                        app.pending_text_pastes.len(),
                        accent,
                    ),
                    None => view::draw_composer(
                        f,
                        input_rect,
                        &app.input,
                        app.byte_cursor(),
                        !step_focused,
                        show_caret,
                        &app.theme,
                        &mut layout_map,
                        true,
                        &mut app.input_scroll,
                        &app.selection,
                        app.pending_images.len(),
                        app.pending_text_pastes.len(),
                    ),
                },
            }
        }
    }

    // Now that `view_messages` is no longer borrowed, persist the
    // per-frame layout state back onto `app` for the next iteration
    // and for click routing.
    // Restore the height cache (populated/refreshed during this draw)
    // so the next frame can reuse it.
    app.layout_height_cache = height_cache;
    app.content_lines = content_lines;
    app.view_height = view_height;
    // Hit-test rects for the footer bars, resolved from the one registry the
    // renderer placed this frame (`TranscriptRender::footer`) — one source
    // of truth instead of per-bar plumbing.
    app.activity_rect = view::footer_rect(&transcript_render.footer, view::FooterRowId::Activity);
    app.todos_rect = view::footer_rect(&transcript_render.footer, view::FooterRowId::Todos);
    app.queue_rect = view::footer_rect(&transcript_render.footer, view::FooterRowId::Queue);
    // Feed the observed composer rect back so the *next* iteration's
    // immediate cursor flush (which runs before this draw closure
    // re-runs) places the caret against the geometry the user is
    // actually looking at.
    app.observe_input_rect(input_rect);
    match sticky {
        Some(info) => {
            app.sticky_step = Some(info.message_idx);
            app.sticky_rect = Some(info.rect);
            app.sticky_summary_line = Some(info.summary_line);
        }
        None => {
            app.sticky_step = None;
            app.sticky_rect = None;
            app.sticky_summary_line = None;
        }
    }

    // Completion menu: slash commands or `@path` file mentions.
    // Honors `completion_dismissed` so Esc / Enter-commit keep the
    // popup hidden until the next edit clears the latch. Also
    // suppressed for a fully-typed command whose exact match is the
    // text already in the box — that is a *resolved* state (the
    // composer paints it bold + accent), the popup has nothing left
    // to offer, and ↑/↓ keep walking history instead of cycling a
    // single pinned row.
    if app.active_modal == Modal::None
        && !app.completion_dismissed
        && app.completion_kind() != CompletionKind::None
    {
        let completions = app.completions();
        // Anchor pass (the frame-side twin of the event loop's pre-compute):
        // any state change that bypassed a keystroke — a paste, an async
        // project-scan landing, a modal teardown — still lands here, so this
        // is the last line of defense keeping "popup visible ⇒ one row
        // highlighted" true. A freshly opened menu starts at its first
        // candidate; a stale index clamps; nothing visible clears.
        app.anchor_completion_selection(&completions);
        let exact_match = completions.iter().any(|c| {
            c.replace_start == 0 && c.replace_end == app.input.len() && c.label == app.input
        });
        if !completions.is_empty() && !exact_match {
            // Hang the popup's leading edge off the trigger token
            // it completes — column 0 of the composer text area
            // for a `/command`, the `@`'s column for a path
            // mention — so the menu aligns with what was typed
            // even after the line wraps.
            let anchor_x = completion_anchor_x(
                &app.input,
                app.byte_cursor(),
                input_rect,
                app.completion_kind(),
            );
            view::draw_completion_menu(
                f,
                &mut layout_map,
                &completions,
                app.suggestion_index,
                input_rect,
                anchor_x,
                &app.theme,
            );
        }
    }

    // Effort-ignition overlay: tint the composer panel and hint
    // bar with the sweeping fire waves. Runs after the composer /
    // hint bar / completion popup have painted so the glow sits
    // on top of every footer surface, and before the modal recess
    // so an open modal still dims the celebration with the rest
    // of the background. Text is never touched — only cell
    // backgrounds are blended (codex's text-safe `Canvas::tint`).
    if !chrome_hidden && let Some(epoch) = app.effort_ignition_epoch {
        let elapsed_ms = epoch.elapsed().as_millis();
        let hint_row =
            (hint_rect.height > 0 && app.active_modal != Modal::Permission).then_some(hint_rect.y);
        crate::effort_ignition::paint_ignition_bands(f, input_rect, hint_row, elapsed_ms);
    }

    // Recess the live surface for the open modal: darken it in place
    // (Dim), occlude it fully (Takeover), or leave it untouched (None).
    // Done after the transcript + chrome are drawn and before the modal
    // panel so the panel overpaints its own crisp area on top of the
    // recessed background.
    view::recess_backdrop(f, recess, &app.theme);

    let spinner_phase = (app.spinner_epoch.elapsed().as_millis() / 100) as usize;

    // The dashboard reports its true list-body height through this
    // slot (its body is not the centered panel-minus-chrome the
    // shared post-match math assumes). Reset each frame; only the
    // `Modal::Host` arm sets it.
    let mut dashboard_list_body_height: Option<u16> = None;

    // Modals
    let drawn_modal_rect = match app.active_modal {
        Modal::Connections => {
            let providers = app.providers_filtered();
            Some(view::draw_connections_modal(
                f,
                &mut layout_map,
                &providers,
                &app.current_provider,
                app.modal_index,
                &app.input,
                app.cursor_position,
                &mut app.model_scroll,
                app.model_modal_follow,
                app.model_search,
                app.modal_keymap_open,
                &app.theme,
            ))
        }
        Modal::Models => {
            let models = app.models_flat_filtered();
            Some(view::draw_models_modal(
                f,
                &mut layout_map,
                &models,
                &app.current_provider,
                &app.current_model,
                app.modal_index,
                &app.input,
                app.cursor_position,
                &mut app.model_scroll,
                app.model_modal_follow,
                app.model_search,
                app.modal_keymap_open,
                &app.theme,
            ))
        }
        Modal::HistorySearch => {
            let ranked = app.history_rows();
            // The activity bar sits directly above the composer, so
            // reserve its rows: the dropdown must never paint over
            // the live status bar above it. The footer registry carries
            // the bar's exact footprint this frame (None when idle,
            // height 0).
            let activity_height =
                view::footer_rect(&transcript_render.footer, view::FooterRowId::Activity)
                    .map_or(0, |r| r.height);
            view::draw_history_panel(
                f,
                &app.input_history,
                &ranked,
                app.modal_index,
                &mut app.history_scroll,
                app.history_modal_follow,
                app.history_preview,
                app.modal_keymap_open,
                input_rect,
                activity_height,
                &app.theme,
            )
        }
        Modal::Permission => None,
        Modal::InputInjection => {
            if let Some(ref req) = app.pending_input {
                Some(view::draw_input_injection(
                    f,
                    req,
                    &app.input,
                    app.cursor_position,
                    input_rect,
                    &app.theme,
                ))
            } else {
                None
            }
        }
        Modal::Question => {
            if let Some(ref qmodel) = app.question {
                Some(view::draw_question_modal(
                    f,
                    &mut app.modal_hit_map,
                    qmodel.request(),
                    qmodel.current(),
                    qmodel.selected(),
                    qmodel.other_text(),
                    qmodel.highlight(),
                    &mut app.question_scroll,
                    app.question_modal_follow,
                    &app.theme,
                ))
            } else {
                None
            }
        }
        Modal::ModelEditor => {
            let title = if app.editor_model_settings_only {
                app.editor_model.clone()
            } else {
                app.editor_target
                    .as_deref()
                    .and_then(|id| app.provider_picker.rows.iter().find(|r| r.id == id))
                    .map(|r| r.name.clone())
                    .unwrap_or_else(|| "model".to_string())
            };
            // ADR-0046: the effort/thinking rows belong ONLY to the
            // per-model settings editor (`editor_model_settings_only`,
            // opened from the Models picker). The provider key editor
            // never shows them — reasoning is set per model, not per
            // provider.
            let effort = app
                .editor_model_settings_only
                .then_some(app.editor_effort.as_str());
            // The model's advertised ladder drives the segmented
            // flat layout; an unresolved model passes an empty
            // slice so the selector degrades to the carousel.
            let effort_levels: Vec<String> = if app.editor_model_settings_only {
                neenee_contracts::resolve_model(&app.editor_model)
                    .effort_levels
                    .iter()
                    .map(|e| e.as_str().to_string())
                    .collect()
            } else {
                Vec::new()
            };
            let thinking = app
                .editor_model_settings_only
                .then_some(app.editor_thinking)
                .filter(|_| app.editor_thinking_available);
            Some(view::draw_model_editor(
                f,
                &title,
                &app.input,
                app.cursor_position,
                !app.editor_model_settings_only,
                app.editor_field,
                effort,
                &effort_levels,
                thinking,
                &app.theme,
            ))
        }
        Modal::ProviderTemplate => Some(view::draw_provider_template_chooser(
            app.template_choice,
            f,
            &app.theme,
            &mut app.template_scroll,
        )),
        Modal::OauthPending => {
            let title: &'static str = match app.custom_auth {
                neenee_contracts::ChannelAuth::ChatGptOAuth => "ChatGPT",
                neenee_contracts::ChannelAuth::CopilotOAuth => "Copilot",
                neenee_contracts::ChannelAuth::XaiOAuth => "xAI",
                neenee_contracts::ChannelAuth::AntigravityOAuth => "Google Antigravity",
                neenee_contracts::ChannelAuth::ApiKey => "OAuth",
            };
            Some(view::draw_oauth_pending(
                title,
                &app.oauth_pending_message,
                &app.oauth_pending_url,
                &app.oauth_pending_user_code,
                app.oauth_pending_error.as_deref(),
                app.oauth_selected_item,
                f,
                &app.theme,
                &mut app.oauth_scroll,
                Some(&mut app.modal_hit_map),
                &app.selection,
                Some(&mut app.layout_map),
            ))
        }
        Modal::CustomProvider => {
            let editing = app.custom_is_editing();
            let title = if editing {
                format!("Edit · {}", app.custom_name)
            } else {
                crate::provider_template_label_for(&app.custom_protocol_wire)
            };
            let model_display = if app.custom_model.is_empty() {
                "—".to_string()
            } else {
                app.custom_model.clone()
            };
            // Suggestion dropdown for the Model filter field.
            let suggestions: Vec<String> =
                if app.current_custom_field() == Some(crate::CustomField::Model) {
                    app.custom_model_suggestions()
                } else {
                    Vec::new()
                };
            Some(view::draw_custom_provider_editor(
                view::CustomEditorView {
                    fields: &app.custom_fields,
                    field: app.custom_field,
                    editing,
                    title: &title,
                    name_buf: &app.custom_name,
                    base_url_buf: &app.custom_base_url,
                    token_buf: &app.custom_token,
                    model_display: &model_display,
                    url_hint: &app.custom_url_hint,
                    suggestions: &suggestions,
                    suggest_index: app.custom_suggest_index,
                    input: &app.input,
                    cursor_position: app.cursor_position,
                },
                f,
                &app.theme,
                &mut app.custom_scroll,
            ))
        }
        Modal::Help => {
            // Project the global-keybinding registry into the rows
            // the Help modal renders. Help and the live input
            // resolver share the same registry, so the keys shown
            // here can never drift from the keys that actually fire.
            let bindings: Vec<view::HelpBinding> = crate::keymap::Registry::new()
                .bindings()
                .iter()
                .map(|b| view::HelpBinding {
                    // Help prose rows use the compact lowercase
                    // chord form (`ctrl+t`), sourced from the same
                    // vocabulary the footers' capitalized form
                    // (`Ctrl+T`) derives from.
                    key: b.key.chord(),
                    description: b.description,
                })
                .collect();
            Some(view::draw_help_modal(
                f,
                &mut app.help_scroll,
                &bindings,
                &app.theme,
            ))
        }
        Modal::Sessions => Some(view::draw_sessions_modal(
            f,
            &app.sessions_overview,
            app.modal_index
                .min(app.sessions_overview.len().saturating_sub(1)),
            app.modal_keymap_open,
            &mut app.session_scroll,
            app.session_modal_follow,
            &app.theme,
            app.startup_overlay == crate::StartupOverlay::SessionsPicker,
            spinner_phase,
            app.session_info_detail,
            app.session_detail.as_ref(),
            &mut app.session_info_scroll,
        )),
        Modal::Host => {
            // The session dashboard is a first-class, full-screen
            // surface (Recess::Takeover already occluded the
            // conversation): lay it out over the whole viewport
            // instead of a centered modal rect.
            let rects = view::draw_dashboard(
                f,
                &app.host_sessions,
                app.modal_index
                    .min(app.host_sessions.len().saturating_sub(1)),
                app.host_focus,
                app.modal_keymap_open,
                &mut app.host_scroll,
                app.host_modal_follow,
                &mut app.host_detail_scroll,
                app.host_prompting,
                app.host_prompt_new,
                &app.input,
                &app.theme,
                spinner_phase,
                viewed_session_id,
            );
            // Stash the list-body height so the page-scroll step
            // (computed after this match from `drawn_modal_rect`)
            // can use the real body height, not panel-minus-chrome.
            dashboard_list_body_height = Some(rects.list_body.height);
            // The session preview overlays the dashboard (Enter on
            // a dock selection). Rendered after the dashboard so
            // it floats on top.
            if let Some(preview_id) = &app.host_preview {
                let row = app.host_sessions.iter().find(|r| &r.id == preview_id);
                view::draw_session_preview(f, row, &mut app.host_preview_scroll, &app.theme);
            }
            Some(rects.area)
        }
        Modal::TokenReport => {
            // Snapshot the shared ledger (standalone path) or the
            // on-demand harness reply (attach path); the attach
            // path renders a loading placeholder until the reply
            // lands.
            let report = app.token_source_report(viewed_session_id);
            let loading = app.token_ledger.is_none() && report.is_none();
            let report = report.unwrap_or_default();
            Some(view::draw_token_report_modal(
                f,
                &report,
                view::ContextUsageView {
                    snapshot: app.context_tokens,
                    window_tokens: crate::providers::model_context_window(&app.current_model),
                },
                app.modal_index
                    .min(view::token_report_round_count(&report).saturating_sub(1)),
                app.token_report_detail,
                loading,
                &mut app.token_report_scroll,
                &app.theme,
            ))
        }
        Modal::UsageStats => {
            // The durable cross-session view (`/usage`, ADR-0122). The
            // daemon-side store aggregates every session's terminal
            // requests; a loading placeholder shows until the
            // `QueryUsageStats` reply lands.
            let loading = app.usage_stats.is_none();
            let report = app.usage_stats.clone().unwrap_or_default();
            Some(view::draw_usage_stats_modal(
                f,
                &report,
                loading,
                &mut app.usage_stats_scroll,
                &app.theme,
            ))
        }
        Modal::Tools => Some(view::draw_tools_modal(
            f,
            app.session_context.as_ref(),
            app.modal_index,
            &mut app.session_scroll,
            app.session_modal_follow,
            &app.theme,
        )),
        Modal::Mcp => Some(view::draw_mcp_modal(
            f,
            app.session_context.as_ref(),
            app.modal_index,
            &mut app.session_scroll,
            app.session_modal_follow,
            &app.theme,
        )),
        Modal::Skills => Some(view::draw_skills_modal(
            f,
            app.session_context.as_ref(),
            app.modal_index,
            app.skills_expanded,
            &mut app.session_scroll,
            &app.theme,
        )),
        Modal::Permissions => Some(view::draw_permissions_manager(
            f,
            app.session_context.as_ref(),
            app.modal_index,
            &mut app.permissions_scroll,
            &app.theme,
        )),
        Modal::Config => {
            let rects = view::draw_config_view(
                f,
                view::ConfigViewProps {
                    category_index: app.config_category,
                    detail_index: app.config_detail_index,
                    focus: app.config_focus,
                    color_scheme: &app.color_scheme,
                    custom_color_scheme: &app.custom_color_scheme,
                    custom_color_draft: &app.custom_color_draft,
                    custom_editing: app.config_custom_editing,
                    input: &app.input,
                    cursor_position: app.cursor_position,
                    transcript_layout: app.transcript_layout,
                    expand_auto_scroll: app.expand_auto_scroll,
                    click_outside_dismiss: app.click_outside_dismiss,
                    workspace: &app.current_workspace,
                    category_scroll: &mut app.config_scroll,
                    detail_scroll: &mut app.config_detail_scroll,
                    theme: &app.theme,
                },
            );
            Some(rects.area)
        }
        Modal::Activity => {
            let user_prompt: Option<String> = app
                .focused_messages()
                .iter()
                .rev()
                // Only a genuine chat prompt is the round's driving
                // prompt. Slash commands (`/review …`) and shell
                // passthroughs (`!ls`) are surfaced as `Role::User`
                // in the transcript but are handled by the harness /
                // bash tool, never seen by the model — so they must
                // not be shown as the Activity modal's "Prompt".
                .find(|m| {
                    m.role == neenee_contracts::Role::User
                        && m.origin == crate::model::document::UserMessageOrigin::Chat
                })
                .map(|m| m.raw.clone());
            Some(view::draw_activity_modal(
                f,
                view::ActivityModalView {
                    active_tab: app.activity_tab,
                    todos: app.todos.as_ref(),
                    user_prompt: user_prompt.as_deref(),
                    round_count: viewed_chrome.round_count,
                    current_turn: viewed_chrome.current_turn,
                    current_model: app.current_model.as_str(),
                    round_started_at: viewed_chrome.round_started_at,
                    activity: &status,
                },
                &mut app.activity_scroll,
                &app.theme,
            ))
        }
        Modal::Queue => Some(view::draw_queue_modal(
            f,
            view::QueueModalView {
                items: &queue_modal_items,
                blocked: app.pending_count(viewed_session_id) > 0
                    && app.is_queue_blocked(viewed_session_id),
            },
            app.modal_index,
            &mut app.queue_scroll,
            app.queue_modal_follow,
            &app.theme,
        )),
        Modal::Btw => Some(view::draw_btw_modal(
            f,
            view::BtwModalView {
                asides: &app.btw_list,
                // Derived from the per-session running set (live via
                // HarnessState) rather than the list snapshot, so a round
                // finishing updates the badge without a list refetch.
                running: &app
                    .btw_list
                    .iter()
                    .map(|row| app.running_sessions.contains(row.id.as_str()))
                    .collect::<Vec<bool>>(),
                active_id: app.side_session_id.as_deref(),
            },
            app.modal_index,
            &mut app.btw_scroll,
            app.btw_modal_follow,
            app.modal_keymap_open,
            &app.theme,
        )),
        Modal::None => None,
    };

    // Provider-delete confirm overlay: a sub-layer painted *on top
    // of* the Connections list. Drawn after the picker so it
    // overpaints its own dimmed backdrop + centered panel, leaving
    // the list visible (dimmed) behind it. Only present while a
    // deletion is staged from `Shift+D`.
    if app.active_modal == Modal::Connections
        && let Some(ref pending_id) = app.pending_provider_delete
    {
        let provider_name = app
            .provider_picker
            .rows
            .iter()
            .find(|r| &r.id == pending_id)
            .map(|r| r.name.clone())
            .unwrap_or_else(|| pending_id.clone());
        app.provider_delete_rect = Some(view::draw_provider_delete_confirm(
            f,
            &provider_name,
            match app.provider_delete_focus {
                ProviderDeleteChoice::Cancel => view::ProviderDeleteChoiceView::Cancel,
                ProviderDeleteChoice::Delete => view::ProviderDeleteChoiceView::Delete,
            },
            &app.theme,
        ));
    } else {
        app.provider_delete_rect = None;
    }

    // Copy toast
    if app.copy_toast_until.is_some() {
        view::draw_copy_toast(
            f,
            &app.copy_toast_message,
            app.copy_toast_failed,
            &app.theme,
        );
    } else if app.notice_toast_until.is_some() {
        // A toast-surfaced command acknowledgment (e.g.
        // `/autopilot on`). Rendered only when no copy toast is
        // showing, since the two share the same top-right slot.
        view::draw_notice_toast(
            f,
            &app.notice_toast_message,
            app.notice_toast_severity,
            &app.theme,
        );
    } else if app.ctrl_c_armed() {
        // The copy toast and the armed toast render at the same
        // screen position, so only one shows at a time. The
        // clearing-input path surfaces the armed state through the
        // copy toast itself ("input cleared — Ctrl+C again to
        // exit"); once it expires, the standalone armed toast
        // takes over for the remainder of the quit window.
        view::draw_armed_toast(f, "press Ctrl+C again to exit", &app.theme);
    }
    if app.esc_armed_ticks > 0 {
        view::draw_armed_toast(f, "Esc again interrupts", &app.theme);
    }

    app.layout_map = layout_map;

    // Capture the open modal's body height for page-scroll step
    // sizing. The renderer returns the full panel rect; the body is
    // that rect minus the header/footer/padding chrome. All
    // centered modals that paint a scrollable body use the same
    // `modal_frame(header, footer)` chrome, so the row count is the
    // shared `modal_chrome_rows` for a header+footer spec. Stays 0
    // for modals that return no rect (Permission sheet, which
    // scrolls the transcript behind it via `view_height` instead),
    // so the page step falls back to the transcript height there.
    app.modal_body_height = match dashboard_list_body_height {
        // The dashboard's scroll body is its list pane, whose height
        // was reported directly by the renderer.
        Some(h) => h,
        None => drawn_modal_rect
            .map(|r| {
                r.height
                    .saturating_sub(crate::primitives::modal_chrome_rows(
                        crate::primitives::ModalSpec {
                            width_percent: 0,
                            header: true,
                            footer: true,
                        },
                    ))
            })
            .unwrap_or(0),
    };

    // Record the open modal's actual panel rect (when one is
    // dismissable) so a click on the backdrop outside it can close it.
    // The rect comes from the renderer that just painted the panel, so
    // dynamic-height modals and click hit-tests cannot drift apart.
    app.modal_rect = if app.active_modal.dismissable_by_outside_click() {
        drawn_modal_rect
    } else {
        None
    };
}
