# 0158. Native Framed Transport for Local Daemon IPC

- **Status:** Accepted
- **Date:** 2026-08-20
- **Builds on:** ADR-0096 (unified daemon), ADR-0105 (one port two protocols), ADR-0134 (wire protocol negotiation)

## Context

From ADR-0096 to ADR-0134, the unified session daemon served its wire protocol (`crates/muta-contracts/src/wire.rs`) uniformly over WebSocket (RFC 6455) across both TCP Loopback and Unix Domain Sockets (UDS).

While this simplified the initial implementation by sharing `tokio-tungstenite` across all transports, wrapping local UDS communication in WebSocket incurred notable architectural and operational friction:
1. **Unnecessary Framing Overhead**: UDS is a point-to-point, kernel-authenticated local IPC channel. Running HTTP/1.1 `Upgrade: websocket` handshakes and RFC 6455 4-byte client XOR masking added redundant CPU cycles and round-trips.
2. **Unix Tool Incompatibility**: Native Unix utilities (`nc -U`, `socat`, simple scripts) could not easily speak with the daemon without implementing a WebSocket client handshake.
3. **Loss of Native Unix Semantics**: WebSockets on UDS obscured the natural transport boundary and precluded future extensions like `SCM_RIGHTS` (fd passing).

## Decision

We introduce a clean separation between **Local Unix IPC** and **Network / Browser Web IPC** while preserving 100% of the typed business contract (`muta_contracts::Wire`):

1. **Native Length-Delimited Framing for Local IPC (UDS / Windows Named Pipes)**:
   - Wire messages on UDS are framed using a 4-byte big-endian length prefix followed by the JSON-serialized `Wire` payload:
     ```text
     +-------------------------------+----------------------------------------+
     |  Payload Length (4B, Big-End) |  JSON UTF-8 Payload (muta_contracts::Wire) |
     +-------------------------------+----------------------------------------+
     ```
   - No HTTP handshake, no masking, no WebSocket envelope on local sockets.
   - Connected clients are authenticated directly by Unix socket permissions (`0600`) and peer credentials (`SO_PEERCRED`).

2. **Retain WebSocket on TCP Loopback for Browser / Remote Clients**:
   - TCP listeners (`127.0.0.1:<port>`) retain the ADR-0105 dual-mode dispatch: plain HTTP `/healthz` and RFC 6455 WebSocket for `apps/web` and LAN clients.
   - Browser Origin validation and Bearer tokens remain enforced on the TCP WebSocket port.

3. **Unified Internal Channel Abstraction (`WireStream` / `WireSink`)**:
   - The daemon's session handling logic (`serve.rs`) and client connectors (`client.rs`) accept a unified `Stream<Item = Result<Wire, _>> + Sink<Wire>` interface.
   - `muta-runtime` converts both Native Framed UDS and TCP WebSocket streams into the same typed pipeline, keeping all agent dispatch, command handling, and monitor logic completely transport-agnostic.

## Consequences

- **Local CLI/TUI Performance**: Native UDS connection latency drops by eliminating the HTTP upgrade round-trip and frame masking.
- **Simplicity & Cleanliness**: Local Unix IPC adheres to standard Unix daemon conventions.
- **Full Backward-Compatibility with Web**: `apps/web` and browser clients continue to work unmodified over the TCP WebSocket endpoint.
