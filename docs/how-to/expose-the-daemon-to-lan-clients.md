# How to expose the daemon to LAN clients

Let another machine on your network — a browser running a web control panel,
a script, a second terminal box — observe and manage the sessions your
daemon hosts. Exposing is an explicit opt-in that always carries a bearer
token (ADR-0054, ADR-0096); the default daemon listens on a Unix socket and
TCP loopback only, so nothing leaves the machine unless you ask for it.

## 1. Start the daemon with a public listener

```bash
neenee daemon start --fg --port 8765 --public
```

(`--public` makes the daemon bind `0.0.0.0` instead of loopback.)
`--port` is optional — omit it and the OS
assigns one. On startup the daemon prints its endpoints to stderr, including
the generated token:

```text
neenee-server: control plane on unix:///run/user/1000/neenee/daemon.sock
neenee-server: serving sessions on ws://0.0.0.0:8765
neenee: exposed listener requires a bearer token; read it from the discovery file …
```

Copy the token and treat it as a secret: it grants full session access. The
token is generated fresh each time the daemon starts without one, and it is
also recorded in the discovery record (`daemon.json`, visible only to your
user) so local clients can connect without re-typing it.

## 2. Connect from another machine

The control plane is one JSON-over-WebSocket endpoint. Every connection
opens with an HTTP upgrade carrying the token, then a first frame that
chooses a role (`Select`):

```js
const WebSocket = require("ws");
const socket = new WebSocket("ws://daemon-host:8765/", {
  headers: { Authorization: "Bearer 9f2c…" },
});
// First frame picks a role, e.g. observe every session:
socket.send(JSON.stringify({
  type: "Select",
  action: { monitor: { watch: true, include_idle: false } },
}));
```

A missing or wrong token is rejected with HTTP 401 before any session data
is exchanged. Browsers cannot set headers on `new WebSocket()`, so they
present the token as a subprotocol instead (ADR-0105):

```js
const socket = new WebSocket("ws://daemon-host:8765/", ["bearer.9f2c…"]);
```

(The daemon echoes the `bearer.<token>` subprotocol when it accepts it.)

Typical roles for a LAN client:

| Role | `Select` action | Use |
|------|-----------------|-----|
| Monitor | `{ monitor: { watch, include_idle } }` | Read-only live view of every session — the web panel's data source |
| Control | `{ control: { verb, … } }` | One management verb per connection: `create_session`, `send_prompt`, `interrupt`, `resolve_permission`, `kill_session` |
| Attach | `{ attach: "<id>" }` \| `"new"` | Drive one session bidirectionally, like the TUI does |

The full frame contract — `MonitoredSession` fields, control-verb replies,
robustness rules — is in [Server WebSocket API](../reference/server-api.md)
and machine-readable in [`server.asyncapi.yaml`](../reference/server.asyncapi.yaml).

## 3. Know the limits

- **The token protects the handshake, not the wire.** On anything beyond a
  trusted LAN, front the listener with a TLS-terminating reverse proxy
  (`wss://`) so the token and the session traffic are not plaintext.
- **No LAN discovery.** There is no mDNS/broadcast: clients connect to the
  host and port you gave them, with the token. The `daemon.json` discovery
  record is a *local* convenience only — a remote client never reads it.
- **One daemon per user.** The exposed endpoint serves every session across
  every project that user is running; there is nothing else to expose.

## References

- [The session daemon and the control plane](../explanation/session-daemon-and-control-plane.md)
  — the architecture this guide operationalizes.
- [Server WebSocket API](../reference/server-api.md) — bind/auth model and
  the full wire contract.
- [ADR-0054](../adr/0054-server-layer-followups.md) — the loopback-default
  security model; [ADR-0096](../adr/0096-unified-session-daemon.md) — the
  unified daemon and control plane.
