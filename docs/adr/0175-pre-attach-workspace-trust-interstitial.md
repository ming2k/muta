# ADR-0175: Pre-attach interstitial for first-contact workspace trust

- **Status:** Accepted
- **Date:** 2026-09-04
- **Builds on:** [ADR-0107](0107-trust-gate-covers-project-skills-and-commands.md),
  [ADR-0134](0134-wire-protocol-negotiation.md),
  [ADR-0139](0139-unified-tui-surface-router-and-view-lifecycle.md),
  [ADR-0140](0140-workspace-authority-and-content-bound-extension-trust.md),
  [ADR-0145](0145-decoupled-workspace-asset-trust-and-tool-hazard-model.md),
  [ADR-0173](0173-unbounded-session-keyboard-ownership-claims-and-interaction-sheets.md)

## Context

Since ADR-0107, the TUI has surfaced a never-trusted project workspace
(`WorkspaceTrustState::Quarantined`) by synthesizing a `UserQuestionRequest`
into the pending-question queue
(`apps/tui/crates/mutx/src/trust_gate.rs`), rendered as the standard
`Question` interaction sheet over the chat composer (ADR-0173 §3).

Three problems with that placement became durable:

1. **It mounts after the chat surface.** The gate is fed by `HarnessState`
   snapshots handled inside the listener task at
   `apps/tui/crates/mutx/src/lib.rs` (the `HarnessState` arm that calls
   `trust_gate::gate_request` and pushes into `pending_question`). By the
   time the snapshot arrives, the chat surface has already mounted, the
   composer has already accepted focus, and the user has already started
   reading the transcript. The trust prompt then lands as an
   interruption, not a precondition — the inverse of ADR-0140 §3's
   posture that an unknown workspace discloses its state "before work
   begins."

2. **The dismissal path does not persist.** "Keep quarantined" and `Esc`
   in the reply interceptor only set an in-memory
   `trust_gate_dismissed: AtomicBool` for the running process. Each new
   attach resets it and re-asks. The user perceives a nag instead of a
   decision.

3. **The queue-depth badge lies.** `event_loop/sync.rs` assigns
   `app.pending_question_depth = pending.len()` without subtracting the
   front item the sheet is already showing, so any trust gate (or any
   single-question ask_user) paints `Question · +1 queued` even when
   nothing else is queued.

The deeper architectural mismatch is that an **admission decision** — a
precondition on the entire workspace — was modeled as one more
interactive sheet, queued alongside tool permissions and AI-initiated
`ask_user` prompts (ADR-0173 §3). A sheet is by definition mounted *on
top of* a surface that has already opened; admission wants to gate
*whether* that surface opens at all.

## Decision

### 1. Make workspace trust a pre-attach surface, not a sheet

Introduce a new TUI surface, `PreAttach`, that mounts before any chat
transcript, composer, or session chrome is drawn. Visually it is a
full-screen black background with a centered trust prompt and an option
list navigated by `↑`/`↓`, selected by `Enter`, and escaped by `Esc`.
Highlight is rendered as an inverted background (the established
select-row affordance), not a cursor marker.

The surface owns the entire terminal area for the duration of the trust
decision. There is no parent surface to drop back to and no composer to
fall through to.

### 2. Gate mounting on the first Quarantined snapshot

The listener task that handles `RoundEvent::HarnessState` (the same
handler that today pushes into `pending_question`) instead publishes a
`PreAttachSignal` carrying the synthesized trust question across the
listener → loop boundary through a dedicated `Arc<Mutex<...>>` cell on
`UiRuntime`. The per-frame sync (`event_loop/sync.rs`) reads that signal,
and if the snapshot's `aggregate()` is `Quarantined` AND no decision has
yet been recorded for this run, mounts `App::pre_attach:
Option<PreAttachState>`.

The existing `trust_gate.rs` builder (`gate_request`,
`answer_to_command`, `TRUST_GATE_REQUEST_ID`) is retained — its question
wording, domain listing, and answer-to-command mapping remain
authoritative — but it no longer routes through the
`pending_question` queue. The `QuestionModel` is reused as the input
state machine for the PreAttach surface, exactly as it is for the
Question sheet.

### 3. Resolve to chat by observing a Trusted snapshot

Selecting "Trust all domains" sends the canonical `/trust` slash command
through the same wire path the existing reply interceptor used; the
daemon persists via `WorkspaceSecurityStore::trust_domains` and
republishes `HarnessState` with the new snapshot. The per-frame sync
observes `aggregate() == Trusted`, clears `App::pre_attach`, and the
chat surface mounts on the next frame — never earlier.

### 4. Resolve to quit by dismissing

`Esc` and the "Keep quarantined" option both quit the TUI. There is no
longer a "stay untrusted and use the app" path: an untrusted workspace
has no useful work to do (model rounds fail preflight per ADR-0140 §3),
so dismissal-as-continue was already a misleading affordance.
Quit-on-dismiss makes the decision honest and eliminates the re-nag on
next launch by symmetry — the workspace is still `Quarantined` on disk,
so the next attach re-mounts PreAttach.

For headless / autopilot / non-interactive clients, the existing
preflight refusal at the daemon side remains the gate; they never reach
PreAttach.

### 5. Lock the gate per-run

`trust_gate_dismissed: AtomicBool` is preserved as a per-run latch
preventing the periodic `HarnessState` snapshots that follow a
dismissal from re-mounting PreAttach within a single process. It is not
a persistence mechanism — persistence is owned entirely by
`WorkspaceSecurityStore` and reached only through the `/trust` slash
command.

### 6. Acceptance override

`MUTX_FORCE_PRE_ATTACH=1` seeds `App::pre_attach` with a synthesized
`Quarantined` snapshot at startup, letting operators visually verify the
surface — wording, highlight, keyboard navigation, transition — without
preparing a quarantined workspace. The override is acceptance scaffolding
only; selecting "Trust all domains" still routes through the real
`/trust` path and persists against the real workspace root (when one is
attached).

The name follows the established `MUTX_*` prefix for TUI-only acceptance
toggles (matching `MUTX_STARTUP_VIEW` and `MUTX_SETTINGS_CATEGORY`).

### 7. Fix the queue-depth badge off-by-one

Independent of PreAttach, `pending_question_depth` is computed as
`pending.len().saturating_sub(1)`, so the badge reports only items
queued *behind* the one already mounted — matching the existing
permission sheet's `> 1` predicate at
`apps/tui/crates/mutx/src/overlays/permission.rs`.

## Alternatives considered

### Wire-level admission handshake (deferred)

A new `Wire::AttachAdmission { snapshot }` frame sent before
`Wire::Welcome` when the resolved project root is `Quarantined`, with
the client replying `Wire::ResolveAdmission { decision }` before the
daemon continues the attach. This is the strictly correct interpretation
of "trust happens before the session exists," since the daemon would
refuse even to send transcript history to an untrusted client.

Deferred because: (a) it forces a `PROTOCOL_VERSION` bump per ADR-0134
and coordinated updates to the web frontend's handshake
(`apps/web/src/lib/stores/daemon.svelte.ts`); (b) the user-visible
behavior under PreAttach is identical — the chat surface never appears
until trust resolves; (c) the daemon-side session is ephemeral and
contains no information the operator has not already authorized (the
project root was opened by the operator, the transcript is empty for a
fresh session, and a resumed session's history is itself project-authored
content the trust decision governs). The marginal leakage over the
wire-level gate is therefore limited to "the daemon admits an untrusted
client learned the session id," which is already public via
`muta session ls`.

A future ADR may revisit this if the daemon ever carries
session-transcript data that should be withheld pending trust, or if a
second frontend needs a uniform admission contract.

### Persist the "Keep quarantined" decision

Add a `Dismissed` state to `WorkspaceSecurityStore` so the dialog stops
re-appearing after the user has explicitly declined. Rejected: it
conflates an admission decision with a UX preference, and the stored
state would have to be re-validated against content changes (the exact
problem ADR-0140 §4 solves for `Trusted`). Quit-on-dismiss is symmetric —
the workspace stays `Quarantined` on disk, the next attach re-asks, and
the operator who actually wants silence runs `/trust` once.

### Keep the in-composer sheet, fix only the bugs

Repair the off-by-one and the dismissal persistence without moving the
surface. Rejected: the deeper complaint — that the prompt interrupts an
already-mounted chat instead of preceding it — is architectural, not a
bug. Patching the symptoms would leave the inversion in place and the
ADR-0140 §3 "before work begins" posture unhonored.

## Consequences

- First-contact workspace trust is presented as a precondition to the
  chat surface, not as an interruption of it. A user opening a
  `Quarantined` workspace sees a black interstitial; nothing else
  renders until they trust or quit.
- `/trust` issued while a chat surface is already mounted (because the
  workspace was `Changed`, not `Quarantined`) continues to flow through
  the daemon banner path at `crates/muta-runtime/src/serve.rs`
  unchanged. PreAttach owns first-contact only; the `Changed`
  re-quarantine notice stays a banner.
- The `pending_question` queue no longer carries trust-gate requests;
  the Question sheet is reserved for AI-initiated `ask_user`, and the
  PreAttach sentinel id (`TRUST_GATE_REQUEST_ID`) is matched only inside
  the PreAttach input handler, not inside the Question sheet reply
  interceptor.
- `MUTX_FORCE_PRE_ATTACH=1` is documented alongside the other
  `MUTX_*` acceptance toggles.
- The wire protocol is unchanged; no `PROTOCOL_VERSION` bump, no web
  frontend coordination required for this change.
- Headless clients (`muta run`, autopilot, web) are unaffected — their
  trust posture continues to flow through the existing preflight and
  banner paths.

## References

- [ADR-0107](0107-trust-gate-covers-project-skills-and-commands.md) —
  the trust gate this revises placementally
- [ADR-0139](0139-unified-tui-surface-router-and-view-lifecycle.md) —
  the surface lifecycle model; PreAttach is modeled as a startup
  surface alongside `SessionsPicker`, not as a retained view or
  transient sheet
- [ADR-0140](0140-workspace-authority-and-content-bound-extension-trust.md)
  — workspace authority axes; §3 is the "disclose before work begins"
  posture this honors
- [ADR-0145](0145-decoupled-workspace-asset-trust-and-tool-hazard-model.md)
  — the per-domain trust model PreAttach presents
- [ADR-0134](0134-wire-protocol-negotiation.md) — the bump discipline
  the deferred wire-level admission would have to follow
- `apps/tui/crates/mutx/src/trust_gate.rs` — the question builder
  (retained)
- `apps/tui/crates/mutx/src/pre_attach.rs` — the surface implementation
- `apps/tui/crates/mutx/src/event_loop/render.rs` — the early-return
  guard alongside the `SessionsPicker` precedent
