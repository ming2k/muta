# 0066. Dual-write provider/model selection

- **Status:** Accepted
- **Date:** 2026-07-16

## Context

A provider/model switch made inside a session (`/provider` or the add-provider
flow) is a live change to *how the next turn talks to the model*, and a
*preference about which model the user wants on future launches*. ADR-0048
treated provider selection as **session-scoped only**: the switch pinned the
selection to `SessionStore` and persisted only the key/url (reachability) to
`config.toml`, never the `default_provider`/`default_model` selection.

The reasoning in ADR-0048 was multi-session isolation: one session switching
provider must not change what another concurrent session sees. That reasoning
holds for *running* sessions but produced a surprising, wrong default for the
common case. Most users run one session at a time; for them the session-scoped
pin never reaches `config.toml`, so:

- A `/provider` switch followed by a restart **reverted to the startup
  default**, silently discarding the user's explicit choice. The switch felt
  like it did nothing.
- The add-provider flow landed the new provider in `config.toml` but left
  `default_model` pinned to the session, so the freshly-added model was not the
  next launch's default.
- `config.default_model` survived the startup migration but was never written
  by the switch handler, so the field read as write-only-from-config despite
  the migration preserving it as "the persisted selection."

The defect was not the session pin — the pin is what makes resume restore the
exact model a session was using. The defect was the *absence* of the global
write: the switch never became the fresh-session default.

The reachability half (key/url) was already global. `apply_switch_api_key`
extends that discipline: a TUI-entered key is written to the legacy per-builtin
field **and** to every non-OAuth channel of the matching instance, because the
catalog builds providers from instance channels, not from the legacy field. A
key written to the legacy field alone would be dropped at the next startup when
the instance already existed.

## Decision

A provider/model switch is **dual-write**: persist the selection as the global
default in `config.toml`, **and** pin it to the session.

1. **`SwitchProvider` and the add-provider flow call `Config::save`**, not
   `Config::save_preserving_provider_selection`. The global `default_provider`
   and `default_model` become the choice the user just made, so the next launch
   — a fresh session with no pin — lands on it.
2. **The session pin stays.** `SessionStore::set_provider_selection` records
   the provider/model so resume restores it exactly, matching the in-flight
   provider held by the live `ProxyProvider`.
3. **Mutations that are not selection changes keep calling
   `save_preserving_provider_selection`.** Favorites, provider-metadata edits,
   and TUI preferences (layout, color scheme) must not leak the in-memory
   selection — which may carry a resumed session's pin — back into
   `config.toml`. Only the two switches whose *whole point* is to change the
   selection persist it.
4. **`migrate_legacy_provider_instances` does not strip `default_model`.** The
   migration seeds an instance's default channel from `default_model`, then
   stops; the global model pointer survives as the active default the runtime
   honors when the default provider serves it.
5. **A TUI-entered key reaches both surfaces.** `apply_switch_api_key` writes
   the legacy per-builtin credential field (consumed by a later migration) and
   every non-OAuth channel of the live instance (consumed by the catalog).
   OAuth channels are skipped because their bearer is owned by the auth flow.

### The two-session model

| Session | After a `/provider` switch in session A |
|---------|-----------------------------------------|
| A (the switching session) | Live `ProxyProvider` swapped; selection pinned so resume restores it |
| B (another running session) | Unchanged. Keeps its in-memory provider and selection |
| Fresh session (next launch) | Reads the updated `config.toml` default — follows the switch |

Running sessions are isolated by their in-memory state; only a fresh session
reads `config.toml`, so only a fresh session follows the new default. This is
the isolation ADR-0048 wanted, scoped to the lifetime where it actually holds.

## Alternatives considered

- **Keep selection fully session-scoped (ADR-0048 as-is).** Rejected. It is
  correct for concurrent sessions but produces the silent-revert-on-restart
  defect for the common single-session case. The user's explicit switch should
  survive a restart without a second `/provider` step.
- **Make selection global-only and drop the session pin.** Rejected. Resume
  would then always follow the global default even when the resumed session
  was deliberately run on a different model (e.g., a pinned specialist model
  for one task). The pin is what makes a durable session a faithful replay.
- **Write the selection to the session only and read it back at startup.**
  Rejected. It re-invents a second selection store parallel to
  `config.toml`, duplicating precedence rules and migration logic. The global
  default already exists for the startup path; the switch should feed it.
- **Persist the key to the legacy field only.** Rejected. The catalog builds
  providers from `config.providers` instance channels, so a key on the legacy
  field is dropped at the next startup when the instance already exists. Both
  surfaces must stay in sync.

## Consequences

**Positive.**

- A `/provider` switch or add-provider flow survives a restart as the default,
  matching what the user sees in the TUI.
- Resume still restores the exact model a session was using, because the pin
  is preserved alongside the global write.
- `default_model` is a live field with one writer (the switch/add flows) and
  one startup reader, so the migration's decision to preserve it is now
  load-bearing rather than vestigial.
- Concurrent running sessions remain isolated; the global write only affects
  the next fresh session.

**Negative.**

- Two `config.toml` writes now happen on a switch where one did before
  (key/url under `save_preserving_provider_selection` previously; now the full
  selection under `save`). Both are already lock-protected and cross-process
  safe; the extra cost is writing two fields.
- The mental model has two homes for the selection (global default + session
  pin) with explicit precedence. The session pin wins for the pinned session;
  the global default wins for fresh sessions. This is documented above rather
  than hidden.

**Neutral.**

- `save_preserving_provider_selection` is no longer the switch handler's write
  path; it remains the write path for favorites, metadata edits, and TUI
  preferences, where leaking the in-memory selection would be a bug.
- ADR-0048's session-scoped framing for `disabled_tools`, `turn_counter`, and
  pursuit state is untouched. Only the provider-pin line of that ADR is
  narrowed to "pin, plus a global-default write for fresh sessions."

## References

- ADR-0048 — session as the single source of truth; this ADR narrows its
  provider-pin claim from session-only to dual-write.
- ADR-0002 — model channel abstraction and the `default_provider` /
  `default_model` pointer as the persisted selection.
- ADR-0055 — request-lifecycle accounting captures provider/model before
  dispatch; compatible with mid-session switches.
