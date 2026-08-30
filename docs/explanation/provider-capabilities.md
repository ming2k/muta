# Provider capabilities

Tool calling and reasoning are often described as "model capabilities." In
practice they are the product of three cooperating layers, and muta
consumes them differently depending on which layers are present. This page
explains where each capability actually lives and why providers differ.

For the per-provider capability matrix, see
[Providers](../reference/providers.md). For the wire-level protocol muta
uses to call tools, see [Rounds and turns](agent-design/rounds-and-turns.md).

## Three layers

| Layer | Owns | Examples |
|-------|------|----------|
| Model weights | Behavior under tool-use prompts; whether reasoning is emitted at all | `deepseek-v4-flash`, `glm-5.2`, `kimi-k2.7-code`, `gemini-2.0-flash` |
| Serving runtime | HTTP API shape, `tools` / `tool_choice` field parsing, guided JSON decoding, SSE chunking, `reasoning_content` field passthrough | vLLM, SGLang, TGI, TensorRT-LLM, and the hosted gateways (`api.openai.com`, `api.deepseek.com`, `open.bigmodel.cn`, Moonshot, Volcengine Ark) |
| Client (muta) | Schema declaration, delta reconstruction, fallback parsing, registry, permission brokering | muta |
A tool call only succeeds when all three layers agree. A model whose weights
were never tool-tuned will emit free text even if the runtime accepts a
`tools` field; a runtime without guided decoding may return malformed JSON
even from a tool-tuned model; a client that fails to reassemble `delta`
fragments by `index` will drop calls mid-stream. muta's design assumes the
serving runtime implements the OpenAI Chat Completions contract and degrades
gracefully when it does not.

## Function calling is not native to weights

A base language model produces token sequences. "Calling a tool" is a
discipline imposed on top of that:

1. The serving runtime injects the client-supplied tool schemas into the
   prompt using the model's chat template (every model family has its own
   tool-use prompt format — Hermes, Llama-3, Qwen, GLM, etc.).
2. The runtime applies guided decoding (vLLM uses `outlines` or `xgrammar`;
   SGLang and TGI have equivalents) to constrain generation so the model
   emits a parseable `tool_calls` JSON structure instead of prose.
3. The runtime exposes the result through the OpenAI-compatible
   `choices[].message.tool_calls` field, or as `delta.tool_calls[]`
   fragments in SSE.

The model weights decide *which* tool to call and *what arguments* to write;
the runtime decides *whether* the output is structured as a tool call at all.
This is why two servings of the same weights (for example a raw vLLM instance
without a tool template versus the vendor's hosted endpoint) can behave
differently on the same `tools` payload.

muta trusts the runtime to deliver well-formed OpenAI-shaped tool calls.
The OpenAI-compatible adapter declares schemas and injects them into every
request body as the `tools` field. It does not implement its own guided
decoding or prompt templating — that is the runtime's job. For the mechanics
of constrained decoding and chat templates, see
[Guided decoding](guided-decoding.md).

## Reasoning and Chain Disclosure

The `reasoning_content` (or equivalent) field that some models emit is produced by the model weights (e.g. DeepSeek, Claude 3.7/Opus 4.7 adaptive thinking, GLM/Qwen reasoning models).

However, models differ in **chain disclosure**:
- **Disclosed reasoning chains (`ThinkingSupport::ReasoningContent`, `AnthropicAdaptive`, etc.)**: The model outputs its complete, authentic chain of thought. muta streams and renders these full reasoning blocks in the TUI.
- **Undisclosed / Summary-only reasoning (`ThinkingSupport::ReasoningSummary`)**: Some models (such as GPT-5.6 Sol / GPT-5.x) do not disclose their internal chain of thought over the API, returning only progress placeholders or brief summaries. To prevent creating empty or phantom thinking boxes that distort TUI layout, selection, and scroll math, muta gates undisclosed reasoning at message creation.
- **Default for unknown models**: Unrecognized/custom models default to disclosed (`chain_disclosed = true`) so local or third-party reasoning models stream freely.

### Route-Level Capability Overrides

Per ADR-0149, model capabilities resolve in three layers:
1. **User Overrides** (`RouteSettings::capability_overrides`)
2. **Remote Metadata** (Dynamic discovery)
3. **Static Baseline Registry**

Users can override thinking disclosure for any route in `config.toml`:

```toml
# Force thinking chain disclosure for a proxy that reveals internal reasoning
[providers.my_proxy.routes."gpt-5.6-sol".overrides]
thinking = "ReasoningContent"

# Or suppress placeholders for a custom model that returns opaque summaries
[providers.my_relay.routes."custom-model".overrides]
thinking = "ReasoningSummary"
```

## Streaming is a runtime contract

SSE chunking is part of the OpenAI Chat Completions contract that serving
runtimes implement. Two runtime behaviors matter to muta:

- **Delta fragmentation.** The runtime is allowed to split a single tool
  call across many SSE chunks, indexed by `delta.tool_calls[].index`. muta
  reassembles them by index in the streaming loop and does not execute a tool
  until the stream terminates.
- **Field selection.** A runtime may omit `reasoning_content` or
  `tool_calls` entirely from deltas where they have no new data. muta's
  parser treats every delta field as optional.

Providers that do not implement `stream_chat_events` fall back to the trait
default, which wraps `stream_chat` and emits only `TextDelta` events. They
cannot surface reasoning or stream tool-call deltas even when the underlying
service might support them. Google implements this event path for text,
usage, and native function-call parts, but it does not currently surface a
reasoning channel.

## Why providers differ

muta's provider adapters encode an opinionated mapping between the three
layers:

- **OpenAI-compatible registry presets** (`kimi-code`, `zai-code`, plus the
  catalog-built `openai`/`deepseek` multi-model entries, all backed by one
  shared OpenAI-compatible adapter) assume a runtime that fully
  implements the OpenAI Chat Completions contract including `tools`,
  `tool_choice`, `reasoning_content`, and SSE tool-call deltas. The registry
  presets are pure data, so they inherit every capability from that one
  shared implementation.
- **Anthropic** (`AnthropicMessagesProvider`) speaks the `/messages` wire
  format with `x-api-key` auth; muta converts the internal tool schema
  into Anthropic `tools` and replays results as `tool_result` blocks.
- **Google** (`GoogleProvider`) speaks a different request shape
  (`systemInstruction`, `model`/`user` roles, and
  `tools[].functionDeclarations`). muta bridges Google's native
  function-calling API by converting the internal OpenAI-shaped tool schema
  into Google declarations, reading `functionCall` parts, and replaying tool
  results as `functionResponse` parts.
- **ChatGPT Responses** (`OpenAiResponsesProvider`) speaks the OpenAI Responses
  API (`/responses` endpoint, `response.*` SSE events) used by the ChatGPT
  subscription backend.

The practical consequence: OpenAI-compatible, Anthropic, and Google providers
can use native structured tool calls. On any provider that omits native tool
support, the model must emit
`{"tool": "<name>", "arguments": {…}}` as ordinary assistant text, which the
client parses back into a tool call after the fact. See
[Rounds and turns](agent-design/rounds-and-turns.md) for the fallback mechanics.

## Capability negotiation summary

| Capability | Negotiated? | Source of truth |
|------------|-------------|-----------------|
| Tool schemas | Declared by client on every request | Client injects the `tools` field each request |
| Tool selection | Model weights decide | `tool_choice: "auto"` lets the model pick |
| Structured tool output | Runtime guided decoding | Serving runtime (vLLM, hosted gateway, etc.) |
| Reasoning | Not negotiated | Model weights emit; runtime passes through; client observes |
| SSE delta fragmentation | Runtime contract | OpenAI-compatible streaming protocol |
| Fallback text protocol | Client-side | Client parses assistant content |

## See also

- [Providers](../reference/providers.md) — per-provider capability matrix
- [Rounds and turns](agent-design/rounds-and-turns.md) — schema injection, streaming, fallback
- [Built-in tools](../reference/tools/index.md) — what schemas get declared
- [Harness architecture](agent-design/harness.md) — how the harness consumes these
  capabilities per round
