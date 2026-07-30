# Hint bar

Single-row strip pinned directly below the input box. It is **input-focused**:
the left side states what the next `Enter` does, and the right side carries the
model name, an optional reasoning-effort tag, and the context-usage indicator.

Long-lived **session** state (the workspace path, the `autopilot` flag) does
**not** live here — it has its own dedicated [status bar](status-bar.md) on the
row directly below this one.

## Appearance

Normal chat, reasoning model:

```text
 Enter send                Claude Opus 4.8  ◆ high  89.2k (8%)
```

Non-reasoning model (effort tag absent):

```text
 Enter send                 Kimi K2.7 Code   89.2k (8%)
```

While a turn is running, the left side explains where the next message lands:

```text
 Enter queue message        Kimi K2.7 Code   89.2k (8%)
```

With a `!`-prefixed shell command staged, the Enter action becomes
`run command`:

```text
 Enter run command          Kimi K2.7 Code   89.2k (8%)
```

On narrow terminals the row degrades in a fixed order: the action sentence
compacts first (`queue` / `run`), then the reasoning tag and the context meter
drop, then the action shrinks to its tiny form; the model name is the last
ambient item to disappear. The Enter action itself never disappears.

| Attribute | Value |
|-----------|-------|
| Location | 1 row below the input box, above the status bar |
| Left cluster | Next-Enter action sentence (`Enter …`) |
| Model name | `brand` + BOLD |
| Reasoning effort | `◆ {effort}` in `info` + BOLD, only while the active model is actually reasoning (Anthropic: thinking opted in; OpenAI: model exposes effort) |
| Context usage | `89.2k` in `text_muted`; `(8%)` in threshold color (green/yellow/red); click opens the token-source report |
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

`draw_hint_bar` / `HintBarView` in `render/chrome.rs`; the returned rect
is the context meter's click target that opens the token-source report.
The focused-step palette switch lives in `draw_composer`
(`render/composer.rs`); the `Ctrl+↑`/`Ctrl+↓` handling lives in
`input/mod.rs`.
