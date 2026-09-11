# 0239. Remove the Help modal and F1 help entry points

- **Status:** Accepted
- **Date:** 2026-09-11
- **Scope:** `mutx` (the chord registry `keymap.rs`, the `DialogKind` surface
  registry `surfaces/mod.rs`, the overlay renderer `overlays/help.rs`, the
  dispatcher `input/router.rs` + `modal_keys.rs`, the footer chip
  `components/footer.rs`, the empty-state carousel `empty_state.rs`)
- **Supersedes:** ADR-0170 (the `F1` / Help item in the command registry),
  ADR-0172 §1 (the `F1` hard-bound global interceptor chord), ADR-0104 (help
  discovery via modal footers, the `? help` chip, and the carousel's `F1`/`?`
  page)
- **Related:** ADR-0205 (surfaces), ADR-0238 (chrome advertises only bindings
  that fire)

## Context

The in-app Help surface was a keybindings cheat sheet reachable from four
entry points: the `F1` hard chord, `Ctrl+H` (Kitty-only), `?` (when no composer
owned the input line), and the `/help` slash command. It was also reachable
from a mandatory `? help` chip appended to any modal footer whose hints had
collapsed, and from an empty-state carousel page.

Four forces make the surface dead weight:

1. **The registry is the discovery surface now.** Since ADR-0170/ADR-0172 every
   command, chord, slash trigger, and description is declared once in
   `keymap::COMMAND_REGISTRY`, and the `Ctrl+P`/`Ctrl+L` Command Palette renders
   it with live availability flags. The Help modal was a second, hand-grouped
   projection of the same table — a drift risk (ADR-0170's whole motivation)
   with no information the palette lacks.
2. **`F1` is not portable.** Terminals and multiplexers routinely intercept or
   drop function keys; the project already trimmed the persistent `F1` pair out
   of chrome (ADR-0104) and had to keep `?` and `/help` as the working
   substitutes. A shortcut that is unreliable where it is advertised is not a
   promise the chrome can keep (ADR-0238).
3. **`Ctrl+H` is worse than unreliable.** It is byte-identical to Backspace
   without the Kitty protocol, so it opened Help only on some terminals and
   deleted a character on others. Its whole reason to exist was the Help modal;
   with the modal gone it is a collision with no payoff.
4. **The `? help` chip promised a surface users no longer need.** A footer that
   collapses its hints should collapse them, not advertise a keymap page whose
   content the palette already carries.

## Decision

**Delete the Help feature end to end. There is no in-app help modal, no `F1`
binding, and no `?`/`Ctrl+H`/`/help` entry point.**

1. **Remove the command.** `CommandId::Help` and its `COMMAND_REGISTRY` entry
   (label, `F1` binding, `/help` slash, L0 footer disclosure) are deleted, along
   with the `command_id_from_name("help")` mapping. `Key::F1` is deleted; `F1`
   resolves to nothing.
2. **Remove the surface.** `DialogKind::Help`, `InputAction::OpenHelp`,
   `App::help_scroll`, `FixedModalSpec::HELP`, `overlays/help.rs`
   (`draw_help_modal`), and every render/scroll/state arm that dispatched on
   them are deleted. The `DialogKind::ALL` switcher list drops to 13 entries.
3. **Remove the entry points.** The dispatcher's `CommandId::Help` and `Ctrl+H`
   arms, the `modal_keys` `?` arm (and its `DialogKind::Help` close/scroll
   cases), and the `Ctrl+H` special case are removed. `?` is now an ordinary
   printable that inserts where text is editable and is inert where it is not;
   `Ctrl+H` is explicitly swallowed so it never inserts a literal `h`.
4. **Remove the discovery apologia.** The modal footer's mandatory `? help`
   chip (`MORE_FULL`, `render_modal_footer_with_more`) is deleted;
   `render_modal_footer_with_more` becomes `render_modal_footer_with_extra`
   (custom-band hints only, never a chip). The empty-state carousel drops its
   `F1`/`?` page and stops pointing at `/help`.
5. **Record the supersession.** The Help-modal portions of ADR-0170, ADR-0172
   §1, and ADR-0104 are superseded; the reference and explanation docs that
   still described the modal, `F1`, `Ctrl+H`, `/help`, or the `? help` chip are
   corrected to the live model.

## Alternatives considered

- **Keep the modal, drop only `F1`.** Rejected: the modal's remaining value is
  its content, and that content is already in the palette. Keeping it sustains
  the drift ADR-0170 was created to prevent.
- **Keep the modal, drop `F1`/`Ctrl+H` and keep `?`/`/help`.** Rejected: same
  as above, plus it keeps the `? help` footer chip and the carousel page alive
  to advertise a redundant surface. Partial removal hides the decision instead
  of making it.
- **Keep `F1` bound to the palette instead of Help.** Rejected: `F1` remains
  unportable, so the binding would still fail where users press it. `Ctrl+P`
  and `Ctrl+L` are the portable chords and already fire (ADR-0172).
- **Keep `L3HelpOnly`/help-only commands as a disclosure tier.** Rejected: no
  command uses it, and the tier described a surface that no longer exists; the
  variant is renamed `L3Reference` rather than left as a dead help reference.
- **Keep `/help` as a CLI-style command that prints the registry.** Rejected:
  the completion popup and `Ctrl+P` palette are richer (live availability,
  fuzzy match, direct dispatch) and require no second rendering path.

## Invariants & Behavioral Boundaries

- **INV-1 — No Help command or binding.** `CommandId::Help`, `DialogKind::Help`,
  `InputAction::OpenHelp`, and `Key::F1` do not exist; `F1` resolves to no
  command and no view advertises it.
- **INV-2 — `?` is never a modal opener.** `?` inserts as text in an editable
  field and is inert elsewhere; it closes nothing and opens nothing.
- **INV-3 — `Ctrl+H` is inert.** It never inserts a literal `h`; where the
  terminal collapses it to Backspace, the Backspace arm owns the byte.
- **INV-4 — Footer collapse advertises no chip.** A collapsed modal footer
  drops/compacts hints and never appends a help affordance.
- **INV-5 — Discovery lives on the palette.** Commands are enumerated by
  `COMMAND_REGISTRY` through the Command Palette, not a second keymap surface.

## Consequences

**Positive.**

- One discovery surface (`Ctrl+P` palette) instead of two projections of the
  registry, eliminating the drift class ADR-0170 named.
- No unreliable `F1` promise and no `Ctrl+H`/Backspace collision to document.
- The footer layout loses the mandatory chip: collapsed hints now simply
  collapse.
- The empty-state carousel teaches only durable capabilities that still exist.

**Negative / neutral.**

- Users who relied on `F1` for a quick chord list must use `Ctrl+P`/`Ctrl+L`.
- `render_modal_footer_with_more` was renamed (`_with_extra`) and its `show_more`
  machinery deleted — a mechanical ripple across the modal call sites.
- `/help` is no longer a valid composer command; typing it falls through to the
  normal unknown-command path. The dashboard console's own `/help` verb
  (ADR-0097) is unrelated and unchanged.
- The empty-state carousel is shorter by one page; its page-0 copy no longer
  demonstrates a slash command by name.

## Verification

- `cargo check -p mutx --all-targets` clean; `cargo nextest run -p mutx` green
  (1173 tests).
- Registry tests: `global_keys_resolve_correctly` no longer expects `F1`;
  `parse_key`/`find_by_slash` no longer know `f1`/`/help`.
- Dispatcher tests: `f1_is_inert` pins the no-op; the former
  `?`-opens-help / `?`-closes-help cases now pin inert/insert behavior.
- Footer tests: `collapsed_strip_never_emits_help_chip` sweeps every width and
  asserts no `?` and no overflow.

## References

- [ADR-0170](0170-composer-first-command-palette-and-action-registry-architecture.md)
  — the registry SSOT; its `F1`/Help item is superseded.
- [ADR-0172](0172-per-surface-keybinding-schemes-and-view-local-keyboard-ownership.md)
  — its `F1` global-interceptor chord is superseded.
- [ADR-0104](0104-demand-driven-hints.md) — its modal-footer/`? help` and
  carousel `F1`/`?` discovery story is superseded.
- [ADR-0205](0205-unified-tui-surface-architecture-and-spatial-modality-taxonomy.md)
  — the surface taxonomy `DialogKind::Help` was removed from.
- [ADR-0238](0238-footer-chrome-advertises-only-bindings-that-fire.md) —
  chrome must not advertise an affordance that does not fire.
- [ADR-0097](0097-session-addressing-and-orchestrator-console.md) — the
  dashboard console's unrelated `/help` verb.
