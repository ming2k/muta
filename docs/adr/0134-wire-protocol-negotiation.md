# 0134. Wire-protocol window negotiation over product-version equality

- **Status:** Accepted
- **Date:** 2026-08-23
- **Builds on:** ADR-0096 (unified session daemon), ADR-0100 (lifecycle
  standard, rule 4 — the version handshake this replaces), ADR-0102
  (unified single binary), ADR-0105 (one port, two protocols)

## Context

Since ADR-0100 rule 4, client/daemon compatibility has been gated on
**product-version exact equality** in two places:

1. **Discovery pre-check** — the discovery record
   (`serve_discovery.rs::Discovery::version`) carries the daemon's
   `CARGO_PKG_VERSION`; `client::versions_compatible()` refused a record
   whose version differed from the client's own (plus the `/proc/<pid>/exe`
   inode check for dev-loop same-version drift).
2. **Handshake** — the first frame's `Wire::Select::version` is compared
   for exact equality in `serve.rs` before any session work; a skew is
   refused with `Error{code: "version_mismatch"}` naming both builds.

This was the right call for a pre-1.0 protocol that "evolves every
release": exact equality is maximally safe, and the single-binary
architecture (ADR-0102) makes the cost of a daemon restart nearly zero
(disk is the authority — ADR-0096; idle exit keeps stale daemons
short-lived). But it has one structural weakness: **it conflates "which
build are you" with "which protocol do you speak."** The `--remote` path
naturally spans machines with different builds, and every release — even
one whose wire changes are purely additive — breaks every client. The
serde discipline already in place (`#[serde(default)]` +
`skip_serializing_if` on every optional field, "absent on older daemons"
documented throughout) buys exactly the compatibility the equality gate
refuses to spend.

The alternative considered and rejected is a bare integer protocol
version with strict equality (`protocol == N`). A hand-bumped number
forgotten is a **silent** corruption bug — the very failure mode version
negotiation exists to prevent — while an over-bump is merely an
inconvenient restart. That asymmetry means the bump decision must be
**mechanically enforced**, not left to memory.

## Decision

1. **The wire protocol gets its own integer version**, defined in
   `crates/neenee-contracts/src/wire.rs` (ADR-0134 also moves the
   envelope there from `neenee-runtime/src/serve.rs`):
   `PROTOCOL_VERSION: u32` (what this build speaks) and
   `MIN_PROTOCOL_VERSION: u32` (the oldest it serves). A plain
   monotonically increasing integer, **not** semver — semver implies a
   compatibility interval a wire format almost never has.

2. **The daemon negotiates a window, not a point.** A client that sends
   `Select{protocol: N}` is served iff
   `MIN_PROTOCOL_VERSION <= N <= PROTOCOL_VERSION`, *regardless of its
   product version*. Outside the window it is refused before any session
   work with `Error{code: "protocol_mismatch"}` and a directional fix
   (too old → update the client; too new → `neenee stop` and let the
   daemon restart on demand).

3. **Absence keeps the old rule.** A client that sends no `protocol`
   predates the field and is judged by ADR-0100 rule 4's exact
   product-version equality on `Select.version` (absent version = served,
   as today). Unknown JSON fields are ignored by serde, so a
   protocol-declaring client against a pre-0.31 daemon still passes that
   daemon's version gate — every combination of old/new client and
   old/new daemon degrades to *some* well-defined check instead of
   undefined behavior.

4. **The discovery record carries `protocol: Option<u32>`** alongside
   `version`, so a local client can refuse an out-of-window daemon in the
   pre-check (`client::incompatibility_error()` prefers the protocol
   message) instead of waiting for the handshake.

   For a **protocol-declaring record**, product-version equality is *no
   longer a local gate* (a revision of the original rule-4 posture):
   version equality on Linux was logically redundant — same inode ⇒ same
   compile ⇒ same version; different inode ⇒ the image check itself
   refuses — and its remaining role was to force a daemon restart on
   every patch bump, interrupting live sessions for no wire-level
   reason. An in-window daemon is served whatever its product build; the
   daemon restarts on idle exit, and anyone who wants the new build
   immediately runs `neenee stop`.

   Exactly one freshness gate survives locally: the **dev-drift lie** —
   same version, *different binary* (`daemon_image_is_current` false).
   That is the one state where every version signal agrees and the client
   is still about to test a stale image — a locally rebuilt binary whose
   protocol number carries no release-process backing (no bump
   discipline, no CI check touched it), so "1 == 1" is an unnotarized
   self-declaration. The inode probe is the only detector for it, and
   ADR-0121's collision detection is preserved through it. Legacy
   records (no `protocol` field) keep ADR-0100 rule 4's exact equality
   unchanged.

5. **Bump discipline** (documented on the constants, enforced by CI):
   - *Additive* changes — new optional fields, new enum variants an older
     peer can never receive — **do not bump**. The serde discipline makes
     them compatible in both directions; this is the answer to "we can't
     ship a version bump for every small change": small compatible
     changes ship without one.
   - Changes an older peer cannot deserialize or would silently
     misinterpret **bump `PROTOCOL_VERSION`**.
   - Dropping support for an old number **raises `MIN_PROTOCOL_VERSION`**.

6. **Mechanical enforcement** — `scripts/check-wire-compat.sh`, run in
   CI's web job:
   - the Rust and web protocol mirrors must agree;
   - a change to the wire surface (`wire.rs`, the ts-rs contract sources,
     or the generated `wire.gen.ts`) without a bump fails the build
     unless the PR carries the `wire-compatible` label — the author's
     explicit, reviewable assertion that the change is purely additive.
   A forgotten bump is now a red CI run, not a silent wire corruption.

7. **The web panel sends `protocol` too.** ts-rs cannot export constants,
   so `daemon.svelte.ts` mirrors `PROTOCOL_VERSION` by hand; the script's
   mirror-agreement check keeps the two from drifting (the same pattern
   CI already uses for the product version). `AttachAction`,
   `ControlRequest`, `SessionOverview`, and `MonitorAction` are now
   ts-rs-generated for the web client, deleting the hand-maintained
   mirrors that had already drifted (the hand-written `AttachAction` was
   missing the `picker` variant).

## Alternatives considered

- **Keep exact product-version equality everywhere.** Rejected: it
  refuses compatible pairs by construction, and `--remote` + the web
  panel make cross-build pairs routine rather than exceptional. It
  remains the rule for protocol-less clients (rule 3) and legacy records
  (rule 4).

- **Bare integer equality (`protocol == N`).** Rejected: strictly weaker
  than the window with the same bump burden, and it forces every client
  to update in lockstep on each bump even when the older protocol is
  still perfectly serviceable.

- **Semver ranges / capability negotiation (per-feature flags).** Deferred:
  the window covers the real cases with one comparison; per-feature
  flags are an optimization to revisit if the protocol stabilizes at 1.0
  and the window becomes the bottleneck (e.g. maintaining two live
  variants).

- **Date-string protocol ids (MCP's `"2024-11-05"` style).** Rejected as
  the primary form: monotonic and self-documenting, but a `u32` window
  comparison is simpler and the daemon already pins its MCP id separately
  (`neenee-mcp`). Nothing here prevents adopting dates later; the window
  logic is type-agnostic in spirit.

## Consequences

- A version-pinned client keeps talking to newer daemons across additive
  wire changes — the actual payoff — and a stale daemon is named as such
  before any session work, with a directional fix in both directions.
- The daemon may serve a client whose product version differs from its
  own; the product version on `Select` is now advisory identity (still
  enforced against pre-protocol daemons). Error messages name protocol
  numbers *and* builds so support triage keeps both facts.
- Two constants must be mirrored in one TypeScript file by hand; CI
  catches drift at PR time (same trade as the product version today).
- CI gains a new failure mode ("bump or label") that requires one line of
  process — applying the `wire-compatible` label — for additive changes.
  That is the deliberate price of making the bump decision a reviewable
  assertion instead of memory.
- The envelope types moving to `neenee-contracts` makes
  `neenee-runtime` re-export them (`serve::Wire` etc. unchanged), so
  existing imports keep working; the module doc on `wire.rs` is now the
  single place the wire contract is explained.

## References

- [ADR-0100](0100-daemon-lifecycle-standard.md) — rule 4, the exact-
  equality handshake this supersedes for protocol-declaring clients
- [ADR-0096](0096-unified-session-daemon.md) — the control plane whose
  wire this governs
- [ADR-0105](0105-one-port-two-protocols.md) — the transport both
  clients ride
- [ADR-0121](0121-instance-isolation-for-development-and-testing.md) —
  collision detection; why the local inode check survives
- `crates/neenee-contracts/src/wire.rs` — the constants, the window
  predicate, the bump discipline
- `scripts/check-wire-compat.sh` — the CI enforcement
