# 0155. First-class `TrustChanged` notice kind

- **Status:** Accepted
- **Date:** 2026-08-28
- **Revises:** ADR-0107 §3 (the notice-vocabulary clause)

## Context

ADR-0107 §3 surfaced trust-gate shadow warnings with
`NoticeKind::ReviewAlert` — at the time the closed enum's only "needs
attention" value — and recorded a dedicated variant as a deliberate
non-goal. Since then every trust-flavored emitter borrowed that value:

- attach-time workspace-configuration quarantine (`serve.rs`,
  `workspace_trust_notice`),
- startup project-asset quarantine and the per-domain shadow warnings
  (`bootstrap.rs`: project skill shadow, project command shadow, quarantined
  assets).

The TUI now derives each notice entry's *topic head* (the `▲ trust` label,
ADR-0114-adjacent `NoticeParts` split) from `kind`. With that mapping in
place, `ReviewAlert → "trust"` is a frontend convention resting on the
coincidence that no genuine review-subsystem alert exists yet: the first
real `ReviewAlert` would be mislabeled `trust`, or force the frontend back
to title string-matching — the anti-pattern the structured split removed.

## Decision

`NoticeKind::TrustChanged` is a first-class contract value (wire:
`trust_changed`, ts_rs-exported to the web bindings) for workspace-trust
notices: previously trusted project-authored content changed on disk, is
quarantined pending review, or a project entry shadows a user-scope entry.

`AgentNotice::trust_changed(title)` stamps the kind uniformly — `Warning`
severity, `Harness` source — mirroring `command_ack`; callers add the
surface (banner vs inline) and detail body. All four trust emitters switched
to it. `ReviewAlert` returns to its literal meaning for review-subsystem
alerts, and frontend topic maps become exact:
`TrustChanged → "trust"`, `ReviewAlert → "review"`.

## Alternatives considered

- **Keep borrowing `ReviewAlert` (ADR-0107 status quo).** Rejected: with
  kind-derived topic heads, a borrowed kind is a lying signal; correctness
  depends on "no real review alert exists".
- **Rename `ReviewAlert` → `TrustReview` instead of adding a variant.**
  Rejected: churns the wire vocabulary to describe one domain while leaving
  no value for review alerts; the additive variant is cheaper and keeps
  git history readable.

## Consequences

- The wire/TS enum gains one value; both ends ship together in this
  monorepo, so there is no compatibility window.
- `docs/reference/server.asyncapi.yaml` updated (also catching up the
  stale, missing `command_ack`).
- Frontends can branch on `kind == TrustChanged` exactly — filtering,
  re-surfacing quarantine banners on reconnect — without title
  string-matching.

## References

- ADR-0107 §3 — the deliberate non-goal being revised (its trust model as a
  whole evolved through ADR-0140 → 0145 → 0147).
- ADR-0147 — current orthogonal workspace security planes.
