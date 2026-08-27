# Hint bar

Single-row strip pinned directly below the input box. It is **input-focused**:
the left side states what the next `Enter` does, and the right side reads —
left to right, in this order — the **model**, the **context usage**, and the
latest-turn **stream speed**: the model name with its reasoning-effort tier
and `@<instance>` provenance suffix, then the context-usage indicator, then
the stream-rate meter closing the row.

Long-lived **session** state (the workspace path, the delegated autonomous
flag) does **not** live here — it has its own dedicated [head
row](status-bar.md) at the row directly below this one.

## Appearance

Normal chat, reasoning model:

```text
 Enter send    claude-opus-4-8 high @anthropic  89.2k (8%)   47.8 tok/s
```

Fixed-effort reasoning model (Kimi K3's single `max` tier always shows; the id-first policy renders the wire id `k3`):

```text
 Enter send    k3 max @kimi-code  89.2k (8%)   47.8 tok/s
```

Non-reasoning model (effort tag absent):

```text
 Enter send    kimi-k2.7-code @kimi-code  89.2k (8%)   47.8 tok/s
```

Before a defensible sample exists, the rate segment renders as
`– tok/s` instead of dropping.

While a turn is running, the left side explains where the next message lands:

```text
 Enter queue message    k3 max @kimi-code  89.2k (8%)   47.8 tok/s
```

On narrow terminals the row degrades in a fixed order: the action sentence
compacts first (`queue message` → `queue`), then the instance suffix, the reasoning
tag, and the rate segment drop, then the context meter, then the action shrinks to
its tiny form; the model name is the last ambient item to disappear. The Enter
action itself never disappears.

The three right-side segments keep their relative order — model → context →
speed — at every width that still shows more than one of them.

| Attribute | Value |
|-----------|-------|
| Location | 1 row below the input box (bottom of the footer stack) |
| Left cluster | Next-Enter action sentence (`Enter …`) |
| Model name | `brand` + BOLD |
| Instance suffix | `@<instance>` in `muted`, after the effort tag — the provider instance's display name, so identical models served by different instances stay attributable (mirrors the `· <provider>` suffix in the Models picker) |
| Reasoning effort | `{effort}` in `info` + BOLD, right after the model name — only while the active model is actually reasoning (Anthropic: thinking opted in; OpenAI: model exposes effort) |
| Stream rate | Rightmost segment: latest completed principal turn's client-observed rate, `47.8 tok/s` in `text_muted` (or `– tok/s` before a defensible sample); click opens the Performance report |
| Context usage | Middle segment between model and rate — committed AI-visible context only: `89.2k` in `text_muted`; `(8%)` in threshold color (green/yellow/red); live composer drafts are excluded; click opens the token-source report |
| Background | `surface` |

There is no compose/browse mode pill: the TUI has a single navigation
state, not two zones (see [Transcript focus](#transcript-focus) below).
When a transcript step carries keyboard focus, the focused step itself is
reverse-highlighted in the transcript — the hint line does not advertise
it.

## Transcript focus

There are no focus *zones* and no zone-toggle key. A single optional
focused step (`App::focused_target`) is the only navigation state:

| Key | Effect |
|-----|--------|
| `Ctrl+↑` / `Ctrl+↓` | Focus / cycle the nearest transcript step |
| `↑` / `↓` (while focused) | Cycle to the previous / next step |
| `Enter` (while focused) | Open the focused step |
| `Esc` (while focused) | Clear the focus |

While a step is focused the composer panel drops to its dimmer palette to
signal that the next key acts on the step, not the input. Typing any
printable character still lands in the prompt — there is no mode that
captures typing. `Tab` is **not** a focus toggle; it only accepts a
completion suggestion when one is open.

## Visibility

Hidden when overlay modals are open, and suppressed while the permission
sheet is open (the sheet takes over the input-box, hint, and status rows).

## Source

`draw_hint_bar` / `HintBarView` in `chrome.rs`; the returned rects are the
context meter's click target (opens the token-source report) and the rate
segment's click target (opens the Performance report).
The focused-step palette switch lives in `draw_composer`; the
`Ctrl+↑`/`Ctrl+↓` handling lives in
`input/mod.rs`.
