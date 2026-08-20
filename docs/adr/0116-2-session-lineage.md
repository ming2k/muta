# 0116-2. Session lineage: forks and asides are first-class, dashboard groups by trunk

> Sequel to [ADR-0103](0103-btw-background-asides.md). Numbered to sit beside
> ADR-0116 (native tokenizer) in the same batch; the canonical id is
> `0116-2-session-lineage`.

- **Status:** Accepted
- **Date:** 2026-08-19

## Context

Two fork primitives already existed — `SessionStore::fork` (explicit `/fork`,
repoints the active pointer) and `fork_to_side` (`/btw`, primary untouched) —
and both stamped `parent_id` on the child snapshot. But the lineage was
**write-only**: `SessionOverview`, `MonitoredSession`, and every dashboard
surface dropped it, so a conversation and its three asides rendered as four
unrelated cards. There was also no way to tell an aside from an explicit fork
after the fact — `parent_id` alone cannot distinguish them.

Independently, the aside **view** inherited the primary's chrome: `App` carried
one global activity bar / round counter / elapsed timer, so entering `/btw`
mid-round showed the primary's streaming state in the aside's page, and a
background aside's activity was invisible everywhere until re-entered.

## Decision

### 1. `SessionForkKind` is persisted, not inferred

`SessionData` gains `fork_kind: Trunk | Fork | Aside` (`#[serde(default)]`:
legacy snapshots load as `Trunk`). `fork()` stamps `Fork`;
`fork_to_side()` stamps `Aside`. A legacy file with a `parent_id` but no kind
degrades to `Fork` in `summary_header` — the relationship survives, only the
flavor is unknown. The kind is stamped **at fork time** because that is the
only moment the caller's intent is known; downstream consumers never guess.

### 2. Lineage flows to every observer

`SessionSummary` → `SessionOverview` → `MonitoredSession` all carry
`parent_id` + `fork_kind`. The monitor tracker folds them from
`SessionsOverview` (the authoritative read of the persisted snapshot), so a
dashboard row is badged from the same source the session picker sees.

### 3. Dashboard: one trunk per conversation, branches badged

A trunk card stays plain — **the main line is exactly one and needs no
badge**. A branch is badged `⑂aside` / `⑂fork` and its name renders muted,
so the dock reads as "conversations with derived branches" rather than N
independent sessions. Grouping is by lineage, not layout: the card grid is
unchanged, the badge is the grouping signal.

### 4. Chrome is view-scoped (`SessionChrome`)

`App::session_chrome: HashMap<session_id, SessionChrome>` records activity /
responding / round / turn per session (primary **and** every aside); the
listener maintains entries for all of them, gated exactly as before on the
*displayed* fields. `enter_side_view` snapshots the primary's chrome into
`saved_primary_chrome` (**only when none is parked** — jumping aside → aside
must not re-snapshot the previous aside's state as if it were the primary's)
and swaps in the aside's own entry; `exit_side_view` restores the primary's
bit-for-bit. Renderers read `App::viewed_chrome()` only: the displayed
activity bar, elapsed timer, round/turn counters, and the redraw-animation
gate all belong to whichever session the user is viewing.

## Alternatives considered

- **Infer the kind from which file is the active pointer.** Rejected: breaks
  the moment the user `/fork`s then opens the parent — lineage provenance must
  be a fact recorded at birth, not a function of mutable present state.
- **Nest branch cards under their trunk physically (tree layout).** Deferred:
  the badge already carries the relationship; a nested layout constrains the
  card grid and buys little at current branch counts. Revisit if a
  conversation routinely carries 5+ asides.
- **Separate aside-list in the dashboard.** Rejected: the asides modal (F5)
  already exists in-session; duplicating it in the dashboard splits one
  concept across two surfaces.

## Consequences

- `/btw` views finally have their own activity bar: a new aside starts idle,
  a re-entered running aside shows its own stream state, and the primary's
  bar survives the detour unchanged.
- The dashboard can answer "which conversations are trunks, which cards are
  derived" — and a user seeing `⑂aside` on a card knows its transcript is a
  window onto another conversation, not a new one.
- Dashboards built against the wire (`apps/web`) get the fields via the
  generated TS types (`parent_id`, `fork_kind`) with no protocol bump.
- `fork_to_side` semantics are now visible and documented by test: an aside's
  parent is the session the *store's pointer* was on at fork time — the
  conversation the user was looking at.

## References

- [ADR-0017](0017-side-conversations.md) — side sessions and the parent link
- [ADR-0103](0103-btw-background-asides.md) — background asides; this ADR's
  direct predecessor
- [ADR-0093](0093-daemon-dashboard.md) — the monitor wire the lineage rides
