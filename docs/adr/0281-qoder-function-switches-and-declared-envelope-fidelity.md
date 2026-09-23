# 0281. Qoder's function switches are a routing axis, not models: membership removal at parse time and the declared reason key

- **Status:** Accepted
- **Date:** 2026-09-22
- **Scope:** `muta-providers` (Qoder registry, wire surfaces), `muta-contracts` (wire surface table), `muta-persistence` (rename re-keying)
- **Deciders:** Muta maintainers
- **Builds on:** [ADR-0273](0273-upstream-availability-as-a-fourth-catalog-axis.md) (the four catalog axes), [ADR-0266](0266-declarative-remote-catalog-descriptors.md) (declarative catalog descriptors), [ADR-0265](0265-declarative-wire-surfaces.md) (declarative dialect tables), [ADR-0201](0201-model-provider-service-surface-and-connection-identity.md) (connection identity)
- **Amends:** [ADR-0273](0273-upstream-availability-as-a-fourth-catalog-axis.md) §2 — its Qoder row states "no reason field exists, so `reason: None`", which is factually wrong: the reason is published as `strategies[].disabled_message_key`. The Qoder row of the shape-mapping table in [Model catalog architecture](../architecture/model-catalog.md) §5.3 carries the same error and is corrected with this record. It also amends [ADR-0201](0201-model-provider-service-surface-and-connection-identity.md) INV-4: that invariant requires only the two hard stores plus `default_connection` to be rewritten, and its store table treats a catalog-cache miss and lost usage recency as acceptable side effects of a rename. They are not — the cached validator is still valid under the new name, and the user's route settings are state, not cache. INV-4 is strengthened to re-key every live name-keyed store.
- **Related:** [ADR-0271](0271-outbound-plan-single-wire-path.md) (golden-wire byte pinning), [ADR-0272](0272-qoder-server-elected-inference-endpoint.md)

---

## Context and Problem Statement

Qoder's scene catalog (`GET /algo/api/v2/model/list`, ADR-0266) returns, in each
scene, a mix of two different kinds of entry:

1. **Models** — `qmodel_38max`, `qfmodel`, `smodel`, `gmodel`, …
2. **Function switches** — `auto`, `ultimate`, `performance`, `efficient`,
   `advanced`. These select a *routing mode*, not a model.

muta read both as models. On the `assistant` scene that produced a 17-entry
catalog of which 4 were switches, rendered in the picker as unusable rows with a
bare `locked` tag. The defect was latent while the switches were
`enable:false` on the development account — ADR-0273's daemon gate refused them —
but it became live the moment an account's plan enabled them:

- Selecting `auto` sends `X-Model-Key: auto` and `model_config.key: auto`, which
  delegates model choice to the server. Every capability muta fitted for the
  channel (effort ladder, thinking support, context window) then describes a
  model nobody selected. `performance` even advertises a 272K window that no real
  entry carries.
- The picker lists routing modes as if they were models, so "which model am I
  using?" has no answer.

Four further defects were found in the same surface while characterizing it, and
are corrected by the same decision because they share one root cause — the
catalog was read as "a list of models with some fields", rather than as two
orthogonal axes:

- **The reason was discarded.** ADR-0273 records that Qoder "states only the
  boolean — no reason field exists". The payload disagrees: a locked entry
  carries `strategies[] = [{ tag, priority, enabled, disabled_message_key }]`,
  e.g. `disabled_message_key: "codeSafeModelReason"`. The vendor's own text table
  (`dynamic-texts.json`, `modelSelector.strategy.codeSafeModelReason`) resolves
  that key to a sentence about an enterprise security policy. muta showed a bare
  `locked`.
- **A fabricated envelope field.** The wire surface declared `model_format` as a
  root-level envelope key. The reference client (`qodercli` 1.1.59) contains no
  such string; its `format` lives *inside* `model_config`. muta was sending a
  field the vendor does not.
- **A phantom model id.** `muta_contracts::model_providers::QODER_MODELS` still
  listed `qoder3` / `qoder3-max` / `qoder3-base` / `qwen3-coder-plus` — ids the
  platform never had, superseded in the registry but not in the contracts crate.
  The wire envelope's model fallback was literally `.unwrap_or("qoder3")`.
- **A rename left orphans.** `rename` re-keyed the hard join keys
  (`credentials.toml`, `auth.toml`, `config.toml`) but its doc comment claimed the
  catalog cache and usage recency "expire on their own". They do not: nothing
  expires them. A renamed connection stranded its cached catalog (validator
  included), its usage recency, and the user's own per-route effort settings.

### The decisive finding: the payload declares no kind

Every field of every entry in a live 104-entry capture was examined for a value
set disjoint between switches and models. The result:

| Field | Separates switch from model? |
|---|---|
| `key`, `display_name` | Yes, but tautologically — they *are* the labels |
| `is_new` | By **presence only**, and it is a marketing "NEW" badge |
| every other field | No |

So there is no declared discriminator to read. `is_new` is a trap: a model that
ages out of the badge would silently leave the catalog if classification keyed on
it. The distinction therefore has to be a **pinned vocabulary**, which is exactly
the kind of knowledge that rots silently — hence the drift tripwire below.

The reference client corroborates the two-axis model from the other side: its
envelope builder routes switches through `business.feature_switches`
(`functionSwitchSelections`) and the model through `model_config.key` — two
independent parameters, never one field.

## Decision Drivers

1. **A routing mode is not a member.** Membership (axis A) answers "which ids
   exist"; a switch has no capabilities, no window, no identity. Treating it as a
   member corrupts axes B, C, and D for a thing that is not a model.
2. **Silence is the failure mode to fear.** The switches are inert *today* only
   because of an unrelated account state. Correctness must not depend on that.
3. **A pinned vocabulary must fail loudly when it goes stale.** If the
   distinction cannot be read, it must be tested against real payloads.
4. **Declared facts are recorded, never invented** (ADR-0273).
   The reason exists and must be carried; `max_tokens` and `stream` are stated by
   the reference client and must be carried; a model id must never be guessed.
5. **A rename re-keys live state; it does not discard it.** Renaming is not
   deletion, and no store may be assumed to expire when it does not.

## Considered Options

| Option | Verdict |
|---|---|
| **A. Drop switches at parse time, vocabulary pinned by a fixture-backed drift test; carry the declared reason key; declare the reference client's envelope fields in the surface table; re-key live stores on rename** | **Chosen** |
| B. Mark switches unavailable (`usable:false`) and keep them listed | Rejected — models a routing mode as a lock, and the moment a plan enables one it becomes selectable |
| C. Filter switches at the presentation layer (picker) only | Rejected — leaves them as members, so every non-picker consumer (routing, capability fitting, telemetry) still sees them |
| D. Classify on `is_new` | Rejected — a marketing badge; a model that ages out would silently vanish |
| E. A hardcoded blocklist in configuration | Rejected — silent by construction; misses `advanced` and the scene-prefixed forms (`quest-auto`, `qwork-advanced`), and fails open on a new name |
| F. Resolve `disabled_message_key` against the vendor text table in muta | Rejected — ships a vendor copy table in core, and resolving the key is localization |

## Decision Outcome

Chosen option: **A**.

### 1. Membership removal at parse time

`registry::qoder::parse_scene_catalog` excludes function switches before a
`DiscoveredModel` is ever built. `surface::FUNCTION_SWITCH_KEYS` is the pinned
vocabulary (`advanced`, `auto`, `efficient`, `performance`, `ultimate`), and
`surface::is_function_switch(key, scene)` strips the owning scene's hyphen prefix
first, so scene-scoped forms (`quest-auto`, `qwork-advanced`, `experts-ultimate`)
are caught in their own scene. Model keys use `_`, never `-`, so the two
namespaces cannot collide.

This is the same mechanism ADR-0273 already blessed for Antigravity
(`deprecatedModelIds`) and that [Model catalog architecture](../architecture/model-catalog.md)
§5.3 states as the general rule: a fully retired or non-model entry is *gone*,
which is **membership removal, not an availability verdict**.

### 2. The declared reason, verbatim

`discovered_from_scene_entry` reads `strategies[].disabled_message_key` and
records it as `Availability.reason`, verbatim and opaque. It is never resolved
against the vendor's text table, never parsed, never matched on. An entry the
payload gives no reason for keeps `reason: None` — undeclared is never invented.

### 3. The reference client's envelope, declared in the surface table

Per ADR-0265 the envelope's constants live in the surface table, not in the
builder. `AgentChatSpec` gains the four fields the reference client states on
every turn (`stream`, `is_reply`, `is_retry`, `aliyun_user_type`), the
`parameters.max_tokens` fallback, and `parameters` projection itself; the
fabricated root-level `model_format` is removed and `format` is bound inside
`model_config` through the existing `ModelBinding` mechanism.

Reasoning effort is projected from the canonical body — the flat
chat-completions body's `reasoning_effort`, clamped upstream against the
channel's advertised ladder — into `parameters.reasoning_effort`, with
`parameters.enable_thinking` derived from it by the vendor's own rule (`"none"`
means off, any other effort means on). `EnvelopeInput.reasoning_effort` is
deleted: a second copy on the input struct would be a fork that could disagree
with the body.

A body carrying no model id is refused rather than stamped with a placeholder,
because `X-Model-Key` and `model_config.key` are what the service routes on.

### 4. Rename re-keys every live store

`rename` re-keys the catalog cache (whose ETag validator is derived from fetch
attributes, never the name, so it stays valid), usage recency, and per-route
effort/thinking settings. `RemoteCatalogCache::rename_connection`,
`ConnectionUsage::rename_connection`, and `RouteSettingsStore::rename_connection`
are case-insensitive no-ops for a case-only rename, since the stored key is
exact. The false "expire on their own" claim is removed.

### Invariants & Behavioral Boundaries

- **`[INV-CATALOG-08]` A non-model entry is a membership fact.** An entry the
  provider publishes as a routing mode, selector, or alias for "the server
  decides" must be excluded at parse time, never recorded as an unavailable or
  disabled model. Marking it unavailable makes it selectable the moment a plan
  enables it, which is the failure this invariant exists to prevent.
- **`[INV-CATALOG-09]` A pinned vocabulary is fixture-backed.** Any
  provider-surface vocabulary that cannot be read from a declared field (because
  the payload declares no discriminator) must be pinned to a committed capture
  and audited by a test that fails when the capture and the vocabulary disagree.
  A vocabulary that can rot silently is not admissible.
- **`[INV-WIRE-02]` The envelope states what the reference client states.**
  Envelope fields are declared in the surface table and pinned by golden-wire
  tests. A field the reference client does not send must not be added; a field it
  always sends must not be omitted. A missing model id fails closed rather than
  defaulting to a placeholder.
- **`[INV-PERSIST-01]` A rename re-keys; it never discards.** Every store keyed
  by connection name is re-keyed on rename. No store may be assumed to expire
  unless an explicit expiry mechanism is implemented and tested; "regenerable"
  is a claim requiring evidence, not an assumption.

The reason key needs no new invariant. `[INV-AVAIL-02]` requires a declared fact
be recorded as declared, which is the *verbatim* half; and resolving
`disabled_message_key` against the vendor's text table is localization, which
`[INV-AVAIL-03]` already forbids. A second ID would be a frozen duplicate of an
existing rule.

### Positive Consequences

- The picker shows models. A paid account cannot select a routing mode by
  accident, and capability fitting, telemetry, and routing all see one kind of
  entry.
- The user's effort selection reaches the wire. Verified live: the same prompt at
  `low` produced 288 reasoning characters and at `xhigh` produced 826 — the
  field changes server behaviour rather than being accepted and ignored.
- A locked model explains itself when the vendor explains it.
- The vocabulary cannot rot silently: a new switch name fails a test that names
  the two possible causes.
- Renaming a connection preserves its cached catalog validator, its usage
  ordering, and the user's own route settings.

### Negative Consequences & Trade-offs

- **The vocabulary is maintained, not derived.** Qoder can publish a new switch
  name that muta will treat as a model until the fixture is refreshed. Mitigated
  by the drift tripwire: the audit fails, and its message names the fix. The
  tripwire's cross-check uses `is_new`, itself a badge — so a failure has two
  possible causes, and the test says so rather than guessing.
- **The fixture is a point-in-time capture.** It must be regenerated when Qoder
  ships a client bump. Mitigated by `examples/qoder_catalog_dump`, a one-command
  capture, and by naming the emulated `Cosy-Version` in the filename.
- **`max_tokens` is a client default, not catalog data.** Qoder's catalog
  publishes no `max_output_tokens`, so the value sent is the reference client's
  own normalizer fallback (32000). If Qoder changes that default, muta follows
  only on a fixture refresh. The alternative — omitting the field — would be a
  larger deviation from the client than including it.
- **One local capture is not a proof of global behaviour.** The reason key is
  present on 13 of 15 locked entries on the captured account; the remaining two
  state none and correctly carry `reason: None`. Coverage of that branch is held
  by synthetic payload tests, not by the fixture's account state.

## Rejected Alternatives & Negative Knowledge

### Option B — mark switches unavailable and keep them listed
- Why considered: reuses ADR-0273's existing axis, no new membership rule.
- Why rejected: it models a routing mode as a locked model. A model is locked
  because of an entitlement; a switch is not a model at all. The distinction
  collapses the moment a plan enables the switch, at which point it becomes
  selectable and delegates model choice to the server — the exact defect.

### Option C — filter at the presentation layer
- Why considered: no change to the parser, contained in the picker.
- Why rejected: membership would still contain the switches, so capability
  fitting, route derivation, telemetry, and the web client would each see them.
  It also violates `[INV-AVAIL-01]`: presentation must not be the owner of what
  membership is.

### Option D — classify on `is_new`
- Why considered: it separates the sets perfectly in the capture (74 models have
  it, 30 switches do not), so it looks like a declared field.
- Why rejected: `is_new` is a "NEW" badge. A model that ages out of the badge
  would be reclassified as a switch and silently vanish from the catalog — a
  time bomb with no failure signal. It is retained only as an *audit* signal,
  where its failure mode is a failing test rather than a missing model.

### Option E — a hardcoded blocklist in configuration
- Why considered: zero code, user-editable, immediate.
- Why rejected: it fails open and silently. The live capture shows five switch
  names, not four, and eight scene-prefixed forms — a `[auto, ultimate,
  performance, efficient]` list misses `advanced` and every prefixed form. It
  also cannot distinguish a scene-prefixed switch from a model. Configuration
  that silently stops working is worse than code that fails a test.

### Option F — resolve `disabled_message_key` in muta
- Why considered: the user would see a real sentence instead of a key.
- Why rejected: it requires shipping the vendor's text table inside muta's own
  source — a vendor copy in core — to render a string the client owns. The key is
  display data whose resolution is a presentation concern.

### G. Deleting the cached catalog on rename (instead of re-keying)
- Why considered: simpler than a four-map re-key, and the catalog is
  regenerable.
- Why rejected: the cache carries a live ETag validator. Discarding it forces a
  full refetch for a connection whose fetch identity did not change, and it
  throws away the fitted metadata and advertised facts with it. Renaming is not
  deletion.

## Links

- Implementation: `crates/muta-providers/src/registry/qoder/{mod.rs,surface.rs}`,
  `crates/muta-providers/src/registry/qoder/wire/envelope.rs`,
  `crates/muta-contracts/src/wire_surface.rs`,
  `crates/muta-persistence/src/{config.rs,connection_usage.rs,route_settings.rs}`,
  `crates/muta-runtime/src/handlers_provider.rs`
- Evidence fixture: `crates/muta-providers/tests/fixtures/qoder-model-list-1.1.58.json`
  (regenerate with `cargo run -p muta-providers --example qoder_catalog_dump -- <connection> <out>`)
- Contract audit: `crates/muta-providers/tests/it/qoder_catalog_contract.rs`
- Golden wire: `crates/muta-providers/tests/qoder_wire_golden.rs`
- Integration evidence: [Qoder provider integration](../explanation/qoder-provider-integration.md)
- Related ADRs: [ADR-0273](0273-upstream-availability-as-a-fourth-catalog-axis.md),
  [ADR-0265](0265-declarative-wire-surfaces.md),
  [ADR-0266](0266-declarative-remote-catalog-descriptors.md)
