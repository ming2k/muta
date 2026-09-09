# 0200. Owned egress path with a packet-level request trace

- **Status:** Proposed
- **Date:** 2026-09-08
- **Builds on:** ADR-0151 (per-attempt client-observed performance telemetry), ADR-0157 (single TTFT definition), ADR-0184 (incremental streaming pipeline / single-parse hot path), ADR-0190 (agent-as-actor task fabric)
- **Implementation:** P0 core landed in `crates/muta-trace` (event vocabulary, delta-coded bounded ring, `Recorder`, pure derivations, robust rate estimator). P1 core landed in `crates/muta-http1` (owned HTTP/1.1 codec, differential-tested against hyper as a dev-dependency) and `crates/muta-net` (DNS/TCP establishment with per-phase events, TLS via rustls + platform trust store with ALPN/version recorded, `TimedIo` syscall tap, keep-alive pool with reuse attribution, `TCP_INFO` sampling for RTT/retransmits, redirect following with credential rules, streaming gzip/brotli decoding, HTTP `CONNECT` and SOCKS5 proxies, request deadlines, traced request lifecycle, measured overhead harness). The egress seam landed in `crates/muta-llm-client/src/egress.rs`: `Egress`/`RequestParts`/`HttpResponse` let every protocol adapter run unchanged on the owned transport, and `crate::RequestBuilder` replaced `reqwest::RequestBuilder` as the request-construction API. `net-transport-default` is a default feature, so an unset `MUTA_EGRESS` means the owned transport. `tests/egress_seam.rs` proves the same provider emits identical events on both. Shadow comparison exists in three forms: hermetic (`tests/shadow.rs`), runtime (`src/shadow.rs`, feature `net-shadow`, `MUTA_NET_SHADOW=1`) and live (`examples/live_shadow.rs`, 5/5 completed against DeepSeek). The L2 probe is `crates/muta-net-probe`, a separate opt-in binary with a pure, unit-tested packet parser. **All six egress call-site crates are migrated**: `muta-models-dev`, `muta-skills`, `muta-mcp`, `muta-providers` (shared `http.rs` handle: OAuth, usage, discovery), `muta-runtime` (URL/proxy validation) and `muta-agent` (`tools/web/http.rs` `WebHttp`: search backends + reader). `reqwest` is an optional dependency behind `reqwest-oracle` only, and `scripts/check-egress-deps.sh` enforces that with an empty allow-list — `cargo tree --workspace --edges normal -i reqwest` finds nothing.
- **Supersedes (on acceptance):** `muta-llm-client/src/client.rs` and `muta-llm-client/src/transport.rs` as the HTTP egress implementation

## Context

### What we can observe today

One provider attempt is instrumented at three points, all sampled by the agent
harness:

- `start_request()` — `crates/muta-agent/src/agent/rounds.rs:381` (dispatch)
- `mark_stream_ready()` — `rounds.rs:396` (the `stream_chat_events` future resolves,
  i.e. **HTTP response headers parsed**)
- `observe_stream_event(.., Instant::now())` — `rounds.rs:590` (first output-bearing
  SSE event, last one, stream end)

Everything in between is a black box owned by `reqwest` + `hyper` + the kernel.
`RequestPerformance` (`crates/muta-contracts/src/token_ledger.rs:96-158`) therefore
records five offsets (`stream_ready_us`, `ttft_us`, `stream_us`, `tail_us`, `e2e_us`)
whose composition we cannot inspect.

### Why that is not enough

Five concrete failures, all diagnosed from the current code:

1. **`stream_ready_us` is mislabeled.** The UI renders it as `Connect & Handshake
   (DNS + TLS + Gateway + Send Request Payload)`
   (`apps/tui/crates/mutx/src/overlays/telemetry/draw.rs:1037-1057`). On a pooled
   connection it contains no handshake at all; on a cold one it contains DNS +
   TCP + TLS; on an OAuth channel it also contains `AuthStore::load()` and,
   when the token has expired, a full token-refresh round trip
   (`crates/muta-providers/src/oauth/credential_source.rs:189-199`); on the
   OAuth-401 self-heal path it contains **two** requests
   (`crates/muta-llm-client/src/protocol/openai/chat_completions/mod.rs:204-225`).
   The number is correct as a duration and meaningless as a label.
2. **Connection regime is invisible.** `reqwest` defaults to
   `pool_idle_timeout = 90s` (`reqwest-0.13.4/src/async_impl/client.rs:301`).
   Human-paced agent turns routinely exceed that, so a large and unknown share of
   requests pay a full DNS/TCP/TLS handshake inside TTFT, with no marker saying so.
3. **Transport batching is undetectable.** If a proxy buffers and flushes the
   head together with the first body bytes, `headers → first token` reads ≈0 and
   the sample looks faster than the server can possibly be. Nothing in the
   current model can distinguish "server was fast" from "transport batched".
4. **The decode-rate numerator is re-tokenized text.** `streamed_output_tokens`
   is a client-side `cl100k_base` count of streamed text/reasoning/tool payloads
   (`crates/muta-agent/src/agent/mod.rs:743-754`), which is systematically wrong
   for any model that does not use cl100k (`tokenizer.rs:386-400`), and the
   exclusion of the first event's tokens is a two-point patch over a
   many-point estimator problem.
5. **One ambiguous number survives.** `preferred_tps()` silently falls back from
   stream rate to E2E rate (`token_ledger.rs:322-324`) and every surface labels
   the result `Stream TPS` (`draw.rs:581,751,840,1143`), re-introducing exactly
   the conflation ADR-0151 removed.

### Why the current dependency cannot be extended

`reqwest`'s only extension point is `connector_layer`, whose contract is

```rust
L: Layer<BoxedConnectorService>,
L::Service: Service<Unnameable, Response = Conn> + Clone + Send + Sync + 'static,
```

(`reqwest-0.13.4/src/async_impl/client.rs:2458-2470`). The IO type inside `Conn`
is already built and boxed by the time a layer runs, so a layer can observe that a
connect happened but **cannot wrap the socket, cannot see reads or writes, and
cannot reach the file descriptor**. There is no supported way to obtain
per-syscall or per-segment timing through `reqwest`.

### Why the client alone is insufficient

A client-side tap observes what the network *delivered to this host*. It cannot
observe the server's write cadence, prefill, or queueing; a buffering proxy
destroys the correspondence entirely. Any "true model throughput" claim needs a
vantage point near the server. This ADR therefore treats the client tap as **one
end of a two-ended trace**, not as the whole answer.

### Cost of doing nothing

Every latency and throughput figure the product shows is currently
unfalsifiable: we cannot tell a slow model from a cold connection from a
buffering proxy from a mis-set tokenizer, and we have no artefact to recompute a
number from after the fact.

## Decision

Replace the HTTP egress path with a transport we own end to end, and make every
displayed number a pure function of a recorded trace.

### 1. Own the egress path (`muta-net`)

Create `crates/muta-net`. The name is deliberate. **"Transport" is an
overloaded term** — in networking it denotes OSI layer 4 (TCP/UDP), in gRPC it
denotes the HTTP/2 connection, in OpenTelemetry the exporter's wire mechanism —
so it is used here as a *module* (`net::transport` = socket + TLS + IO), never
as the crate name. The crate is the outbound HTTP path (`egress` in operations
vocabulary) and is named for the layer it owns, not for one of its sub-layers.
`muta-http1` (the codec) and `muta-trace` (the trace model) are separate crates
with names that state exactly what they contain.

`muta-net` owns:

- **Resolution**: DNS with per-phase timing (default backend `getaddrinfo` via
  `tokio::net::lookup_host` so `/etc/hosts`, NSS, VPN and search domains keep
  working; optional `hickory-resolver` backend behind a feature for caching).
- **Sockets**: `TcpStream` creation with explicit socket options
  (`TCP_NODELAY` on, as today via reqwest's default), plus the tap attachments
  described below.
- **TLS**: `tokio-rustls` directly, ALPN advertising, session resumption, and
  handshake detail capture (resumed, version, cipher, ALPN). Trust roots via
  `rustls-platform-verifier` where available, `webpki-roots` as the portable
  fallback.
- **IO**: `TimedIo<I>` wrapping the transport stream, implementing `AsyncRead` /
  `AsyncWrite` and recording every syscall boundary.
- **HTTP/1.1 framing**: `muta-http1`, our own client codec (see below). **No
  third-party HTTP implementation ships in the production path.**
- **Policy**: connection pool (per-host idle list, max idle per host, idle
  timeout, poison-on-error), redirects (301/302/303/307/308 with credential
  rules), proxies (HTTP CONNECT + SOCKS5), transparent decompression
  (gzip/brotli, opt-out for SSE), timeouts (connect, write, overall for
  non-streaming, and the streaming idle policy currently in
  `muta-agent::STREAM_IDLE_TIMEOUT`), and the retry classifier.

#### The ownership boundary

Every layer from the DNS query to the parsed HTTP message is ours:

| Layer | Owner | Notes |
|---|---|---|
| Request/response *policy* (redirects, proxies, decompression, timeouts, retry classification, auth injection) | **us** | today hidden inside `reqwest` |
| Connection pool, reuse, idle eviction, poisoning | **us** | today hidden inside `reqwest`/`hyper-util` |
| DNS, TCP (socket options), TLS (rustls), IO tap | **us** | today hidden behind `reqwest` |
| HTTP/1.1 framing (status line, headers, chunked decoding, trailers) | **us** (`muta-http1`) | byte → frame → event mapping is the trace's |
| Kernel TCP/IP | OS | unchanged |

`muta-http1` covers, and is tested against: request serialization; status line
and header parsing (duplicate headers, case-insensitivity, size limits); body
framing for `Content-Length`, `Transfer-Encoding: chunked` (extensions and
trailers), close-delimited bodies and no-body statuses (`HEAD`/204/304);
`1xx`/`100-continue`; keep-alive and `Connection: close` semantics. The `http`
crate (types only, no logic) is retained for `Request`/`HeaderMap`; it is data,
not behaviour.

#### Hyper is a test oracle, not a dependency

`hyper` does **not** ship. It appears only under `[dev-dependencies]`, where a
differential harness runs it against `muta-http1` over recorded byte streams
(real traffic captures plus the adversarial corpus in §Testing); the codec is
accepted when parity holds across the corpus. This is a deliberate posture, not
a hedge:

- the point of the tap is byte → frame → event fidelity; any third-party
  implementation keeps its own read buffer in that path and re-introduces the
  exact opacity this ADR exists to remove;
- the codec surface we need is small, enumerable and fully testable (the list
  above), which is why it is owned rather than borrowed;
- it removes the question of a dependency's long-term health from the production
  risk register entirely.

For the record, hyper's health is in fact good: 1.11.0 shipped 2026-07-20 with a
steady cadence (1.6 → 1.11 across 2025–2026), 786 M total / 173 M recent
downloads, 5 162 dependents, formal governance under the `hyperium` org, and
sponsorship from a long list of companies. Its one real weakness is maintainer
concentration — a single maintainer authors roughly 70% of commits and is the
sole maintainer-role holder — which is precisely the kind of risk a
dev-dependency-only posture neutralises. `reqwest`, by contrast, is removed
outright (decision 1b) because its extension points cannot express the tap at
all.

### 1b. `reqwest` is removed everywhere, not just on the LLM path

`reqwest` is currently a workspace dependency used by six crates. All of it
becomes `muta-net` egress, in this order:

| Crate | Call sites | Why it belongs in the trace |
|---|---|---|
| `muta-llm-client` | `client.rs`, `endpoint.rs`, `protocol/*` | the LLM attempt itself |
| `muta-providers` | `list_models.rs`, `oauth/mod.rs`, `oauth/device.rs`, `oauth/chatgpt_device.rs` | token refresh happens **inside** the current TTFT window; catalog refresh happens inside the stream loop |
| `muta-mcp` | `client.rs` | MCP HTTP transport |
| `muta-skills` | `remote.rs` | remote skill fetch |
| `muta-models-dev` | `lib.rs` | catalog fetch |
| `muta-agent` | `tools/search/*`, `tools/reader/*` | web search/reader tools |

One egress layer, one trace, one retry classifier, one timeout policy. CI gains
a `cargo tree -i reqwest` gate that fails while the dependency remains.

### 2. Three-level tap with graceful degradation

| Level | Scope | Availability | What it yields |
|---|---|---|---|
| **L0** | Protocol events | All platforms, always on | `head`, `body chunk`, `trailer`, `eof`, SSE events |
| **L1** | Syscall tap | Linux (and any platform for the read/write part) | `(Instant, dir, len)` per `read`/`write`, `recvmsg` kernel RX timestamps, `TCP_INFO` samples |
| **L2** | Segment capture | Linux, opt-in, `CAP_NET_RAW` | Per-segment `(t, len, tcp flags, seq, ack)`, retransmission detection |

L0 is mandatory and portable. L1 is the default on Linux: `TimedIo` records every
syscall boundary, and while a request is in flight a sampler polls `TCP_INFO`
(1 ms cadence, on a `dup`ed fd) for `rtt`, `bytes_acked`, `bytes_sent`,
`unacked`, `notsent`, `cwnd`, `retrans`. The `recvmsg`/`SO_TIMESTAMPING`
(`SOF_TIMESTAMPING_RX_SOFTWARE | SOF_TIMESTAMPING_SOFTWARE`) path is **gated on
P0 evidence** that syscall-boundary timestamps are insufficient — until then L1
is syscall boundaries plus `TCP_INFO`. Where it is built, each returned buffer
carries a kernel timestamp and the coalescing ambiguity is recorded as a
per-event `coalesced` flag rather than hidden. L2 is **not a capability of the daemon**: it ships as a
separate opt-in probe binary (`muta-net-probe`) that the user runs explicitly with
the required privilege, so the always-running process never carries
packet-sniffing authority. It compiles out on non-Linux.

No level is required for correctness of the product: the tap degrades, and the
trace records which levels were active (`TraceFidelity::{L0, L1, L2}`) so a
consumer can never mistake a coarse trace for a fine one.

### 3. Trace as the single source of every number

Create `crates/muta-trace`:

```rust
pub struct RequestTrace {
    pub id: TraceId,                    // == x-muta-request-id
    pub key: RequestUsageKey,           // session/round/turn/attempt
    pub host: HostInfo,                 // provider, model, endpoint, remote addr
    pub connection: ConnectionInfo,     // reused, age_ms, local_port, dns/tcp/tls phases
    pub fidelity: TraceFidelity,        // which tap levels were active
    pub events: EventLog,               // delta-encoded ring buffer
    pub counters: TraceCounters,        // bytes/events/overflow, never lossy
    pub derived: DerivedTimings,        // pure function of `events`
}
```

Events are delta-encoded `(dt_ns: u32, kind: u8, a: u32, b: u32)` — 12 bytes each,
so a 10 000-event stream is ~120 KB. A ring buffer caps a request at 256 K events
with overflow counted, never dropped silently. The last N (default 8) completed
traces are retained in memory; traces are persisted **on demand** (inspector
open, error, or explicit capture) or when a sample crosses a configured anomaly
threshold. The trace is a diagnostic artefact, **not** a permanent ledger.

Derived timings are pure functions in `muta-trace::derive`, with the raw trace as
their only input. `RequestPerformance` becomes a projection of
`DerivedTimings`; no surface may compute a timing of its own.

Mandatory scopes, each with an explicit anchor and a validity verdict:

| Scope | Anchor → anchor | Validity |
|---|---|---|
| `dns_us` / `tcp_us` / `tls_us` | phases of connection setup | absent on a reused connection |
| `ttfb_us` | dispatch → response head | always |
| `first_byte_us` | dispatch → first body byte | always |
| `first_frame_us` | dispatch → first origin protocol frame | always |
| `ttft_us` | dispatch → first output-bearing token | always |
| `server_ttft_us` | first origin frame → first output token | `None` when head and body were batched (`first_byte_us - ttfb_us < 2 ms`) |
| `stream_us` / `decode_tps` | batch-aware robust slope over the token timeline | `None` below 16 tokens or 4 batches; CI reported |
| `e2e_us` / `goodput_tps` | dispatch → validated response | always, labeled E2E |
| `rtt_us` / `retrans` / `cwnd` | `TCP_INFO` | Linux L1 only |
| `queue_us` / `prefill_us` / `decode_us` / `write_cadence` | gateway-injected | `timing_source: Provider` |

**Invariant:** no fallback substitution between scopes. A missing scope renders
`–`. Aggregates use one declared statistic (median for latency, token-weighted
for rates) and never mix scopes under one label.

#### Trace discipline

- **No payloads by default.** An event records direction, length and
  classification — never body bytes, never header values, never credentials, so
  secrets cannot enter a trace by construction and the inspector renders lengths
  and timings only. The single exception is an explicitly invoked, size-bounded,
  in-memory `payload capture` mode for protocol debugging: off by default, never
  enabled by a config default, never persisted, and never part of an aggregate.
- **Hot path.** The tap never allocates, never takes a lock shared across
  connections, and never blocks the IO future. Each connection owns its trace
  buffer; a full ring drops the oldest event and increments `overflow` — counted,
  surfaced, never silent.
- **Per-event provenance.** Every timestamp carries its source
  (`syscall` | `kernel_sw` | `pcap`) and its clock domain. Kernel timestamps
  arrive in the `CLOCK_REALTIME` domain and are converted through an offset pair
  recorded at sampling time; intra-process durations use `CLOCK_MONOTONIC`.
- **Self-measurement.** The tap counts its own cost (events/s, p99 record time,
  drops) and exposes it beside the metrics it produces, so the instrument is held
  to the same standard as the instrumented.
- **Bounded export.** A trace crosses the wire only on demand, size-capped and
  chunked; persistence is opt-in and bounded.

### 4. Two-ended trace

Every attempt sends `x-muta-request-id` (UUIDv7). The gateway echoes it and adds
a bounded `x-muta-server-trace` header (base64 of a compact message) carrying
`queue_start`, `prefill_start`, `first_token`, `last_token`, and a write-cadence
digest; the full server trace, when wanted, is fetched on demand by id. Client and
server traces join on the id. **Cross-host comparison uses durations and ordering
only** — no clock synchronization is attempted, and no offset is invented.

## Testing

- **Hermetic scripted server**: a tokio TCP server emitting bytes on a scripted
  schedule — one SSE event split across segments, a mid-stream stall, head and
  body flushed together, half-close, RST — producing golden traces with asserted
  derived values.
- **Differential codec testing**: `muta-http1` and hyper parse the same recorded
  byte streams; every divergence is a failure. The corpus is real captures plus
  an adversarial set (chunk extensions, trailers, duplicate headers, obs-text,
  `Content-Length` + `Transfer-Encoding` conflict, `1xx`, `HEAD`/204/304,
  oversized headers) and is fuzzed with `cargo-fuzz` under a no-panic discipline
  and hard size limits.
- **Network matrix**: `tc netem` for delay, jitter, loss, reorder and duplication;
  derived metrics must stay within stated bounds and never panic.
- **Cross-check**: on Linux CI, compare L1 timestamps and `TCP_INFO` against
  `tcpdump` and `ss -ti` on the same traffic.
- **Overhead gate**: benchmark the tap on/off; the CI budget is the acceptance
  criterion below.

## P0 gate: measured so far

`crates/muta-net/tests/overhead.rs` measures the tap hermetically and prints its
numbers, so a regression shows up as a changed line rather than only a red test.
On the development host:

| Criterion | Budget | Measured |
|---|---|---|
| Recording one event | ≤ 20 µs | p50 60 ns, p99 70 ns, **p99.9 1.55 µs** (max 58 µs — an amortized ring-growth outlier, ~18 reallocations per 200 000 events) |
| Tap cost on the data path | ≤ 20 µs/event | **+1.84 µs per loopback round trip** (two events) |
| Timestamp fidelity | arrival, not issue | read event lands **126 µs** after the peer's write instant on loopback (tolerance 2 ms) |
| Segment-level cross-check | ≤ 1 ms p99 vs capture | `scripts/check-tap-fidelity.sh` — runs `crates/muta-net/examples/tap_fidelity.rs` under `tcpdump` and compares inter-arrival deltas; **skips loudly** without root/tcpdump rather than reporting a pass |

### Live shadow, one real channel

`crates/muta-llm-client/examples/live_shadow.rs` ran the same provider request
through both transports against `https://api.deepseek.com/v1/responses`
(Responses API, reasoning model), five times, with `MUTA_EGRESS=net` selecting
the owned transport:

| Observation | Result |
|---|---|
| Transport selection | `MUTA_EGRESS=net` → owned |
| Requests completed | **5/5** on both transports, each ending in a terminal `completed` event |
| Assistant text | identical **5/5** |
| Owned trace scopes | `dns_us`, `tcp_us`, `tls_us` (≈95–120 ms cold), `ttfb_us` (≈218–332 ms), `first_byte_us`, `rtt_us` (38–61 µs from `TCP_INFO`) all measured |

Two findings from running it changed the design:

1. **A live endpoint is stochastic, so payload equality cannot be the live
   criterion.** Across the five pairs the event *shapes* differed (the reasoning
   model emitted thinking in some calls and not others) while the text matched.
   `ShadowReport::agrees()` therefore asserts *completion* (both streams finished
   without error), and `payloads_identical()` is reported separately as the
   hermetic, deterministic-endpoint check. Without that split the runtime shadow
   would warn on every live request and be ignored.
2. **Reasoning presence is model behaviour, not transport behaviour.** A shadow
   that treated "owned produced no reasoning frame this time" as divergence
   would be measuring the model's mood. The report says so rather than guessing.

### The cutover is the default, and it is enforced

`net-transport-default` is a **default feature**: an unset `MUTA_EGRESS` now
means the owned transport, and `MUTA_EGRESS=reqwest` selects the oracle in builds
that compile it. `reqwest` itself is an *optional* dependency behind
`reqwest-oracle`, so the production graph contains no third-party HTTP
implementation at all — `cargo tree -p muta-llm-client -i reqwest` reports "did
not match any packages" in a default build.

`scripts/check-egress-deps.sh` turns that into a gate: it fails if `reqwest`
reappears in the default graph, verifies the oracle still resolves, and requires
every crate that still declares `reqwest` non-optionally to be on a documented
allow-list. The list is a ratchet — it may only shrink. Today it holds
`muta-agent` (web tools, under concurrent refactor).

Reaching a green flip required two real fixes the old code had hidden: the
owned transport now honours a request's overall deadline (including the body
read, matching reqwest), and it maps `NetError` onto the provider error the retry
classifier reads, so connect/timeout failures stay retryable.

The L2 probe is `crates/muta-net-probe`: a separate opt-in binary (never a
daemon capability) whose Ethernet/IPv4/TCP parser is pure and unit-tested, so
the only untested part is the socket.

## Egress inventory (complete)

Every HTTP egress path now runs on `muta-net`, and `reqwest` appears only as the
optional differential oracle:

| Crate | Egress | Notes |
|---|---|---|
| `muta-llm-client` | `Egress` seam | `crate::RequestBuilder` replaced `reqwest::RequestBuilder`; `reqwest` is behind `reqwest-oracle` |
| `muta-providers` | `src/http.rs` | OAuth (token/device/chatgpt-device), usage probes, model discovery |
| `muta-mcp` | Streamable-HTTP request + notify | stdio transport unchanged |
| `muta-skills` | `remote.rs` `Fetcher` | skill index + file downloads |
| `muta-models-dev` | catalog fetch | `api.json` with a 10 s deadline |
| `muta-runtime` | URL/proxy validation | uses `muta_net::Target::from_url` / `Proxy::parse` |
| `muta-agent` | `tools/web/http.rs` `WebHttp` | search backends + reader; `max_redirects = 0` so the SSRF guard sees every hop |

`scripts/check-egress-deps.sh` asserts this, and its allow-list is empty:
`cargo tree --workspace --edges normal -i reqwest` finds nothing.

## The latency timeline and the single rate

The Performance surfaces were rebuilt on the recorded trace, replacing the
`Connect & Handshake` / `Prefill & Server Queue` waterfall and the two competing
rates:

- **One timeline, from Enter to the settled turn.** The TUI records when the
  composer was submitted; the ledger records dispatch; the trace records
  connect, upload, headers, first frame, first token, last token and EOF. The
  rows are `Enter → Request dispatched → Connection ready → Request sent →
  Response headers → Server started → First token → Last token → Stream closed →
  Turn end`, each with its duration and what it means. The gap between Enter and
  dispatch is the daemon's own work (queueing, context projection, hooks) — it
  used to be invisible.
- **TTFT is anchored at "request sent", not at dispatch.** The peer's TCP ACK is
  the kernel's business and the application cannot see it; the closest
  observable instant is the last request byte handed to the kernel. That
  excludes connection setup and the upload, which are shown as their own rows,
  so nothing is hidden — the perceived wait remains visible as
  `first token − dispatch`.
- **One rate: `output_tokens / (last token − first token)`.** `output_tokens` is
  the attempt's completion count — the provider's when it reported one, the local
  estimate otherwise — so a reader can reproduce the division from two numbers
  on screen. The end-to-end rate is deleted: it answered a different question in
  the same units, and no label can carry that. A span below 20 ms, fewer than two
  output events, or an implausible result renders `–` rather than a substitute.
- **Transport timings reach the ledger.** `muta-net` derives them from its own
  trace when the body ends; `muta-llm-client`'s egress hands them up through
  `muta_contracts::TransportTimings` and `Provider::take_transport_timings`;
  the agent merges them into the attempt's `RequestPerformance` (DNS, TCP, TLS,
  request-sent, RTT, retransmits) exactly once per attempt.

## Alternatives considered

- **Keep `reqwest`, add `connector_layer`/`dns_resolver` instrumentation only.**
  Rejected: it yields connect totals and DNS, but the IO type is sealed, so no
  per-read timing, no fd, no `TCP_INFO`. It fixes one of five failures.
- **Vendor-patch `reqwest` in `[patch.crates-io]` to wrap the IO.** Rejected as
  the primary path: it buys L1 timestamps while keeping a permanent fork against
  a fast-moving crate, and it still cannot expose the fd cleanly. Recorded as the
  cheapest escape hatch if the owned transport slips.
- **Client-local TCP passthrough proxy (sidecar).** Rejected as the primary path
  because it adds a hop, cannot see plaintext (so cannot inject or read
  `x-muta-*` headers), and still needs a separate tap protocol; **accepted as an
  optional diagnostic sidecar** for capture on machines where L1/L2 are
  unavailable.
- **eBPF.** Rejected for the product: needs privileges, is not portable, and
  produces a second source of truth. Accepted as an offline deep-dive tool.
- **Keep hyper as the shipping HTTP/1.1 codec (own only below/around it).**
  Rejected for the primary path: it leaves a foreign read buffer between the IO
  tap and the body frames — the exact opacity this ADR exists to remove — and it
  keeps a dependency in the production path that the tap's own goals argue
  against. Retained in `[dev-dependencies]` as the differential oracle, which is
  where its correctness is worth the most to us.
- **Rewrite the codec without an oracle.** Rejected: the codec is owned, but it is
  not accepted on faith; differential testing against a reference implementation
  over recorded byte streams is what makes ownership safe.
- **Keep the three-anchor model and only fix the labels.** Rejected: it cannot
  answer "was this a cold connection", "did the transport batch", or "what was
  the server's cadence" — the questions the telemetry exists to answer.

## Consequences

### Positive

- Every timing and rate becomes reproducible from a recorded artefact, and every
  scope carries its own anchor, validity rule, and provenance
  (`ClientObserved` vs `Provider`).
- Cold-connection, OAuth-refresh, upload and batching contamination are not
  estimated away — they are measured and attributed.
- `TCP_INFO` gives ACK-level truth (RTT, retransmits, cwnd) that no HTTP-level
  library exposes, which turns "the network feels slow" into a number.
- The retry classifier, timeout policy and pooling move into code we own and can
  test directly, instead of being derived from a foreign error enum.

### Negative

- A real transport implementation: pool, TLS roots, redirects, proxy,
  decompression **and the HTTP/1.1 codec** all become our maintenance surface.
  Estimated 1.5–2.5k lines plus tests.
- L1/L2 are Linux-first; other platforms get L0 only and must say so.
- L2 requires privileges and is therefore never on by default.
- Three new crates (`muta-net`, `muta-http1`, `muta-trace`) and a new wire surface
  (trace fetch) to keep coherent.

### Neutral

- The client stays HTTP/1.1-only (as today: the `http2` feature is not enabled
  anywhere in the workspace). L0 events are protocol-neutral, so adopting HTTP/2
  later is an additive change to the codec and the frame-level tap, not a rewrite
  of the trace model or the derived metrics.
- `RequestPerformance` keeps its field names where they remain true; fields whose
  meaning was wrong (`stream_ready_us` as "connect") are re-derived from the trace
  rather than renamed ad hoc.
- Protocol adapters (`protocol/{openai,anthropic,google}`) keep their request and
  event shapes; only their send path changes.

### Migration

| Phase | Work | Exit criterion |
|---|---|---|
| **P0** | `muta-trace` model + L0/L1 tap behind a flag, shadow mode | read-event timeline matches `tcpdump` within 1 ms p99; `TCP_INFO` rtt matches `ss -ti` within 10% |
| **P1** | `muta-net`: resolver/socket/TLS/IO/pool/policy + `muta-http1` codec, differential-tested against hyper as a dev-dependency; LLM adapters ported | parity across the recorded corpus; byte-identical response corpus against the reqwest path under shadow |
| **P2** | Cutover; delete `client.rs`/`transport.rs`; migrate all six reqwest call-site crates | `cargo tree -i reqwest` gate passes; classification test suite passes unchanged |
| **P3** | Gateway injection + join; `timing_source: Provider` | server-side scopes populated end to end for at least one channel |
| **P4** | L2 module + transport inspector + trace export | inspector renders a golden trace; L2 retransmits match `ss -ti` |

**P0 is a gate, not a formality.** If the shadow run shows that read-boundary
timestamps do not correspond to segment arrivals closely enough for L1 to earn
its cost, the response is to narrow the programme (keep L0 plus connect
decomposition, drop L2 and the owned codec) rather than to push through. Every
phase is reversible: the new client ships behind a flag with the current path as
the fallback until P2's exit criterion is met, and the new `RequestPerformance`
fields are optional `serde` fields with `ts_rs` exports, so the wire stays
compatible in both directions without a protocol bump (ADR-0134).

### Acceptance criteria

1. Every displayed timing/rate is a pure function of a trace; snapshot tests over
   golden traces prove it.
2. No cross-scope fallback; a `validity` verdict is mandatory on every derived
   scope; non-estimable scopes render `–`.
3. Tap overhead ≤ 1% CPU and ≤ 20 µs p99 per event at 1 000 events/s, gated in CI.
4. Zero silent event loss up to ring capacity; overflow is counted and surfaced.
5. L1 timestamps agree with packet capture within 1 ms p99 on the CI corpus.
6. Retry classification behaviour is unchanged (existing tests are the oracle).
7. No third-party HTTP implementation in the production dependency graph after
   P1; `reqwest` absent after P2; `hyper` present only under `[dev-dependencies]`
   (CI gates both).

## References

- ADR-0151 — per-attempt client-observed performance telemetry.
- ADR-0157 — single TTFT definition.
- ADR-0184 — incremental streaming pipeline, single-parse hot path.
- ADR-0122 — durable cross-session usage statistics (settlement fan-out the
  derived metrics ride on).
- Code: `crates/muta-agent/src/agent/mod.rs:641-942`,
  `crates/muta-agent/src/agent/rounds.rs:381-697`,
  `crates/muta-llm-client/src/{client,transport,sse}.rs`,
  `apps/tui/crates/mutx/src/overlays/telemetry/`.
- Prior art: `reqwest` pool defaults, `SO_TIMESTAMPING` (Linux), `TCP_INFO`
  (`tcp(7)`), `hyper::client::conn::http1` (differential oracle only),
  RFC 9112 (HTTP/1.1).
