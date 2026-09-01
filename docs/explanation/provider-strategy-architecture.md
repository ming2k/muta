# Provider Multi-Strategy Adaptation Architecture

Every large language model provider presents a divergent API surface: wire
protocols differ, thinking and reasoning tokens appear under different schema
keys, context limits and effort controls follow distinct vocabularies, and
billing endpoints report remaining quotas using proprietary semantics.

muta resolves these divergences through a decoupled, multi-layered strategy
architecture across six core dimensions. This page explains the design
rationale and mechanics behind each layer.

For step-by-step instructions on registering a new provider, see
[How to add a provider](../how-to/add-a-provider.md). For the provider
matrix and parameter schemas, see [Providers](../reference/providers.md).

```text
                        ┌──────────────────────────────┐
                        │   User Request / TUI Input   │
                        └──────────────┬───────────────┘
                                       │
                        ┌──────────────▼───────────────┐
                        │   Three-Layer Capabilities   │
                        │    (User > Remote > Base)    │
                        └──────────────┬───────────────┘
                                       │
     ┌─────────────────────────────────┼─────────────────────────────────┐
     │                                 │                                 │
┌────▼─────────────┐          ┌────────▼─────────┐             ┌─────────▼────────┐
│  Wire Protocol   │          │  Reasoning &     │             │  Live Discovery  │
│  Inference Core  │          │  Effort Strategy │             │  & ETag Cache    │
└────┬─────────────┘          └────────┬─────────┘             └─────────┬────────┘
     │                                 │                                 │
     └─────────────────────────────────┼─────────────────────────────────┘
                                       │
                        ┌──────────────▼───────────────┐
                        │   Authoritative Ledger &     │
                        │   Usage / Quota Fetchers     │
                        └──────────────────────────────┘
```

---

## 1. Wire Inference Protocols

Inference drivers in `muta-llm-client` decouple high-level agent rounds from
transport-level HTTP and SSE details. Each route resolves to one of four
canonical wire protocols:

| Wire Protocol | Primary Vendors | Key Wire Characteristics |
|---------------|-----------------|--------------------------|
| `OpenAiChatCompletions` | DeepSeek, Kimi, Z.AI, OpenCode, Qwen, Ollama | Standard `/v1/chat/completions`, `tools` schema, `reasoning_content` stream deltas |
| `OpenAiResponses` | OpenAI (o1, o3, GPT-5) | Structured items, background reasoning summaries, stateful conversation tokens |
| `AnthropicMessages` | Anthropic Claude | Native `/v1/messages`, `cache_control` prompt caching, adaptive thinking budgets |
| `GoogleGenerateContent` | Google Gemini | `/v1beta/models/{id}:generateContent`, `systemInstruction`, inline multimodal parts |

The driver handles SSE frame parsing, delta reassembly across fragmented
indices, and error code translation so the orchestration harness operates on
uniform round events regardless of the underlying vendor.

---

## 2. Three-Layer Capability Resolution Order

Model capabilities (context window size, reasoning support, tool calling, and
multimodal vision) resolve strictly according to the canonical three-layer
resolution order defined in ADR-0149:

```text
1. User Overrides (Top Layer)
   └─ RouteSettings::capability_overrides in config.toml
      (Wins over everything: forces knobs on/off per route)

2. Remote Live Metadata (Middle Layer)
   └─ RemoteModelMetadata advertised by trusted live catalogs
      (Fitting templates update context window, vision, and effort ladders)

3. Static Baseline Registry (Bottom Layer)
   └─ BaselineModels statically submitted at link time
      (Deterministic fallback for off-line operation)
```

### Runtime Model Fitting (ADR-0065)

When a trusted provider advertises a model identifier unrecognized by the
static baseline, the runtime-fitted overlay registers a `FittedModel`
dynamically. The agent can immediately route requests to newly released models
without requiring a recompilation or application update.

---

## 3. Dynamic Model Discovery and Conditional Caching

Models change dynamically as vendors deploy updates. muta queries provider
discovery endpoints asynchronously during startup and manual connection
refreshes:

- **OpenAI & Anthropic**: `GET /v1/models`
- **Google Cloud Code**: `POST /v1internal:fetchAvailableModels`
- **ChatGPT Codex**: `GET /backend-api/codex/models`

### RFC 7232 Conditional Revalidation

Discovery requests store HTTP `ETag` validators in the local discovery cache.
Subsequent startup checks issue conditional requests carrying `If-None-Match`.
When the server returns HTTP `304 Not Modified`, the existing cached catalog
is retained instantly, reducing startup latency.

---

## 4. Thinking and Reasoning Effort Strategies

Providers implement reasoning controls differently:

- **Thinking Chain Disclosure**: Models marked with `ThinkingSupport::None`
  emit standard text; `ThinkingSupport::ReasoningContent` exposes the full
  internal chain of thought for streaming display; `ThinkingSupport::ReasoningSummary`
  indicates hidden or summarized internal thinking.
- **Effort Ladders**:
  - **Claude**: Discretized tiers (`Low`, `Medium`, `High`, `Max`).
  - **Gemini**: Quantitative token budgets (`EFFORT_GEMINI_BUDGET`) or discrete
    levels (`EFFORT_GEMINI_LEVEL`).
  - **OpenAI**: Lowercase discrete tiers (`low`, `medium`, `high`).
  - **Kimi**: Dynamic platform rungs read from endpoint capability arrays.

The catalog clamps user-requested effort levels to the highest supported tier
advertised by the active model channel.

---

## 5. Usage, Balance, and Quota Monitoring

Provider account entitlements and balance querying vary widely. The
`ProviderUsageFetcher` trait provides a uniform abstraction for polling
account balances and quota health:

```text
┌───────────────────────────┐
│   ProviderUsageFetcher    │
└─────────────┬─────────────┘
              │
   ┌──────────┼──────────────┬──────────────┬──────────────┐
   │          │              │              │              │
┌──▼───────┐ ┌▼───────────┐ ┌▼───────────┐ ┌▼───────────┐ ┌▼───────────┐
│ DeepSeek │ │    Kimi    │ │ OpenRouter │ │SiliconFlow │ │Antigravity│
│ Balance  │ │  Vouchers  │ │ Key Credit │ │Total/Charge│ │Quota Bucket│
└──────────┘ └────────────┘ └────────────┘ └────────────┘ └───────────┘
```

- **DeepSeek**: Polls `GET /user/balance` for currency breakdown (topped-up,
  granted, and total balance).
- **Kimi (Moonshot)**: Polls `GET /v1/users/me/balance` for cash balances and
  active voucher allocations.
- **OpenRouter**: Polls `GET /api/v1/auth/key` for credit limits and rate limits.
- **SiliconFlow**: Polls `GET /v1/user/info` for total and charge quotas.
- **Google Antigravity**: Polls `POST /v1internal:retrieveUserQuotaSummary` to
  track multi-model remaining quota fractions and replenishment timestamps.

---

## 6. Authoritative Token Accounting and Performance Telemetry

Token accounting follows ADR-0122:

- **Reported vs. Estimated**: The `TokenSourceLedger` prioritizes authoritative
  upstream usage structures returned in completion chunks. Local character-class
  estimation serves as a fallback only when upstream usage is absent.
- **Prompt Cache Tracking**: Measures prompt cache read tokens, write tokens,
  and cache misses separately to compute accurate cost savings.
- **Microsecond Latency Breakdown**: Records monotonic intervals for connection
  setup (`stream_ready_us`), time-to-first-token (`ttft_us`), active generation
  (`stream_us`), and total turn duration (`e2e_us`), yielding exact Stream
  Tokens-Per-Second (TPS) metrics.
- **Durable Sinks**: Completed turn ledgers stream into session metrics stores
  for cross-session analytics.
