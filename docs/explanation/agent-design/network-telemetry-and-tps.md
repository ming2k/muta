# Client-Observed Network Telemetry, TTFT, and Streaming TPS

This document explains how muta measures Time-To-First-Token (TTFT) and
Tokens-Per-Second (Streaming TPS) from the client boundary, what the application
layer can and cannot observe across the operating system's network stack, and
how the telemetry pipeline distinguishes physical network behavior from upstream
model decode speed.

## The Packet-Level Reality vs. Application Layer Visibility

When an AI coding assistant dispatches a prompt and receives a streamed response,
the underlying communication consists of a sequence of TCP segments and TLS
records. However, user-space applications (using standard POSIX socket APIs like
`connect`, `write`, and `read`) do not have direct packet-level visibility into
every step of this exchange.

The diagram below details the classic TCP lifecycle of an HTTP request alongside
what the client application layer can directly observe, what is managed
invisibly by the OS kernel, and where muta hooks into the lifecycle.

```text
Client                                                     Server
  |                                                          |
  |=================== 1. TCP Three-Way Handshake ===========|
  |                                                          |
  |  [Packet 1] SYN (seq=x)                                  |
  | -------------------------------------------------------> |
  |                                                          |
  |  [Packet 2] SYN + ACK (seq=y, ack=x+1)                   |
  | <------------------------------------------------------- |
  |                                                          |
  |  [Packet 3] ACK (seq=x+1, ack=y+1)                       |
  | -------------------------------------------------------> |
  |   (TCP connection established - ESTABLISHED)             |
  |                                                          |
  |=================== (TLS Handshake: 1-2 RTTs) ============|
  |                                                          |
  |  ClientHello / ServerHello / Key Exchange / Finished     |
  |                                                          |
  |=================== 2. HTTP Request & Response ===========|
  |                                                          |
  |  [Packet 4] PSH + ACK (HTTP Request Payload)             |
  | -------------------------------------------------------> |
  |                                                          |
  |  [Packet 5] ACK (Server ACKs HTTP request)               |
  | <------------------------------------------------------- |
  |  (Note: Packet 5 & 6 are frequently coalesced by server) |
  |                                                          |
  |  [Packet 6] PSH + ACK (HTTP 200 OK + Stream Headers/Body)|
  | <------------------------------------------------------- |
  |                                                          |
  |  [Packet 7] ACK (Client ACKs response data)              |
  | -------------------------------------------------------> |
  |                                                          |
  |=================== 3. TCP Teardown / Keep-Alive =========|
  |                                                          |
  |  [Packet 8] FIN + ACK                                    |
  | -------------------------------------------------------> |
  |                                                          |
  |  [Packet 9] ACK                                          |
  | <------------------------------------------------------- |
  |                                                          |
  |  [Packet 10] FIN + ACK                                   |
  | <------------------------------------------------------- |
  |                                                          |
  |  [Packet 11] ACK                                         |
  | -------------------------------------------------------> |
```

### Packet-by-Packet Observability Matrix

| Packet # | Content & Type | Visible to Application? | Handling Mechanism & Where muta Observes |
|---|---|---|---|
| **Packet 1** | `SYN` | ❌ No direct packet visibility | Kernel network stack automatically constructs and transmits during `connect()`. |
| **Packet 2** | `SYN + ACK` | ❌ No direct packet visibility | Kernel network driver and IP stack receive and acknowledge; invisible to application. |
| **Packet 3** | `ACK` | ⚠️ Indirect (connect return) | Kernel sends final ACK. `connect()` returns `0`. Application knows connection is established, but cannot see the ACK packet. In `muta-net`, this marks `TcpEnd`. |
| *(TLS)* | TLS Handshake (1-2 RTTs) | ⚠️ Phase-level visibility | Managed by `tokio-rustls`. `muta-net` records `TlsStart` and `TlsEnd`, capturing negotiated ALPN and TLS version. |
| **Packet 4** | `PSH + ACK` (HTTP Payload) | ✅ Direct (Active Trigger) | Application invokes `write()` / `poll_write()` in `muta-net::TimedIo`. Kernel copies bytes into socket send buffer. Marks `RequestWriteEnd` (`Request sent`). |
| **Packet 5** | `ACK` (Server ACKs Request) | ❌ Inaudible to User-space | Pure transport-layer ACK. Managed entirely by kernel stack (updating sliding window/ACK sequence). Does not wake user-space. |
| **Packet 6** | `PSH + ACK` (HTTP 200 Headers / First Chunks) | ✅ Direct (Incoming Payload) | Kernel strips TCP/IP headers and buffers payload in socket receive buffer. `muta-net::TimedIo` reads out chunk: HTTP headers parsed (`HeadComplete`), and first SSE chunk read. |
| **Packet 7** | `ACK` (Client ACKs Data) | ❌ Inaudible to User-space | Kernel automatically transmits TCP ACK. Application has no event or callback for this. |
| **Packet 8** | `FIN + ACK` | ⚠️ Indirect (Close Trigger) | Emitted when connection closes. With HTTP keep-alive, this is **suppressed**: idle connections remain pooled in `muta-net::Pool` for reuse. |
| **Packet 9** | `ACK` | ❌ Inaudible to User-space | Kernel state machine transitions (`FIN_WAIT_2`). |
| **Packet 10** | `FIN + ACK` | ⚠️ Indirect (EOF Trigger) | Client kernel receives server FIN. Application `read()` returns `0` (EOF). `muta-net` marks `Stream closed` / `BodyEnd`. |
| **Packet 11** | `ACK` | ❌ Inaudible to User-space | Client kernel replies with final ACK and enters `TIME_WAIT`. Socket handle is already closed or pooled. |

---

## The Measurement Problem: Why Naive TTFT & TPS Lie

### 1. Packet 5 (Server ACK) Is Inaudible

A common theoretical intuition is: *"TTFT should start when the server finishes
receiving the request (Packet 5 ACK) and end when the first token appears."*

However, in standard operating system network stacks:
1. **Packet 5 is never exposed to socket APIs.** A socket read does not unblock
   on an incoming TCP ACK; it unblocks only when incoming *data* (Packet 6) is
   available.
2. In practice, modern HTTP servers and CDNs frequently combine Packet 5 and
   Packet 6 into a single delayed TCP frame, or reply with response headers
   before acknowledging the full upload window.
3. Therefore, the closest defensible client-side anchor for "request delivered"
   is **when the client application finishes flushing the last request byte into
   the kernel send buffer** (`Request sent` / `RequestWriteEnd`).

### 2. Why Connection Reuse Dominates Perceived TTFT

If TTFT is naively measured from user dispatch to first token:
- **Cold connection**: DNS (20–50ms) + TCP SYN/ACK (30–80ms) + TLS handshake
  (60–150ms) + Upload = **150–300ms of setup delay** counted against the model.
- **Pooled connection**: Reuses existing warm TCP/TLS stream = **0ms setup
  delay**.

Without explicit connection regime awareness, a developer comparing models
would conclude the model randomly became 300ms faster or slower, when only the
socket pool state changed.

### 3. Why E2E Rate Was Abolished (The Single-Rate Doctrine)

Past iterations of the performance telemetry supported two rates:
- **Stream TPS**: $\frac{\text{streamed tokens}}{\text{last token} - \text{first token}}$
- **E2E TPS**: $\frac{\text{total tokens}}{\text{request dispatch} \to \text{response validated}}$

This caused severe user-facing confusion:
1. When a turn generated only 3 tokens after a 12-second reasoning prefill, the
   E2E rate showed $\approx 0.25\text{ tok/s}$. Users interpreted this as a
   broken model decode speed, when it was actually a prefill/queue latency issue.
2. Conversely, on single-chunk tools or burst arrivals, systems falling back to
   E2E rate secretly changed the denominator without changing the label.

**The muta rule**: There is **only one throughput metric** — **Streaming
Rate** ($\text{tok/s}$), and it is measured strictly across the active decode
window:

$$\text{Streaming Rate} = \frac{\text{Completion Tokens}}{t_{\text{last\_token}} - t_{\text{first\_token}}}$$

If the duration is under 20ms, or fewer than two output events were observed,
the rate renders as **`–` (unmeasured)** rather than fabricating a fake number.

---

## How muta Solves It: The End-to-End Latency Timeline

Instead of flattening network transit, handshake, prefill, and generation into a
single ambiguous number, muta’s Session Telemetry modal (`Ctrl+O`) breaks the turn
into a deterministic, 9-stage latency timeline:

```text
LATENCY TIMELINE
  From Enter to the settled turn — one row per stage

    0.00s ● Enter                you submitted the prompt
  │
    0.41s ● Request dispatched   0.41s local: queue, context projection, hooks
  │
    0.43s ● Connection ready     reused pooled connection — no handshake
  │
    0.55s ● Request sent         upload complete after 0.12s
  │
    1.44s ● Response headers     server accepted the request · 1.03s from dispatch
  │
    1.45s ● Server started       first frame from the origin · 1.04s from dispatch
  │
    2.19s ● First token          TTFT 1.64s after the request was sent · 2.19s from dispatch
  │
    3.87s ● Last token           streamed 1.68s · 210 tok @ 125.0 tok/s
  │
    3.88s ● Stream closed        0.01s after the last token
  │
    3.89s ■ Turn end             validated after 3.48s
  socket: RTT 42ms · retransmits 0
```

### The 9 Lifecycle Milestones

1. **`Enter` ($t_0$)**: Captured synchronously by the TUI composer
   (`last_submit_ms`) at the instant the user presses Enter.
2. **`Request dispatched`**: Recorded when the agent loop enters
   `request_accounting.start_request()`. The gap ($t_1 - t_0$) exposes local
   daemon processing: session lock acquisition, context pruning, skill injection,
   and `TurnStart` lifecycle hooks.
3. **`Connection ready`**:
   - On cold connections: measures exact `DNS`, `TCP`, and `TLS` durations via
     `muta-net`.
   - On warm connections: labeled explicitly as `reused pooled connection`,
     ensuring zero setup latency is credited or debited.
4. **`Request sent`**: Triggered when `muta-net::TimedIo` finishes writing the
   final request byte into the socket buffer.
5. **`Response headers`**: Marks the instant the server’s HTTP status line and
   headers are fully parsed by `muta-http1`.
6. **`Server started`**: Recorded on the first incoming origin protocol frame
   (such as an OpenAI role chunk, Anthropic `message_start`, or thinking
   preamble), proving the upstream inference engine has scheduled and begun the
   response.
7. **`First token` (TTFT)**:
   - **Perceived TTFT**: $t_{\text{first\_token}} - t_{\text{dispatch}}$ (total
     time the user waited).
   - **Network/Server TTFT**: $t_{\text{first\_token}} - t_{\text{request\_sent}}$
     (pure time spent waiting on the wire, server queue, and prompt prefill).
8. **`Last token`**: Marks the final content-bearing token event. The span
   between `First token` and `Last token` is the exact denominator for the
   **Streaming Rate**.
9. **`Stream closed` & `Turn end`**: Captures trailer parsing, stream finalization,
   and tool-call validation before execution begins.

### Kernel TCP Telemetry (`TCP_INFO`)

On Linux hosts, `muta-net` duplicates the underlying socket file descriptor and
polls `getsockopt(TCP_INFO)` at a 5ms cadence in a background task:
- **Kernel Smoothed RTT (`tcpi_rtt`)**: Ground-truth physical round-trip time.
- **Retransmit Counter (`tcpi_total_retrans`)**: Proves whether stalls or
  jitter were caused by packet drops on the local Wi-Fi / WAN rather than model
  deliberation.

---

## Token Counting & Rate Calculation Discipline

1. **Streaming Rate Numerator**:
   - When the provider reports authoritative usage in stream completion metadata
     (OpenAI `stream_options.include_usage`, Anthropic `message_delta.usage`,
     Google `usageMetadata`), that reported count is used as the numerator.
   - If the upstream omits token usage, muta falls back to its exact incremental
     BPE tokenizer (`muta_contracts::tokenizer::StreamingCounter`), which handles
     merges across chunk boundaries without over-counting.
2. **Defensible Rate Guardrails**:
   - Minimum duration: $\ge 20\text{ms}$ (`MIN_DEFENSIBLE_STREAM_SPAN_US`).
   - Minimum event count: $\ge 2$ stream events.
   - Plausibility ceiling: $\le 2,000\text{ tok/s}$ (`MAX_PLAUSIBLE_STREAM_TPS`).
     Rates above this ceiling indicate batched transport flushes rather than
     token decode cadence, and are displayed as `–`.
3. **Session Aggregation**:
   Round-level and session-level rates are computed as:

   $$\text{Session Stream Rate} = \frac{\sum \text{Turn Completion Tokens}}{\sum \text{Turn Stream Duration (seconds)}}$$

   This guarantees that short single-line turns do not disproportionately distort
   the developer’s insight into overall model throughput.
