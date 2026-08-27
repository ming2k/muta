# Model bar

Single-row strip pinned directly below the input box. The row **splits in
two**: the model-identity group (`model effort @<instance>`) anchors the left
half, and the ambient gauges — **context usage** and the latest-turn **stream
speed** — pin to the right edge. The middle fill absorbs the remaining width,
so the two halves own the row evenly from their edges.

Everything *actionable* about the composer moved inside the box itself: the
next-`Enter` action sentence and the `as:` target row are composer-owned meta
rows (see [Input box](input-box.md#meta-rows)). Long-lived **session** state
(the workspace path, the delegated autonomous flag) lives on its own dedicated
[head row](status-bar.md) below this one.

## Appearance

Reasoning model, fixed-effort (Kimi K3's single `max` tier always shows; the
id-first policy renders the wire id `k3`):

```text
 k3 max @kimi-code                        89.2k (8%) Ctrl+O   47.8 tok/s Ctrl+S
```

Non-reasoning model (effort tag absent), before the first turn lands a
defensible speed sample (the rate gauge and its keycap hide entirely — no
placeholder):

```text
 kimi-k2.7-code @kimi-code                89.2k (8%) Ctrl+O
```

Each gauge that opens a drill-down modal carries the **keycap hint** of its
keyboard twin right after its value — `Ctrl+O` for the context meter (opens
the token-source report), `Ctrl+S` for the stream rate (opens the Performance
report). Progressive disclosure: the glance row stays quiet, the hint names
the chord, the modal holds the detail.

On narrow terminals the row degrades in a fixed order: the keycap hints drop
first (the gauges are the payload), then the instance suffix (pure
provenance), then the reasoning tag, then the rate segment, then the context
meter; the model name is the last ambient item to disappear.

The segments keep their relative order — identity → context → speed — at every
width that still shows more than one of them.

| Attribute | Value |
|-----------|-------|
| Location | 1 row below the input box (bottom of the footer stack) |
| Layout | Split row: identity group flush left (`MODEL_BAR_INNER_PADDING`), gauges flush right; `MODEL_BAR_GAP_MIN` keeps the halves from colliding |
| Model name | `brand` + BOLD, leftmost |
| Instance suffix | `@<instance>` in `muted`, after the effort tag — the provider instance's display name, so identical models served by different instances stay attributable (mirrors the `· <provider>` suffix in the Models picker) |
| Reasoning effort | `{effort}` in `info` + BOLD, right after the model name — only while the active model is actually reasoning (Anthropic: thinking opted in; OpenAI: model exposes effort) |
| Context usage | Right cluster, first gauge: committed AI-visible context only — `89.2k` in `text_muted`; `(8%)` in threshold color (green/yellow/red); live composer drafts are excluded. Click (or `Ctrl+O`) opens the token-source report |
| Stream rate | Right cluster, last gauge: latest completed principal turn's client-observed rate, `47.8 tok/s` in `text_muted`; hidden entirely until a defensible sample exists, refreshed by each completed turn. Click (or `Ctrl+S`) opens the Performance report |
| Keycap hints | `Ctrl+O` / `Ctrl+S` in `muted`, one space after their gauge's value; the hint sits inside the gauge's click rect (one click target per drill-down); first to drop under width pressure |
| Background | `surface` |

## Keyboard

| Key | Action |
|-----|--------|
| `Ctrl+O` | Open the context/token usage report (`OpenTokenReport`) |
| `Ctrl+S` | Open the latest-turn performance report (`OpenPerformanceReport`) |

Both are `NoModal`-gated global bindings (they do not fire while another modal
owns the surface) and are declared in the shared keymap registry, so they
appear in the Help modal alongside the other Ctrl-row chords.

## Visibility

Hidden when overlay modals are open, and suppressed while the permission
sheet is open (the sheet takes over the input-box, model-bar, and status
rows).

## Source

`draw_model_bar` / `ModelBarView` in `chrome.rs`; the returned rects are the
context meter's click target (gauge + `Ctrl+O` hint; opens the token-source
report) and the rate segment's click target (gauge + `Ctrl+S` hint; opens the
Performance report).
