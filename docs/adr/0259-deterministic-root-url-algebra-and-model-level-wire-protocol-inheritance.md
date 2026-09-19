# 0259. Deterministic Root URL Algebra and Model-Level Wire Protocol Inheritance

- **Status:** Accepted
- **Date:** 2026-10-15
- **Implementation:** `muta-contracts`, `muta-providers`, `muta-agent`
- **Builds on:** ADR-0149 (model capability resolution order), ADR-0171 (three-layer model catalog), ADR-0199 (unified cascading model resolution), ADR-0258 (first-class declarative model providers)
- **Supersedes:** `copilot_route` ad-hoc branch in `crates/muta-agent/src/catalog/derive.rs`, heuristic `.strip_suffix` in `crates/muta-providers/src/list_models.rs`

## Context and Problem Statement

The model catalog resolution and discovery mechanisms in `muta` suffered from two structural defects:

1. **Heuristic URL Guessing (`.strip_suffix`)**:
   In `crates/muta-providers/src/list_models.rs:models_endpoint_for`, the discovery URL was computed by taking the chat completion endpoint and peeling off string suffixes:
   ```rust
   trimmed.strip_suffix("/chat/completions").unwrap_or(trimmed) + "/models"
   ```
   For corporate proxies, versioned gateways (`/v1beta`), or subpath proxies (`/proxy/team/v1/chat/completions`), this substring stripping produced broken URLs (e.g. `/proxy/team/models` instead of `/proxy/team/v1/models`), causing 404s and silent catalog discovery failures.

2. **Model-Level Wire Protocol Disconnect**:
   `muta_contracts::Model` defines `pub protocol: WireProtocol`, and `DiscoveredModel` defines `pub protocol: Option<WireProtocol>`. However, `derive.rs:base_route` completely ignored these model-level properties, resolving exclusively through `provider.protocol`.
   To support GitHub Copilot—which serves Claude over `/v1/messages`, GPT over `/chat/completions`, and o-series over `/responses`—the codebase created a bespoke bypass function (`copilot_route`) rather than honoring the model's protocol in the general route resolution path.

## Decision Drivers

- **Deterministic URL Algebra**: Endpoints must be derived via strict algebraic URL path composition (`Root URL` + `Relative Path`), never through reverse heuristic stripping.
- **Model Protocol Autonomy with Provider Default Fallback**: Wire protocol is an inherent attribute of a model. If a model advertises its protocol, it must be honored; otherwise, it inherits the provider's default protocol.
- **Eradication of Bespoke Routing Functions**: Multi-wire relays and Copilot must be regular instances of the general route resolution algorithm; `copilot_route` must be deleted.
- **Typed Discovery Normalization Pipeline**: Catalog parsing must be structured as a typed `CatalogParser` trait with vendor adapters, with non-blocking, fail-safe degradation.

## Decision Outcome

We adopt **Deterministic Root URL Algebra and Model-Level Wire Protocol Inheritance**.

### 1. Root URL Algebra (Zero Heuristic Stripping)

A provider's endpoint is strictly specified as an **API Root URL** (e.g. `https://api.openai.com/v1`, `https://api.anthropic.com/v1`, `https://relay.corp.com/v1`).
All transport and catalog URLs are derived by deterministic path appending:

$$\text{Inference URL}(wire, model) = \text{RootUrl} \oplus \text{RelativeInferencePath}(wire, model)$$
$$\text{Catalog URL}(format) = \text{RootUrl} \oplus \text{RelativeCatalogPath}(format)$$

| Wire Protocol | Relative Inference Path |
| :--- | :--- |
| `ChatCompletions` | `chat/completions` |
| `Responses` | `responses` |
| `AnthropicMessages` | `messages` |
| `GoogleGemini` | `models/{model}:generateContent` |

| Catalog Format | Relative Discovery Path |
| :--- | :--- |
| `OpenAi` / `Anthropic` / `Google` | `models` |
| `GoogleCloudCode` | `v1internal:fetchAvailableModels` |
| `Codex` | `backend-api/codex/models` |

### 2. Model-Level Wire Protocol Cascading Resolution

The effective wire protocol for a runtime channel is computed through a strict 4-level cascade:

$$\text{Effective Protocol}(c, m) = \text{Override}(c, m).\text{wire} \;\;{\prec}\;\; \text{Discovered}(m).\text{wire} \;\;{\prec}\;\; \text{Baseline}(p, m).\text{wire} \;\;{\prec}\;\; p.\text{default\_wire} \;\;{\prec}\;\; \text{ChatCompletions}$$

1. **Connection Model Override**: User explicitly pinned a model to a protocol in `connections.toml`.
2. **Discovered Metadata**: The remote catalog payload explicitly advertised the protocol (e.g. Copilot's `endpoints` or models.dev metadata).
3. **Provider Baseline**: The model declared a protocol in its compiled-in `Model` specification.
4. **Provider Default**: The provider's declared default wire protocol.
5. **System Fallback**: `WireProtocol::ChatCompletions`.

`copilot_route` is eradicated: Copilot is now a standard provider whose catalog parser populates `DiscoveredModel.wire_protocol`, and standard route derivation handles the rest.

### 3. Strongly-Typed Catalog Parser Pipeline

`crates/muta-providers/src/list_models.rs` is reorganized around a `CatalogParser` trait:

```rust
pub trait CatalogParser: Send + Sync {
    fn parse(&self, body: &[u8]) -> Result<Vec<DiscoveredModel>, CatalogParseError>;
}
```

Implementations include:
- `OpenAiCatalogParser`: Standard OpenAI format with optional reasoning/thinking extraction.
- `CopilotCatalogParser`: Extracts `capabilities.endpoints` to populate `DiscoveredModel.wire_protocol`.
- `AnthropicCatalogParser`: Standard Anthropic format.
- `GoogleCatalogParser`: Standard Gemini `/models` format.
- `KimiCatalogParser`: Extracts `supports_thinking_type`.

### 4. Non-Blocking Discovery with Silent Degradation

Network model discovery is always asynchronous and must never block agent startup or model channel derivation.
If discovery fails (HTTP 404, authorization error, timeout, unparseable payload), the failure is logged at `debug` level and the system immediately falls back to `Baseline(p) ∪ Include(c)`.

## Invariants

- **`[INV-ROUTE-01]` Zero Suffix Stripping**: No URL in the system may be constructed by stripping suffixes from another endpoint URL. All URLs derive from a validated root.
- **`[INV-ROUTE-02]` Model Protocol Supremacy**: When a model explicitly specifies or discovers a `wire_protocol`, that protocol must take precedence over the provider's default protocol.
- **`[INV-ROUTE-03]` Non-Blocking Fallback**: A failure in catalog discovery must never panic, crash the runtime, or prevent an existing configured model channel from initializing.

## Consequences

### Positive
- Eradicates spurious 404s on reverse proxies and corporate subpath relays.
- Deletes `copilot_route` and unifies all multi-wire relays under a single algebraic channel derivation.
- Standardizes vendor-specific discovery extensions into modular parser adapters.

### Negative
- Existing configurations specifying full `/chat/completions` paths must be normalized to root URLs during load migration.

## References
- ADR-0149: Model capability resolution order
- ADR-0171: Three-layer model catalog and pluggable network sources
- ADR-0199: Unified cascading model resolution architecture
- ADR-0201: ModelProvider is the service surface, Connection is a named pipe
- ADR-0258: First-class declarative model providers and connection pipe purity
