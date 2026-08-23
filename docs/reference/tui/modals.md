# Modals

Centered overlays that take over the viewport until dismissed. Each modal
declares a **recess policy** — `Modal::recess`, the single source of truth that
both the footer-collapse flag and the per-frame paint consult — describing how
the surface beneath it recedes. A terminal cannot alpha-blend, so recess is
expressed in one of three ways:

- **Dim** (most centered modals): the footer keeps its height and the whole
  live surface — transcript, activity bar, input box, hint line — is darkened
  in place so it stays visible for context while the centered panel reads as
  the focal layer. The brightness is the `modal_dim_factor` theme field.
- **Takeover** (the sessions picker, the session dashboard, and the Settings
  view): the footer collapses to zero height and the surface is fully
  occluded — a clean slate for a context switch.
- **None** ([question modal](#question-modal); the
  [permission sheet](#permission-sheet) is inline): floats on the fully-live
  surface with no dimming.

## Shared chrome

Every centered modal uses the same low-level primitives and, where possible,
the shared modal component in
`crates/neenee-tui/src/components/modal.rs`:

- `recess_backdrop(frame, modal.recess(), theme)` is called once per frame by
  the event loop *after* the transcript and chrome are drawn and *before* the
  centered panel. For a **Dim** modal it scales every cell's color by
  `theme.modal_dim_factor()` (background stays visible); for **Takeover** it
  clears + fills with `theme.backdrop()` (full occlusion); for **None** it is a
  no-op.
- `centered_rect(px_w, px_h, viewport)` carves the modal rectangle out of
  the viewport (the frame minus the global 1-row top/bottom margin). The
  surrounding gutters are kept as `app_bg`.
- `modal_frame(area, theme.panel(), header, footer)` produces a borderless
  solid-bg panel with 2-col horizontal and 1-row vertical inner padding,
  vertically split into `header(Length 1) → gap(Length 1) → body(Min 0) →
  gap(Length 1) → footer(Length 1)`. Header/footer/gap rows are omitted when
  not requested.
- `draw_modal_page(ModalPage { ... })` composes geometry, frame chrome,
  header, `ScrollBody`, and modal footer hints for simple centered modals.
- `draw_selectable_list_page(SelectableListPage { ... })` adds selected-row
  follow scrolling and item/empty footer selection for list modals.

```text
                ┌──── centered_rect(px_w, px_h) ────┐
   app_bg gutter│ ░░ modal padding (top, full row)  │app_bg gutter
                │  Header  ·  brand+muted           │
                │                                   │
                │  Body  (scrollable, follow=sel.)  │
                │                                   │
                │  Footer  ·  muted                 │
   app_bg gutter│ ░░ modal padding (bot, full row)  │app_bg gutter
                └───────────────────────────────────┘
```

The two [toasts](#toasts) are non-modal and use `ToastBubble` from
`components/toast.rs`.

## Overview

| Modal | Trigger | `centered_rect` | Source |
|-------|---------|-----------------|--------|
| [Models](#models-modal) | `Ctrl+M` / `/models` | 72 × 60 | `draw_models_modal` |
| [Connections](#connections-modal) | `/connections` | 72 × 60 | `draw_connections_modal` |
| [Model editor](#model-editor) | Models or Connections modal `e` | 60 × 36 | `draw_model_editor` |
| [Sessions](#sessions-modal) | `/sessions` | 80 × 64 | `draw_sessions_modal` |
| [Tools](#tools-modal) | `/tools` | 64 × content | `draw_tools_modal` |
| [History search](#history-search-modal) | `Ctrl+R` | 70 × 72 | `draw_history_modal` |
| [Question](#question-modal) | `ask_user` tool | 78 × 70 | `draw_question_modal` |
| [Permission sheet](#permission-sheet) | Automatic | (inline, not centered) | `draw_permission_sheet` |
| [Help](#help-modal) | `Ctrl+H` / `?` / `F1` / `/help` | 58 × 70 | `draw_help_modal` |
| [Activity](#activity-modal) | Click activity bar | 72 × 70 | `draw_activity_modal` |
| [Usage statistics](#usage-statistics-modal) | `/usage` | 76 × 86% | `draw_usage_stats_modal` |
| [Asides](#asides-modal) | `F5` / `/btw list` | 66 × 84% | `draw_btw_modal` |
| [Toasts](#toasts) | Transient | top-right, 3 rows | `draw_armed_toast`, `draw_copy_toast` |

## Closing

- `Esc` or `Ctrl+C` closes most modals.
- Permission sheet: `Esc` rejects; `Ctrl+C` closes and rejects.
- Model editor: `Ctrl+C` restores the stashed composer input and exits the
  configuration flow.
- Session dashboard (a full-screen takeover, not a centered modal): `Esc`
  leaves the screen — quitting the whole TUI when `neenee dashboard` opened
  it at startup, or closing it back to the conversation when `/dashboard`
  opened it mid-session. `Ctrl+C` follows the app-wide double-press quit
  (arm, then exit) instead of closing; with text staged in the dashboard's
  inline `p` / `n` prompt the chain is clear → arm → quit.

**Retained views (ADR-0133).** Nearly every surface is now a *retained
view*: dismissing one (Esc, outside click, Ctrl+C — one shared dismiss
verb) saves its scroll, selection, and follow state to a per-view registry,
and reopening restores exactly where the user was. The old reset-on-open
ritual is gone; data-refresh queries run on a view's first open only.
Retained: Help, Activity/Todos, Tools, MCP, Skills, Permissions, Usage
stats, Context report, `/btw` asides, Settings, the Models and Connections
pickers, input history (Ctrl+R), the Queue overview, the session dashboard,
and the sessions picker. The pickers additionally park the composer draft
in per-view slots (a draft parked for Models is never clobbered by one
parked for history). Switching sessions forgets retained state (it belongs
to the conversation).

**Picker→editor navigation (ADR-0133).** The model editor, the
provider-template chooser, the custom-provider editor, and the OAuth sheet
return through a bounded navigation stack to the surface that opened them
— no hard-coded destinations. Queue's open-time outbox auto-block is
paired with a release on *every* leave path. Drill-in sub-layers (the
dashboard's preview and inline prompt, the sessions info view, the context
report's turn breakdown) step back one level per Esc through the same
shared pop the outside-click path uses.

**`Ctrl+L` — the global view switcher.** Open over any surface (a
transient chooser, itself never retained): open views first in MRU order,
then every other view as discovery. Typing filters the list fuzzily against
each view's name and entry point; `Enter` switches — hiding the origin with
its state saved — and `Esc` cancels back untouched, restoring the origin's
cursor from the registry.
**Click-outside-to-dismiss.** Read-only / info modals — Help, Tool-step
detail, Tools, Sessions, Permissions, Activity, History, and the two pickers
(Models, Connections) — close when the user clicks outside their panel,
mirroring `Esc`. Entry modals that hold precious in-progress input (Model
editor) and the decision modals (Question, Permission sheet) stay open so an
accidental click never discards an API key or a pending decision. The single
source of truth is `Modal::dismissable_by_outside_click()`.

## Models modal

Flat (provider, model) picker — the daily-driver switch surface. Every model
served by every configured connection appears as its own row, grouped into
**three labeled sections**:

1. **FAVORITES** — ★-marked models (favorite is model-level, ADR-0046),
   ASCII by model id;
2. **RECENT** — models with activation history, most recently used first
   (ASCII id as the tiebreaker);
3. **ALL MODELS** — every remaining pair, ASCII by the model id (provider
   label as the tiebreaker).

Each non-empty section is announced by a dim uppercase label row the
selection cursor skips over — ↑/↓ walks only model rows. An empty section
renders no label at all. Precedence is favorite > recent > rest: a favorite is
pinned user intent and always wins over the emergent recency signal. The
currently-active pair is **not** pinned to the top of the list; it keeps its
natural section position and is identified by its `●` glyph (the modal also
opens with the cursor on it).

Rows are **id-first**: the wire model id is the label (upstream discovery only
guarantees the id, so the list never mixes curated display names with raw
ids). Borrows the composer input as a fuzzy filter over the model id (a query
that matches only the provider name keeps its rows, unhighlighted); the
filtered results keep the same three-section grouping.

```text
╭───────────────────────────────────────────────╮
│ Models  ❯ opus                                │  ← header (real caret here)
│                                               │
│ FAVORITES                                     │  ← dim label (not selectable)
│  ●  claude-opus-4-8   · anthropic  ◆ think on │  ← selected → brand bg
│                                               │
│ RECENT                                        │
│  ●  gpt-4o           · openai                 │
│                                               │
│ ALL MODELS                                    │
│  ●  gemini-3-pro     · google                 │
│  …                                            │
│                                               │
│ type to filter · ↑↓ navigate · enter activate │
│ e settings · esc                              │
╰───────────────────────────────────────────────╯
```

| Key | Effect |
|-----|--------|
| printable | Append to the filter (composer is the input source) |
| `↑` / `↓` | Move selection |
| `Enter` | Activate the highlighted (provider, model) row |
| `e` | Open the per-model settings editor (effort / thinking) |
| `d` | Remove the highlighted model from a custom provider |
| `Esc` | Close |

`Ctrl+M` opens this modal only on terminals that support the Kitty enhanced
keyboard protocol. In a raw terminal `Ctrl+M` is byte-identical to `Enter`,
so on unsupported terminals the key falls through to `Enter` and `/models`
is the reliable trigger.

## Connections modal

Provider-instance management surface. Rows are the configured provider
instances, ranked last-used → name; each row shows the instance name and its
provider *type* (`· OpenAI`). This surface only *manages* instances — it has
no activate concept, so switching the active provider is done from the Models
picker. When no instance exists, an empty-state hint prompts the user to press
`a`.

| Key | Effect |
|-----|--------|
| printable | Append to the filter (composer is the input source) |
| `↑` / `↓` | Move selection |
| `/` | Enter the search sub-layer (`Esc` clears it) |
| `a` | Add a connection — open the provider-template chooser |
| `e` | Edit — API key for built-ins, full meta editor for custom providers |
| `D` | Delete a custom provider (confirm overlay) |
| `Esc` | Close |

### Add connection (template chooser)

The secondary page `a` opens, rendered inside the same panel with a
`Connections › Add connection` breadcrumb. One row per provider template,
**sorted alphabetically by title**. An unfocused row shows its title alone; the
focused row additionally reveals the template's one-line description and is
marked by a full-width brand background highlight (no `›` cursor marker). Each
row carries a trailing auth-scheme badge — `⚿ oauth` for browser/device-flow
subscriptions, `⚿ token` for API-key templates — separated from the title by
whitespace, never a `·`. The wire protocol and the seeded model count are
deliberately omitted: the models an endpoint actually serves are only knowable
with a working credential, and the protocol is locked by the template.

```text
╭──────────────────────────────────────────────────────────────────╮
│ Connections › Add connection                                      │
│                                                                   │
│  Anthropic                                                  ⚿ token │
│    Claude models over the Anthropic /messages API                │
│  Anthropic (sub2api)                                        ⚿ token │
│  Antigravity (sub2api)                                      ⚿ token │
│  Antigravity OAuth                                          ⚿ oauth │
│  ChatGPT OAuth                                              ⚿ oauth │
│  …                                                               │
│                                                                   │
│ ↑↓ navigate  Enter select  Esc back                              │
╰──────────────────────────────────────────────────────────────────╯
```

| Key | Effect |
|-----|--------|
| `↑` / `↓` | Move selection (wraps) |
| `Enter` | Select — OAuth templates start the browser flow, token templates open the editor |
| `Esc` | Back to the Connections list |

## Model editor

Unified API-key + model-id editor (ADR-0002 phase 4). Two fields with `Tab`
cycling focus; the composer input is the value of the focused field, the
unfocused one is held in a buffer.

```text
╭───────────────────────────────────╮
│ Edit · openai                     │
│                                   │
│  API key   ••••••••••••••••       │  ← muted (unfocused, masked)
│  Model id  gpt-4o                 │  ← bold brand label (focused, caret)
│                                   │
│ tab switch field · enter save · esc cancel │
╰───────────────────────────────────╯
```

The API key is masked as `•` per character whenever it is not focused.

| Key | Effect |
|-----|--------|
| printable | Append to the focused field |
| `Tab` | Cycle focus between API key and Model id |
| `Enter` | Save the focused field and switch to the other |
| `Esc` / `Ctrl+C` | Cancel and restore the stashed composer input |

## Sessions modal

Sessions picker. Each row shows an overview plus created/active relative
times; `Enter` resumes the selected session.

```text
╭──────────────────────────────────────────────────────────╮
│ Sessions                                                 │
│                                                          │
│ ●  fix login redirect bug      created 2h · active 3m    │  ← active + selected
│    refactor database layer     created 1d · active 5h    │
│    write API docs              created 3d · active 2d    │
│                                                          │
│ ↑↓ navigate · Enter open · d delete · Esc close          │
╰──────────────────────────────────────────────────────────╯
```

The `●` badge marks the currently active session. Overview text is
truncated with `…` when it would collide with the meta column.

The `i` info sub-view (session id, title, timestamps, message count, full
last prompt) is a selectable document — drag to select, `Ctrl+Shift+C` to
copy (see [Selecting modal text](#selecting-modal-text)). Useful for
copying a session id straight out of the read-out. The list itself stays a
picker (keyboard-driven, not selectable text).

## Asides modal

The live `/btw` asides list (ADR-0103 §5), opened by `F5` or `/btw list`.
Every background aside appears as one row, newest first: a `run` badge while
its round is in flight (an `open` badge when it is the focused view), a
relative last-activity time, and the aside's title (its first prompt).

```text
╭──────────────────────────────────────────────────────────╮
│ Asides (2)                                               │
│                                                          │
│ run  2m   check the migration plan assumptions            │
│      1h   quick question about the retry logic            │
│                                                          │
│ ↑↓ select · Enter open · F5 refresh · D close aside ·     │
│ Esc close                                                │
╰──────────────────────────────────────────────────────────╯
```

- `Enter` jumps back into the aside: the harness answers with
  `SideViewOpened` carrying the aside's full transcript, so the view shows
  its complete history (inherited parent context included).
- `D` closes the aside outright — cancels its round, removes it from the
  list, and deletes its session files. Deliberately uppercase (matching the
  queue's delete) so a stray keypress never loses a background aside.
- `F5` re-queries the list in place; the harness also pushes a fresh list on
  every registry mutation. The modal stays open across refreshes.
- `Esc` / outside click closes the modal without touching any aside.

## Tools modal

Interactive tool manager, opened by `/tools` (or `t`/`Enter` from the Session
dashboard's `TOOLS` line). A centered, scrollable list of every tool available
to the live session — builtins and `mcp:<server>` tools — each with
its source, a short description, and an `[on]`/`[off]` badge. `Space` toggles a
tool; the harness applies it and replies with a fresh snapshot that re-renders
the list. Data comes from the session-context snapshot.

```text
╭────────────────────────────────────────────────────────╮
│ Tools                                                  │
│                                                        │
│  ●  bash              builtin    run a shell command[on]│  ← selected → brand bg
│  ●  read_text         builtin    read a text file    [on]│
│  ○  mcp__fs__search   mcp:fs     semantic file search[off]│
│  …                                                     │
│                                                        │
│ ↑↓ select · Space toggle · Esc close                   │
╰────────────────────────────────────────────────────────╯
```

| Key | Effect |
|-----|--------|
| `↑` / `↓` | Move selection |
| `Space` | Toggle the selected tool on/off |
| `Esc` | Close |

## History search modal

Two-mode input-history browser, opened with `Ctrl+R`. It opens in **browse**
mode and drops into a **search** sub-layer on `/`. `Enter` always inserts the
selected entry into the composer for editing — it never sends.

```text
╭──────────────────────────────────────────────────╮   browse mode
│ Input History  · / to search                     │   (no query field)
│                                                  │
│   1  /repeat @hourly check for failing tests      │   newest first
│   2  how do I open the file?                     │
│   3  explain this function ↵                      │   ↵ = multi-line entry
│                                                  │
│ ↑↓ navigate · / search · Tab preview · Enter insert · Esc│
╰──────────────────────────────────────────────────╯
```

Pressing `/` borrows the composer line as a live fuzzy query (the composer
draft is stashed and restored on close):

```text
╭──────────────────────────────────────────────────╮   search mode
│ Input History  ❯ open                            │   ← caret here
│                                                  │
│   1  h̲o̲w̲ do I open the file?                    │   best score first
│   2  explain t̲h̲i̲s̲ function                     │   matched chars branded
│                                                  │
│ type filter · ↑↓ navigate · Tab preview · Enter insert · Esc back│
╰──────────────────────────────────────────────────╯
```

The single source of truth for the rows is `App::history_rows()` — recomputed
each call, so the cursor, the list, and `Enter`-insert all index into the same
vector. In browse mode (or in search mode before any query) the list is
reverse-chronological — newest first. Once a query is present in search mode
the rows are the fuzzy-ranked matches, best score first, with input order as
the stable tiebreaker.

The dropdown is an **extension of the composer**, not an independent window:
it floats anchored to the top edge of the input box and grows upward. It shares
the composer's surface language — a solid panel fill with full panel-bg padding
rows on the top and bottom edges, so it reads as continuous with the input box
rather than a separately-bordered floating window (no left accent bar, no
half-block glyphs). Its height
tracks the actual row count (capped at ten entries, beyond which the body
scrolls) — a short list collapses to just its rows plus the edge and
header/footer rows, instead of reserving a fixed minimum. The activity bar
sits directly above the composer and is always treated as above the dropdown:
the panel reserves the activity bar's rows as a ceiling and never paints over
them, so the live status surface stays visible even while browsing history.

| Key | Effect |
|-----|--------|
| `/` (browse) | Enter search mode (borrow the composer line as the query) |
| `↑` / `↓` | Move selection |
| `Tab` | Toggle a full-text **preview** of the selected entry (selectable text: drag + `Ctrl+Shift+C` copies the prompt) |
| `Enter` | Insert the focused entry into the composer (browse or search) |
| `Ctrl+X` | **Clear the entire history** — arms a confirmation (`y` wipes, any other key cancels) |
| `Esc` (search) | Leave search → back to browse |
| `Esc` (browse) | Close the modal |

The list is the **prompt text itself** — there is no origin status strip
(`~/project · #session… · time`) under the selected row anymore; the
workspace/session stamp is still stored on each entry (it drives the inline
↑/↓ per-session recall) but is no longer displayed, since the row numbers and
text already anchor selection.

By default the history is **deduplicated on the prompt text** (`[input_history]
dedup = true`): sending the same prompt twice — even in different sessions —
keeps one row, and re-sending bumps it to the top of the newest-first list.
Set `dedup = false` to keep per-session entries instead. `/command`
invocations (`/model`, `/new`, …) are **not recorded** by default
(`[input_history] record_commands = false`): they are UI gestures, already
visible in the transcript, and only clutter the prompt picker. Set it to
`true` to make commands recallable from `Ctrl+R` again.

Characters whose char-index is in `FuzzyMatch.positions` are styled
differently (brand + bold when unselected, contrast + underlined when
selected) so the user sees why each entry surfaced. The modal is
click-outside-to-dismissable: clicking outside the panel closes it and
restores the stashed draft, exactly like a second `Esc`.

## Question modal

Centered modal for `UserQuestionRequest`. Presents one question at a time
with options (single- or multi-select), plus a built-in **Other** option
that exposes a free-text input.

Unlike other centered modals, the question modal uses the **None** recess
policy — the surface is not dimmed or occluded and the footer is not
collapsed, so the transcript, activity bar, input box, and hint bar all stay
fully visible at full brightness. The modal panel simply floats on top with
its own solid background.

Long text wraps automatically: the question text, option labels, and option
descriptions all word-wrap to fit the modal body width.

```text
╭────────────────────────────────────────────────────────╮
│ Question 1/2                                           │
│                                                        │
│  Which framework?                                      │
│                                                        │
│   1.  React                                            │  ← highlighted (single-select: no marker — the highlight is the selection)
│       component-based                                  │  ← description (dim)
│                                                        │
│   2.  Vue                                              │
│       progressive                                      │
│                                                        │
│   3.  Other                                            │
│                                                        │
│ ↑↓ navigate · 1-9 jump · Enter next · Esc cancel       │
╰────────────────────────────────────────────────────────╯
```

`[x]` / `[ ]` mark **multi-select** options — selection there is a separate
toggle set from the highlight, so the checkbox is the only way to tell a
*selected* row from a merely *hovered* one. **Single-select** shows **no
marker**: it is *live* — the highlighted row *is* the selection, so moving
`↑`/`↓` or jumping with a digit immediately commits the choice and `Enter`
advances (or submits from the final page) with exactly what is highlighted.
There is no "Space to confirm" step. The leading
digit prefix (`1.`–`8.`) advertises the 1-9 jump shortcut. Each option's
description (when present) is rendered on its own indented line in the dim
foreground color.

| Key | Effect |
|-----|--------|
| `↑` / `↓` | Move highlight (last row is always **Other**) |
| `1`–`9` | Jump to the Nth option |
| `Space` | Toggle the highlighted option *(multi-select only; no-op for single-select)* |
| `Enter` | Advance to the next question; submit all answers from the final page |
| `Shift+Tab` | Return to the previous question, preserving per-page state |
| `Esc` | Settle the parked request as cancelled and close the modal |

See [User questions](../../explanation/agent-design/user-questions.md) for
how the agent side blocks on the answer.

## Permission sheet

Blocking tool-permission prompt rendered **inline**, replacing the composer
(input-box) area; the transcript above stays visible. It is the only modal
without a backdrop or centered rect. Collapsed by default; expanding
**Details** grows the body upward into the transcript, up to
`PERMISSION_MAX_BODY_ROWS = 14`.

```text
… transcript (visible, scrollable above) …

┃ Run shell command  src/main.rs
┃                                    ← collapsed header
┃ Allow once   Always allow   Reject   Details
┃  ←→ select · Enter · Esc reject     ← footer band (theme.raised())
```

Expanded variant:

```text
┃ Run shell command  src/main.rs
┃
┃ Execute a shell command and return stdout/stderr.
┃
┃ Arguments
┃ {
┃   "cmd": "cargo test"
┃ }
┃ Allow once   Always allow   Reject   Hide
┃  ←→ select · Enter · Esc reject · ↑↓ scroll details
```

A follow-up **always allow until exit?** confirmation flips the action set
to `Confirm always · Cancel`.

| Key | Effect |
|-----|--------|
| `←` / `→` | Move between action buttons |
| `Enter` | Activate the highlighted action |
| `Esc` | Reject (or cancel the confirm-always step) |
| `↑` / `↓` | Scroll the details body (expanded only) |

The sheet uses a warn-colored left bar (`panel_block(theme.warn(), …)`) as
its severity cue, and `theme.raised()` for the footer band.

## Help modal

Keybindings cheat sheet. The narrowest centered modal: 58 × 70.

Opens via `Ctrl+H`, `?` (top level, empty input), `F1`, or `/help`. `Ctrl+H`
is the legacy shortcut but is **terminal-dependent**: it is byte-identical
to Backspace (`0x08`), so it only opens help when the Kitty enhanced-keyboard
protocol (`DISAMBIGUATE_ESCAPE_CODES`) is active. Multiplexers that don't
forward Kitty flags — notably tmux, which strips the protocol on most
shipping versions — collapse `Ctrl+H` and `Ctrl+Backspace` onto the same
byte, so both keys open help there rather than `Ctrl+Backspace` deleting a
word (use `Alt+Backspace` to delete a word inside tmux). `?` and `F1` have
no such collision and work everywhere; prefer them inside tmux/screen. For
the full key-collision table and tmux configuration that restores the
distinction, see [Terminal UI § Key collisions under tmux /
screen](../../explanation/tui.md#key-collisions-under-tmux--screen).

```text
╭──────────────────────────────────────╮
│ Help                                 │
│                                      │
│ General                              │  ← section header (fg bold)
│ enter     send message               │
│ …                                    │
│                                      │
│ Transcript focus                     │
│ ctrl+↑/↓   focus a step              │
│ ↑↓         cycle steps               │
│ enter      open the focused step     │
│ esc        clear the focus           │
│ …                                    │
│                                      │
│ esc · close                          │
╰──────────────────────────────────────╯
```

Sections: **General**, **Line editing**, **Transcript focus**, **Views &
tools**, **Modes**. Closes with a one-line note: `Drag to select · Ctrl+C or
Ctrl+Shift+C to copy.`

The body is a selectable document: dragging over the keycap rows and
descriptions selects them, and `Ctrl+Shift+C` copies — the same interaction
as transcript text (see [Selecting modal
text](#selecting-modal-text)).

## Activity modal

Tabbed overview of the current round, opened by clicking the activity bar.
Two tabs cycled with `←`/`→`:

| Tab | Contents |
|-----|----------|
| **Activity** | The current round's user prompt (wrapped) and the live status block: `round N · turn M · <model> · <elapsed>` + activity label + optional review alert |
| **Tasks** | The unified todo list: `done/total` header plus one row per item with a status glyph |

| Key | Effect |
|-----|--------|
| `←` / `→` | Cycle tabs |
| `↑` / `↓` | Scroll the active tab's body |
| `Esc` | Close |

## Usage statistics modal

The durable cross-session view (`/usage`, ADR-0122). Unlike every other
modal its data is **session-independent**: it aggregates the day-partitioned
store at `data/usage/daily/` (a sibling of `projects/`), so deleting
sessions or pruning project buckets never removes history — the numbers
reflect each day's real consumption. Fetched on demand
(`AgentRequest::QueryUsageStats`); a loading placeholder shows until the
reply lands.

One scrolling body with three sections:

| Section | Contents |
|---------|----------|
| **Summary** | Range, total tokens, input/output split, cache read/write, estimated share, request count |
| **Daily tokens** | A two-week bar chart (newest at the right, `·` for empty days) plus one row per local day: total, in/out, request count |
| **By model** | One row per `(provider, model)` across all days, sorted by descending total, with request counts |
| **Recent requests** | The newest terminal attempts (newest last): local time, lifecycle state (colored), model, tokens (`~` marks estimated) |

| Key | Effect |
|-----|--------|
| `↑` / `↓` / `PgUp` / `PgDn` | Scroll the body |
| `Esc` / outside click | Close |

Interrupted, failed, and abandoned attempts are included and marked, so the
daily totals are honest about what was actually requested.

The body is a selectable document: any run of rows (a KV pair, a table row,
a chart label) can be dragged over and copied with `Ctrl+Shift+C` (see
[Selecting modal text](#selecting-modal-text)).

## Selecting modal text

Documentary modal bodies render through `components/selectable_body.rs` and
register every visual row as a selection region under the modal-document
sentinel. Migrated surfaces:

| Surface | What becomes copyable |
|---------|----------------------|
| Help (`?`) | The whole cheat sheet — keycap labels and descriptions |
| Usage Statistics (`/usage`) | Summary KV, daily/model tables, event log |
| Context Usage (`/usage` → round drill-in) | The round's KV read-out, turns table, legend |
| Activity modal (Activity / Todos) | Prompt, status detail, last failure, todo items |
| Sessions `i` info sub-view | Session id, title, timestamps, full last prompt |
| History `Tab` preview | The full prompt text of the focused entry |
| Permission sheet body | Tool description and the arguments JSON |
| OAuth pending sheet | Instructions, URL, verification code |
| In-modal `?` keymap sub-pages | Key labels and descriptions (every modal that has one) |

The consequence for the user:

- Dragging across rows inside the panel selects the text (highlight follows
  the pointer; wrapped lines select per visual row).
- `Ctrl+Shift+C` copies the selection, exactly like on the transcript.
- A press on the text never dismisses the modal or triggers a button;
  outside-click dismiss and all click affordances keep working unchanged
  (a press on chrome or blank areas still behaves as before).

Picker-style modal bodies (Models, Connections, Tools, MCP, Sessions list,
Asides, Queue, Skills) are deliberately *not* draggable: their rows are
interactive targets (Enter / Space / `e` shortcuts), and drag-select there
would fight the click affordances. The distinction is content-vs-control,
not text-vs-text. (Their `?` keymap sub-pages *are* selectable — a sub-page
is documentation even when its parent is a picker.) Two adjacencies worth
naming: the Skills modal's *expanded* detail block is documentary but lives
inside the picker's row stream — making just those rows selectable would
split one scroll surface across two interaction models, so it stays
non-draggable (the description is re-readable in `/skills` output); and the
Question modal is a pure decision surface (options + free-text field), so
its body is control, not document.

Declarations (indents, todo status glyphs) are painted as row prefixes that
stay out of copied text, and regions are anchored per *visual* row after
application-layer wrapping, so wrapped continuation lines and scrolled
views select and copy the visible text.

## Toasts

Transient top-right notifications rendered above all other chrome. Both
use a 3-row panel via the shared toast component, positioned at
`x = term_w − toast_w − 2, y = 1, w = min(text, 58) + 2`, with thick
left+right borders colored by variant.

```text
                                    ┃ Esc again interrupts ┃
                                    ┃                      ┃
                                    ┃                      ┃
```

| Toast | Border color | Trigger |
|-------|--------------|---------|
| `draw_armed_toast` | `theme.warn()` | An armed action awaits a second keypress (`Ctrl+C` to exit, `Esc` to interrupt) |
| `draw_copy_toast` (success) | `theme.ok()` | Clipboard write completed |
| `draw_copy_toast` (failure) | `theme.err()` | Clipboard write failed |

## Source

Modal-specific renderers live in `crates/neenee-tui/src/overlays/`
(one renderer file per modal: `provider`, `permission`, `history`, `help`,
`session`, `tools`, `permissions_manager`, `activity`, `btw`,
`tool_step_detail`, `toast`, plus feature-specific helpers). Shared composed pieces live in
`crates/neenee-tui/src/components/`: `modal`, `list`, `scroll`,
`footer`, `toast`, and `options` cover the common modal shell, selectable list
body, scroll body, footer hints, notification bubble, and question option
rows. Low-level primitives (`recess_backdrop`, `centered_rect`,
`modal_frame`, `panel_block`, raw `render_body`) remain in
`crates/neenee-tui/src/primitives.rs`. The chrome-hiding flag is
read by `draw_transcript` in `crates/neenee-tui/src/view.rs`.
