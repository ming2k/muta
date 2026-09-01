# ADR-0166: Lossless, fail-closed stateless Responses replay

- **Status:** Accepted
- **Date:** 2026-09-01

## Context

OpenAI Responses has two continuation modes. A stored route can send
`previous_response_id`; a stateless route must resend every prior response
output item, including `reasoning.encrypted_content`. ChatGPT Subscription uses
the stateless mode and accepts streaming requests only.

The adapter previously stored only `response.completed.response.output`. The
ChatGPT Subscription stream can leave that terminal array empty while sending
the complete items through `response.output_item.done`. An empty array was then
treated as a valid opaque replay artifact. That suppressed the assistant's
semantic text and function calls, after which tool-trace projection silently
removed the now-orphaned tool outputs. Each following model request therefore
lost the preceding turn and could repeat the same tools indefinitely.

The same adapter also implemented `Provider::chat` with `stream: false`, even
though the subscription endpoint rejects that transport shape. Steward and
digest callers could therefore fail independently of ordinary agent streams.

## Decision

1. Make the event stream the canonical response path for streaming-only
   Responses dialects. Implement completion-style calls by collecting the same
   validated event stream; never send an unsupported non-streaming request.
2. Accumulate every complete `response.output_item.done` item by
   `output_index`. At `response.completed`, use a non-empty terminal `output`
   array when present; otherwise reconstruct it from the complete indexed
   items. Preserve every JSON field, including encrypted reasoning and unknown
   future item types.
3. Treat stream completion as a strict protocol boundary. Output indexes must
   be contiguous, output items must be typed objects, the terminal event must
   be unique and final, and a completed response must contain at least one
   replayable output item.
4. Model opaque replay artifacts as three states: absent, valid, or corrupt.
   Absence permits semantic projection for turns produced by another route. A
   non-empty typed array is replayed exactly. Empty or malformed artifacts fail
   request construction with an explicit new-session instruction.
5. Reject orphaned function outputs and empty call identifiers. Unanswered
   function calls from interrupted turns may still be omitted, but no function
   output may disappear silently.
6. Do not migrate corrupt historical artifacts. Encrypted reasoning and exact
   provider-owned items cannot be reconstructed from the semantic transcript;
   inventing them would create false continuity. Affected sessions must start a
   new conversation on the stateless route.

## Alternatives considered

- **Fall back to assistant text and local tool calls for empty artifacts.**
  Rejected because this hides persisted corruption and still omits encrypted
  reasoning required for faithful stateless continuation.
- **Synthesize provider output items from the semantic transcript.** Rejected
  because provider-owned identifiers, item fields, and encrypted state are
  irrecoverable.
- **Rely on doom guards, hard turn limits, or lower reasoning effort.** Rejected
  because these reduce the cost of a loop without restoring missing history.
- **Keep separate streaming and non-streaming subscription implementations.**
  Rejected because the backend exposes only one valid transport contract and a
  second path can drift.

## Consequences

- New ChatGPT Subscription turns retain exact model state and tool history
  across stateless requests.
- Corrupt historical sessions stop before another provider request and explain
  that a new session is required.
- Provider protocol defects surface as classified errors instead of being
  converted into plausible but incomplete conversations.
- The stream parser retains complete output items for the lifetime of one
  response, increasing memory by one response output only.
- Standard stored Responses routes continue to use `previous_response_id` and
  remain unaffected by the stateless replay rule.

## References

- [OpenAI latest-model guide](https://developers.openai.com/api/docs/guides/latest-model)
- [OpenAI Responses streaming API reference](https://developers.openai.com/api/reference/typescript/resources/beta/subresources/responses/methods/create)
- [ADR-0148](0148-doom-guard-threshold-and-content-aware-mutation-signatures.md)
- [ADR-0161](0161-route-scoped-inference-protocols-and-prompt-cache-contracts.md)
