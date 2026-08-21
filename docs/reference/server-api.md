# Server WebSocket API

This page is the frontend integration guide for the session daemon's WebSocket transport.
Its machine-readable contract is [`server.asyncapi.yaml`](server.asyncapi.yaml).

> **Why AsyncAPI rather than OpenAPI or TypeSpec?** The public surface is a
> bidirectional WebSocket event stream, not an HTTP request/response API.
> AsyncAPI models channels, send/receive operations, messages, and ordering
> directly. OpenAPI would only describe the HTTP upgrade incompletely, while a
> TypeSpec file would still need a project-specific WebSocket convention.

## Roles and entry points

The daemon (`neenee daemon start --fg` — the single binary, ADR-0102) serves one
control-plane endpoint per user, on owner-only native local IPC plus a TCP
loopback listener by default (fixed port 9800, ephemeral fallback), and on all
interfaces when `--public` (ADR-0096/0105/0130). Unix uses a domain socket and
Windows uses a Named Pipe. Three client roles share the protocol,
distinguished by the first frame the client sends after the upgrade (`Select`):

| Role | Handshake | Direction | Purpose |
|------|-----------|-----------|---------|
| **Attach** | `Select{action: New \| Attach(id?)}` | bidirectional | Drive a session: send `Request`s, receive `Response`s |
| **Monitor** | `Select{action: Monitor{watch, include_idle}}` | server → client | Observe every session the daemon knows about (ADR-0093) |
| **Control** | `Select{action: Control(verb)}` | one round-trip | Manage sessions: create / prompt / interrupt / approve / kill (ADR-0096) |

The in-TUI `/serve` command (legacy single-session prehost) is superseded by
the unified daemon; the protocol below is the daemon's control plane.

## Bind and authentication

- **Native local IPC (default, ADR-0096/0130):** Unix uses
  `$XDG_RUNTIME_DIR/neenee/daemon.sock`, `0600` inside a `0700` directory.
  Windows uses `\\.\pipe\neenee-<user-sid>-daemon-<instance-hash>`, rejects remote clients,
  and applies a protected DACL for the current user and LocalSystem. No bearer
  token is required because the OS endpoint is the authentication boundary.
  The instance root selector moves Unix runtime files; Windows instance
  isolation is encoded in the discovered endpoint and state root.
- **TCP loopback (default port 9800, ADR-0105):** binds `127.0.0.1` and, with
  `[daemon] local_auth` on (the default), **requires a bearer token**,
  generated per daemon start and published in the owner-only (0600)
  discovery record — co-located CLI/TUI clients read it from there and
  authenticate transparently. `--no-local-auth` / `local_auth = false`
  restores trust-the-loopback. When the default port is taken, the daemon
  falls back to an ephemeral port; the record always carries the actual
  one. `NEENEE_PORT` overrides the default (ADR-0121) — below an explicit
  `--port`, above the well-known 9800.
- **TCP exposed (`--public`):** binds `0.0.0.0` and **requires a bearer
  token** (else HTTP 401). The daemon generates a token on startup and
  points at the discovery record. Exposing is an explicit opt-in that always
  carries a token; front it with a TLS-terminating reverse proxy for remote
  use (the token protects the handshake, not the wire).

### Credential channels (ADR-0105)

Two equivalent credentials are accepted on an authenticated TCP handshake:

- `Authorization: Bearer <token>` — for clients that can set headers
  (every Rust client).
- `Sec-WebSocket-Protocol: bearer.<token>` — the browser channel, since
  `new WebSocket()` cannot set headers. The daemon echoes the subprotocol
  when it accepts it.

Additionally, a loopback handshake carrying a browser `Origin` header is
only accepted when the origin host is itself loopback (`127.0.0.1`,
`localhost`, `[::1]`, any port) — WebSocket is not same-origin-protected,
so without this check any visited page could drive the daemon. Non-browser
clients send no `Origin` and are governed by the token alone; the check is
skipped on `--public` (the mandatory token is the boundary there).

### HTTP endpoints on the same port (ADR-0105)

The TCP listener splits plain HTTP from WebSocket upgrades by peeking at
the request head. Plain HTTP serves:

- `GET /` (and any unknown path, SPA fallback) — the embedded web panel
  bundle (a placeholder page when the daemon was built without
  `apps/web/dist`).
- `GET /healthz` — unauthenticated `{"version", "auth", "panel"}` probe so
  a browser can tell "daemon needs a token" apart from "nothing listening".

### Security model

| Mode | Bind | Auth | Use |
|------|------|------|-----|
| default (Unix) | `$XDG_RUNTIME_DIR/neenee/daemon.sock` | none — filesystem permissions (`0600` in a `0700` runtime dir) | local CLI / TUI |
| default (Windows) | `\\.\pipe\neenee-<user-sid>-daemon-<instance-hash>` | none — protected current-user DACL; remote pipe clients rejected | local CLI / TUI |
| default (TCP loopback) | `127.0.0.1:9800` | bearer token (default; `local_auth = false` disables) + loopback-origin check | local co-processes, the web panel |
| `--public` | `0.0.0.0` | bearer token (mandatory) | remote client / another machine |

Because the default binds native local IPC plus loopback, a casual host
exposes nothing beyond this machine. Exposure is an explicit opt-in that
cannot happen without a token. See ADR-0054, ADR-0105, and ADR-0130 for the rationale. For a
remote client walkthrough see
[How to expose the daemon to LAN clients](../how-to/expose-the-daemon-to-lan-clients.md).

## Attach: drive a session

After the upgrade, the client selects a session:

```json
{ "type": "Select", "action": "new" }
{ "type": "Select", "action": { "attach": "session-id" } }
{ "type": "Select", "action": { "attach": null } }
```

An attach client declares its working directory in the frame's optional
`project` field:

```json
{ "type": "Select", "action": "new", "project": "/abs/path/to/project" }
```

`project` scopes `new` creation, auto-attach (`{ "attach": null }`), and lazy
resume to that project (ADR-0096). A client that omits it is scoped by the
daemon's own process working directory — whatever directory the first client
that spawned the daemon used — so current clients always send it. `project`
has no effect on monitor or control selects.

The server answers one of:

```json
{ "type": "Welcome", "session_id": "…", "round_counter": 6, "messages": [] }
{ "type": "Pick", "sessions": [ { "id": "…", "overview": "…", … } ] }
{ "type": "Error", "message": "…", "code": "version_mismatch" }
```

- `Welcome` binds the connection: `messages` is the full persisted transcript
  (a replacement snapshot, not incremental), `round_counter` the authoritative
  monotonic round counter, and `round_interrupts` (C11, optional — absent on
  older daemons) the durable round-interrupt records, each
  `{ reason: "user" | "superseded" | "terminated", at_ms, round }`, to
  re-project into the transcript at their timestamp seams. Process it before
  rendering subsequent live events.
- `Pick` means several sessions are hosted and the client must choose
  (`Attach(Some(id))` on a new connection).
- `Error` is terminal. `code` (ADR-0105, optional) is the stable
  machine-readable reason — currently only `"version_mismatch"` — so clients
  can branch without string-sniffing `message`.

From then on the connection carries zero or more live frames in both
directions — `{ "type": "Request", … }` client → server,
`{ "type": "Response", … }` server → client. The server subscribes to the
live broadcast after sending `Welcome`, so an event produced in that narrow
interval can be missed; the transport has no sequence numbers or replay
cursor.

Node client with a token:

```js
const WebSocket = require("ws");
const socket = new WebSocket("ws://host:8765/", {
  headers: { Authorization: "Bearer a1b2c3d4e5f6..." },
});
```

Browsers use the `bearer.<token>` subprotocol instead — see
[Credential channels](#credential-channels-adr-0105).

Sessions can be renamed over the attach channel with
`Request{RenameSession{id, title}}`: `id` takes a full id or a 4+ character
hex short-id prefix and resolves live or archived sessions exactly like
`DeleteSession`. `title` sets the manual title (ADR-0022: AI titling never
overwrites a manual one); `null` clears the manual override, returning
pickers and monitor rows to the AI-title / first-prompt fallback. On success
the harness pushes a fresh `SessionsOverview` snapshot and a hosted
session's monitor row is republished as a `session_updated` diff carrying
the new title; an unknown id answers
`{ "type": "Error", "message": "No session matches '…'." }`.

## Monitor: observe the host (ADR-0093)

```json
{ "type": "Select", "action": { "monitor": { "watch": true, "include_idle": false } } }
```

The server sends `{ "type": "Monitor", "kind": "snapshot", … }` first, then —
while `watch` holds — `session_added` / `session_updated` / `session_removed`
diffs. With `watch: false` it closes after the snapshot (one-shot poll, which
is what `neenee daemon status` does). Each diff carries a whole
[`MonitoredSession`](#monitoredsession) row; consumers upsert by `id`.

`include_idle: false` (the default) filters both the snapshot and the diff
stream to sessions whose `status` is not `idle`, so a quiet host reports an
empty list.

A monitor client never sends anything after its `Select`; the channel is
read-only and cannot steer any session.

### `MonitoredSession`

```json
{
  "id": "session-123",
  "overview": "fix the flaky parser test",
  "created_at": 1786100000,
  "updated_at": 1786100123,
  "message_count": 14,
  "hosting": "hosted",
  "status": "running",
  "round": 3,
  "turn": 1,
  "output_tokens": 512,
  "elapsed_ms": 83000,
  "current_tool": "bash",
  "activity": "waiting for model",
  "context_tokens": 48200,
  "note": null
}
```

- `status` is derived display state, not protocol state: `idle`, `running`,
  `needs_approval`, `needs_input`, `interrupted`, `failed`. The `needs_*`
  values are overlays on a still-running round (cleared when model output
  resumes); `note` carries the blocking reason (e.g. `permission: write_file`).
- `hosting` is always `hosted` — under ADR-0096 the daemon owns every
  session, so it can be attached to. Older producers may omit `hosting`;
  treat missing as `hosted`.
- `elapsed_ms` runs while a round is active and freezes at its terminal
  event; `turn` is the 0-based model-request index within `round`.

## Control: manage sessions (ADR-0096)

A control client issues one session-management verb per connection, then
reads a single reply:

```json
{ "type": "Select", "action": { "control": { "verb": "create_session", "project": "/abs/path", "prompt": null } } }
→ { "type": "ControlReply", "ok": true, "session_id": "…" }
```

The verbs (`Select{action: {control: {…}}}`):

| `verb` | Extra fields | Effect |
|--------|--------------|--------|
| `create_session` | `project`, optional `prompt` | Host a new session for a project; reply carries its `session_id` |
| `send_prompt` | `session_id`, `text` | Queue a new round |
| `interrupt` | `session_id` | Stop the current round |
| `resolve_permission` | `session_id`, `request_id`, `decision` (`once`/`always`/`reject`) | Answer a pending tool-permission prompt |
| `kill_session` | `session_id` | Tear the session down (monitors get `session_removed`) |
| `shutdown` | — | Stop the daemon itself (ADR-0100): the same budgeted graceful drain as SIGINT/SIGTERM — listeners close, connections drain, every session's `SessionEnd` hooks fire, the discovery record is removed, exit 0. The `ControlReply{ok:true}` is sent *before* the drain starts (it would otherwise cancel the replier). This is what `neenee daemon stop` sends. |

`ControlReply` is `{ ok, session_id?, error? }`. On `ok:false`, `error`
explains (unknown session, host cannot create, …). The connection closes
after the reply; issue another verb on a fresh connection.

## JSON representation

Two discriminator levels:

- `type` identifies the transport envelope: `Select`, `Welcome`, `Pick`,
  `Error`, `Request`, `Response`, `Monitor`.
- `Monitor` frames carry a second `kind` discriminator on the flattened
  `MonitorEvent`: `snapshot`, `session_added`, `session_updated`,
  `session_removed`, `daemon_draining`.
- Rust enums otherwise use serde's default externally tagged representation.

A request carrying fields therefore looks like:

```json
{
  "type": "Request",
  "Chat": {
    "text": "Hello",
    "images": [],
    "sent_at_ms": 1770000000000
  }
}
```

Tuple/newtype variants put their value directly under the variant key:

```json
{ "type": "Request", "SlashCommand": "/help" }
```

After flattening, unit request variants require a `null` value:

```json
{ "type": "Request", "Interrupt": null }
```

Unit **responses and events** serialize as strings when they are nested on
their own. A top-level response is flattened into `Wire::Response`, so its unit
variant appears as a key with a `null` value:

```json
{ "type": "Response", "ConversationCleared": null }
```

Nested unit `RoundEvent` values remain strings. For example, a stream start is:

```json
{
  "type": "Response",
  "Round": {
    "session_id": "session-123",
    "event": "StreamStart"
  }
}
```

A session-scoped streaming response has three levels:

```json
{
  "type": "Response",
  "Round": {
    "session_id": "session-123",
    "event": { "StreamDelta": "partial text" }
  }
}
```

`TurnStarted` carries both levels of the execution position. `round` is
one-based for the enclosing user exchange; `turn` is the zero-based ReAct
model-request index within that round:

```json
{
  "type": "Response",
  "Round": {
    "session_id": "session-123",
    "event": { "TurnStarted": { "round": 7, "turn": 0 } }
  }
}
```

These edge cases are exercised by the server integration test; use the
AsyncAPI contract and that test as the authority for envelope shapes.

## Core frontend flows

### Chat and streaming

Send `Chat`, append/render text on `StreamDelta`, and finalize the assistant
message on `StreamEnd`. `StreamStart`, `StreamDiscard`, reasoning deltas, tool
events, notices, retries, and state snapshots may appear between them. Keep
state per `Round.session_id`; primary and `/btw` side sessions can stream
concurrently.

### Interrupt

Send:

```json
{ "type": "Request", "Interrupt": null }
```

If interruption happens before any output, the server can emit `UnsentInput`,
which includes the prompt and images to restore to the input editor. The
restore is advisory: the harness has already reverted the conversation, so
the client owns how to surface the prompt — adopting it into an idle
composer, or (if the user is mid-composition) leaving the composer alone and
offering the prompt via history/notice instead.

### End session (ADR-0112)

Send on the attach connection when the operator is *done* with the session —
not detaching:

```json
{ "type": "Request", "EndSession": null }
```

The server intercepts the frame at the connection layer (it never reaches the
driver queue), tears the hosted session down through the same path as the
`kill_session` control verb — cancel the driver, fire `SessionEnd` hooks,
clear WIP declarations, publish `session_removed` — and answers with the
terminal `Exit` response on the same connection before closing it. Disk
history is kept; resume-the-transcript remains possible. The TUI sends it on
`/exit` and double-`Ctrl+C`; a headless run sends it after its terminal
round. A plain socket drop does *not* end the session (detach semantics,
ADR-0096).

### Permission request

When receiving `Round.event.PermissionRequest`, show its user-facing `label`,
`description`, `arguments`, and `scope`, then reply with the same request id:

```json
{
  "type": "Request",
  "PermissionReply": {
    "request_id": "permission-123",
    "decision": "Once",
    "parent_call_id": null
  }
}
```

Valid decisions are `Once`, `Always`, and `Reject`. For a permission nested in
an `Envoy` event, set `parent_call_id` to the enclosing envoy
`parent_call_id`; otherwise use `null`.

### User questions

Render every question and return one list of selected labels per question:

```json
{
  "type": "Request",
  "UserQuestionReply": {
    "request_id": "question-123",
    "answers": [["Option A"], ["Other text"]],
    "parent_call_id": null
  }
}
```

Preserve question order. A multi-select question may contain multiple labels.
The UI may use a free-form answer where supported by its interaction model.
To cancel, send an empty outer `answers` array. A valid submission always
contains one inner array per question, so an unanswered multi-select page is
represented by an empty inner array and remains distinct from cancellation.

### Interactive command input

For `InputRequest`, use `secret` to decide whether to mask input. Reply with:

```json
{
  "type": "Request",
  "InputReply": {
    "request_id": "input-123",
    "text": "operator input",
    "parent_call_id": null
  }
}
```

An empty `text` cancels the input path. As with permission/questions, propagate
the envoy parent id for nested requests.

### Session context and configuration

Send `{ "type": "Request", "QuerySessionContext": null }` to request model,
tool, cached permission, skill, and MCP state. Mutation requests such as
`ToggleTool`, `ToggleMcpServer`, and `RevokePermission` are followed by a fresh
`SessionContext` snapshot.

The complete request and response variant inventory, field requirements, enum
values, and examples are in the AsyncAPI contract.

## Client robustness rules

A production frontend should:

1. Treat `Welcome` (and a monitor `snapshot`) as a replacement snapshot, not
   as incremental messages.
2. Route every `Round` event by `session_id`.
3. Preserve request ids and envoy `parent_call_id` values exactly.
4. Render the plain `ToolResult.output` when its `structured` variant is
   unknown. `ToolOutput` intentionally evolves as tools gain richer output.
5. Log and ignore unknown response/event variants rather than closing the
   socket. Rust enums are closed internally, but frontend/server versions may
   differ during development.
6. Reconnect with backoff after disconnect. There is currently no resume token;
   the next attach connection starts with a fresh `Welcome` snapshot, and the
   next monitor connection with a fresh `snapshot`.
7. Do not assume one request maps to one response. This is an asynchronous,
   multiplexed event protocol.
8. On a `--public` listener, send the bearer token on the handshake. Loopback
   connections need no token; treat any token you do hold as a secret — it
   grants full session access. For a public listener without TLS, front it with
   a TLS-terminating reverse proxy; the bearer token protects the handshake but
   not the wire from eavesdropping.
9. Upsert monitor diffs by `id`; handle `session_removed` even though hosted
   sessions are not yet torn down.
10. Handle `daemon_draining` (ADR-0101): sent once to every watch client
    when the daemon begins its graceful shutdown, right before the stream
    closes. Treat it as terminal for that daemon — surface a notice, do not
    immediately reconnect (the process exits within its grace budget); the
    next connection either discovers a fresh daemon or spawns one.
11. Send `version` on `Select` (ADR-0100 rule 4): the daemon refuses a
    mismatched client with a both-versions `Error` naming the fix. An absent
    `version` is served; a discovered record without one (`daemon.json`
    predating the field) is a mismatch for clients — stop the daemon and let
    it restart at the new version.

## Contract maintenance

The Rust serde types remain the runtime source of truth:

- envelope: `crates/neenee-runtime/src/serve.rs` (`Wire`, `AttachAction`,
  `ControlRequest`) and the daemon runtime `crates/neenee-runtime/src/host.rs`
- requests/responses/events: `crates/neenee-contracts/src/events.rs`
- monitor rows and status: `crates/neenee-contracts/src/monitor.rs`
- session registry (hosting + control verbs): `crates/neenee-runtime/src/registry.rs`
- transcript: `crates/neenee-contracts/src/message.rs`
- tool output: `crates/neenee-contracts/src/tool_output.rs`

Any wire-visible change to those types must update
`docs/reference/server.asyncapi.yaml`, this guide when behavior changes, and
the server contract tests. The AsyncAPI file can be opened in AsyncAPI Studio
or validated with the AsyncAPI CLI. The bind/auth model is specified by
`ServeOptions` / `ServeExpose` / `ServeHandle` in `serve.rs` (see ADR-0054).
