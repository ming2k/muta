# Tool-call wire formats: OpenAI vs Anthropic

Both the OpenAI Chat Completions API and the Anthropic Messages API support
*native function calling*, and both can return **multiple tool calls in a single
assistant response** (parallel tool use). muta speaks both: models tagged
`WireFormat::OpenAi` go through `OpenAiChatCompletionsProvider`, models tagged
`WireFormat::AnthropicCompat` go through `AnthropicMessagesProvider`
(`crates/muta-contracts/src/model.rs`, `crates/muta-providers/src/`).

The two protocols carry the *same* loop — declare tools, model emits calls,
client runs them, client appends results, re-send — but disagree on almost every
field name and on how messages are shaped. This page names those differences and
shows where muta bridges them. For the loop itself, see
[Chat API primitives](chat-api-primitives.md). For which provider implements
which capability, see [Provider capabilities](provider-capabilities.md).

## Yes, both can return multiple calls at once

When the requested tools are independent (e.g. "read file A and file B"), the
model may return several calls in one response. The two formats express this
differently:

- **OpenAI** — the assistant message carries a `tool_calls` array; each element
  has its own `id`. You reply with **one `role:"tool"` message per call**, each
  tagged with the matching `tool_call_id`.
- **Anthropic** — the assistant message's `content` array carries several
  `tool_use` blocks, each with its own `id`. You reply with **one `role:"user"`
  message** whose `content` holds one `tool_result` block per call, each tagged
  with the matching `tool_use_id`.

Either way the contract is: **every call you were given must get a result back**,
or the next request is rejected. (OpenAI lets you set
`parallel_tool_calls: false` to force one-at-a-time; Anthropic guides the same
through `tool_choice` / prompting.) muta enforces the pairing itself — see
*Orphan stripping* below.

## Field-by-field comparison

| Concern | OpenAI Chat Completions | Anthropic Messages |
|---|---|---|
| Endpoint | `POST /v1/chat/completions` | `POST /v1/messages` |
| Auth header | `Authorization: Bearer <key>` | `x-api-key: <key>` + `anthropic-version: 2023-06-01` |
| System prompt | a `role:"system"` message in the array | top-level `system` string (not a message) |
| Message content | usually a plain string | always a typed **block array** (`text` / `image` / `tool_use` / `tool_result`) |
| `max_tokens` | optional | **required** |
| Tool schema | `tools:[{type:"function", function:{name, description, parameters}}]` | `tools:[{name, description, input_schema}]` |
| Model emits a call | `message.tool_calls[]` (sibling of `content`) | a `tool_use` block inside `content[]` |
| Call arguments | `function.arguments` — a **JSON string** (parse again) | `input` — a **JSON object** |
| Returning a result | `role:"tool"` message, **one per call** | `tool_result` block on a `role:"user"` message |
| Result ↔ call link | `tool_call_id` | `tool_use_id` |
| Roles available | `system`, `user`, `assistant`, `tool` | `user`, `assistant` only |

### The same exchange, both ways

OpenAI:

```json
// model returns
{ "role": "assistant", "content": null,
  "tool_calls": [
    { "id": "call_1", "type": "function",
      "function": { "name": "read_text", "arguments": "{\"path\":\"a.txt\"}" } }
  ] }
// you reply — one role:"tool" message per call
{ "role": "tool", "tool_call_id": "call_1", "content": "…file contents…" }
```

Anthropic:

```json
// model returns
{ "role": "assistant",
  "content": [
    { "type": "tool_use", "id": "toolu_1", "name": "read_text",
      "input": { "path": "a.txt" } }
  ] }
// you reply — tool_result block on a role:"user" message
{ "role": "user",
  "content": [
    { "type": "tool_result", "tool_use_id": "toolu_1", "content": "…file contents…" }
  ] }
```

## How muta bridges the two

muta's internal `Message` (`crates/muta-contracts/src/`) is **OpenAI-shaped**: a
flat list with a `Tool` role, `tool_calls`, a JSON-string `arguments`, and a
`tool_call_id`. `OpenAiChatCompletionsProvider` serializes that almost verbatim. The
work lives in `AnthropicMessagesProvider::request_body`
(`crates/muta-providers/src/anthropic_compat.rs`), which reshapes the flat
list into Messages format on the way out:

- **System lifting.** Leading `Role::System` messages are concatenated and moved
  to the top-level `system` field; no system role survives in `messages`.
- **Tool schema rewrite.** The harness produces OpenAI
  `{type:"function", function:{…parameters}}` specs; the provider rewrites each
  to `{name, description, input_schema}`, mapping `parameters` → `input_schema`
  verbatim (both are JSON Schema).
- **Tool result re-roling.** A `Role::Tool` message becomes a `role:"user"`
  message carrying a single `tool_result` block keyed by `tool_use_id`.
- **Arguments re-typing.** `tool_calls[].arguments` (a JSON *string*) is parsed
  into the `input` *object* each `tool_use` block needs (`parse_arguments`).
- **Orphan stripping.** Before sending, the provider collects every answered
  call id and drops any `tool_use` whose result is missing — both APIs reject a
  call with no matching result, so an interrupted round can't poison the next
  request.

On the way back, both providers reassemble the response into one
`Role::Assistant` `Message`: OpenAI reads `tool_calls[]` directly; the Anthropic
provider walks the `content` blocks, turning each `tool_use` into a `ToolCall`
and each `text`/`thinking` block into content/reasoning.

### Call identity across provider boundaries

A provider-issued call id is a correlation label for one call and its result,
not a session-global identity. Long histories and provider switches can
therefore contain separate completed calls that happen to carry the same id.
The durable transcript preserves the ids exactly as returned because it is the
audit and resume source of truth.

The provider-facing request is stricter than the durable transcript. Before a
tool trace reaches the Responses wire, its adapter pairs calls and results by
their occurrences, removes unpaired halves, and assigns deterministic
request-local ids. A remap always changes both the call and its result, so
their relationship stays intact. The remap exists only in the request
projection and never mutates session history. Other adapters enforce the
validity rules of their own wire protocols at the same projection boundary.

Stored Responses continuations are a special case. When a
`previous_response_id` already carries the parent call, the new request sends
only its result and retains the original call id needed by the remote chain.
Locally replayed calls must still avoid colliding with ids in that chain.

### Streaming deltas differ too

Parallel calls stream as fragments that the client must reassemble by index:

- **OpenAI** streams `delta.tool_calls[].index`; arguments arrive as
  `function.arguments` string fragments.
- **Anthropic** streams `content_block_start` (opens a `tool_use` block at an
  `index`, carrying `id` + `name` up front) then `input_json_delta` fragments
  (`partial_json`) for the arguments, closed by `content_block_stop`.

muta normalizes both into `ProviderStreamEvent::ToolCallDelta { index, id,
name, arguments }` and concatenates `arguments` fragments per index, so the rest
of the harness never sees the wire difference. See
`parse_anthropic_stream_data` and the OpenAI delta loop in `openai_compat.rs`.

## Practical guidance

- **Targeting both?** Keep one OpenAI-shaped internal representation and convert
  at the provider boundary — exactly what muta does. Don't fork the harness on
  wire format.
- **Parsing arguments:** OpenAI needs `JSON.parse(arguments)`; Anthropic gives
  you an object already.
- **Don't drop results:** return a result for *every* call the model made, in
  the format that API expects (N `tool` messages vs N `tool_result` blocks in
  one user message).
- **Order/dependency:** if your tools have side effects or must run in sequence,
  disable parallel calls rather than relying on the model to serialize them.

## See also

- [Chat API primitives](chat-api-primitives.md) — the role/stateless/function-calling contract both formats implement
- [Provider capabilities](provider-capabilities.md) — where tool calling actually lives across weights, runtime, and client
- [Providers](../reference/providers.md) — per-provider capability matrix and the `WireFormat` split
- [How to add a provider](../how-to/add-a-provider.md) — implementing a new wire adapter
- Official references:
  [OpenAI function calling](https://platform.openai.com/docs/guides/function-calling),
  [Anthropic tool use](https://docs.anthropic.com/en/docs/build-with-claude/tool-use)
