# 0074. Consolidate the AI SDK crates into one `neenee-llm-client`

- **Status:** Accepted
- **Date:** 2026-07-23

## Context

The LLM wire-protocol adapters were split across four crates:
`neenee-ai-sdk-core` (shared transport substrate) plus one crate per vendor
protocol — `neenee-ai-sdk-openai`, `neenee-ai-sdk-anthropic`,
`neenee-ai-sdk-google`. Each protocol crate was consumed by exactly one crate
(`neenee-providers`), the three protocols never imported each other, and their
only shared dependency was `neenee-ai-sdk-core`. They also changed in lockstep
whenever the `Provider` trait in `neenee-core` moved.

That is the signature of premature decomposition: crate boundaries that earn
their keep when a package has a distinct responsibility and more than one
consumer, applied here to parallel modules with a single consumer and a shared
substrate.

A second, independent problem lived behind the split: every provider called
`reqwest::Client::new()` on each request (twelve call sites across the four
providers' `chat` / `stream_chat` / `stream_chat_events`). Constructing a client
per call discards the connection pool and TLS session cache on every turn, so
keep-alive and TLS resumption never carried across requests.

## Decision

Collapse the four SDK crates into one crate, `neenee-llm-client`, organised in
two layers:

- **Transport** — `endpoint` (connection config + per-turn usage state), `sse`
  (byte reassembly), `transport` (HTTP success/retry/error classification and
  JSON decode), `json` (framing helpers), and a new `client` module owning a
  pooled [`Client`].
- **Protocols** — `protocol::{openai, anthropic, google}`, each a thin executor
  over its pure `request` / `response` modules plus the shared transport.

Introduce a pooled [`Client`] that owns one `reqwest::Client` for a provider's
lifetime (a provider is built once per session) and centralises the
send → HTTP-success → decode pipeline. Each provider embeds a `Client`; the
twelve per-call `reqwest::Client::new()` sites are gone. OpenAI, Anthropic, and
the Responses provider hand a protocol-built `RequestBuilder` to
`Client::send` / `Client::send_json`; Google keeps its manual pipeline because
it post-processes transport errors with a vendor-specific clarifier.

Keep `neenee-providers` as the channel registry / factory / discovery facade
that selects *which* backend; `neenee-llm-client` knows *how*. The registry is a
legitimately shared crate (four consumers, including the view layer), so it is
not folded in.

Cargo package names change (the four `neenee-ai-sdk-*` packages are gone); the
dependency DAG and runtime behaviour are preserved, with the single improvement
that one connection pool is now reused across a session's turns.

## Alternatives considered

### Keep the four crates

Rejected. The boundaries were weak (single consumer, shared substrate, lockstep
change) and the split existed without a recorded ADR. It added manifest churn
without isolating a dependency direction or a consumer.

### Fold everything into `neenee-providers`

Rejected. `neenee-providers` is the channel registry and is consumed by the
orchestration, session, application, and view layers. Merging the wire-protocol
transport into it would force the view layer (which only reads static model
tables) to transitively pull the HTTP/SSE machinery, and would mix "which
backend" with "how to talk to it."

### Name the crate `neenee-protocol` or keep `neenee-ai-sdk`

Rejected. The crate's headline artifact is the pooled `Client` plus transport;
the protocols are one internal organising axis. `protocol` undersells the
transport substrate and `sdk` implies a single-service official client, whereas
this is a multi-backend adapter the consumer never picks directly.
`neenee-llm-client` names the job.

## Consequences

- The workspace drops from four provider-transport crates to one. Adding a
  protocol is "add a module," not "add a crate plus four manifest edges."
- Connection pooling now works: one `reqwest::Client` per provider instance,
  reused across every chat, stream, and tool round in a session.
- `neenee-agent` keeps a direct dependency on `neenee-llm-client` for two shared
  items (`json::find_balanced_object` and `COPILOT_CLIENT_HEADERS`); relocating
  those to dissolve the edge is deferred (the edge is acyclic and the items are
  genuinely shared with the protocol layer).
- The four `neenee-ai-sdk-*` package names are removed; downstream embeds that
  named them update to `neenee-llm-client`.

## References

- [ADR-0005](0005-strict-layering-and-renames.md) — the strict-DAG rule this
  preserves.
- [ADR-0073](0073-flat-coding-focused-workspace.md) — the flat workspace layout
  this crate lives under.
- [Workspace layout](../dev/workspace-layout.md)
- [Crate layering](../explanation/crate-layering.md)
