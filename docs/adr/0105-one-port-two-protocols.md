# 0105. One port, two protocols: embedded panel serving and control-plane hardening

- **Status:** Partially superseded by ADR-0136
- **Date:** 2026-08-15
- **Builds on:** ADR-0096 (unified daemon), ADR-0100 (lifecycle), ADR-0101 (shutdown), ADR-0054 (secure serve defaults)

> ADR-0136 retains this decision's one-port WebSocket/health split, bearer
> authentication, browser subprotocol, and Origin checks. It supersedes the
> embedded static bundle, the Web-asset crate, the `neenee panel` command, and
> the `panel` health field: `apps/web` now builds and deploys independently,
> while plain HTTP serves only `/healthz`.

## Context

The daemon's TCP listener (ADR-0096/0054) bound loopback with **no
authentication** and spoke only WebSocket. Three problems converged:

- **Browser drive-by.** WebSocket handshakes are not subject to the
  same-origin policy: any page the user visits can `new
  WebSocket("ws://127.0.0.1:<port>")` and drive the control plane — read
  transcripts, run tools — because the daemon validated neither `Origin` nor
  any credential on loopback. "A local co-process is trusted" (ADR-0054)
  covers local binaries; it does not cover the browser, which is a remote
  code execution environment pointed at loopback.
- **Browser clients could not authenticate even when they wanted to.** The
  `Authorization: Bearer` check (ADR-0054) is unreachable from
  `new WebSocket()`, which sets no custom headers — so the web panel was
  structurally barred from any authenticated daemon, including the default
  loopback once a token existed.
- **The web panel had no distribution path.** Browsers cannot read the
  discovery record, so the panel hardcoded `ws://127.0.0.1:9800` while the
  daemon bound an ephemeral port; and the daemon served no static files, so
  "run the panel" meant "run a second server you had to know about".

## Decision

### 1. One TCP port speaks two protocols

The TCP accept loop peeks at (does not consume) each connection's request
head (`serve::classify`): a `GET … Upgrade: websocket` goes to the existing
control plane; any other plain-HTTP `GET`/`HEAD` is answered by
`serve_http` — the embedded web-panel bundle (a new `neenee-web-assets`
crate codegen-embeds `apps/web/dist` at build time, with a placeholder page
when the dist was never built, so `cargo build` never requires the Node
toolchain) and `GET /healthz`, an unauthenticated probe reporting
`{version, auth, panel}` — how a browser client distinguishes "daemon needs
a token" from "nothing listening", which a failed WS handshake cannot
express. Static serving is deliberately minimal: GET/HEAD only,
`Connection: close`, `X-Content-Type-Options: nosniff`, and a restrictive
CSP on the panel HTML. Anything richer belongs behind a real reverse proxy.

### 2. Loopback gets a bearer token by default

`[daemon] local_auth` (default **true**; CLI `--no-local-auth` opts out):
the loopback listener generates a per-start token (CSPRNG, as `--public`
already did) and publishes it in the discovery record, which is owner-only
(0600) — so co-located CLI/TUI clients authenticate transparently by reading
the record (they already did for `--public`), while other local processes
and other users on a shared machine are locked out. The UDS listener stays
exempt: filesystem permissions are its boundary. Escape hatches for tests
(`ServeOptions::default()` keeps `local_auth: false`) and exotic setups are
explicit, not ambient.

### 3. The browser channel is a subprotocol, and Origins are checked

- Browser token transport: `new WebSocket(url, ["bearer.<token>"])` — the
  one channel a browser *can* customize. The handshake validates the token
  (constant-time) and echoes the subprotocol; `Authorization: Bearer`
  remains the channel for non-browser clients.
- `Origin` validation on every loopback handshake: a browser origin must be
  loopback-served (`127.0.0.1` / `localhost` / `[::1]`, any port — the panel
  served by this daemon or a local dev server qualifies); foreign or `null`
  origins get 403. Non-browser clients send no `Origin` and are governed by
  the token. On `--public` the token is mandatory and the check is skipped
  (remote origins are legitimate there).

### 4. Discovery UX catches up

- The CLI default port is fixed at **9800** (`--port` overrides; on
  `AddrInUse` the daemon falls back to an ephemeral port and the discovery
  record carries the truth), because browsers cannot read the record.
- `neenee panel` prints the panel URL including the token query parameter
  (`http://127.0.0.1:9800/?token=…`) — the token is printed only on this
  explicit operator request, never in banners/logs; the panel persists it
  to localStorage on first visit.
- `Wire::Error` gains an optional machine-readable `code` (first value:
  `version_mismatch`) so clients render targeted guidance instead of
  string-sniffing.

## Alternatives considered

- **UDS-only control plane.** Browsers cannot connect to UDS; this abandons
  the web panel. Rejected.
- **axum/hyper for the HTTP side.** A full framework for two routes is
  dependency weight without a payoff; the WS path stays on
  tokio-tungstenite either way. The peek-split is ~40 lines and
  covered by tests. Revisit if the HTTP surface ever grows past static +
  health.
- **Query-param tokens.** Leak into logs, history, and referrer chains;
  the subprotocol channel does not appear in any of those. Used only by the
  `neenee panel` URL as an explicit paste-once bootstrap into localStorage.
- **No loopback token (Origin check only).** Origin checking stops browsers,
  not other local processes or other users on the host; the token closes
  both for free given the discovery record already exists.

## Consequences

- A stock daemon now serves the panel at `http://127.0.0.1:9800` and refuses
  unauthenticated or foreign-origin control-plane connections — `neenee
  daemon start`, then `neenee panel`, is the whole setup story.
- Older clients that never read `discovery.token` fail closed against new
  daemons (401) with an actionable message; new clients against old daemons
  are refused by the version handshake as before (ADR-0100 rule 4).
- The workspace gains a crate whose build depends on `apps/web/dist`
  *when present*; CI builds the web bundle before packaging release
  binaries so releases embed the panel.
- `server-api.md` and the AsyncAPI contract document the auth channels,
  the error `code`, and the HTTP routes.

## References

- ADR-0054 (secure serve defaults), ADR-0096 (unified daemon),
  ADR-0100/0101 (lifecycle/shutdown).
- Prior art: Jupyter's per-start token + printed URL; Vite/Docker
  Desktop Host-header checks against DNS rebinding; Kubernetes
  `bearer.` subprotocol precedent.
