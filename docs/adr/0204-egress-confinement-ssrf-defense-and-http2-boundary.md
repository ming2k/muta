# 0204. Egress Confinement, Strict Anti-SSRF Defense, and the HTTP/2 Multiplexing Boundary

- Status: Accepted
- Date: 2026-09-09
- Scope: net, security, transport, web, agent, contracts
- Deciders: Muta maintainers
- Consulted: -
- Informed: -
- Builds on: [ADR-0200](0200-owned-transport-and-packet-level-request-trace.md) (Owned egress path with a packet-level request trace), [ADR-0202](0202-singleton-web-provider-selection.md) (Singleton web provider selection), [ADR-0183](0183-homogeneous-agent-kernel-and-spatiotemporal-aspect-engine.md) (Homogeneous agent kernel and spatiotemporal aspect engine)
- Supersedes: Ad-hoc tool-level URL validation, unpinned pre-connect DNS resolution, and non-binding transport boundary guidelines

---

## Context and Problem Statement

Following the successful migration to an owned egress transport ([ADR-0200](0200-owned-transport-and-packet-level-request-trace.md)) comprising `muta-net`, `muta-http1`, and `muta-trace`, the system has established microscopic observability over outbound HTTP/1.1 requests and eliminated third-party client dependencies.

However, two systemic architectural challenges require an uncompromising, long-term resolution:

1. **Vulnerability to DNS Rebinding and Ad-Hoc SSRF Checks**:
   Autonomous AI agents execute web interactions (`search_web`, `read_url`, MCP tools) based on untrusted inputs and model outputs. Prior defense was implemented as an ad-hoc pre-check (`assert_public_url`) in `crates/muta-agent/src/tools/ssrf.rs`. Because `assert_public_url` resolves DNS independently from the transport client's subsequent socket connect, a classic Time-of-Check to Time-of-Use (**TOCTOU**) DNS rebinding attack window exists: a malicious nameserver returning a public IP with TTL 0 followed by `127.0.0.1` or `169.254.169.254` during connection establishment could pierce the security perimeter and probe internal host services or cloud metadata endpoints.
2. **Protocol Scaling vs. Architectural Opacity (The HTTP/2 Boundary)**:
   As subagents execute concurrent tasks directed at the same LLM gateway or provider endpoint, connection pool limits and serial HTTP/1.1 head-of-line blocking present throughput bottlenecks. The system requires an authoritative architectural contract defining:
   - When and how HTTP/2 multiplexing may be integrated;
   - How to prevent future contributors from falling into the twin traps of either *hand-rolling an entire HTTP/2 state machine from scratch* (maintenance quicksand) or *importing an opaque third-party HTTP runtime like reqwest/hyper* (which would destroy the microsecond-level L0/L1 syscall trace guarantees established in ADR-0200).

---

## Decision Drivers

- **Long-Termism & Zero Baggage**: Move security invariants down into the core transport layer (`muta-net`). Eliminate ad-hoc, call-site dependent validation.
- **Deterministic SSRF Immunity**: Eliminate DNS rebinding by construction through **Transport-Level Resolve-and-Pin**.
- **Non-Negotiable Trace Fidelity**: Every byte transferred across the network must traverse `TimedIo` and record syscall-level timestamps (`muta-trace`), regardless of the higher-level framing protocol.
- **Negative Knowledge Preservation**: Formally document why in-house HTTP/2 framing is rejected and why monolithic HTTP clients remain banned.
- **Fail-Closed Principle**: Any ambiguity in network resolution, address parsing, or redirect destination terminates the request immediately.

---

## Considered Options

### Option 1: Retain Call-Site SSRF Pre-Checks and Defer HTTP/2

Keep `assert_public_url` in `muta-agent` and rely on short-term DNS caching.
- *Rejected*: Inherently insecure against DNS rebinding. Does not protect other egress call sites (`muta-mcp`, `muta-skills`). Leaves the HTTP/2 boundary unmanaged.

### Option 2: Re-introduce Monolithic HTTP Stacks (Hyper/Reqwest) for HTTP/2

Switch providers or web tools back to `reqwest` or `hyper-util` to gain transparent HTTP/2 multiplexing.
- *Rejected*: Violates [ADR-0200](0200-owned-transport-and-packet-level-request-trace.md). Destroys L0/L1 syscall observation, re-introduces connection-pool opacity, inflates the dependency graph, and violates the workspace zero-reqwest gate (`scripts/check-egress-deps.sh`).

### Option 3 (Chosen): Transport-Level Resolve-and-Pin Confinement with a Layered HTTP/2 Seam

1. Embed **Egress Confinement** directly into `muta-net`: DNS resolution yields an IP that is verified against strict globally-routable rules *and directly pinned to the TCP connect call*. The hostname is retained exclusively for TLS SNI and HTTP `Host` headers.
2. Formally specify the **HTTP/2 Multiplexing Boundary**: HTTP/2 multiplexing, when enabled, must sit strictly *above* `muta-net`'s `TimedIo` socket abstraction using a standalone, audited framing parser (e.g. `h2`), ensuring trace invariants remain intact. Hand-rolling an H2 engine from scratch is explicitly prohibited.

---

## Decision Outcome

We adopt **Option 3**.

```text
 ┌─────────────────────────────────────────────────────────────┐
 │                      Egress Call Site                       │
 │  (muta-agent web tools, muta-llm-client, muta-mcp, etc.)    │
 └──────────────────────────────┬──────────────────────────────┘
                                │ URL + ConfinementPolicy
                                ▼
 ┌─────────────────────────────────────────────────────────────┐
 │                         muta-net                            │
 │                                                             │
 │  1. DNS Resolve & Pin: Host -> SocketAddr                   │
 │  2. Confinement Gate: Verify SocketAddr ∈ PublicCIDR        │
 │       (Rejects: Loopback, Private, Link-Local, CGNAT, Meta) │
 │  3. Direct Dial: TcpStream::connect(PinnedSocketAddr)       │
 │  4. TLS SNI & Host: Preserves logical Hostname              │
 │  5. TimedIo Tap: Wraps socket for L0/L1 Syscall Telemetry   │
 └──────────────────────────────┬──────────────────────────────┘
                                │
          ┌─────────────────────┴─────────────────────┐
          │                                           │
          ▼                                           ▼
 ┌──────────────────┐                       ┌──────────────────┐
 │    HTTP/1.1      │ (Default Stream)      │     HTTP/2       │ (Multiplexed Pool)
 │   muta-http1     │                       │    h2 Seam       │ (Above TimedIo)
 └──────────────────┘                       └──────────────────┘
```

---

## Technical Specification

### 1. Transport-Level Resolve-and-Pin (`muta-net`)

`muta-net` introduces `EgressConfinement`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EgressConfinement {
    /// Standard unrestricted egress (for configured LLM endpoints and proxies).
    #[default]
    Unrestricted,
    /// Strict public egress (for web search, URL reader, and untrusted tools).
    /// Enforces resolve-and-pin and rejects non-globally-routable IPs.
    StrictPublic,
}
```

When `StrictPublic` is configured:
1. **Resolution & Validation**: DNS resolution returns IP candidates. All candidates are validated against the forbidden subnet list. If any candidate address falls into a forbidden range, the connection is aborted immediately (`NetError::Security("non-public address rejected")`).
2. **Pinned Socket Dialing**: `TcpStream::connect(pinned_addr)` connects directly to the validated IP. No secondary DNS lookup can take place.
3. **SNI & Authority Decoupling**: The TLS handshake uses the logical domain name for Server Name Indication (SNI) and certificate validation, while the underlying socket connects strictly to the pinned IP.

### 2. Forbidden Address Space (The Red Line)

The following address ranges are strictly rejected under `StrictPublic`:

| Address Range | Classification | Vulnerability Target |
|---|---|---|
| `127.0.0.0/8`, `::1` | Loopback | Local daemons, Muta control plane (`127.0.0.1:9800`) |
| `10.0.0.0/8`, `172.16.0.0/12`, `192.168.0.0/16` | RFC 1918 Private | Intranet services, internal gateways, router admin panels |
| `169.254.0.0/16`, `fe80::/10` | Link-Local / Cloud Metadata | `169.254.169.254` (AWS, GCP, Azure, OpenStack IMDS) |
| `100.64.0.0/10` | Carrier-Grade NAT | Shared provider infrastructure |
| `0.0.0.0/8`, `::/128` | Unspecified | Current host binding bypass |
| `224.0.0.0/4`, `ff00::/8` | Multicast | Network storming, internal broadcast probing |

### 3. Cross-Origin Redirect Credential Stripping

When following HTTP redirects:
- If the target URL transitions across different origins (scheme or host change), all sensitive request headers (`Authorization`, `Proxy-Authorization`, `Cookie`) are stripped unconditionally before dispatching the next hop.
- Redirection hops are capped at `max_redirects` (default 5). Circular redirects or chains exceeding the budget fail closed.

### 4. The Pragmatic HTTP/2 Multiplexing Boundary

1. **HTTP/1.1 as the Solid Default**: HTTP/1.1 remains the default stream engine. Through predictive pre-connection (warming DNS and TCP/TLS during composer typing), connection latency is eliminated without protocol complexity.
2. **No Hand-Rolling H2 (`[INV-EGRESS-05]`)**: Writing a custom HTTP/2 framing engine (HPACK, flow control state machine, priority trees, stream multiplexing) is declared an architectural anti-pattern.
3. **The Isolation Seam**: If HTTP/2 is enabled for high-concurrency subagent channels, it must consume a `TimedIo<TlsStream>` through a dedicated, audited framing crate (`h2`). Under no circumstances may an opaque HTTP client (`reqwest`, `hyper-util`) be introduced into the production workspace.

---

## Negative Knowledge (Rejected Approaches)

- **Do NOT rely on DNS cache TTLs for SSRF prevention**: Relying on OS or runtime DNS caches to prevent rebinding fails because adversarial nameservers return `TTL = 0`, forcing a re-resolution on connect.
- **Do NOT perform IP validation solely in user-space tool code**: Call-site validation is inherently leaky and cannot protect subsequent redirect hops without complex state threading.
- **Do NOT author an in-house HTTP/2 protocol engine**: HTTP/2 is an order of magnitude more complex than HTTP/1.1. Attempting to own the binary frame state machine introduces massive defect risk with zero strategic gain over an audited, focused framing crate.

---

## Invariants & Behavioral Boundaries

- **`[INV-EGRESS-01]` Zero-Blackbox Production Egress**: Every outbound HTTP request in production must originate from `muta-net`. Re-introducing `reqwest` or equivalent monolithic HTTP clients into the workspace is forbidden and enforced by `scripts/check-egress-deps.sh`.
- **`[INV-EGRESS-02]` Fail-Closed Resolve-and-Pin**: Web tools operating on arbitrary external URLs must activate `EgressConfinement::StrictPublic`. The transport must connect solely to the verified IP address.
- **`[INV-EGRESS-03]` Credential Sanitization Across Origins**: Redirects between distinct authority boundaries must purge `Authorization` and sensitive headers.
- **`[INV-EGRESS-04]` Sycall Trace Invariant**: Every connection, regardless of application framing (HTTP/1.1 or future HTTP/2), must wrap its stream in `TimedIo` and record events into `muta-trace`.
- **`[INV-EGRESS-05]` HTTP/2 Ownership Red Line**: Maintainers and AI assistants shall not attempt to write an in-house HTTP/2 framing parser.
