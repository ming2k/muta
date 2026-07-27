# Explanation

Conceptual background and design rationale.

## Architecture

| Page | Purpose |
|------|--------|
| [Crate layering](crate-layering.md) | The workspace crate topology, each layer's responsibility, the dependency DAG, and how a request flows across layers |

## Storage and persistence

How neenee decides where each file lives, why it follows the XDG Base
Directory Specification, and what is safe to delete.

| Page | Purpose |
|------|--------|
| [Persistence and the XDG layout](persistence.md) | The four-category model (config / data / state / cache), override precedence, and what each category means operationally |

## Agent design

The design canon for neenee's agent — how a round is steered, gated, isolated,
made durable, and kept honest. The pages share a set of recurring themes
(capability gating, isolation boundaries, durable vs ephemeral state,
streaming, fallback, control-plane separation) that the section index lays out
before the individual docs.

| Page | Purpose |
|------|--------|
| [Agent design](agent-design/index.md) | Section index: the recurring design themes, a suggested reading order, and how a round flows through the canon |
| [Harness architecture](agent-design/harness.md) | Control plane around provider calls, the round loop, safety bounds |
| [Rounds and turns](agent-design/rounds-and-turns.md) | The two-layer execution model (round vs turn) and the lifecycle inside one turn: declaration, gating, execution, and how outcomes re-enter the conversation |
| [Session persistence](agent-design/session-persistence.md) | The durable local session scene: model window, archived transcript, projection metadata, and resume recovery contract |
| [Model context](agent-design/model-context.md) | The request-scoped context sent to a provider: rebuilt system prompt, model-visible messages, tool schemas, tool-call arguments, and tool results |
| [Envoys](agent-design/envoys.md) | The `envoy` tool's read-only child agent: isolation model, event streaming, and the TUI zoom view |
| [MCP servers](agent-design/mcp.md) | Local stdio MCP server discovery, the `mcp__<server>__<tool>` wrapper, failure isolation, and access-tier gating |
| [User questions](agent-design/user-questions.md) | How the `ask_user` tool blocks the agent, renders a modal, and returns answers |
| [Unattended operation](agent-design/unattended.md) | The design intent of running without human intervention: what the flag enforces (the broker gate) versus the broader no-confirmations/no-questions posture it expresses, and where it is forced on |
| [Skills](agent-design/skills.md) | On-demand domain expertise: the catalog/body two-channel model, the source/priority cascade, and explicit versus implicit invocation |
| [Lifecycle hooks](agent-design/hooks.md) | User-configured actions on the agent's lifecycle events (PreToolUse, Stop, SessionStart, PreCompact…): one event axis with capability implied by the event |
| [Token accounting](agent-design/token-accounting.md) | How token counts are measured: upstream `usage` preferred with a char-class estimation fallback, the reported-vs-estimated ledger, and the accuracy report modal |
| [Prompt caching](agent-design/prompt-caching.md) | How prompt caching saves cost across providers: the three strategies (`Breakpoints` / `SessionKey` / `Automatic`), the per-protocol JSON paths, and the single rule that keeps cache savings honest (no inline reads) |


## Provider protocol and UI

Layers adjacent to the agent: the chat API primitives that shape it, the
wire-level contract with model servers, and the terminal rendering surface.

| Page | Purpose |
|------|--------|
| [Chat API primitives](chat-api-primitives.md) | The three protocol primitives — role authority, stateless memory, function calling — that shape the agent |
| [Terminal UI](tui.md) | How the TUI is built (full-screen app, semantic document model, live rendering) and why it is not terminal text |
| [Markdown rendering](markdown-rendering.md) | The custom markdown parser → semantic `Block` model → grid rendering pipeline: why it exists, the two-path parse, inline range tracking, adaptive table layout, and how selection returns original source |
| [Table hit-testing and cell-locked selection](table-hit-testing.md) | How table cells get a parallel hit-test system: layout, dual coordinate maps, cell-locked drag, and border-stripped copy |
| [Request flow](request-flow.md) | HTTP transaction shape, SSE streaming, and the ReAct loop's message evolution |
| [Interrupt semantics](interrupt-semantics.md) | Why neenee is streaming-only, the three-phase interrupt model (pre-response unsend / local drop / remote tool cancel), what survives in context, and the billing reality of an interrupted round |
| [Provider capabilities](provider-capabilities.md) | Where tool calling and reasoning actually live across model weights, serving runtime, and client |
| [Guided decoding](guided-decoding.md) | Constrained decoding, FSM compilation, and chat templates — the layer that guarantees valid tool calls |
