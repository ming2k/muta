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
invisibly by the OS kernel, and where muta hooks into the lifecycle. Those hooks
live in `netune`, the owned egress path — transport, byte-level tap, connection
pool and trace vocabulary — extracted as an independent library
([ADR-0210](../../adr/0210-extract-netune-transport-library.md)).

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

The **Layer** column states where each exchange can be *seen* — a different
question from where it happens:

- **`Kernel`** — the segment exists only inside the kernel network stack. No
  user-space event corresponds to it, so it can never become a trace moment.
- **`Kernel → user`** — the kernel owns the segment, but its completion surfaces
  as a user-space syscall boundary. What is observable is the syscall's return,
  not the segment: the moment is real, while the cost attributed to the segment
  is an inference drawn from it.
- **`User`** — the exchange itself runs in user space, so the boundary and the
  payload are both directly observable.

The **Recordable** column is a plain predicate: whether any moment in this
table's sense is produced at all. *How* it is reached is the `Layer` column's
answer, and *what it asserts* is the next one. A moment is always a *boundary the
caller crossed*, never a claim about the peer's state.

| Packet # | Content & Type | Layer | Recordable | Trace moment — what it means | Mechanism & Where muta Observes |
|---|---|---|---|---|---|
| **Packet 1** | `SYN` | `Kernel` | No | — | The kernel constructs and transmits it during `connect()`. The call is the nearest anchor, but it spans Packets 1–3 as one interval; the segment itself exposes no boundary. |
| **Packet 2** | `SYN + ACK` | `Kernel → user` | Yes | **`TcpEnd`** — the local handshake completed. Receiving this segment moves the socket to ESTABLISHED and wakes the `connect()` waiter, whose return is the moment. The paired span `TcpStart` → `TcpEnd` is the TCP phase: one round trip as the caller sees it (SYN out → SYN+ACK in). | Processed in softirq. The kernel validates it, transitions the socket, emits Packet 3 and wakes the waiter — all before user space runs again. |
| **Packet 3** | `ACK` | `Kernel` | No | — | Emitted by the stack in the same pass that consumed Packet 2, after the state transition and after the wakeup. It is a consequence of the observable, not the observable itself. |
| *(TLS)* | TLS Handshake (1-2 RTTs) | `User` | Yes | **`TlsStart` / `TlsEnd`** — the handshake completed. The span is pure user-space time, and `TlsInfo` records the negotiated ALPN and TLS version, so whether this phase cost one RTT (TLS 1.3) or two (TLS 1.2) is resolvable per attempt rather than assumed. | rustls runs the whole handshake in user space over the established socket. See below on why this phase takes a phase row instead of a packet number. |
| **Packet 4** | `PSH + ACK` (HTTP Payload) | `User` | Yes | **`RequestWriteEnd`** (`Request sent`) — the last request byte entered the kernel send buffer. It does **not** assert that the server received the request; it is the closest defensible anchor for that, which is why the measurement problem below is stated in terms of this moment. | The application invokes `write()` / `poll_write()` on the byte-level tap (`netune::TimedIo`); the kernel copies the bytes into the socket send buffer. |
| **Packet 5** | `ACK` (Server ACKs Request) | `Kernel` | No | — | A pure transport-layer ACK: it advances the kernel's sliding window and wakes no user-space task. This is precisely why `Request sent` anchors on the local write rather than on the server's acknowledgement. |
| **Packet 6** | `PSH + ACK` (HTTP 200 Headers / First Chunks) | `User` | Yes | **`HeadComplete`**, then `BodyStart` / `ChunkBoundary` — the head finished parsing and the first body bytes were read. The moment is when the reading task ran, so it carries scheduler delay on top of arrival: the bytes reached the host at or before it, which makes the moment an upper bound on the host's receipt time and the latency it yields a pessimistic one, never a flattering one. | The kernel strips the TCP/IP headers and buffers the payload in the socket receive buffer; the read path (`netune::TimedIo`) receives it. When the head and the first body byte arrive within the batch gap, the interval between them is refused as `TransportBatched` rather than reported as origin latency. |
| **Packet 7** | `ACK` (Client ACKs Data) | `Kernel` | No | — | The kernel transmits it unprompted. No callback or event exists for it. |
| **Packet 8** | `FIN + ACK` (Client Closes) | `Kernel` | Yes | **`ConnectReused`** — the moment this packet was *not* sent, because the socket returned to the pool. It describes the connection regime of the request that picks the socket up next, not of the one being traced. | Under HTTP keep-alive the FIN is never emitted and `netune::Pool` retains the socket. An explicit close does originate in user space, but it produces no observation boundary, and `netune` records no close event. |
| **Packet 9** | `ACK` | `Kernel` | No | — | Kernel state machine only: the socket transitions to `FIN_WAIT_2`. |
| **Packet 10** | `FIN + ACK` (Server Closes) | `Kernel → user` | Yes | **`BodyEnd`** — the response body was fully consumed. That coincides with EOF only when the body is close-delimited; for a length-delimited or chunked body the moment lands on the terminator, long before any FIN. | The kernel receives the server's FIN and collapses it into a sentinel: the next `read()` returns `0`. Data's absence is the entire signal. On a pooled connection an arriving FIN belongs to an idle socket and is not part of any request's timeline. |
| **Packet 11** | `ACK` | `Kernel` | No | — | The kernel replies with the final ACK and enters `TIME_WAIT`. The socket handle is already closed or pooled. |

### Why a Segment Cannot Be Reached: From the Wire to the Application

A received frame climbs a fixed ladder, and **every rung but the top is
invisible to the process**. A packet stops climbing at the first rung whose
outcome no user-space code observes, and a trace can only record the top.

```text
  rung                        what happens there
  ────────────────────────────────────────────────────────────────────────────
  6  application              read() / write() copy between the socket buffer
                              and user memory; the TLS handshake is the other
                              phase that runs up here
                              ► Packet 4 (write) · Packet 6 (read) · TLS

  ── the wakeup: the only door between kernel and user space ─────────────────
  5  reactor                  the readiness callback marks the fd → tokio reactor
                              → the task is scheduled to run
                              ► user space runs here, and only where a waiter
                                was actually woken

  4  input path: woken        SYN+ACK → ESTABLISHED and the connect() waiter ·
     (softirq)                data → queued on the receive queue, reader woken ·
                              FIN → CLOSE_WAIT, next read returns EOF
                              ► Packet 2 · Packet 6 · Packet 10

  3  input path: silent       pure ACK: the send window advances, the call
     (softirq)                returns. No waiter exists, so nobody is woken and
                              no task ever runs on its behalf
                              ► Packet 5 · Packet 9 · Packet 11

  ── below this line no user-space code runs, so no moment can exist ─────────
  2  IP + driver + NAPI       ring drained, headers stripped; our own ACK and FIN
     (softirq)                are emitted from here without entering user space
                              ► Packet 3 · Packet 7 · the FIN of Packet 8, when
                                it is emitted at all

  1  NIC + DMA                the frame lands in a descriptor ring; the hard
     (hard IRQ)               interrupt handler only schedules the poll
                              ► Packet 1

  0  wire                     ──── unprivileged user space cannot look below ──
```

The input path splits into rungs 4 and 3 on a single question — **is any task
waiting?** That split, not the packet type, is what decides whether a moment is
available. Our own outbound ACK and FIN cross no boundary at all, which is why
Packets 3 and 7 are unrecordable even though the client is the one emitting them.
Packet 8 is the exception that proves the rule: what a trace records there is
not the segment but the *decision not to produce it*, taken in user space when
the pool keeps the socket.

Three distinct reasons a segment produces no moment, in the order they arise:

1. **It terminates below the wakeup.** A pure acknowledgement advances kernel
   bookkeeping and returns. No task is waiting on it, the kernel schedules
   nobody, and an unobserved state change leaves no boundary behind — so there is
   nothing a timer could have been started against. Packets 5, 9 and 11, plus
   every acknowledgement the client itself emits, end here.
2. **It reaches the wakeup but stops at the state change.** Where a wakeup does
   happen — the SYN+ACK that unblocks `connect()`, the FIN that surfaces as EOF —
   user space still learns a *state it can query*, not the segment. The moment is
   genuine; attributing the segment's own cost to it is the inference the
   `Layer` column marks.
3. **The read path coalesces.** Whatever does reach the application arrives
   batched: several data segments can accumulate on the receive queue and be
   handed to a single `read()`. The observed event count therefore counts *reads
   the application performed*, not segments the peer sent — which is why a peer
   delivering a burst and a peer decoding steadily can look identical at the
   syscall boundary. An event count is not a segment count, and cannot be used as
   one.

That last point is the whole reason the observability is asymmetric in the first
place: the client can be arbitrarily far from the wire and still be exactly
right about what it did observe. Climbing below the application rung requires
leaving the default posture — kernel receive timestamps or an actual capture, the
two techniques the optional L2 probe exists for.

### Why TLS Takes a Phase Row Instead of a Packet Number

This table enumerates **TCP control-packet semantics**: SYN, SYN+ACK, ACK and FIN,
plus the two payload slots (Packets 4 and 6) that are listed so the control
packets can be located in the timeline. TLS adds no control semantics of its own —
every handshake byte travels inside ordinary `PSH + ACK` data segments, that is,
inside the Packet 4 and Packet 6 slots. There is no packet to number.

Three properties make the phase row the correct shape, in increasing order of
force:

1. **The segment count is not fixed.** A TLS 1.3 `ClientHello` is one segment; the
   server's flight (ServerHello, EncryptedExtensions, Certificate,
   CertificateVerify, Finished) is one to several, and a long certificate chain
   splits it further. The plaintext rows can be numbered because a three-way
   handshake is exactly three segments and the request/response lifecycle that
   follows is fixed.
2. **The contents are encrypted after ServerHello.** TLS 1.3 protects everything
   from EncryptedExtensions onward under handshake traffic keys, so packet-level
   visibility would not decompose the handshake even where it exists — a capture
   shows opaque records.
3. **It does not need packet visibility.** Because rustls drives the handshake in
   user space, the phase is measurable *without* any packet-level access, and it
   is in fact the only connection-setup phase measured directly at both ends.
   That is what makes the phase row sufficient rather than a concession.

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
single ambiguous number, muta’s Session Telemetry modal (`Ctrl+O`) renders the
turn as a ladder of moments. Seven rows are always present; two appear only when
their moment was actually recorded — the composer timestamp behind `User
request`, and a connection the transport observed.

```text
LATENCY TIMELINE
  From Enter to the settled turn — one row per stage

    0.00s ● Enter                you submitted the prompt
  │
    0.41s ● Dispatched           0.41s local: queue, context projection, hooks
  │
    0.55s ● Connected            cold start: DNS 21ms + TCP 38ms + TLS 92ms
  │
    0.67s ● Request sent         upload took 0.12s
  │
    1.63s ● Server started       headers 0.96s · first frame 0.97s from dispatch
  │
    2.31s ● First token          TTFT 1.64s after the request was sent · 1.90s from dispatch
  │
    3.99s ● Last token           streamed 1.68s · 210 tok @ 125.0 tok/s
  │
    4.00s ● Stream closed        0.01s after the last token
  │
    4.01s ■ Turn end             validated after 3.60s
  socket: RTT 42ms · retransmits 0
```

### The Lifecycle Milestones

1. **`User request` ($t_0$)**: Captured synchronously by the TUI composer
   (`last_submit_ms`) at the instant the user presses Enter. Drawn only when that
   stamp is available, since it is the one moment the daemon cannot observe.
2. **`Dispatched`**: Recorded when the agent loop enters
   `request_accounting.start_request()`. The gap ($t_1 - t_0$) exposes local
   daemon processing: session lock acquisition, context pruning, skill injection,
   and `TurnStart` lifecycle hooks.
3. **`Connection ready`**:
   - On cold connections: measures exact `DNS`, `TCP`, and `TLS` durations via
     `netune`.
   - On warm connections: labeled explicitly as `reused pooled connection`,
     ensuring zero setup latency is credited or debited.
   - Either way the row is placed on **the end of the last phase actually paid** —
     `TLS` end when a handshake ran, `TCP` end otherwise, or the instant the pool
     handed the socket over. It is deliberately *not* placed on the response
     head: a connection is ready before the request is written, and a row
     anchored on the head would render after `Request sent` and make the ladder
     run backwards.
   - A record whose socket nothing observed draws no row at all, rather than one
     at an instant nobody measured. The same three-valued honesty governs the
     socket summary below it: the retransmit count is shown only where `TCP_INFO`
     actually sampled, because the RTT and the count come from one sample — a
     present RTT is what separates a measured zero from an untouched field.
4. **`Request sent`**: Triggered when `netune::TimedIo` finishes writing the
   final request byte into the socket buffer.
5. **`Server started`**: Marks the first origin frame together with the parsed
   response head — the two instants are usually milliseconds apart, so they share
   one row and the detail carries both costs (the head is parsed by
   `netune-http1`, the frame is the first origin-emitted protocol event, such as
   an OpenAI role chunk, Anthropic `message_start`, or a thinking preamble). The
   frame is what proves the upstream inference engine has scheduled the response.
6. **`First token` (TTFT)**:
   - **Perceived TTFT**: $t_{\text{first\_token}} - t_{\text{dispatch}}$ (total
     time the user waited).
   - **Network/Server TTFT**: $t_{\text{first\_token}} - t_{\text{request\_sent}}$
     (pure time spent waiting on the wire, server queue, and prompt prefill).
7. **`Last token`**: Marks the final content-bearing token event. The span
   between `First token` and `Last token` is the exact denominator for the
   **Streaming Rate**.
8. **`Stream closed` & `Turn end`**: Captures trailer parsing, stream finalization,
   and tool-call validation before execution begins.

### Kernel TCP Telemetry (`TCP_INFO`)

On Linux hosts, `netune` duplicates the underlying socket file descriptor and
polls `getsockopt(TCP_INFO)` at a 250ms cadence in a background task. Sampling is
change-driven: only a *change* in `(rtt, retransmits)` becomes an event, so the
trace holds the socket's story rather than its heartbeat.

The two signals it contributes are:
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
