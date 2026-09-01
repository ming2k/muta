use super::model_bar::context_usage_spans;
use super::*;
use crate::design::BAR_LEGEND_GAP_MIN;
use crate::model::layout::LayoutMap;
use crate::view::Theme;
use mutx_engine::{Color, Rect};

fn activity_row_text(width: u16, status: &str, phase: usize) -> String {
    activity_row_text_with_clause(width, status, None, false, phase)
}

fn activity_row_text_with_clause(
    width: u16,
    status: &str,
    backoff_clause: Option<&str>,
    awaiting: bool,
    phase: usize,
) -> String {
    let mut terminal = mutx_engine::TestTerminal::new(width, 1);
    terminal.draw(|frame| {
        draw_activity_bar(
            frame,
            Rect::new(0, 0, width, 1),
            None,
            crate::chrome::ActivityBarView {
                status,
                backoff_clause,
                silent_clause: None,
                awaiting_permission: awaiting,
            },
            phase,
            &Theme::default(),
        );
    });
    terminal
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>()
}

/// Render the activity bar and collect the foreground color of each cell,
/// so a test can assert e.g. the permission state paints in the warning
/// hue rather than the shimmer palette.
fn activity_row_colors(width: u16, status: &str, awaiting: bool, phase: usize) -> Vec<Color> {
    activity_row_colors_with_clause(width, status, None, awaiting, phase)
}

fn activity_row_colors_with_clause(
    width: u16,
    status: &str,
    backoff_clause: Option<&str>,
    awaiting: bool,
    phase: usize,
) -> Vec<Color> {
    let mut terminal = mutx_engine::TestTerminal::new(width, 1);
    terminal.draw(|frame| {
        draw_activity_bar(
            frame,
            Rect::new(0, 0, width, 1),
            None,
            crate::chrome::ActivityBarView {
                status,
                backoff_clause,
                silent_clause: None,
                awaiting_permission: awaiting,
            },
            phase,
            &Theme::default(),
        );
    });
    terminal
        .buffer()
        .content
        .iter()
        .map(|cell| cell.fg)
        .collect()
}

#[test]
fn activity_bar_preserves_interrupt_hint_at_minimum_width() {
    let row = activity_row_text(
        36,
        "retrying a provider request after a very detailed transient failure",
        8,
    );
    assert!(row.contains("Esc Esc interrupt"), "row was {row:?}");
    assert!(row.contains('…'), "long status was not truncated: {row:?}");
}

#[test]
fn tilde_home_shortens_a_home_rooted_path() {
    let home = dirs::home_dir()
        .or_else(|| std::env::var_os("HOME").map(std::path::PathBuf::from))
        .expect("test requires a discoverable home directory");
    let under = home.join("projects").join("xx");
    let rendered = tilde_home(&under);
    assert_eq!(
        rendered,
        std::path::PathBuf::from("~")
            .join("projects")
            .join("xx")
            .display()
            .to_string()
    );

    // The home directory itself collapses to a bare `~`.
    assert_eq!(tilde_home(&home), "~");
}

fn todo_list_with(item: &str, status: muta_contracts::TodoStatus) -> muta_contracts::TodoList {
    let mut todos = muta_contracts::TodoList::new();
    todos.items.push(muta_contracts::TodoItem {
        id: muta_contracts::TodoId(1),
        content: item.to_string(),
        status,
        created_at: 0,
        updated_at: 0,
    });
    todos
}

fn todo_row_text(todos: &muta_contracts::TodoList, width: u16) -> String {
    let mut terminal = mutx_engine::TestTerminal::new(width, 1);
    terminal.draw(|frame| {
        draw_todo_bar(frame, Rect::new(0, 0, width, 1), todos, &Theme::default());
    });
    terminal
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>()
}

#[test]
fn backoff_clause_renders_beside_status_and_degrades_narrow() {
    // Master label keeps the workflow story; the transport countdown is a
    // separate, muted clause — never replacing the label.
    let wide = activity_row_text_with_clause(
        100,
        "waiting for model",
        Some(" · retry 2/8 next in 4s"),
        false,
        0,
    );
    assert!(wide.contains("waiting for model"), "{wide:?}");
    assert!(wide.contains("retry 2/8 next in 4s"), "{wide:?}");

    // Under width pressure the compact attempt counter survives and the
    // master label is still intact.
    let narrow = activity_row_text_with_clause(
        44,
        "waiting for model",
        Some(" · retry 2/8 next in 4s"),
        false,
        0,
    );
    assert!(narrow.contains("waiting for model"), "{narrow:?}");
    assert!(
        narrow.contains("2/8") || !narrow.contains("next in"),
        "compact form should drop the countdown tail: {narrow:?}"
    );

    // No clause configured → no stray separators.
    let plain = activity_row_text_with_clause(80, "answering", None, false, 0);
    assert!(plain.contains("answering"), "{plain:?}");
}

#[test]
fn todo_bar_leads_with_brand_tag_on_a_plain_surface() {
    // The tag treatment: `TODOS` leads at the gutter in the brand accent
    // on the plain frame surface — no pin glyph, no raised tint — so the
    // row reads as quiet metadata rather than another pinned panel. We
    // assert all of this against the real buffer cells (the substring-only
    // tests can't see color or background).
    let theme = Theme::default();
    let todos = todo_list_with("write the docs", muta_contracts::TodoStatus::InProgress);
    let mut terminal = mutx_engine::TestTerminal::new(80, 1);
    terminal.draw(|frame| {
        draw_todo_bar(frame, Rect::new(0, 0, 80, 1), &todos, &theme);
    });
    let cells = terminal.buffer().content.clone();

    // (1) The tag leads at the gutter, brand-colored.
    assert_eq!(cells[0].symbol(), "T", "expected 'TODOS' tag at col 0");
    assert_eq!(cells[0].fg(), theme.brand(), "TODOS tag not brand-colored");

    // (2) The bar sits on the plain surface: no raised tint anywhere on
    // the row (sample the trailing cell too).
    assert_eq!(cells[0].bg(), Color::Reset, "tag must not sit on a tint");
    assert_eq!(cells[79].bg(), Color::Reset, "row must stay plain");
}

#[test]
fn todo_bar_shows_tag_progress_current_item_and_legend() {
    // InProgress item is the surfaced "current" content.
    let todos = todo_list_with("write the docs", muta_contracts::TodoStatus::InProgress);
    let text = todo_row_text(&todos, 80);
    assert!(text.contains("TODOS 0/1"), "row was {text:?}");
    assert!(text.contains("write the docs"), "row was {text:?}");
    assert!(text.contains("Ctrl+T expand"), "row was {text:?}");
}

#[test]
fn todo_bar_falls_back_to_first_pending_when_nothing_is_in_progress() {
    let todos = todo_list_with("write the docs", muta_contracts::TodoStatus::Pending);
    let text = todo_row_text(&todos, 80);
    assert!(text.contains("TODOS 0/1"), "row was {text:?}");
    // The first Pending item reads as "next up" when nothing is mid-flight.
    assert!(text.contains("write the docs"), "row was {text:?}");
}

#[test]
fn todo_bar_drops_legend_under_width_pressure() {
    let todos = todo_list_with("write the docs", muta_contracts::TodoStatus::InProgress);
    // At 20 cols the `expand` label cannot fit alongside the preview.
    let text = todo_row_text(&todos, 20);
    assert!(text.contains("TODOS 0/1"), "row was {text:?}");
    assert!(!text.contains("expand"), "legend leaked: {text:?}");
}

#[test]
fn todo_bar_keeps_real_gap_before_the_legend() {
    // Long content truncates to the preview budget; the `Ctrl+T` keycap
    // must still keep a real gap from the text instead of butting against
    // the `…`. At 40 cols the preview is truncated *and* the full legend
    // still fits, so this exercises exactly the cramped layout the gap is
    // there to prevent.
    let todos = todo_list_with(
        "a very long todo item that must be truncated to leave the legend room",
        muta_contracts::TodoStatus::InProgress,
    );
    let text = todo_row_text(&todos, 40);
    let ctrl = text.find("Ctrl").expect("legend should fit at 40 cols");
    let dots = text[..ctrl]
        .rfind('…')
        .expect("preview should be truncated");
    let between = &text[dots + '…'.len_utf8()..ctrl];
    assert!(
        between.chars().all(|c| c == ' '),
        "legend must be separated from the preview by spaces: {text:?}"
    );
    assert!(
        between.chars().count() >= BAR_LEGEND_GAP_MIN,
        "legend too close to content ({} cols): {text:?}",
        between.chars().count()
    );
}

#[test]
fn activity_bar_carries_no_todos_badge() {
    // Decoupled: the activity bar is a pure liveness surface now and never
    // embeds the `todos d/t` summary (that lives on its own bar below).
    let mut terminal = mutx_engine::TestTerminal::new(80, 1);
    terminal.draw(|frame| {
        draw_activity_bar(
            frame,
            Rect::new(0, 0, 80, 1),
            None,
            ActivityBarView {
                status: "Working",
                backoff_clause: None,
                silent_clause: None,
                awaiting_permission: false,
            },
            0,
            &Theme::default(),
        );
    });
    let text = terminal
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(!text.contains("todos"), "badge leaked onto bar: {text:?}");
    assert!(!text.contains("Ctrl+T"), "hint leaked onto bar: {text:?}");
}

#[test]
fn narrow_runtime_row_keeps_interrupt_keys_without_todos_badge() {
    let mut terminal = mutx_engine::TestTerminal::new(36, 1);
    terminal.draw(|frame| {
        draw_activity_bar(
            frame,
            Rect::new(0, 0, 36, 1),
            None,
            ActivityBarView {
                status: "retrying a provider request after a detailed transient failure",
                backoff_clause: None,
                silent_clause: None,
                awaiting_permission: false,
            },
            8,
            &Theme::default(),
        );
    });
    let text = terminal
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(text.contains("Esc Esc"), "row was {text:?}");
    // The todos summary lives on the dedicated todo bar, not here.
    assert!(!text.contains("todos"), "badge leaked: {text:?}");
    // Session-state flags live on the hint bar; the activity row never
    // carries them, even when they would fit.
    assert!(!text.contains("autopilot"), "row was {text:?}");
}

/// A pending permission request paints the status label in a steady warning
/// hue rather than the ordinary shimmer palette, so the bar reads as a
/// distinct attention state ("the round is paused on your decision") above
/// the permission sheet. The warning hue must actually appear on the label
/// cells, distinguishing it from the brand-colored shimmer.
#[test]
fn activity_bar_paints_awaiting_permission_in_warning_hue() {
    let theme = Theme::default();
    let awaiting = activity_row_colors(80, "awaiting permission", true, 4);
    let normal = activity_row_colors(80, "working", false, 4);

    // The warning color must be present somewhere in the awaiting row.
    assert!(
        awaiting.contains(&theme.warning),
        "awaiting-permission row must use the warning hue"
    );
    // A permission state must not shimmer (the shimmer sweeps the brand hue
    // across phases). The normal row, by contrast, carries brand-derived
    // colors at this phase.
    assert!(
        !awaiting.contains(&theme.warning) || awaiting != normal,
        "awaiting row must differ from the ordinary shimmer row"
    );
    // Sanity: the normal row does carry some non-warning color from the
    // shimmer (so the comparison above is meaningful).
    assert!(
        normal
            .iter()
            .any(|&c| c != theme.muted() && c != Color::Reset),
        "normal row should carry shimmer colors"
    );
}

#[test]
fn format_token_count_uses_si_suffixes() {
    assert_eq!(format_token_count(0), "0");
    assert_eq!(format_token_count(999), "999");
    assert_eq!(format_token_count(1000), "1.0k");
    assert_eq!(format_token_count(20_200), "20.2k");
    assert_eq!(format_token_count(1_000_000), "1.0M");
    assert_eq!(format_token_count(3_200_000_000), "3.2B");
}

#[test]
fn context_usage_spans_render_used_and_percentage() {
    let theme = Theme::default();
    let spans = context_usage_spans(20_200, 256_000, &theme, theme.panel());
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(text, "20.2k (8%)");
    // Color psychology: calm/muted at low ratio, warning at >=70%, error at >=90%.
    assert_eq!(spans[1].style.fg, theme.muted());

    let warn_spans = context_usage_spans(195_000, 256_000, &theme, theme.panel());
    assert_eq!(warn_spans[1].style.fg, theme.warn());

    let crit_spans = context_usage_spans(240_000, 256_000, &theme, theme.panel());
    assert_eq!(crit_spans[1].style.fg, theme.err());
}

/// Split-row contract: the telemetry gauges (`context`, `rate`, and unified
/// `Ctrl+O` hint) anchor the left half, and the identity group (`model effort
/// @instance`) pins right — reading left → right as **context → speed →
/// identity**. Under width pressure the keycap hint drops first, then
/// the instance suffix (provenance is nice-to-have) while the model
/// name, effort tag, context meter, and stream rate all still fit.
#[test]
fn model_bar_orders_context_speed_then_model() {
    let row_text = |width: u16, tps: Option<f64>| -> String {
        let mut terminal = mutx_engine::TestTerminal::new(width, 1);
        terminal.draw(|f| {
            draw_model_bar(
                f,
                Rect::new(0, 0, width, 1),
                ModelBarView {
                    current_model: "kimi-k2.7-code",
                    model_available: true,
                    provider_name: Some("kimi-code"),
                    reasoning_effort: Some("max"),
                    last_turn_tps: tps,
                    ..Default::default()
                },
                &Theme::default(),
            );
        });
        let buf = terminal.buffer();
        (0..buf.area().width as usize)
            .map(|x| buf.content[x].symbol().to_string())
            .collect::<String>()
    };

    // Wide enough for everything: `ctx rate Ctrl+O` left,
    // `model effort @instance Ctrl+N` right, in that left-to-right order.
    let wide = row_text(80, Some(47.8));
    let ctx_pos = wide.find("(0%)").expect("context meter shown");
    let rate_pos = wide.find("47.8 tok/s").expect("stream rate shown");
    let model_pos = wide.find("kimi-k2.7-code").expect("model shown");
    assert!(
        ctx_pos < rate_pos && rate_pos < model_pos,
        "row must read context → speed → identity: {wide:?}"
    );
    let inst_pos = wide.find("@kimi-code").expect("instance suffix shown");
    assert!(model_pos < inst_pos, "instance follows the model: {wide:?}");
    // Progressive disclosure: single unified keycap trails the telemetry cluster.
    let telemetry_key = wide.find("Ctrl+O").expect("telemetry keycap hint shown");
    assert!(
        rate_pos < telemetry_key,
        "keycap trails the gauges: {wide:?}"
    );
    let conn_key = wide.find("Ctrl+N").expect("connection keycap hint shown");
    assert!(
        inst_pos < conn_key,
        "connection keycap trails the identity cluster: {wide:?}"
    );
    // Justified split: the identity cluster pins flush to the row's
    // right edge (mirrored `inner` indent).
    assert!(
        wide.trim_end().ends_with("kimi-k2.7-code max @kimi-code Ctrl+N"),
        "identity must end at the right edge: {wide:?}"
    );

    // No TPS sample yet: the rate gauge hides entirely —
    // no `– tok/s` placeholder noise before the first turn completes.
    let cold = row_text(80, None);
    assert!(
        !cold.contains("tok/s"),
        "rate gauge must hide without a sample: {cold:?}"
    );
    assert!(
        cold.contains("(0%)") && cold.contains("Ctrl+O"),
        "context gauge and single telemetry keycap survive: {cold:?}"
    );

    // Narrower row: the keycap hint drops first (52 still keeps the
    // provenance suffix), then the instance suffix (46), while the
    // gauges, model name, and effort tag survive in order.
    let narrow = row_text(52, Some(47.8));
    assert!(
        !narrow.contains("Ctrl+O"),
        "keycap hint hides first: {narrow:?}"
    );
    assert!(
        narrow.contains("@kimi-code"),
        "provenance suffix survives at 52: {narrow:?}"
    );
    let tighter = row_text(46, Some(47.8));
    assert!(
        !tighter.contains('@'),
        "instance should hide next: {tighter:?}"
    );
    let ctx_pos = tighter.find("(0%)").expect("context survives");
    let rate_pos = tighter.find("tok/s").expect("rate survives");
    let model_pos = tighter.find("kimi-k2.7-code").expect("model survives");
    assert!(
        ctx_pos < rate_pos && rate_pos < model_pos,
        "order must hold after dropping the hints: {tighter:?}"
    );
}

#[test]
fn model_bar_renders_model_and_context() {
    let theme = Theme::default();
    let mut terminal = mutx_engine::TestTerminal::new(80, 3);
    terminal.draw(|f| {
        draw_model_bar(
            f,
            Rect::new(0, 2, 80, 1),
            ModelBarView {
                current_model: "mock-model",
                model_available: true,
                provider_name: Some("mock-instance"),
                ..Default::default()
            },
            &theme,
        );
    });
    let buf = terminal.buffer();
    let text = (0..buf.area().width as usize)
        .map(|x| buf.content[2 * 80 + x].symbol().to_string())
        .collect::<String>();
    assert!(
        text.contains("mock-model @mock-instance"),
        "row was {text:?}"
    );
}

#[test]
fn model_bar_renders_unavailable_model_indicator() {
    let theme = Theme::default();
    let mut terminal = mutx_engine::TestTerminal::new(80, 1);
    terminal.draw(|f| {
        draw_model_bar(
            f,
            Rect::new(0, 0, 80, 1),
            ModelBarView {
                current_model: "old-delisted-model",
                model_available: false,
                provider_name: Some("zai-code"),
                ..Default::default()
            },
            &theme,
        );
    });
    let buf = terminal.buffer();
    let text = (0..buf.area().width as usize)
        .map(|x| buf.content[x].symbol().to_string())
        .collect::<String>();
    assert!(
        text.contains("old-delisted-model [unavailable]"),
        "row was {text:?}"
    );
}

#[test]
fn model_bar_click_rects_follow_context_speed_order() {
    let theme = Theme::default();
    let mut terminal = mutx_engine::TestTerminal::new(80, 1);

    let mut captured = ModelBarRects::default();
    terminal.draw(|f| {
        captured = draw_model_bar(
            f,
            Rect::new(0, 0, 80, 1),
            ModelBarView {
                current_model: "kimi-k2.7-code",
                provider_name: None,
                last_turn_tps: Some(47.8),
                ..Default::default()
            },
            &theme,
        );
    });
    let ctx = captured.context.expect("context rect present");
    let perf = captured.performance.expect("performance rect present");
    let conn = captured.connection.expect("connection rect present");
    assert!(
        ctx.x + ctx.width <= perf.x,
        "context meter must sit left of the stream-rate segment"
    );
    // The gauges anchor the row's left edge: the context rect starts at
    // the inner indent, one cell in.
    assert_eq!(ctx.x, 1, "gauges must lead the row from the left indent");
    // Rects carry their gauge segment text; the trailing gauge includes
    // the single Ctrl+O keycap hint.
    let buf = terminal.buffer();
    let slice = |r: Rect| -> String {
        (r.x..r.x + r.width)
            .map(|x| buf[(x, r.y)].symbol().to_string())
            .collect::<String>()
    };
    assert_eq!(slice(ctx), "0 (0%)", "context rect mismatch");
    assert_eq!(slice(perf), "47.8 tok/s Ctrl+O", "rate rect mismatch");
    assert_eq!(slice(conn), "kimi-k2.7-code Ctrl+N", "connection rect mismatch");
    // The identity cluster sits right of the gauges, pinned to the row's
    // right edge (one trailing indent cell).
    let row: String = (0..80).map(|x| buf[(x, 0)].symbol().to_string()).collect();
    let model_pos = row.find("kimi-k2.7-code").expect("model on the row");
    assert!(
        perf.x + perf.width <= model_pos as u16,
        "identity must sit right of the gauges: {row:?}"
    );
    assert_eq!(
        &row[80 - 1 - "kimi-k2.7-code Ctrl+N".len()..80 - 1],
        "kimi-k2.7-code Ctrl+N",
        "model must end at the right indent: {row:?}"
    );
}

#[test]
fn model_bar_reasoning_tag_shows_effort_when_set() {
    // Render the full model row for three effort states and read back the
    // whole line: the bare `{effort}` tag must appear right after the
    // model name when reasoning is in use and be absent entirely
    // otherwise.
    fn row_text(effort: Option<&str>) -> String {
        let mut terminal = mutx_engine::TestTerminal::new(80, 1);
        terminal.draw(|f| {
            draw_model_bar(
                f,
                Rect::new(0, 0, 80, 1),
                ModelBarView {
                    current_model: "mock",
                    reasoning_effort: effort,
                    ..Default::default()
                },
                &Theme::default(),
            );
        });
        let buf = terminal.buffer();
        (0..buf.area().width as usize)
            .map(|x| buf.content[x].symbol().to_string())
            .collect::<String>()
            .trim()
            .to_string()
    }

    // No reasoning → no effort word anywhere on the row.
    let off = row_text(None);
    assert!(!off.contains("high"), "effort leaked in: {off:?}");
    assert!(!off.contains('◆'), "no diamond glyph in: {off:?}");
    // Reasoning on → bare `high` appears after the model name.
    let on = row_text(Some("high"));
    assert!(on.contains("high"), "missing effort tag in: {on:?}");
    let model_pos = on.find("mock").expect("model name on the row");
    let effort_pos = on.find("high").expect("effort tag");
    assert!(model_pos < effort_pos, "effort must follow the model name");
    // A different effort level renders its own value, not a hardcoded one.
    assert!(row_text(Some("max")).contains("max"));
}

#[test]
fn model_bar_shows_the_instance_suffix_after_the_model_name() {
    // The `@<instance>` suffix must trail the model name so identical
    // models served by different instances stay attributable — and must
    // vanish entirely when no instance is known.
    fn row_text(provider_name: Option<&str>) -> String {
        let mut terminal = mutx_engine::TestTerminal::new(80, 1);
        terminal.draw(|f| {
            draw_model_bar(
                f,
                Rect::new(0, 0, 80, 1),
                ModelBarView {
                    current_model: "mock",
                    provider_name,
                    ..Default::default()
                },
                &Theme::default(),
            );
        });
        let buf = terminal.buffer();
        (0..buf.area().width as usize)
            .map(|x| buf.content[x].symbol().to_string())
            .collect::<String>()
            .trim()
            .to_string()
    }

    let named = row_text(Some("kimi-code"));
    assert!(
        named.contains("@kimi-code"),
        "missing @instance in: {named:?}"
    );
    // The suffix is the last segment of the identity group:
    // `model effort @instance`.
    let model_pos = named.find("mock").expect("model name on the row");
    let inst_pos = named.find("@kimi-code").expect("instance suffix");
    assert!(model_pos < inst_pos, "instance must follow the model name");
    // Unknown / empty instance → no `@` anywhere on the row.
    assert!(!row_text(None).contains('@'));
    assert!(!row_text(Some("")).contains('@'));
}

#[test]
fn model_bar_full_cluster_orders_model_effort_instance() {
    // The right cluster reads `Kimi K3 max @kimi-code` — effort tight
    // after the model name, the @instance provenance last. The identity
    // group (`model effort @instance`) joins with single spaces; it sits
    // across the wider gap from the left-anchored gauges.
    let mut terminal = mutx_engine::TestTerminal::new(120, 1);
    terminal.draw(|f| {
        draw_model_bar(
            f,
            Rect::new(0, 0, 120, 1),
            ModelBarView {
                current_model: "mock",
                provider_name: Some("kimi-code"),
                reasoning_effort: Some("max"),
                ..Default::default()
            },
            &Theme::default(),
        );
    });
    let buf = terminal.buffer();
    let text = (0..buf.area().width as usize)
        .map(|x| buf.content[x].symbol().to_string())
        .collect::<String>();
    let model_pos = text.find("mock").expect("model name");
    let effort_pos = text.find("max").expect("effort");
    let inst_pos = text.find("@kimi-code").expect("instance suffix");
    assert!(
        model_pos < effort_pos && effort_pos < inst_pos,
        "expected `model effort @instance` order in: {text:?}"
    );
    assert!(
        text.contains("mock max @kimi-code"),
        "identity group should join with single spaces in: {text:?}"
    );
}

#[test]
fn model_bar_ignition_label_takes_over_the_identity_cluster() {
    // During the ignition's label phase the right cluster swaps the
    // whole `model effort @instance` identity for the converging `M A X`
    // label; once the phase ends the normal cluster returns.
    fn row_text(elapsed_ms: Option<u128>) -> String {
        let mut terminal = mutx_engine::TestTerminal::new(100, 1);
        terminal.draw(|f| {
            draw_model_bar(
                f,
                Rect::new(0, 0, 100, 1),
                ModelBarView {
                    current_model: "k3",
                    provider_name: Some("kimi-code"),
                    reasoning_effort: Some("max"),
                    context_tokens: Some(12_400),
                    ignition_elapsed_ms: elapsed_ms,
                    ..Default::default()
                },
                &Theme::default(),
            );
        });
        let buf = terminal.buffer();
        (0..buf.area().width as usize)
            .map(|x| buf.content[x].symbol().to_string())
            .collect::<String>()
    }

    // Mid-label-phase: the `M A X` label replaces the identity cluster.
    let label = row_text(Some(900));
    assert!(
        label.contains('M') && label.contains('A') && label.contains('X'),
        "label phase must render M A X: {label:?}"
    );
    assert!(
        !label.contains("@kimi-code"),
        "instance cluster is hidden during the label takeover: {label:?}"
    );

    // After the label phase the identity cluster is back, effort included.
    let settled = row_text(Some(1250));
    assert!(settled.contains("max"), "effort returns: {settled:?}");
    assert!(
        settled.contains("@kimi-code"),
        "instance returns: {settled:?}"
    );

    // No ignition at all renders the ordinary cluster.
    let plain = row_text(None);
    assert!(plain.contains("k3"), "model id renders: {plain:?}");
}

/// Paint the completion menu into a test buffer and return the rect the
/// popup actually occupied (found by scanning for the popup background),
/// so assertions can check alignment and full-width highlighting without
/// duplicating the layout math.
fn paint_completion_menu(
    input_anchor_x: u16,
    selected: Option<usize>,
) -> (mutx_engine::TestTerminal, Rect) {
    let theme = Theme::default();
    let completions = vec![
        crate::completion::Completion {
            label: "/repeat".to_string(),
            description: "Schedule a prompt on a cron".to_string(),
            insert_text: "/repeat".to_string(),
            replace_start: 0,
            replace_end: 2,
            kind: crate::completion::CompletionItemKind::Slash,
            alias_of: None,
            doc: None,
        },
        crate::completion::Completion {
            label: "/permissions".to_string(),
            description: "Manage permissions".to_string(),
            insert_text: "/permissions".to_string(),
            replace_start: 0,
            replace_end: 2,
            kind: crate::completion::CompletionItemKind::Slash,
            alias_of: None,
            doc: None,
        },
    ];
    let mut terminal = mutx_engine::TestTerminal::new(80, 12);
    terminal.draw(|f| {
        let mut layout_map = LayoutMap::new();
        draw_completion_menu(
            f,
            &mut layout_map,
            &completions,
            selected,
            Rect::new(0, 10, 80, 2), // input box occupies rows 10..12
            input_anchor_x,
            &theme,
        );
    });
    // The two rows directly above the input box are the popup.
    (terminal, Rect::new(0, 8, 80, 2))
}

#[test]
fn completion_menu_left_edge_aligns_with_anchor_column() {
    let (terminal, popup) = paint_completion_menu(2, None);
    let buf = terminal.buffer();
    let body = Theme::default().body();
    // Row start of the popup: cells left of the anchor column keep the
    // app background; the popup body starts exactly at the anchor column.
    let y = popup.y;
    let at_anchor = buf.get(2, y).expect("cell at anchor column");
    assert_eq!(at_anchor.bg, body, "popup body must start at the anchor");
    assert_eq!(at_anchor.symbol(), "/");
    let left_of_anchor = buf.get(1, y).expect("cell left of anchor");
    assert_ne!(
        left_of_anchor.bg, body,
        "popup must not start before the anchor"
    );
}

#[test]
fn completion_menu_selected_row_is_one_solid_band_full_width() {
    let theme = Theme::default();
    let (terminal, popup) = paint_completion_menu(2, Some(0));
    let buf = terminal.buffer();
    let brand = theme.brand();
    let body = theme.body();
    let y = popup.y; // first popup row = selected row
    // Find the popup's horizontal extent on this row (cells whose bg is
    // the popup body/brand rather than the app background).
    let row_cells: Vec<u16> = (0..buf.area().width)
        .filter(|&x| {
            let bg = buf.get(x, y).map(|c| c.bg);
            bg == Some(brand) || bg == Some(body)
        })
        .collect();
    assert!(!row_cells.is_empty(), "popup row not found");
    let (first, last) = (*row_cells.first().unwrap(), *row_cells.last().unwrap());
    // Every cell of the selected row inside the popup extent carries the
    // selection background — label, the padding between label and
    // description, and the fill out to the popup's right edge — so the
    // highlight reads as one continuous band.
    for x in first..=last {
        assert_eq!(
            buf.get(x, y).map(|c| c.bg),
            Some(brand),
            "cell ({x}, {y}) broke the selection band"
        );
    }
    // The band spans across the menu width for the candidate.
    assert!(
        last - first >= 12,
        "popup band too narrow: {first}..={last}"
    );
    // The unselected row keeps the popup body background across its full
    // width (no brand cell leaks onto it).
    let second_row = popup.y + 1;
    for x in first..=last {
        assert_eq!(
            buf.get(x, second_row).map(|c| c.bg),
            Some(body),
            "cell ({x}, {second_row}) of the unselected row lost the body bg"
        );
    }
}

#[test]
fn completion_menu_caps_width_and_stays_anchored() {
    let theme = Theme::default();
    let completions = [
        ("/models", "Switch the active model"),
        ("/tools", "Manage session tools (enable/disable)"),
        (
            "/delegate",
            "Toggle delegated autonomous mode — agent runs without human intervention (on/off)",
        ),
    ]
    .iter()
    .map(|(l, d)| crate::completion::Completion {
        label: l.to_string(),
        description: d.to_string(),
        insert_text: l.to_string(),
        replace_start: 0,
        replace_end: 1,
        kind: crate::completion::CompletionItemKind::Slash,
        alias_of: None,
        doc: None,
    })
    .collect::<Vec<_>>();
    let mut terminal = mutx_engine::TestTerminal::new(80, 12);
    terminal.draw(|f| {
        let mut layout_map = LayoutMap::new();
        draw_completion_menu(
            f,
            &mut layout_map,
            &completions,
            None,
            Rect::new(0, 10, 80, 2),
            2,
            &theme,
        );
    });
    let buf = terminal.buffer();
    let body = theme.body();
    // The popup keeps its anchor: body-colored cells start at column 2,
    // never stretch to the right edge of the 80-column viewport.
    let y = 9u16; // last popup row (3 candidates above rows 10..12)
    let cells: Vec<u16> = (0..80u16)
        .filter(|&x| buf.get(x, y).map(|c| c.bg) == Some(body))
        .collect();
    let (first, last) = (*cells.first().unwrap(), *cells.last().unwrap());
    assert_eq!(first, 2, "popup must stay anchored at the typed token");
    assert!(
        (last - first + 1) as usize <= 80 * 3 / 5,
        "popup must not fill the viewport: {first}..={last}"
    );
    let row_text: String = (first..=last)
        .filter_map(|x| buf.get(x, y).map(|c| c.symbol().to_string()))
        .collect();
    assert!(row_text.starts_with("/delegate"), "row was {row_text:?}");
}

#[test]
fn completion_menu_renders_compact_entry_list_without_inline_descriptions() {
    let (terminal, popup) = paint_completion_menu(2, None);
    let buf = terminal.buffer();
    let row_text = |y: u16| -> String {
        (0..buf.area().width)
            .filter_map(|x| buf.get(x, y).map(|c| c.symbol().to_string()))
            .collect()
    };
    let first = row_text(popup.y);
    // Pure command entry in the left menu: no inline description text or separator
    assert!(first.contains("/repeat"), "row was {first:?}");
    assert!(
        !first.contains("Schedule a prompt on a cron"),
        "inline description should not appear in candidate list: {first:?}"
    );
    assert!(!first.contains('·'), "row was {first:?}");
}

#[test]
fn completion_menu_marks_alias_rows_with_canonical_target() {
    // An alias candidate is marked with [*] in the menu list.
    let theme = Theme::default();
    let completions = vec![
        crate::completion::Completion {
            label: "/delegate".to_string(),
            description: "Toggle delegated mode".to_string(),
            insert_text: "/delegate".to_string(),
            replace_start: 0,
            replace_end: 2,
            kind: crate::completion::CompletionItemKind::Slash,
            alias_of: None,
            doc: None,
        },
        crate::completion::Completion {
            label: "/yolo".to_string(),
            description: "Toggle delegated mode".to_string(),
            insert_text: "/delegate".to_string(),
            replace_start: 0,
            replace_end: 2,
            kind: crate::completion::CompletionItemKind::SlashAlias,
            alias_of: Some("/delegate".to_string()),
            doc: None,
        },
    ];
    let mut terminal = mutx_engine::TestTerminal::new(80, 12);
    terminal.draw(|f| {
        let mut layout_map = LayoutMap::new();
        draw_completion_menu(
            f,
            &mut layout_map,
            &completions,
            None,
            Rect::new(0, 10, 80, 2),
            2,
            &theme,
        );
    });
    let buf = terminal.buffer();
    let row_text = |y: u16| -> String {
        (0..buf.area().width)
            .filter_map(|x| buf.get(x, y).map(|c| c.symbol().to_string()))
            .collect()
    };
    let alias_row = row_text(9); // popup bottom row = second candidate
    assert!(
        alias_row.trim_start().starts_with("/yolo [*]"),
        "alias shows [*] marker: {alias_row:?}"
    );
    let canonical_row = row_text(8);
    assert!(
        canonical_row.trim_start().starts_with("/delegate"),
        "canonical row is plain: {canonical_row:?}"
    );
    assert!(
        !canonical_row.contains("[*]"),
        "canonical rows carry no alias marker: {canonical_row:?}"
    );
}

#[test]
fn completion_menu_hover_doc_flyout_only_appears_when_entry_is_selected() {
    let theme = Theme::default();
    let doc = crate::completion::CommandDoc {
        name: "/schedule".to_string(),
        summary: "Schedule a prompt on a cron or countdown".to_string(),
        usage: vec!["/schedule <when> <prompt>".to_string()],
        category: Some("Automation".to_string()),
        subcommands: vec![
            ("list".to_string(), "List scheduled prompts".to_string()),
            (
                "cancel".to_string(),
                "Cancel one schedule by id".to_string(),
            ),
        ],
    };
    let completions = vec![crate::completion::Completion {
        label: "/schedule".to_string(),
        description: "Schedule a prompt".to_string(),
        insert_text: "/schedule".to_string(),
        replace_start: 0,
        replace_end: 2,
        kind: crate::completion::CompletionItemKind::Slash,
        alias_of: None,
        doc: Some(doc),
    }];

    // 1. Unselected (selected = None): No right-side doc window is rendered
    let mut term_unselected = mutx_engine::TestTerminal::new(80, 12);
    term_unselected.draw(|f| {
        let mut layout_map = LayoutMap::new();
        draw_completion_menu(
            f,
            &mut layout_map,
            &completions,
            None,
            Rect::new(0, 10, 80, 2),
            2,
            &theme,
        );
    });
    let buf_unselected = term_unselected.buffer();
    let panel_bg = theme.panel();
    let has_panel_unselected =
        (0..80u16).any(|x| buf_unselected.get(x, 9).map(|c| c.bg) == Some(panel_bg));
    assert!(
        !has_panel_unselected,
        "unselected completion must not render hover doc flyout"
    );

    // 2. Selected (selected = Some(0)): Right-side doc window is rendered with panel_bg
    let mut term_selected = mutx_engine::TestTerminal::new(80, 12);
    term_selected.draw(|f| {
        let mut layout_map = LayoutMap::new();
        draw_completion_menu(
            f,
            &mut layout_map,
            &completions,
            Some(0),
            Rect::new(0, 10, 80, 2),
            2,
            &theme,
        );
    });
    let buf_selected = term_selected.buffer();
    let has_panel_selected =
        (0..80u16).any(|x| buf_selected.get(x, 9).map(|c| c.bg) == Some(panel_bg));
    assert!(
        has_panel_selected,
        "selected completion must render hover doc flyout with panel bg"
    );
}

#[test]
fn completion_menu_hover_doc_flyout_shows_alias_to_target_header() {
    let theme = Theme::default();
    let doc = crate::completion::CommandDoc {
        name: "/delegate".to_string(),
        summary: "Toggle delegated mode".to_string(),
        usage: vec!["/delegate".to_string()],
        category: Some("Agent".to_string()),
        subcommands: vec![],
    };
    let completions = vec![crate::completion::Completion {
        label: "/yolo".to_string(),
        description: "Toggle delegated mode".to_string(),
        insert_text: "/delegate".to_string(),
        replace_start: 0,
        replace_end: 2,
        kind: crate::completion::CompletionItemKind::SlashAlias,
        alias_of: Some("/delegate".to_string()),
        doc: Some(doc),
    }];

    let mut term = mutx_engine::TestTerminal::new(80, 12);
    term.draw(|f| {
        let mut layout_map = LayoutMap::new();
        draw_completion_menu(
            f,
            &mut layout_map,
            &completions,
            Some(0),
            Rect::new(0, 10, 80, 2),
            2,
            &theme,
        );
    });
    let buf = term.buffer();
    let row_text = |y: u16| -> String {
        (0..buf.area().width)
            .filter_map(|x| buf.get(x, y).map(|c| c.symbol().to_string()))
            .collect()
    };
    // Check that the flyout header contains "/yolo -> /delegate"
    let full_text: Vec<String> = (0..12).map(row_text).collect();
    let found_header = full_text.iter().any(|r| r.contains("/yolo -> /delegate"));
    assert!(
        found_header,
        "flyout header should show `/yolo -> /delegate`, got buffer:\n{}",
        full_text.join("\n")
    );
}

/// Read back the one-row bar as joined text for assertion.
fn queue_row_text(view: QueueBarView<'_>, width: u16, theme: &Theme) -> String {
    let mut terminal = mutx_engine::TestTerminal::new(width, 1);
    terminal.draw(|f| {
        draw_queue_bar(f, Rect::new(0, 0, width, 1), view, theme);
    });
    let buf = terminal.buffer();
    let mut out = String::new();
    for x in 0..width as usize {
        out.push_str(buf.content[x].symbol());
    }
    out.push('\n');
    out
}

#[test]
fn queue_bar_leads_with_brand_tag_on_a_plain_surface() {
    // Matching the todo bar: the `FOLLOW-UPS` tag leads at the gutter in the
    // brand accent on the plain frame surface — no tray glyph, no raised
    // tint — so the two bars read as one quiet family.
    let theme = Theme::default();
    let item = QueueItemView {
        queued_at_ms: 1_700_000_000_000,
        text: "fix the flaky test".to_string(),
    };
    let mut terminal = mutx_engine::TestTerminal::new(70, 1);
    terminal.draw(|f| {
        draw_queue_bar(
            f,
            Rect::new(0, 0, 70, 1),
            QueueBarView {
                items: &[item],
                paused: false,
                blocked: false,
            },
            &theme,
        );
    });
    let cells = terminal.buffer().content.clone();

    // (1) The tag leads at the gutter, brand-colored.
    assert_eq!(cells[0].symbol(), "F", "expected 'FOLLOW-UPS' tag at col 0");
    assert_eq!(
        cells[0].fg(),
        theme.brand(),
        "FOLLOW-UPS tag not brand-colored"
    );

    // (2) The bar sits on the plain surface: no raised tint anywhere
    // (sample the row's trailing cell too).
    assert_eq!(cells[0].bg(), Color::Reset, "tag must not sit on a tint");
    assert_eq!(cells[69].bg(), Color::Reset, "the row must stay plain");
}

#[test]
fn queue_bar_empty_state_hints_how_to_stage() {
    let text = queue_row_text(
        QueueBarView {
            items: &[],
            paused: false,
            blocked: false,
        },
        70,
        &Theme::default(),
    );
    // Identity + zero count on the single row; no time label anymore.
    assert!(text.contains("FOLLOW-UPS 0"), "row was {text:?}");
    assert!(!text.contains("--:--"), "time label leaked: {text:?}");
    // The layout hides an empty queue, so the bar renders no hint for it.
    assert!(!text.contains("queue empty"), "empty hint leaked: {text:?}");
}

#[test]
fn queue_bar_previews_next_item_with_count_and_text() {
    let item = QueueItemView {
        queued_at_ms: 1_700_000_000_000,
        text: "fix the flaky test in parser".to_string(),
    };
    let text = queue_row_text(
        QueueBarView {
            items: &[item],
            paused: true,
            blocked: false,
        },
        92,
        &Theme::default(),
    );
    // Identity + count reflects the one item; no time label anymore.
    assert!(text.contains("FOLLOW-UPS 1"), "row was {text:?}");
    assert!(!text.contains(":"), "time label leaked: {text:?}");
    // Legend: the keycap units are same-rank peers (R2) — joined by
    // plain whitespace, never a `·` (which would imply one modifies the
    // other).
    assert!(
        text.contains("Ctrl+P block  Ctrl+Q expand"),
        "peer keycaps must use R2 whitespace: {text:?}"
    );
    assert!(!text.contains('·'), "no R1 dot between peers: {text:?}");
    // A live insert is transcript-owned (ADR-0126) and never rides the
    // bar, so every bar item previews plainly — no `steer›` badge.
    assert!(!text.contains("steer›"), "steer badge leaked: {text:?}");
    // The preview rides inline on the same row.
    assert!(
        text.contains("fix the flaky test"),
        "preview text missing: {text:?}"
    );
}

#[test]
fn queue_bar_never_renders_the_tab_affordance() {
    // The Tab toggle for the insert/next-round send target was removed —
    // a busy Enter always queues for the next round — so the queue bar's
    // legend must never mention Tab.
    let item = QueueItemView {
        queued_at_ms: 1_700_000_000_000,
        text: "add a comment".to_string(),
    };
    let text = queue_row_text(
        QueueBarView {
            items: &[item],
            paused: false,
            blocked: false,
        },
        70,
        &Theme::default(),
    );
    assert!(!text.contains("Tab"), "tab legend leaked: {text:?}");
    // A non-steering item never wears the mid-round `steer›` badge.
    assert!(!text.contains("steer›"), "steer badge leaked: {text:?}");
}
