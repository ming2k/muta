# Server WebSocket API

This page is the frontend integration guide for the current `neenee-server`
hot-attach transport. Its machine-readable contract is
[`server.asyncapi.yaml`](server.asyncapi.yaml).

> **Why AsyncAPI rather than OpenAPI or TypeSpec?** The current public surface
> is a bidirectional WebSocket event stream, not an HTTP request/response API.
> AsyncAPI models channels, send/receive operations, messages, and ordering
> directly. OpenAPI would only describe the HTTP upgrade incompletely, while a
> TypeSpec file would still need a project-specific WebSocket convention. If
> the planned daemon later adds REST endpoints, describe those separately with
> OpenAPI/TypeSpec and keep this WebSocket channel in AsyncAPI.

## Scope and current limitations

This contract describes `crates/neenee-server/src/serve.rs` as it exists now:

- `/serve [port]` starts a listener attached to the **currently running TUI
  session**.
- It binds `0.0.0.0`, and accepts WebSocket upgrades at `/` (the handshake path
  is not restricted in the current implementation).
- Omitting `port` asks the OS for a free port; the TUI prints the selected port.
- Invoking `/serve` again with no argument stops accepting new connections.
- There is no authentication, TLS, origin check, version negotiation,
  subprotocol, HTTP endpoint, or standalone daemon yet.
- Multiple clients may attach. Every client can send requests into the same
  agent request queue and receives the broadcast response stream.
- A slow client may lose broadcast events; the server logs the lag and
  continues. Reconnect to obtain a fresh transcript snapshot.

Because the listener binds all interfaces and has no authentication, use it on
a trusted local network only. Prefer firewalling the selected port or tunneling
it through SSH. Do not expose it directly to the public internet.

## Start and connect

From a running `neenee-code` TUI:

```text
/serve 8765
```

Then connect from a browser or Node client:

```ts
const socket = new WebSocket("ws://127.0.0.1:8765/");

socket.addEventListener("open", () => {
  socket.send(JSON.stringify({
    type: "Request",
    Chat: {
      text: "Summarize the current project",
      images: [],
      sent_at_ms: Date.now(),
    },
  }));
});

socket.addEventListener("message", ({ data }) => {
  if (typeof data !== "string") return;
  const frame = JSON.parse(data);

  if (frame.type === "History") {
    replaceTranscript(frame.messages);
    return;
  }

  if (frame.type === "Response") {
    handleAgentResponse(frame);
  }
});
```

The server ignores binary messages. Send one complete JSON value per WebSocket
text frame; newline separators are not required despite the older source
comment referring to newline-delimited JSON.

## Frame lifecycle

The connection has a simple ordering contract:

1. The server accepts the WebSocket handshake.
2. The server sends exactly one `History` frame containing the full persisted
   transcript at that moment.
3. The connection carries zero or more live `Response` frames.
4. The frontend may send `Request` frames at any time after the socket opens.

The client should process `History` before rendering subsequent live events.
Note that the server subscribes to the live broadcast **after** loading and
sending history, so an event produced in that narrow interval can be missed.
The transport currently has no sequence numbers or replay cursor.

## JSON representation

There are two discriminator levels:

- `type` identifies the transport envelope: `Request`, `History`, or `Response`.
- Rust enums use serde's default externally tagged representation.

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

These edge cases are exercised by the server integration test; use the
AsyncAPI contract and that test as the authority for envelope shapes.

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

1. Treat `History` as a replacement snapshot, not as incremental messages.
2. Route every `Round` event by `session_id`.
3. Preserve request ids and envoy `parent_call_id` values exactly.
4. Render the plain `ToolResult.output` when its `structured` variant is
   unknown. `ToolOutput` intentionally evolves as tools gain richer output.
5. Log and ignore unknown response/event variants rather than closing the
   socket. Rust enums are closed internally, but frontend/server versions may
   differ during development.
6. Reconnect with backoff after disconnect. There is currently no resume token;
   the next connection starts with a fresh `History` snapshot.
7. Do not assume one request maps to one response. This is an asynchronous,
   multiplexed event protocol.
8. Do not send secrets until transport authentication/TLS exists, especially
   when connected over a non-loopback network.

## Contract maintenance

The Rust serde types remain the runtime source of truth:

- envelope: `crates/neenee-server/src/serve.rs` (`Wire`)
- requests/responses/events: `crates/neenee-core/src/events.rs`
- transcript: `crates/neenee-core/src/message.rs`
- tool output: `crates/neenee-core/src/tool_output.rs`

Any wire-visible change to those types must update
`docs/reference/server.asyncapi.yaml`, this guide when behavior changes, and the
server contract tests. The AsyncAPI file can be opened in AsyncAPI Studio or
validated with the AsyncAPI CLI.
