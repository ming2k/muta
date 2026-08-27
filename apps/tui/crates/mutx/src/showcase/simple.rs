//! Simpler showcases: provider picker, model editor, history search, sessions
//! picker, activity modal, help, and toasts.
//!
//! These share the [`common::run_showcase`] runner; each is its own small
//! state struct + key handler. Several are navigation-only (up/down/tab).

use std::cell::Cell;
use std::io;

use crossterm::event::KeyCode;

use muta_contracts::{ProviderPickerRow, ProviderPickerSnapshot, SessionOverview};
use muta_contracts::{TodoId, TodoItem, TodoList, TodoStatus};

use crate::ActivityTab;
use crate::composer::{ComposerText, ComposerView};
use crate::fuzzy;
use crate::model::layout::LayoutMap;
use crate::showcase::common::{self, ShowAction};
use crate::view::Theme;
use crate::view::{
    ActivityModalView, draw_activity_modal, draw_armed_toast, draw_connections_modal,
    draw_copy_toast, draw_help_modal, draw_history_panel, draw_model_editor, draw_models_modal,
    draw_sessions_modal,
};

// ─────────────────────────── provider picker ──────────────────────────────

struct ProviderState {
    index: usize,
    query: String,
    cursor: usize,
    scroll: usize,
    search: bool,
    picker: ProviderPickerSnapshot,
}

pub fn provider() -> io::Result<()> {
    let theme = Theme::default();
    let mk = |id: &str, name: &str, preset_id: &str, models: &[&str], fav: bool, key: bool| {
        ProviderPickerRow {
            id: id.to_string(),
            name: name.to_string(),
            model: models.first().copied().unwrap_or("").to_string(),
            models: models.iter().map(|m| m.to_string()).collect(),
            model_info: Vec::new(),
            builtin: true,
            protocol: String::new(),
            base_url: String::new(),
            key_ready: key,
            preset_id: preset_id.to_string(),
            client_identity: Default::default(),
            last_used_ms: fav.then_some(1_700_000_000_000),
            auth: Default::default(),
        }
    };
    let picker = ProviderPickerSnapshot {
        default_id: "anthropic".into(),
        rows: vec![
            mk(
                "openai",
                "OpenAI",
                "openai",
                &["gpt-4o", "gpt-4o-mini"],
                false,
                false,
            ),
            mk(
                "anthropic",
                "Anthropic",
                "anthropic",
                &["claude-opus-4-8", "claude-sonnet-4-6"],
                true,
                true,
            ),
            mk(
                "kimi-code",
                "Kimi Code",
                "kimi-code",
                &["k3", "kimi-k2.7-code"],
                false,
                true,
            ),
        ],
    };
    let mut state = ProviderState {
        index: 0,
        query: String::new(),
        cursor: 0,
        scroll: 0,
        search: false,
        picker,
    };

    common::run_showcase(
        &mut state,
        |f, s| {
            let title = format!(
                " connections picker  {} providers  / to search  q/Ctrl+C=quit",
                s.picker.rows.len(),
            );
            let hint = " ↑↓ navigate  Enter select  / search  Esc back/quit ";
            common::draw_with_chrome(f, &title, hint, &theme, |f| {
                let mut lm = LayoutMap::new();
                let query = if s.search { s.query.trim() } else { "" };
                let ranked = crate::providers::providers_filtered_from(&s.picker, query);
                // The draw closure borrows state immutably; follow-selection
                // re-anchors the scroll each frame, so a frame-local offset is
                // sufficient for the showcase.
                let mut scroll = s.scroll;
                draw_connections_modal(
                    f,
                    &mut lm,
                    &ranked,
                    &s.picker.default_id,
                    s.index,
                    &s.query,
                    s.cursor,
                    &mut scroll,
                    true,
                    s.search,
                    false,
                    &theme,
                    &crate::model::selection::SelectionState::None,
                );
            });
        },
        |s, key| -> ShowAction {
            match key.code {
                KeyCode::Esc => {
                    // Two-stage Esc, mirroring the real picker: search → browse,
                    // then browse → quit.
                    if s.search {
                        s.search = false;
                        s.query.clear();
                        s.cursor = 0;
                        s.index = 0;
                        return ShowAction::Continue;
                    }
                    ShowAction::Exit
                }
                KeyCode::Up => {
                    if s.index > 0 {
                        s.index -= 1;
                    }
                    ShowAction::Continue
                }
                KeyCode::Down => {
                    s.index += 1;
                    ShowAction::Continue
                }
                KeyCode::Char('/') if !s.search => {
                    s.search = true;
                    s.index = 0;
                    ShowAction::Continue
                }
                KeyCode::Backspace if s.search => {
                    if s.cursor > 0 {
                        s.cursor -= 1;
                        s.query.remove(s.cursor);
                    }
                    s.index = 0;
                    ShowAction::Continue
                }
                KeyCode::Char(c) if s.search => {
                    s.query.insert(s.cursor, c);
                    s.cursor += 1;
                    s.index = 0;
                    ShowAction::Continue
                }
                _ => ShowAction::Continue,
            }
        },
    )
}

// ──────────────────────────── model editor ────────────────────────────────

// ──────────────────────────── models picker ──────────────────────────────

/// A live showcase of the flat **Models** picker — the surface that
/// exercises the full-width, gap-grouped row standard (status glyphs +
/// identity cluster + optional trailing reasoning tag). Run it to eyeball
/// the Gestalt spacing and the edge-to-edge fill under the brand cursor.
pub fn models() -> io::Result<()> {
    let theme = Theme::default();
    // Seed a snapshot with a couple of providers and a favorited model so the
    // star glyph, the current dot, and the two-tier ASCII sort all render.
    let mk = |id: &str, name: &str, preset_id: &str, models: &[&str]| ProviderPickerRow {
        id: id.to_string(),
        name: name.to_string(),
        model: models.first().copied().unwrap_or("").to_string(),
        models: models.iter().map(|m| m.to_string()).collect(),
        model_info: models
            .iter()
            .map(|m| muta_contracts::ProviderModelInfo {
                model: m.to_string(),
                protocol: String::new(),
                effort: None,
                thinking: None,
                favorite: *m == "claude-sonnet-4-6",
                // Spread recency across the seeded models so the RECENT
                // section renders with a meaningful order in the showcase.
                last_used_ms: match *m {
                    "gpt-4o" => Some(1_700_000_000_000),
                    "claude-opus-4-8" => Some(1_699_000_000_000),
                    _ => None,
                },
            })
            .collect(),
        builtin: true,
        protocol: String::new(),
        base_url: String::new(),
        key_ready: true,
        preset_id: preset_id.to_string(),
        client_identity: Default::default(),
        last_used_ms: None,
        auth: Default::default(),
    };
    let picker = ProviderPickerSnapshot {
        default_id: "anthropic".into(),
        rows: vec![
            mk("openai", "OpenAI", "openai", &["gpt-4o", "gpt-4o-mini"]),
            mk(
                "anthropic",
                "Anthropic",
                "anthropic",
                &["claude-opus-4-8", "claude-sonnet-4-6"],
            ),
        ],
    };
    let mut state = ProviderState {
        index: 0,
        query: String::new(),
        cursor: 0,
        scroll: 0,
        search: false,
        picker,
    };

    common::run_showcase(
        &mut state,
        |f, s| {
            let title = format!(
                " models picker  {} pairs  / to search  q/Ctrl+C=quit",
                s.picker.rows.iter().map(|r| r.models.len()).sum::<usize>(),
            );
            let hint = " ↑↓ navigate  Enter activate  * favorite  e settings  Esc back/quit ";
            common::draw_with_chrome(f, &title, hint, &theme, |f| {
                let mut lm = LayoutMap::new();
                let query = if s.search { s.query.trim() } else { "" };
                let ranked = crate::providers::models_flat_filtered_from(
                    &s.picker,
                    &s.picker.default_id,
                    "claude-sonnet-4-6",
                    query,
                );
                let mut scroll = s.scroll;
                draw_models_modal(
                    f,
                    &mut lm,
                    &ranked,
                    &s.picker.default_id,
                    "claude-sonnet-4-6",
                    s.index,
                    &s.query,
                    s.cursor,
                    &mut scroll,
                    true,
                    s.search,
                    false,
                    &theme,
                    &crate::model::selection::SelectionState::None,
                );
            });
        },
        |s, key| -> ShowAction {
            match key.code {
                KeyCode::Esc => {
                    if s.search {
                        s.search = false;
                        s.query.clear();
                        s.cursor = 0;
                        s.index = 0;
                        return ShowAction::Continue;
                    }
                    ShowAction::Exit
                }
                KeyCode::Up => {
                    if s.index > 0 {
                        s.index -= 1;
                    }
                    ShowAction::Continue
                }
                KeyCode::Down => {
                    s.index += 1;
                    ShowAction::Continue
                }
                KeyCode::Char('/') if !s.search => {
                    s.search = true;
                    s.index = 0;
                    ShowAction::Continue
                }
                KeyCode::Backspace if s.search => {
                    if s.cursor > 0 {
                        s.cursor -= 1;
                        s.query.remove(s.cursor);
                    }
                    s.index = 0;
                    ShowAction::Continue
                }
                KeyCode::Char(c) if s.search => {
                    s.query.insert(s.cursor, c);
                    s.cursor += 1;
                    s.index = 0;
                    ShowAction::Continue
                }
                _ => ShowAction::Continue,
            }
        },
    )
}

// ──────────────────────────── model editor ────────────────────────────────

struct ModelEditorState {
    input: String, // the live API-key value
    cursor: usize,
}

pub fn model_editor() -> io::Result<()> {
    let theme = Theme::default();
    let mut state = ModelEditorState {
        input: String::new(),
        cursor: 0,
    };

    common::run_showcase(
        &mut state,
        |f, s| {
            let title = " key editor  API key  q/Ctrl+C=quit".to_string();
            let hint = " type to edit  Enter save  Esc quit ";
            common::draw_with_chrome(f, &title, hint, &theme, |f| {
                draw_model_editor(
                    f,
                    "OpenAI",
                    &s.input,
                    s.cursor,
                    true,
                    0,
                    None,
                    &[],
                    None,
                    None,
                    &theme,
                );
            });
        },
        |s, key| -> ShowAction {
            match key.code {
                KeyCode::Esc => ShowAction::Exit,
                KeyCode::Backspace => {
                    if s.cursor > 0 {
                        s.cursor -= 1;
                        s.input.remove(s.cursor);
                    }
                    ShowAction::Continue
                }
                KeyCode::Char(c) => {
                    s.input.insert(s.cursor, c);
                    s.cursor += 1;
                    ShowAction::Continue
                }
                _ => ShowAction::Continue,
            }
        },
    )
}

// ──────────────────────────── history search ──────────────────────────────

struct HistoryState {
    history: Vec<muta_contracts::HistoryEntry>,
    query: String,
    cursor: usize,
    index: usize,
}

pub fn history() -> io::Result<()> {
    let theme = Theme::default();
    let history: Vec<muta_contracts::HistoryEntry> = [
        "Refactor the renderer into overlay modules",
        "Fix the tool_call_id routing bug",
        "Add a question modal MVU extraction",
        "Wire the showcase subcommand into main",
        "How does the permission sheet scroll work?",
        "cargo test -p mutx snapshot_tests",
        "Update the README with the new showcase command",
        "Why does the activity bar hide during streaming?",
    ]
    .into_iter()
    .enumerate()
    .map(|(i, text)| {
        muta_contracts::HistoryEntry::new(
            text.to_string(),
            Some(format!("demo-{i}")),
            Some("~/projects/muta".to_string()),
            (i as u64) * 600,
        )
    })
    .collect();
    let mut state = HistoryState {
        history,
        query: String::new(),
        cursor: 0,
        index: 0,
    };

    common::run_showcase(
        &mut state,
        |f, s| {
            let texts: Vec<&str> = s.history.iter().map(|e| e.text.as_str()).collect();
            let ranked = fuzzy::rank(&texts, &s.query);
            let index = s.index.min(ranked.len().saturating_sub(1));
            let title = format!(
                " history search  {} entries  type to fuzzy-filter  q/Ctrl+C=quit",
                s.history.len(),
            );
            let hint = " type to filter  ↑↓ navigate  Esc clear/quit ";
            common::draw_with_chrome(f, &title, hint, &theme, |f| {
                use mutx_engine::{Line, Span};
                use mutx_engine::{Modifier, Paragraph, Style};
                let area = f.area();
                let composer_y = area.height.saturating_sub(2);
                let input_rect =
                    mutx_engine::Rect::new(area.x + 1, composer_y, area.width.saturating_sub(2), 1);
                let glyph = Span::styled("› ", Style::default().fg(theme.muted()));
                let qspan = Span::styled(
                    if s.query.is_empty() {
                        "type to search across all sessions"
                    } else {
                        &s.query
                    },
                    Style::default()
                        .fg(if s.query.is_empty() {
                            theme.muted()
                        } else {
                            theme.fg()
                        })
                        .add_modifier(Modifier::BOLD),
                );
                f.render_widget(Paragraph::new(Line::from(vec![glyph, qspan])), input_rect);
                let mut scroll = 0;
                let selection = crate::model::selection::SelectionState::None;
                let mut layout_map = crate::model::layout::LayoutMap::new();
                let _ = draw_history_panel(
                    f,
                    &s.history,
                    &ranked,
                    index,
                    &mut scroll,
                    true,
                    false,
                    false,
                    input_rect,
                    0,
                    &theme,
                    &selection,
                    &mut layout_map,
                );
            });
        },
        |s, key| -> ShowAction {
            match key.code {
                KeyCode::Esc => {
                    if s.query.is_empty() {
                        return ShowAction::Exit;
                    }
                    s.query.clear();
                    s.cursor = 0;
                    s.index = 0;
                    ShowAction::Continue
                }
                KeyCode::Up => {
                    if s.index > 0 {
                        s.index -= 1;
                    }
                    ShowAction::Continue
                }
                KeyCode::Down => {
                    s.index += 1;
                    ShowAction::Continue
                }
                KeyCode::Backspace => {
                    if s.cursor > 0 {
                        s.cursor -= 1;
                        s.query.remove(s.cursor);
                    }
                    s.index = 0;
                    ShowAction::Continue
                }
                KeyCode::Char(c) => {
                    s.query.insert(s.cursor, c);
                    s.cursor += 1;
                    s.index = 0;
                    ShowAction::Continue
                }
                _ => ShowAction::Continue,
            }
        },
    )
}

// ──────────────────────────── sessions picker ─────────────────────────────

struct SessionsState {
    sessions: Vec<SessionOverview>,
    index: usize,
}

pub fn sessions() -> io::Result<()> {
    let theme = Theme::default();
    let sessions: Vec<SessionOverview> = vec![
        SessionOverview {
            id: "abc123".into(),
            overview: "Refactor the renderer into overlay modules".into(),
            created_at: now_ms() - 3_600_000,
            updated_at: now_ms() - 600_000,
            message_count: 12,
            active: true,
            parent_id: None,
            fork_kind: muta_contracts::SessionForkKind::Trunk,
        },
        SessionOverview {
            id: "def456".into(),
            overview: "Fix the tool_call_id routing bug".into(),
            created_at: now_ms() - 86_400_000,
            updated_at: now_ms() - 43_200_000,
            message_count: 4,
            active: false,
            parent_id: Some("abc123".into()),
            fork_kind: muta_contracts::SessionForkKind::Aside,
        },
        SessionOverview {
            id: "ghi789".into(),
            overview: "Add the question modal MVU extraction".into(),
            created_at: now_ms() - 172_800_000,
            updated_at: now_ms() - 172_800_000,
            message_count: 28,
            active: false,
            parent_id: None,
            fork_kind: muta_contracts::SessionForkKind::Trunk,
        },
    ];
    let mut state = SessionsState { sessions, index: 0 };

    common::run_showcase(
        &mut state,
        |f, s| {
            let index = s.index.min(s.sessions.len().saturating_sub(1));
            let title = format!(
                " sessions picker  {} sessions  q/Ctrl+C=quit",
                s.sessions.len()
            );
            let hint = " ↑↓ navigate  Esc quit ";
            common::draw_with_chrome(f, &title, hint, &theme, |f| {
                let mut scroll = 0;
                let mut info_scroll = 0;
                let selection = crate::model::selection::SelectionState::None;
                let mut layout_map = crate::model::layout::LayoutMap::new();
                draw_sessions_modal(
                    f,
                    &s.sessions,
                    index,
                    false,
                    &mut scroll,
                    true,
                    &theme,
                    false,
                    0,
                    false,
                    None,
                    &mut info_scroll,
                    &selection,
                    &mut layout_map,
                );
            });
        },
        |s, key| -> ShowAction {
            match key.code {
                KeyCode::Esc => ShowAction::Exit,
                KeyCode::Up => {
                    if s.index > 0 {
                        s.index -= 1;
                    }
                    ShowAction::Continue
                }
                KeyCode::Down => {
                    s.index += 1;
                    ShowAction::Continue
                }
                _ => ShowAction::Continue,
            }
        },
    )
}

// ──────────────────────────── activity modal ──────────────────────────────

struct ActivityState {
    todos: TodoList,
    tab: ActivityTab,
    scroll: Cell<usize>,
    started: std::time::Instant,
}

pub fn activity() -> io::Result<()> {
    let theme = Theme::default();
    let todos = TodoList {
        items: vec![
            TodoItem {
                id: TodoId(1),
                content: "Restructure showcase into a directory module".into(),
                status: TodoStatus::Completed,
                created_at: 0,
                updated_at: 0,
            },
            TodoItem {
                id: TodoId(2),
                content: "Implement permission sheet showcase".into(),
                status: TodoStatus::InProgress,
                created_at: 0,
                updated_at: 0,
            },
            TodoItem {
                id: TodoId(3),
                content: "Wire all modals into the dispatcher".into(),
                status: TodoStatus::Pending,
                created_at: 0,
                updated_at: 0,
            },
            TodoItem {
                id: TodoId(4),
                content: "Verify build + clippy".into(),
                status: TodoStatus::Pending,
                created_at: 0,
                updated_at: 0,
            },
        ],
        ..Default::default()
    };
    let mut state = ActivityState {
        todos,
        tab: ActivityTab::Activity,
        scroll: Cell::new(0),
        started: std::time::Instant::now(),
    };

    common::run_showcase(
        &mut state,
        |f, s| {
            let title = " activity modal  q/Ctrl+C=quit";
            let hint = " ←→ / Tab cycle tabs  ↑↓ scroll  Esc quit ";
            common::draw_with_chrome(f, title, hint, &theme, |f| {
                let mut scroll = s.scroll.get();
                draw_activity_modal(
                    f,
                    ActivityModalView {
                        active_tab: s.tab,
                        todos: Some(&s.todos),
                        user_prompt: Some("Build a showcase for all TUI components"),
                        round_count: 3,
                        current_turn: 2,
                        current_model: "claude-sonnet-4-5",
                        round_started_at: Some(s.started),
                        activity: "running runner — exploring the codebase",
                        provider_retry: None,
                    },
                    &mut scroll,
                    &theme,
                    &crate::model::selection::SelectionState::None,
                    &mut crate::model::layout::LayoutMap::new(),
                );
                s.scroll.set(scroll);
            });
        },
        |s, key| -> ShowAction {
            match key.code {
                KeyCode::Esc => ShowAction::Exit,
                KeyCode::Left | KeyCode::Char('h') => {
                    s.tab = crate::ActivityTab::Activity;
                    s.scroll.set(0);
                    ShowAction::Continue
                }
                KeyCode::Right | KeyCode::Tab | KeyCode::Char('l') => {
                    s.tab = crate::ActivityTab::Todos;
                    s.scroll.set(0);
                    ShowAction::Continue
                }
                KeyCode::Up => {
                    if s.scroll.get() > 0 {
                        s.scroll.set(s.scroll.get().saturating_sub(1));
                    }
                    ShowAction::Continue
                }
                KeyCode::Down => {
                    s.scroll.set(s.scroll.get() + 1);
                    ShowAction::Continue
                }
                _ => ShowAction::Continue,
            }
        },
    )
}

// ──────────────────────────── help + toast ────────────────────────────────

pub fn help() -> io::Result<()> {
    let theme = Theme::default();
    let mut state = ();
    common::run_showcase(
        &mut state,
        |f, _| {
            common::draw_with_chrome(
                f,
                " help  keybindings  q/Esc=quit",
                " Esc quit ",
                &theme,
                |f| {
                    let mut scroll = 0;
                    // The showcase demo has no keybinding registry; pass an
                    // empty projection so only the static fallback rows render.
                    let bindings: &[crate::view::HelpBinding] = &[];
                    let selection = crate::model::selection::SelectionState::None;
                    let mut layout_map = crate::model::layout::LayoutMap::new();
                    draw_help_modal(
                        f,
                        &mut scroll,
                        bindings,
                        &theme,
                        &selection,
                        &mut layout_map,
                    );
                },
            );
        },
        |_, key| match key.code {
            KeyCode::Esc => ShowAction::Exit,
            _ => ShowAction::Continue,
        },
    )
}

struct ToastState {
    idx: usize,
}

pub fn toast() -> io::Result<()> {
    let theme = Theme::default();
    let variants: [(&str, bool); 3] = [
        ("copied to clipboard", false),
        ("clipboard read failed", true),
        ("press Ctrl+C again to exit", false), // armed uses a different fn
    ];
    let mut state = ToastState { idx: 0 };

    common::run_showcase(
        &mut state,
        |f, s| {
            let (msg, failed) = variants[s.idx];
            let title = format!(
                " toast  variant {}/{}  Tab=next  q/Ctrl+C=quit",
                s.idx + 1,
                variants.len()
            );
            let hint = " Tab next  Esc quit ";
            common::draw_with_chrome(f, &title, hint, &theme, |f| {
                if s.idx == variants.len() - 1 {
                    draw_armed_toast(f, msg, &theme);
                } else {
                    draw_copy_toast(f, msg, failed, &theme);
                }
            });
        },
        |s, key| match key.code {
            KeyCode::Tab => {
                s.idx = (s.idx + 1) % variants.len();
                ShowAction::Continue
            }
            KeyCode::Esc => ShowAction::Exit,
            _ => ShowAction::Continue,
        },
    )
}

// ─────────────────────────── effort ignition ──────────────────────────────

/// Live demo of the effort-ignition celebration (the codex `ultra` port):
/// selecting Kimi K3's `max` tier sweeps two fire waves across the composer
/// band, converges a `M A X` label on the hint bar, tints the `›` prompt
/// toward the fire accent, and lands a `✦ → ✧` spark. Press `Space` to
/// re-ignite.
pub fn effort_ignition() -> io::Result<()> {
    use std::time::Instant;

    use crate::effort_ignition::{self, ignition_finished};
    use crate::model::selection::SelectionState;
    use crate::view::{HintBarView, draw_composer_igniting, draw_hint_bar};

    let theme = Theme::default();
    struct State {
        epoch: Option<Instant>,
        draft: String,
    }
    let mut state = State {
        epoch: Some(Instant::now()),
        draft: "refactor the scheduler to batch dispatches".to_string(),
    };

    common::run_showcase(
        &mut state,
        |f, s| {
            let area = f.area();
            let width = area.width;
            // Composer occupies the middle band; the hint bar sits one row
            // below it, mirroring the live footer's stacking.
            let composer_height = 5u16;
            let composer_y = area.height.saturating_sub(composer_height + 2).max(1);
            let composer_rect = mutx_engine::Rect::new(0, composer_y, width, composer_height);
            let hint_rect = mutx_engine::Rect::new(0, composer_y + composer_height + 1, width, 1);

            common::draw_app_background(f, &theme);

            let elapsed_ms = s.epoch.map(|e| e.elapsed().as_millis());
            let mut layout_map = LayoutMap::new();
            let mut input_scroll = 0usize;
            draw_composer_igniting(
                ComposerView {
                    frame: f,
                    input_rect: composer_rect,
                    theme: &theme,
                    layout_map: &mut layout_map,
                    input_scroll: &mut input_scroll,
                    selection: &SelectionState::None,
                },
                ComposerText {
                    input: &s.draft,
                    byte_cursor: s.draft.len(),
                },
                crate::view::ComposerDrawOptions {
                    focused: true,
                    show_caret: false,
                    record: false,
                    image_count: 0,
                    paste_count: 0,
                },
                (true, elapsed_ms),
            );
            draw_hint_bar(
                f,
                hint_rect,
                HintBarView {
                    current_model: "k3",
                    model_available: true,
                    provider_name: Some("kimi-code"),
                    messages: &[],
                    reasoning_effort: Some("max"),
                    busy: false,
                    can_retry: false,
                    context_tokens: Some(12_400),
                    ignition_elapsed_ms: elapsed_ms,
                    composer_send_mode: None,
                    queue_editing_badge: None,
                },
                &theme,
            );

            // The wave + spark overlay: paints over the composer and hint bar
            // after both have rendered, exactly like the live event loop.
            if let Some(ms) = elapsed_ms
                && !ignition_finished(ms)
            {
                effort_ignition::paint_ignition_bands(f, composer_rect, Some(hint_rect.y), ms);
            }

            // Title line at the top.
            f.put(
                1,
                0,
                mutx_engine::Style::default()
                    .fg(theme.brand())
                    .add_modifier(mutx_engine::Modifier::BOLD),
                "effort ignition  Kimi K3 max",
            );
        },
        |s, key| match key.code {
            KeyCode::Esc => ShowAction::Exit,
            KeyCode::Char(' ') => {
                // Re-arm the celebration.
                s.epoch = Some(Instant::now());
                ShowAction::Continue
            }
            _ => ShowAction::Continue,
        },
    )
}

// ────────────────────────────── helpers ───────────────────────────────────

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
