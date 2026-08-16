# 0103. `/btw` as background turn-level asides: multi-slot registry, Ctrl-C exit, two-row header

- **Status:** Accepted (§3 header-band semantics refined by [ADR-0104](0104-demand-driven-hints.md))
- **Date:** 2026-08-15

## Context

ADR-0017 shipped `/btw` as a codex-style **view**: one live side session at a
time, entered via `SideViewOpened` and left via `Esc`/`Ctrl+C` (both mapped to
`AgentRequest::ExitSideView`), which **tears the side session down** (cancel
its round, drop its `Agent`). Three properties of that design stopped matching
how a side conversation is actually used:

1. **Exit means destroy.** Leaving the side view cancels the side round and
   drops the side `Agent`, even though the side file persists. The user cannot
   "fire a question into the background and come back later" — the only way to
   keep a side round running is to sit inside its view.
2. **`Esc` is overloaded.** `Esc` both *leaves a view* and *interrupts the
   running round* (double-Esc interrupt). Inside the side view `Esc` exits
   instead of interrupting, so the key means a different thing per view, and
   `Ctrl+C` is a third, view-dependent thing (exit inside `/btw`, clear/quit in
   the main view). Every daily-driver TUI (shell, editor, REPL) uses **Ctrl-C
   to leave a nested surface** and **Esc/`^C` to interrupt**; users already
   carry that muscle memory.
3. **The side view is a dead end.** One slot (`Option<SideSession>`), one
   buffer (`side_messages`), no list, no way back except `/sessions`. The
   header row mixes *page identity* (title, parent status) with *page
   shortcuts* (`Esc back`), so there is no consistent place for view-level
   affordances, and the main view shows nothing about live asides.

Additionally, the side transcript buffer started empty on every entry
(`SideViewOpened` cleared it), so the inherited context was invisible — the
user saw a blank page even though the model saw the full parent history — and
re-entering a side later showed a blank transcript again.

## Decision

### 1. Asides are turn-level *background conversations*, not a modal view

A `/btw` aside keeps running when the user leaves its view. Leaving a view
detaches; it never cancels. The aside's round, its `Agent`, and its session
file all stay live (persisting, as before, to `sessions/<id>.json` +
`.jsonl`). Concretely the runtime registry becomes a **map**:

```rust
pub side: Arc<AsyncRwLock<HashMap<String, SideSession>>>,  // keyed by side session id
pub side_order: Arc<AsyncRwLock<Vec<String>>>,             // MRU order for listing
pub active_side: Arc<AsyncRwLock<Option<String>>>,         // which aside the composer targets
```

replacing `Option<SideSession>` + `active_view_side: AtomicBool`. A round
started in an aside is owned by that aside's own `RoundLifecycle` and finishes
regardless of view focus; per-turn events keep routing by `session_id`
(`AgentResponse::Round`), so a background aside streams into its own buffer
while the user works in the main view.

### 2. Key bindings

- **Ctrl-C** is the single *leave-the-view* key: inside an aside view it
  detaches back to the main view (the aside keeps running). In the main view
  its existing copy/clear/quit chain is unchanged.
- **Esc** is the single *interrupt* key everywhere. Inside an aside view, Esc
  (armed twice, exactly like the main view) interrupts **that aside's** round
  (`AgentRequest::InterruptSide { side_id }`); in the main view it interrupts
  the primary as today. Interrupting an aside does **not** close it.
- **F5** (new global binding, `Gate::NoModal`) opens the asides list modal
  from any view; it is also the modal shortcut surfaced on the header rows.
  `Ctrl+G` would collide with readline's abort-to-start-of-line in terminals
  without the Kitty protocol, so a function key is used.

### 3. Two-row header

The page header grows a second row, and its content rule is fixed:

- **Row 1 — identity & status (always).** Existing content: title, tag,
  workspace / parent status, right-aligned persistent flags (`autopilot`) or
  index metadata. View-level shortcuts move **off** this row.
- **Row 2 — view-level affordances (always present).** A keycap legend for
  the *current view*: main view shows `F5 asides` (only when at least one
  aside is live) plus `Esc interrupt`; the aside view shows `Ctrl-C back`,
  `F5 asides`, and `Esc interrupt aside`. This is the Envoy footer pattern
  ([`draw_envoy_footer`]) collapsed onto the top band, so all three page
  types share one geometry (`PAGE_HEADER_ROWS = 2`).

The main view's row-2 also carries the **live aside count**
(`btw 2 · 1 running`) — view-level state belongs on the view-level band,
which is also why the jump affordance lives there.

### 4. `/btw` command grammar

- `/btw` — open a **new** aside view (no round started).
- `/btw <text>` — open a new aside and immediately send `<text>` as its
  first turn.
- `/btw list` — open the asides modal (same as F5).
- An aside that is opened and left **without ever starting a round** is
  discarded outright (registry entry dropped **and its session files
  deleted) — it never appears in the list or `/sessions`. The discard happens
  at detach time, so no empty-file litter accumulates.

### 5. Asides modal

A centered list modal (`Modal::Btw`) fed by a new response:

```rust
BtwList(Vec<BtwAsideSummary>)   // id, title, running, parent status, updated_at
```

rows ordered newest-first, with `Enter` = jump (send
`AgentRequest::FocusSide { side_id }` → `SideViewOpened` with the transcript
back-fill) and `D` = detach-and-discard (explicit user delete). The modal is
refreshable in place (`/btw list` while open) and its count is the same data
the header row 2 shows.

### 6. Transcript back-fill on (re)entry

`SideViewOpened` carries `messages: Vec<Message>` + `commands` (the aside's
full persisted transcript at open time) exactly like
`AgentResponse::ConversationReplaced`, and the TUI rebuilds the side buffer
from it instead of clearing it. Two effects: the aside view finally *shows*
the inherited parent context ("keeps the complete context, especially the
previous full turn"), and re-entering an aside shows its history. The model's
context was always the forked window; now the pixels match.

Entering/leaving views never mutates the buffers' content beyond this
one-shot back-fill.

### 7. Persistence (answering "does `/btw` need persistence?")

**Yes — and it already had it; this ADR keeps it, with one carve-out.** An
aside is a forked session file written eagerly from birth (`defer_persist:
false`), so a crash or restart loses nothing, and the aside remains
recoverable from disk. The carve-out is the discard rule (§4): an aside with
no round ever started has no user content, so detach deletes it. There is no
separate `/btw` store — reusing the session persistence keeps one durability
story.

## Alternatives considered

- **Keep the single-slot `Option<SideSession>`.** Rejected: the brief
  explicitly wants a list, jumping, and "let it run in the background";
  multiple concurrent asides are inherent to that. The lift from `Option` to
  `HashMap` is small because routing, persistence, and events were already
  keyed by `session_id`.
- **`Esc` leaves the view, Ctrl-C interrupts.** Rejected: inverts daily-driver
  muscle memory (Ctrl-C is the "get me out" key in shells/REPLs/editors) and
  wastes Esc's established armed-interrupt semantics.
- **Keep exit-means-cancel and add a separate "background" flag.** Rejected:
  two exit paths with different semantics is exactly the ambiguity the
  redesign removes. Detach (Ctrl-C) is always non-destructive; explicit
  destruction is a deliberate act in the modal (`D`) or the empty-discard
  rule.
- **Put view shortcuts on the footer (Envoy-style).** Rejected for the shared
  case: the bottom is already owned by composer + hint bar + queue bar, and
  the brief asks for the header to be the view-granularity surface. Envoy
  keeps its three-row footer (it is a read-only page with no composer).
- **A dedicated `/btw` store keyed by parent.** Rejected: the forked session
  file already carries `parent_id` lineage; adding a second persistence path
  would duplicate compaction, blob, and event-log machinery for no gain.

## Consequences

Positive:

- Leaving a view is always safe; background asides are first-class.
- One key for one verb everywhere (Ctrl-C = leave view, Esc = interrupt,
  F5 = asides list).
- The header has a fixed contract: row 1 identity/status, row 2
  view-level affordances — extensible to future pages.
- The aside transcript shows the real inherited context and survives
  re-entry.
- No empty-session litter from abandoned `/btw` opens.

Negative / costs:

- The protocol gains variants (`FocusSide`, `InterruptSide`, `CloseSide`,
  `BtwList`, `SideViewOpened` payload) — an internal wire change, shipped
  atomically with the TUI.
- One extra header row costs a transcript row on every page; mitigated by the
  main view only showing row 2's aside segment when asides exist.
- The registry is more state to keep coherent (map + MRU order + active
  pointer); the old `AtomicBool` routing flag disappears in favour of the
  active id.

Migration:

- `/btw <text>` behaves as before; bare `/btw` no longer implies "must type
  something now" — an empty aside is discarded on detach instead of erroring
  on a second `/btw`. `Esc`-to-exit is replaced by Ctrl-C; no on-disk format
  changes.

## Implementation map

- Runtime registry & routing: `crates/neenee-runtime/src/side.rs`,
  `session_driver.rs`, `bootstrap.rs`.
- `/btw` grammar: `handlers_slash.rs` (`BuiltinCmd::Btw` arm).
- Detach / discard / interrupt / focus / list handlers:
  `handlers_session.rs` (`detach_side_view`, `interrupt_side`,
  `close_side`), contracts in `neenee-contracts/src/events.rs`.
- TUI: `Modal::Btw` + `overlays/btw.rs`; `App` side-buffer back-fill
  (`SideViewOpened` arm in `lib.rs`); Ctrl-C/Esc re-routing in
  `input/mod.rs`, `event_loop/actions.rs`, `commands.rs`; two-row header in
  `page_header.rs`/`view.rs`/`design.rs`; F5 in `keymap.rs`.
- Empty-aside file cleanup: `SessionStore::delete_session` +
  `SideSession::is_pristine` (`neenee-persistence/src/session/mod.rs`).

## References

- [ADR-0017](0017-side-conversations.md) — the original single-slot side
  conversation design; superseded where this ADR disagrees (exit semantics,
  single slot, empty side buffer).
- Envoy footer precedent: `crates/neenee-tui/src/page_header.rs`
  (`draw_envoy_footer`).
- codex `side.rs` — the reference UX for parent-status surfacing.
