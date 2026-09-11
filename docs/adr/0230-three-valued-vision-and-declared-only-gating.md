# 0230. Three-valued vision capability and declared-only gating

- **Status:** Accepted
- **Date:** 2026-09-10
- **Scope:** `muta-contracts/model` (capability resolution), wire (`ProviderModelInfo`), `muta-llm-client` (wire projection), `mutx` (composer)
- **Amends:** ADR-0149 (capability resolution order), ADR-0182 (route-projected capabilities), ADR-0065 (fitted-model overlay)

## Context

Image input was the one capability the client resolved to a **two-valued** fact,
everywhere, and the two values were "yes" and "no matter what":

- The baseline layer is `Model::vision: bool`; the fitted overlay coerced an
  unadvertised field with `vision: model.vision.unwrap_or(false)`
  (`muta-agent/src/catalog/discovery.rs`); the fallback for an id nobody knows
  carried `vision: false` (`muta-contracts/src/model.rs`); and
  `ProviderModelInfo.vision` was a `bool` whose absence defaulted to `false`.
- A `false` therefore meant three different things — *vetted text-only*,
  *the endpoint said nothing*, and *we never heard of this id* — and it
  triggered a **silent strip**: `openai/chat_completions/request.rs` and
  `openai/responses/request.rs` dropped `Message::images` with only a
  `tracing::debug!` behind it.

Three concrete failures followed:

1. **Vision is the capability vendors do not publish.** A relay that serves a
   multimodal model through a stock `/models` response (or a fitted entry whose
   endpoint omits the field) resolved to `Some(false)`, so its images were
   stripped and the model answered confidently about a picture it never saw.
   That is the worst failure mode in the system: the user cannot detect it, and
   the hand-maintained baseline table is the only thing standing between them
   and it — exactly the workload ADR-0149 already flagged as fragile.
2. **The projection was transport-inconsistent.** Only the two OpenAI builders
   stripped. `anthropic/request.rs` emitted `image` blocks and
   `google/request.rs` emitted `inline_data` unconditionally, so the same
   declared-text-only route *failed the turn* on one transport and degraded
   silently on another.
3. **The client gated where it had no grounds to.** The composer refused an
   image paste whenever `vision == false` — including for routes that had never
   declared anything, which is precisely the case where the client knows least.

## Decision

Vision is **three-valued end to end**, and a value must be *declared* before
anything may act on it.

```text
Some(true)   a layer declared image input
Some(false)  a layer declared that images are rejected
None         no layer declared anything — the route is UNKNOWN, not text-only
```

1. **Resolution keeps the third value.** `ModelCapabilities::vision: Option<bool>`
   and `RouteCapabilities::vision` / `ProviderModelInfo.vision: Option<bool>`.
   `ModelCapabilities::for_channel` no longer falls through to `baseline.vision`
   blindly: the new `declared_vision(id)` answers the baseline layer only when a
   layer actually declares something — a vetted registry entry, or a fitted entry
   whose endpoint advertised the field. A baseline miss, an unadvertised fitted
   field, and an unknown id all resolve to `None`.
2. **Unknown is permissive for requests.** `ModelCapabilities::accepts_images()`
   is `true` unless some layer said `Some(false)`. An undeclared route carries
   its images to the provider; a provider that cannot take them rejects the
   request loudly. A client that strips them says nothing at all.
3. **Gates require a declaration.** Only `Some(false)` may refuse a paste
   (`apps/tui/crates/mutx/src/clipboard_ops.rs`), and the refusal names its
   escape hatch (`Vision` in the model editor, ADR-0149 layer 1). Undeclared
   routes accept the paste and let the wire decide — but an accepted paste is
   not a promise the model will see the picture, so the toast says
   "image support unverified for this model" when no layer declared anything.
   Display surfaces report a claim only when there is one
   (`derive_capabilities` lists `vision` for `Some(true)` only).
4. **One projection, every transport.** `muta-llm-client::vision::project_images_for_route`
   is the single policy point; chat-completions, Responses, Anthropic, and
   Google all call it. It returns the dropped-image count, so the eventual
   user-visible notice needs no re-scan.
5. **The tool pool stays optimistic.** `read_image` survives on an undeclared
   route: the agent seeds tool admission from `accepts_images()`, matching the
   permit-attempt posture above. Dropping the tool on unknown would recreate the
   relay bug ADR-0149/ADR-0182 fixed for the fitted overlay.
6. **A refusal is learned, never fatal — and recognition is an *outcome*, never
   a parse.** A refusal of the request itself (`ProviderError::is_request_refusal`,
   derived from the HTTP status the transport already mapped) that occurs while
   attachments are present triggers one experiment: the identical turn is retried
   with the attachments withheld.

   - **Success latches the route.** Images were the cause; the route is marked
     image-incapable for the rest of the process (keyed by `RouteFingerprint`, so
     a model switch disarms it), and the user is told — with the notice stating
     the empirical finding, since the harness never read the provider's wording.
   - **Failure disproves the hypothesis.** Nothing is latched, and the error you
     see is the refusal of the request you actually composed — never the failure
     of the harness's own modified request.

   The experiment re-projects the *armed request checkpoint*
   (`StreamingRoundState::project_pending_request`, with an unconditional
   `strip_images`) rather than clearing it, so the turn's history, tools, hooks,
   and accounting stay exactly as assembled and only the attachments disappear.
   One probe per round, outside the transient-fault retry budget — the same shape
   as the context-overflow compaction recovery beside it.

   **The vendor's error format is not consulted at all.** No classifier reads
   `error.message`, no marker list, no upstream code table: each vendor formats
   its envelope differently — `{"error":{"message":…}}`,
   `{"error":{"details":[…]}}`, `{"message":…}`, a bare string — and their prose
   drifts, so any such predicate is a permanent liability whose failure modes are
   both bad (a false negative re-bricks a session; a false positive withholds a
   capability that works). Two facts are ours rather than theirs — *the request
   was refused* and *it carried attachments* — and the differential turns exactly
   those into the answer. A text-based recognition predicate was in fact written
   and then deleted during review for precisely this reason; §"Alternatives
   considered" records what it cost and why it failed.

   **The trigger must preserve the inference's validity, not merely its
   cheapness.** The probe reasons *"re-sent with the attachments withheld →
   succeeded → the attachments were the cause"*, which is sound only if
   **re-sending the identical bytes would have failed identically**. Two
   properties follow, and `ProviderError::is_request_refusal` is defined by them
   rather than by error *kind*:

   - **Deterministic.** A transient failure invalidates the inference outright: a
     re-send tends to succeed *regardless of what changed*, so "succeeded after
     stripping" is evidence of nothing — while the harness would latch it as
     proof that the route rejects images, permanently disabling a working
     capability for the session. `408`, `429`, `5xx`, and transport/timeout
     failures are therefore excluded, and the transport already retries them with
     backoff, so the probe would race that recovery anyway.
   - **Caused by the payload.** An endpoint/method/conflict/auth failure is
     deterministic but says nothing about the body: probing it latches nothing
     (it recurs, so the hypothesis self-disproves) yet makes the user wait an
     extra round trip — e.g. before seeing "model not found" after a typo.
     `404`, `405`, `409`, `410`, `401`, and `403` are excluded. `classify_http_error`
     reports all of those as `InvalidRequest`, so a kind-based test would have
     swept them in; the predicate reads the status.

   The surviving set is the "this payload is unacceptable" family: `400`/`422`
   (validation), `413` (too large — a base64 image is the likeliest cause), and
   `415` (unsupported media type, the canonical image rejection).
   `ContextOverflow` is excluded because compaction owns it — and it arrives as
   `400`/`413`/`422`, i.e. inside the probeable range. An in-band refusal (HTTP
   2xx carrying an `error` object) has no status to read, so it falls back to the
   kinds only a request-shape refusal produces.

   **Cost, as a secondary consequence.** A validation refusal is rejected before
   inference and consumes no tokens — the system's own ledger corroborates this,
   recording the attempt as `RequestUsageStatus::Failed` with no token counts —
   so the failed attempt costs bandwidth, and the retry without attachments is
   the turn the user needed anyway. A route whose refusals are misattributed pays
   one extra round trip once, not a second conversation.

   This closes the loop for the durability problem §4 creates. Consider the
   motivating sequence: an image is pasted while a multimodal model is active,
   then the user switches to a model that cannot take images. History is
   append-only (ADR-0186), so *every* subsequent request on that route carries
   the image, and `/retry` re-sends the identical doomed request — the session is
   bricked on that route, permanently, because the projection was the only thing
   that could have changed and nothing changed it. Learning the route turns that
   wall into a single withheld attempt, reported to the user, after which the
   session continues normally on the text-only model.

### Invariants & Behavioral Boundaries

- **INV-VISION-1 — Three values, no coercion.** No layer may map absence of
  information onto `Some(false)`. Any `unwrap_or(false)` on a vision field is a
  defect; the honest query is `declared_vision(id)`.
- **INV-VISION-2 — Declared-only gating.** Application code may gate, block,
  hide, refuse, or advertise on vision **only** when the resolved value is
  `Some(false)` (to refuse) or `Some(true)` (to advertise). `None` is "try it":
  it must never be rendered as a text-only claim nor block a user action.
- **INV-VISION-3 — Projection at the wire, never in history.** The durable
  transcript keeps its images (ADR-0186 content-addressed blobs). Dropping is a
  per-request projection taken against the *current* route, so switching models
  mid-session needs no per-message memory and rewrites no history.
- **INV-VISION-4 — Silence is prohibited where the client drops pixels.** Any
  code path that discards an `ImagePart` must count it and be reachable from a
  surface that can tell the user (`NoticeKind::ImageInputWithheld`; the notice is
  the surface, and it names the user's remedy). The announcement is per *policy
  activation*, not per request: once a route is known to withhold, the user has
  been told, and the toast on a later paste on an undeclared route repeats the
  "unverified" caveat. A dropped attachment that no user-visible surface ever
  mentions is a defect.
- **INV-VISION-5 — No route may be bricked by an image in history.** A failure
  caused by image input must never be terminal for the session: it is either
  prevented (a declared `Some(false)` route never receives attachments) or
  learned-and-retried. A terminal, unrecoverable image rejection is a defect,
  because the durable history that carries the image cannot be edited.
- **INV-VISION-6 — A learned refusal requires evidence, and the evidence is the
  probe's outcome.** Latching a route as image-incapable requires a *successful*
  retry with the attachments withheld. A failed probe must leave nothing latched
  and must surface the original refusal, not the failure of the harness's own
  modified request. Corollary A: **no predicate may decide "was this about
  images?" from the vendor's error text** — no message classifier, marker list, or
  upstream code table. Vendor envelopes and their prose differ per vendor and
  drift, so such a predicate can only ever be a liability (false negative: a
  bricked session; false positive: a withheld capability) that the differential
  already answers without parsing anything.
  Corollary B: **the probe may only run where re-sending the identical request
  would fail identically.** A transient failure makes the experiment
  unfalsifiable; arming it there would convert a flaky server fault into a
  durable, wrong capability claim.

### Positive Consequences

- A relay or unreleased model that never advertised vision keeps its images;
  the failure, when there is one, arrives from the provider as an error rather
  than as an indistinguishable confident answer.
- The capability table stops being load-bearing for correctness: an absent entry
  is a legal, useful state instead of a silent `false`.
- All four transports project identically, retiring a real "works on OpenAI,
  fails on Anthropic/Google" divergence.
- The composer's refusal now means exactly one thing ("the route declared it
  cannot"), names the override, and is testable in one place.

### Negative Consequences & Trade-offs

- **A wrong declaration still costs a wall.** When a layer says `Some(false)` for
  a route that in fact accepts images, the paste is refused until the user
  overrides it; the message now names the override, and `Some(false)` from the
  registry is hand-vetted per ADR-0149.
- **An undeclared route pays one failed attempt.** The first image-bearing
  request on such a route is attempted, refused, and retried — two round trips
  and one visible notice, once per route per process lifetime. Accepted: the
  alternative is a permanently bricked session (INV-VISION-5), and the fix that
  would remove the failed attempt entirely — persisting the learned
  `vision: false` into `RouteSettings::capability_overrides` (ADR-0149 layer 1)
  so every later process starts correct — needs a route-settings write plus a
  channel re-derive from the round loop, which currently has neither.
- **No vendor error text is interpreted, so a genuinely informative vendor
  message is not exploited.** A provider that names the exact cause gets the
  same treatment as one that says nothing usable: one probe. That is a
  deliberate trade of a little latency for the removal of an entire
  maintenance surface — a text classifier was implemented and deleted for this
  reason (see "Alternatives considered").
- **A refusal that is not about images costs one extra round trip.** A
  request-shape refusal while attachments are present is re-sent once without
  them, so a genuinely unrelated 400 pays two validation failures instead of one
  (no tokens either way) before the original error surfaces. Bounded to one per
  round — and the alternative is a session that cannot continue at all.
- **The learned latch is in-process.** It is keyed by `RouteFingerprint` and
  lives on the agent, so it survives model switches (by disarming) and turns
  (by holding), but not a daemon restart — where the first image request on that
  route re-learns once. The durable half belongs in the capability override
  described above, not in a new latch store.
- **A flaky bad-request refusal could latch spuriously.** `classify_http_error`
  treats 4xx as non-retryable, so a 400 that succeeds on an immediate re-send is
  already anomalous; if one did, the probe would credit the image and withhold
  attachments from that route until the model changes. Accepted as the cheaper
  error: the false negative it guards against is unrecoverable, and the latch is
  per-route and reversible through the `Vision` override.
- **The retried request is not what ADR-0218 archived.** The request-projection
  archive keeps the freshly assembled shape while the retried request has its
  attachments projected out; the difference is reported through the notice and
  the log rather than by re-writing the audit record, because re-recording on a
  retry is exactly what ADR-0218 forbids.
- **Wire semantics widened without a version bump.** A v12 peer reads our
  `Some(true)` as `true` and an absent field as `false`; a v13 peer reads a v12
  peer's `false` as `Some(false)` and `true` as `Some(true)`. Both directions
  degrade to the old behavior, so `PROTOCOL_VERSION` does not move for the
  three-valued change alone (`NoticeKind::ImageInputWithheld` rides the existing
  v13 bump).

## Alternatives considered

- **Keep `bool`, default unknown to `false`, and only remove the paste gate.**
  Rejected: it preserves the silent strip, which is the actual defect. Removing
  the gate without the third value would have made the client lossier, not
  kinder — paste accepted, pixels dropped, no signal anywhere.
- **Keep `bool` and treat all unknown routes as vision-capable in the baseline
  table.** Rejected: it cannot be expressed. `bool` cannot say "undeclared", so
  the table would have to *claim* `true` for models nobody has verified, turning
  a vetted table into a guess with no way to tell the two apart.
- **Rewrite history when the model changes (strip images at switch time).**
  Rejected: the transcript is the durable record (ADR-0186) and a per-request
  projection already answers the question correctly. Editing history would
  destroy data the user pasted, break content-addressed reuse, and make the
  outcome depend on *when* the switch happened rather than on the route.
- **Fail loud only: send images everywhere, never strip, no notice.** Rejected
  as the sole policy for a *declared* `Some(false)` route: the declaration is
  knowledge, and a guaranteed 400 that consumes a composed prompt is strictly
  worse than a clear refusal. Declared-only refusal plus permissive unknown is
  the combination that respects both facts.
- **A text classifier over the vendor's error message: latch immediately when
  the wording names image input.** Implemented, then deleted. Two attempts at
  tuning it failed in opposite directions, which is what killed it: a narrow
  marker list missed phrasings nobody anticipated (Google's "Unable to process
  input image" reads `input image`, not `image input` — verified miss), and a
  coarse one produced false positives on unrelated failures that merely quoted
  the payload (a tool-schema 400 whose dump contained `image_url`). Once the
  probe made recognition empirical, the classifier's only remaining job was
  picking the notice's wording — a maintenance obligation with no behavioral
  value. Deleting it also removed the last place the harness would have depended
  on any vendor's format.
- **Parse the vendor's error envelope and match on its error `code`/`type`.**
  Rejected: the envelopes and their vocabularies differ per vendor
  (`error.code` vs `error.type` vs `error.status` vs `error.details[]`), so this
  is the marker list again with more parsing to keep working, and a relay that
  forwards someone else's code — or invents its own — defeats it. The only thing
  that does not vary is the outcome of re-sending without the attachments.
- **Ask the user**: on a refusal, present a "was this about images?" choice.
  Rejected: the harness can answer it itself for one cheap round trip, and
  asking the user to diagnose the harness's own request is exactly the burden
  this recovery exists to remove.
- **Let the wall stand: accept a permanent failure on a text-only route.**
  Rejected. History is append-only (ADR-0186), so an image already in the
  transcript makes *every* later request on that route fail, including purely
  textual follow-ups, and `/retry` re-sends the same doomed request. The user
  would have to abandon the session to proceed. This is what INV-VISION-5
  forbids, and it is why the learn-and-retry recovery exists rather than a
  "the provider will tell them" posture.
- **Strip images from the durable transcript when a model switch makes them
  unreadable.** Rejected: it deletes user data to satisfy a per-request
  constraint, makes the outcome depend on *when* the switch happened, and
  destroys the record the user may need when they switch back.
- **Reduce the retry to a plain `/retry` affordance.** Rejected: the retry must
  carry a *different projection* to have any chance of succeeding, and the
  checkpoint deliberately re-sends the identical request. Without the
  re-projection the affordance is an infinite loop.
- **Per-provider hardcoded vision lists as the only mechanism.** Rejected: it is
  the status quo's cost, and it is unbounded maintenance for a capability vendors
  routinely omit.

## References

- ADR-0149 — three-layer capability resolution order (user ⊕ remote ⊕ baseline).
- ADR-0182 — route-projected capabilities (`RouteCapabilities`), and the
  distributed single source of truth for frontends.
- ADR-0065 — runtime-fitted model capability overlay.
- ADR-0186 — single-transcript projection (images persist as content-addressed
  blobs; the wire body is a projection).
- ADR-0203 — declared models and sparse capability patches.
- ADR-0128 — `/retry` resume points (why re-sending the identical request is not
  a recovery for this failure).
- ADR-0218 — durable request-projection archive (why the recovered request is not
  re-archived).
- Code: `crates/muta-contracts/src/model.rs` (`declared_vision`,
  `ModelCapabilities::accepts_images`), `crates/muta-contracts/src/error.rs`
  (`ProviderError::is_request_refusal`), `crates/muta-llm-client/src/vision.rs`,
  `crates/muta-agent/src/agent/state.rs`
  (`project_images_away_if_unusable`, `suppress_images_for_current_route`),
  `crates/muta-agent/src/orchestration.rs` (the recovery branch),
  `apps/tui/crates/mutx/src/clipboard_ops.rs`.
