# 0159. Application-Layer Protocol Orthogonalization

- **Status:** Accepted
- **Date:** 2026-08-20
- **Builds on:** ADR-0096 (unified daemon), ADR-0105 (one port two protocols), ADR-0134 (wire protocol negotiation), ADR-0158 (native framed transport)

## Context

ADR-0158 separated physical transport framing (4-byte length-delimited UDS vs WebSocket TCP). However, the Layer 3 application wire (`Wire`, `AgentRequest`, `AgentResponse`, `RoundEvent`) still suffered from historical architectural conflations:

1. **Broadcast Pollution by Unary RPCs**: Ephemeral query-response interactions (like composer auto-completion and configuration reads) were broadcast onto the global session bus.
2. **Dual-Track State Synchronization**: Clients had to maintain dual logic for initial handshake hydration (`Welcome` + `attach_sync_buffer`) and runtime mutations (`RoundEvent`).
3. **UI Leakage in Protocol**: Specialized variants existed for specific UI features (e.g. `/btw` side-view routing).
4. **Fragmented Error Envelopes**: Errors were reported across four inconsistent formats.

## Decision

We establish four orthogonal communication primitives:

1. **Point-to-Point RPC (`Call` / `Reply`)**:
   - Request-response interactions carry a unique `id: u64` and are answered via unicast to the requesting client.
2. **State Synchronization (`StatePatch`)**:
   - Session state domains (`Transcript`, `TodoList`, `SecurityTrust`, `Pressure`, `RuntimeMeta`) synchronize via versioned replace/update patches.
3. **Interactive Gates (`GateRequest` / `GateResponse`)**:
   - Agent pauses waiting for human decisions (permissions, questions) are uniquely keyed by `gate_id`.
4. **Streaming Chunks (`StreamChunk`)**:
   - Token generation and terminal stdout/stderr stream without envelope overhead.
5. **Unified Protocol Error (`ProtocolError`)**:
   - All layers standardize on `{ domain, code, message, details }`.

## Consequences

- Zero broadcast pollution from client-local interactions.
- Frontends simplify to standard domain reducers.
- Protocol is completely UI-neutral and multi-client safe.
