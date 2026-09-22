# 0265. Declarative wire surfaces: dialect tables, request envelopes, and the model-identity carrier set

- **Status:** Accepted
- **Date:** 2026-09-20
- **Implementation:** `muta-contracts`, `muta-llm-client`, `muta-providers`
- **Builds on:** [ADR-0258](0258-first-class-declarative-model-providers-and-connection-pipe-purity.md), [ADR-0260](0260-provider-dialect-inheritance.md)
- **Amends:** [ADR-0131](0131-elastic-upstream-model-passthrough-and-thinking-disclosure.md) (slot declaration, not value remapping), [ADR-0260](0260-provider-dialect-inheritance.md) (generalizes `inference_path` to a declared surface)

## Context

A provider dialect (`ProviderDialect`, `OpenAiChatDialect`, …) is an enum key,
but the facts that key stands for were spread across hand-written code: the
inference path and query, the static identity headers, the emulated client
version, the request envelope, and the slots that carry the model identity each
lived in a different file. Adding or fixing one property meant editing several
places with no compile-time link between them.

This produced a real defect. Qoder's `Cosy-Version` header was declared twice:
once in the dialect's header table used by the inference builder
(`chat_completions/request.rs`) and once — absent — in the table used by the
catalog fetcher. The catalog signature therefore omitted a header the service
requires, and every catalog request failed with `403 {"code":"101","message":
"Signature invalid"}` while inference worked. The two copies had silently
drifted because nothing tied them together.

Separately, Qoder's inference endpoint does not accept a flat chat-completions
body. It routes through an agent framework and requires an
`agent_chat_generation` envelope; a flat body is rejected with
`400 None flow nodes found for router agent_router`. The envelope's slots
(task, agent, session, model config, business context, parameters) were
implemented ad hoc rather than declared.

## Decision

A dialect is an enum key that resolves to a **`DialectSurface`**: a table of
data describing its inference endpoint, its identity, its request envelope, and
its live catalog. The executor reads the table; it never branches on a provider
name or dialect beyond resolving the key to the table.

`DialectSurface` has three parts:

- `InferenceSpec` — the path, the fixed query, the signed-path form, the
  request envelope, and the **model-identity carriers**.
- `IdentitySpec` — the emulated client version, the header that carries it
  (`version_header`), and the static identity headers.
- `Option<CatalogSpec>` — the live catalog shape (see ADR-0266).

### Model-identity carriers

Where a model's identity is written is declared as a set of `ModelBinding`
slots: a `BodyField`, a `BodyPointer` (JSON pointer), a `PathSegment`, a
`Header`, or a `QueryParam`, each filled from an `IdentityValue` (`WireId`,
`CatalogSource`, `DisplayName`, `Constant`). Qoder declares
`X-Model-Key ← WireId`, `X-Model-Source ← CatalogSource`, and the envelope's
`model_config.{key,display_name,source}` mirrors.

This is **slot declaration, not value remapping**. ADR-0131 forbids substituting
the upstream model value; it does not forbid declaring where that value appears.
The value is always the user's choice verbatim. `DisplayName` and
`CatalogSource` are sibling facts the catalog publishes about the same model,
never a rewrite of its identity.

### Request envelopes

`Envelope` is either `Flat` (the plain chat-completions object) or
`AgentChat(&AgentChatSpec)`. The `AgentChatSpec` declares the literal slots
(`chat_task`, `agent_id`, `session_type`, `task_id`, `source`, `version`,
`model_format`, `business_*`, `fresh_uuid_pointers`). The envelope builder is a
pure function: it projects the flat body's structural slots (`messages`,
`tools`, `system`) and fills the declared constants, the model bindings, the
capability-derived `parameters`, and the `business` context. Fresh identity ids
are minted per request (the service rejects a replay with code 103) from an
injected UUID list, so the builder is deterministic under test.

### Single version source

`IdentitySpec.emulated_version` is the one home for the emulated client version.
It is read by the `version_header` entry of the identity table, by the signature
payload's `cosyVersion`, and by the envelope's `business.version`. A second copy
is a signature mismatch that no test would otherwise catch; the surface makes
the duplication impossible by construction.

### No provider-name branches

`ProviderDialect` (and its protocol-specific views) resolve to a surface through
one `surface()` method. `request.rs` composes the identity headers from
`surface.identity.headers_with_version()` rather than a local list. The catalog
fetcher composes them from the same method. Both readers, one source.

## Invariants

- A wire-level fact a dialect depends on is declared once in its `DialectSurface`
  and read by every path that needs it; no path keeps a second copy.
- The model-identity value on the wire is the channel's wire model id verbatim;
  a carrier declares *where* the identity appears, never *what* it is (ADR-0131).
- An `AgentChat` envelope's literal slots come from the declared spec; its
  structural slots project the same request the flat body would.
- The emulated version has exactly one source; the header, the signature
  payload, and the envelope read it from there.
- No request-composition path contains a provider-name or dialect-value branch
  beyond resolving the dialect key to its surface (ADR-0260).

## Rejected alternatives and negative knowledge

- **A second header table next to the surface.** This is precisely the defect
  this ADR removes; the catalog and inference paths must not list headers
  independently. During implementation this defect reappeared **three** times
  and each is now guarded by a test: (1) the version header was in an inference
  path but not the catalog path; (2) the fix inverted it — the live executor read
  the raw `headers` table (no version) while the catalog read
  `headers_with_version()`; (3) a dead Qoder branch in the generic
  `request::headers()` declared Qoder headers a third time, exercised only by a
  test, never in production.
- **A per-dialect envelope builder branch.** An `if dialect == Qoder { … }` in
  the request path re-creates the coupling the surface removes; the envelope is
  declared, and one builder interprets the declaration.
- **Writing the model id into a fixed set of slots.** Hardcoding "the model goes
  in `model` and `X-Model-Key`" cannot express a dialect that also needs
  `model_config.key` or a path segment without another branch; the carrier set
  is the general form. The corollary: a reader must not write an identity slot
  directly — `apply_model_bindings` is the only writer. (During implementation
  the envelope hardcoded `model_config.{key,display_name,source}` while the
  bindings declared the same pointers, so the two agreed only by coincidence;
  now the bindings drive the write.)
- **Remapping the display name into the wire id** (e.g. sending `Qwen3.8-Flash`
  where the catalog key is `qfmodel`). Forbidden by ADR-0131; the wire id is the
  catalog key, and the display name is presentation.
- **Implementing only the carrier kinds in use today.** A `match` that handles
  `Header` and ignores the rest compiles but makes the declaration a lie; all
  five carrier kinds are implemented so a future dialect's declaration is
  truthful.
