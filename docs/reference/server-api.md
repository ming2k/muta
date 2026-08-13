# Server WebSocket API

This page is the frontend integration guide for the session-host transport.
Its machine-readable contract is [`server.asyncapi.yaml`](server.asyncapi.yaml).

> **Why AsyncAPI rather than OpenAPI or TypeSpec?** The public surface is a
> bidirectional WebSocket event stream, not an HTTP request/response API.
> AsyncAPI models channels, send/receive operations, messages, and ordering
> directly. OpenAPI would only describe the HTTP upgrade incompletely, while a
> TypeSpec file would still need a project-specific WebSocket convention.

## Roles and entry points

The daemon (`neenee serve`, or the `neenee-server` binary) serves one
control-plane endpoint per user, on a Unix domain socket by default and on
TCP when exposed (ADR-0096). Three client roles share the protocol,
distinguished by the first frame the client sends after the upgrade
(`Select`):

| Role | Handshake | Direction | Purpose |
|------|-----------|-----------|---------|
| **Attach** | `Select{action: New \| Attach(id?)}` | bidirectional | Drive a session: send `Request`s, receive `Response`s |
| **Monitor** | `Select{action: Monitor{watch, include_idle}}` | server → client | Observe every session the daemon knows about (ADR-0093) |
| **Control** | `Select{action: Control(verb)}` | one round-trip | Manage sessions: create / prompt / interrupt / approve / kill (ADR-0096) |

The in-TUI `/serve` command (legacy single-session prehost) is superseded by
the unified daemon; the protocol below is the daemon's control plane.

## Bind and authentication

- **Unix domain socket (default, ADR-0096):** the daemon's primary local
  channel at `$XDG_RUNTIME_DIR/neenee/daemon.sock`, `0600` inside a `0700`
  runtime dir. No bearer token — the filesystem permissions are the auth
  boundary. CLI and TUI use this.
- **TCP loopback:** binds `127.0.0.1` and requires no token — a local
  co-process is trusted.
- **TCP exposed (`--expose` / `--public`):** binds `0.0.0.0` and **requires a
  bearer token** (`Authorization: Bearer <token>` on the handshake, else HTTP
  401). The daemon generates and prints the token on startup. Exposing is an
  explicit opt-in that always carries a token; front it with a
  TLS-terminating reverse proxy for remote use (the token protects the
  handshake, not the wire).

### Security model

| Mode | Bind | Auth | Use |
|------|------|------|-----|
| default (Unix socket) | `$XDG_RUNTIME_DIR/neenee/daemon.sock` | none — filesystem permissions (`0600` in a `0700` runtime dir) are the boundary | local CLI / TUI |
| default (TCP loopback) | `127.0.0.1` | none | local co-process on the same machine |
| `--public` (`neenee serve`) / `--expose` (`neenee-server` binary) | `0.0.0.0` | bearer token (mandatory) | remote client / another machine |

Because the default binds a Unix socket plus loopback, a casual host exposes
nothing beyond this machine. Exposure is an explicit opt-in that cannot
happen without a token. See ADR-0054 for the rationale. For a remote client
walkthrough see
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
{ "type": "Error", "message": "…" }
```

- `Welcome` binds the connection: `messages` is the full persisted transcript
  (a replacement snapshot, not incremental), `round_counter` the authoritative
  monotonic round counter. Process it before rendering subsequent live events.
- `Pick` means several sessions are hosted and the client must choose
  (`Attach(Some(id))` on a new connection).
- `Error` is terminal.

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

(Browsers cannot set headers on `new WebSocket()`; use a client that can, or a
proxy that injects the header.)

## Monitor: observe the host (ADR-0093)

```json
{ "type": "Select", "action": { "monitor": { "watch": true, "include_idle": false } } }
```

The server sends `{ "type": "Monitor", "kind": "snapshot", … }` first, then —
while `watch` holds — `session_added` / `session_updated` / `session_removed`
diffs. With `watch: false` it closes after the snapshot (one-shot poll, which
is what `neenee status` does). Each diff carries a whole
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

`ControlReply` is `{ ok, session_id?, error? }`. On `ok:false`, `error`
explains (unknown session, host cannot create, …). The connection closes
after the reply; issue another verb on a fresh connection.

## JSON representation

Two discriminator levels:

- `type` identifies the transport envelope: `Select`, `Welcome`, `Pick`,
  `Error`, `Request`, `Response`, `Monitor`.
- `Monitor` frames carry a second `kind` discriminator on the flattened
  `MonitorEvent`: `snapshot`, `session_added`, `session_updated`,
  `session_removed`.
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
which includes the prompt and images to restore to the input editor.

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

## Contract maintenance

The Rust serde types remain the runtime source of truth:

- envelope: `crates/neenee-transport/src/serve.rs` (`Wire`, `AttachAction`,
  `ControlRequest`) and the daemon runtime `crates/neenee-transport/src/host.rs`
- requests/responses/events: `crates/neenee-core/src/events.rs`
- monitor rows and status: `crates/neenee-core/src/monitor.rs`
- session registry (hosting + control verbs): `crates/neenee-transport/src/registry.rs`
- transcript: `crates/neenee-core/src/message.rs`
- tool output: `crates/neenee-core/src/tool_output.rs`

Any wire-visible change to those types must update
`docs/reference/server.asyncapi.yaml`, this guide when behavior changes, and
the server contract tests. The AsyncAPI file can be opened in AsyncAPI Studio
or validated with the AsyncAPI CLI. The bind/auth model is specified by
`ServeOptions` / `ServeExpose` / `ServeHandle` in `serve.rs` (see ADR-0054).
