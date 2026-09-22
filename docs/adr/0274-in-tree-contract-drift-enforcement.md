# 0274. Contract-drift enforcement lives in-tree: the retirement of the shell-script gates

- **Status:** Accepted
- **Date:** 2026-09-21
- **Implementation:** `.github/workflows/ci.yml`, `crates/muta-contracts/tests/it/protocol_mirror.rs`
- **Amends:** [ADR-0134](0134-wire-protocol-negotiation.md) (protocol-mirror + bump enforcement), [ADR-0200](0200-owned-transport-and-packet-level-request-trace.md) (tap-fidelity gate), [ADR-0204](0204-egress-confinement-ssrf-defense-and-http2-boundary.md) (`[INV-EGRESS-01]` enforcement), [ADR-0210](0210-extract-netune-transport-library.md) (egress-gate reference)

---

## Context and Problem Statement

For most of the project's life a set of ad-hoc shell scripts under `scripts/` carried the workspace's cross-cutting CI gates:

- `scripts/check-wire-compat.sh` — protocol-mirror agreement and bump-or-`wire-compatible`-label (ADR-0134)
- `scripts/generate-wire.sh` — regenerate `apps/web/src/lib/generated/wire.gen.ts` and `--check` it for drift
- `scripts/check-egress-deps.sh` — assert no production dependency reaches `reqwest` (ADR-0200/0204/0210)
- `scripts/check-tap-fidelity.sh` — a privileged, skip-loudly `tcpdump` cross-check (ADR-0200)
- `scripts/check-governance.sh`, `scripts/check-adr-governance.sh` — documentation/ADR governance (advisory)

These were retired ("Retire ad-hoc `scripts/` governance and wire-compat checks", commit `c3020726`), but the retirement was incomplete. `.github/workflows/ci.yml` still invoked four of them; `apps/web/src/lib/types.ts`, `apps/web/src/lib/stores/daemon.svelte.ts`, and `docs/dev/release.md` still named them as the live enforcers; and several Accepted ADRs cite them as their binding enforcement mechanism. The result was **silent gate loss**: a wire-surface change that forgot to regenerate `wire.gen.ts`, or a new `reqwest` dependency in the normal graph, no longer failed CI.

The problem was not bash — the project uses shell elsewhere (`apps/web/e2e/cli-smoke.sh`) — but the **ad-hoc, out-of-band** placement of gates that a reader could not verify from the build itself.

## Decision Drivers

- **One source of truth per invariant.** A gate is defined once, where the build already runs, not duplicated into a script the workflow calls by convention.
- **No silent gate loss.** A retired gate is either replaced by an equivalent in-tree check, or explicitly recorded as a review-time policy — never left dangling.
- **No bespoke, unverifiable shell.** A gate expressible as a `cargo test` is one; the few that genuinely need git history or the resolved graph stay as short, readable workflow steps.
- **Immutability of Accepted records.** The Accepted ADRs citing the deleted scripts are not edited; this record amends their enforcement clauses.

## Decision

1. **Wire-mirror freshness** is a `web`-job step: regenerate `wire.gen.ts` via the `export_bindings` tests (single-threaded) and `git diff --exit-code` the committed mirror.
2. **Protocol-number mirror agreement** (ADR-0134) is a durable in-tree test — `muta-contracts::tests::it::protocol_mirror` — that parses the web client's `PROTOCOL_VERSION` and asserts it equals the Rust constant.
3. **Zero-egress** (`[INV-EGRESS-01]`) is a `lockfile`-job step: `cargo tree --workspace --edges normal -i reqwest` must find nothing.
4. **Release-manifest consistency** (workspace version ↔ `apps/web/package.json`) is a `lockfile`-job step.
5. **The protocol bump-or-`wire-compatible`-label discipline** (ADR-0134) stays a **review-time gate** (documented in `docs/dev/release.md`): it is a base-diff policy, not a state invariant, so it is not mechanically re-implemented.
6. **The tap-fidelity cross-check** (ADR-0200) is retired as a gate; it was privileged and skip-loudly, and never gated merges.
7. **The documentation/ADR governance shell checks** were advisory (`|| true`) and are retired; governance remains a review process.

## Consequences

### Positive

- Every gate is defined once, in the workflow or test suite that already runs; no `scripts/` indirection to trace.
- The wire-drift and zero-egress gates are mechanically enforced again, and visible in the build.
- Accepted ADRs' enforcement clauses are reconciled by reference, without mutating immutable records.

### Negative / Neutral

- The protocol bump policy is now a review obligation rather than a mechanical failure: a forgotten bump is caught by a reviewer, not CI.
- Docs that named the deleted scripts are updated to name the current mechanism.
