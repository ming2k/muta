# 0210. Extract the transport stack as the independent `netune` library

- **Status:** Accepted
- **Date:** 2026-09-09
- **Builds on:** ADR-0200 (owned egress path and packet-level request trace)

## Context

ADR-0200 made the transport ours: `muta-trace` (the trace model),
`muta-http1` (the codec), `muta-net` (the transport), `muta-net-probe` (the
L2 probe). What began as an observability programme grew a second axis of
capability — intervention:

- **L1 fault injection** (`muta-net` `FaultIo`): scripted delays, split
  reads, truncations and resets against any stream, in-process, unprivileged.
- **L2 forge** (`muta-net-probe`): pure `FrameSpec` → frame builders with
  forged seq/ack and flags; the transmit path behind the `inject` feature.
- **TLS fingerprinting** (`muta-trace` `tlsfp`): ClientHello parsing, JA3,
  JA4, and a baseline test that snapshots the identity our configuration
  presents (which has already caught one real drift: rustls 0.23.42 → 0.23.44
  added ML-DSA signature algorithms and changed the extension hash).

These are not LLM-agent capabilities. They are general-purpose network
capabilities with use cases in every project that talks HTTP — which is why
the extraction question recurred until the API had been shaped by real use.

The four crates form a closed family: `muta-http1` is consumed only by
`muta-net`; `muta-trace` is consumed by `muta-net` and one direct consumer
(`muta-llm-client`); `muta-net-probe` depends only on `libc`. External
dependencies are small and boring (serde, bytes, http types, rustls, tokio,
libc) — no third-party HTTP implementation anywhere.

## Decision

Extract the four crates into the independent **`netune`** repository
("net + tune": measure the network, and tune it), as a lockstep-versioned
workspace:

| Crate in netune | Was | Notes |
|---|---|---|
| `netune-trace` | `muta-trace` | trace model, derivations, `tlsfp` |
| `netune-http1` | `muta-http1` | codec, hyper differential harness |
| `netune` | `muta-net` | transport, tap, pool, proxies, `FaultIo` |
| `netune-probe` | `muta-net-probe` | L2 parse/build + `inject` feature |

Names inside the code follow the crates: `netune_trace::`, `netune::`,
`netune_probe::`; the default user-agent becomes `netune/{version}`. The
historical ADR-0200 references inside migrated doc comments are kept where
they explain *why*, rewritten where they only pointed at workspace paths.

### Versioning and release

- **Lockstep**: one workspace version for all four crates (0.1.0 at
  extraction). They are one capability; independent versions would create a
  compatibility matrix nobody benefits from.
- **Cutover (complete)**: netune 0.1.0 is published to crates.io
  (`netune`, `netune-trace`, `netune-http1`, `netune-probe`) and muta's seven
  egress call-site crates consume it as a registry dependency. The repository
  lives at `~/projects/netune` (GitHub: ming2k/netune), lockstep-versioned
  with a release workflow that publishes all four crates on a `v*` tag.

### Boundaries that must not change

1. **The privilege posture is structural**: `netune-probe`'s transmit path
   exists only under `--features inject`; the default build of every crate is
   receive/observe-only. Injection is never a default capability of anything
   long-running.
2. **No third-party HTTP implementation** in the production path; hyper stays
   a dev-dependency oracle in `netune-http1`.
3. **No credentials or payload bytes in traces** (ADR-0200's discipline).

### The HTTP/2 plan (recorded at extraction, built when there is a consumer)

The stack is HTTP/1.1-only today (no `h2` ALPN advertised anywhere, the
transport is keep-alive-pool based, and the trace model is protocol-neutral
by design). HTTP/2 belongs in a **new crate, `netune-http2`**, not a feature
of `netune-http1`: the binary frame format, stream state machines, HPACK
dynamic-table state, and multiplexed connection pool share no implementation
code with the line-oriented 1.1 codec (hyper's `h2` is likewise a separate
crate). The shared layer — method, status, headers — is already the `http`
types both would consume. Sequencing: `netune-http2` starts when a consumer
needs it; the transport's ALPN, pool, and tap semantics change additively at
that point, exactly as ADR-0200's "neutral" consequences section anticipated.

## Consequences

- muta's six egress call-site crates depend on `netune`/`netune-trace` from
  an external repo; `scripts/check-egress-deps.sh` and the reqwest gate are
  unaffected (they key on reqwest, not on transport internals).
- The fingerprint baselines move with the code — they were always a property
  of the TLS engine versions, not of muta.
- Two repos now share the release burden: a muta transport change that needs
  a netune change requires a netune release first. Accepted: it is the
  standard cost of any shared library, and the lockstep policy keeps it to
  one number.
- ADR-0200's Implementation section stays as the historical record; netune's
  README is now the living description of the capability surface.

## References

- ADR-0200 — the original transport programme and its gates.
- netune repository — `netune-staging/` at extraction time (moved to its own
  repository on publish).
