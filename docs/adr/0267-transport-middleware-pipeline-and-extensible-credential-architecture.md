# 0267. Transport Middleware Pipeline and Extensible Credential Architecture: Eradication of Provider-Specific Branches, Uniform Connection Lifecycle, and Protocol Purity

- **Status:** Accepted
- **Date:** 2026-09-20
- **Implementation:** `muta-contracts`, `muta-llm-client`, `muta-providers`, `muta-runtime`, `mutx`
- **Builds on:** [ADR-0258](0258-first-class-declarative-model-providers-and-connection-pipe-purity.md), [ADR-0260](0260-provider-dialect-inheritance.md), [ADR-0265](0265-declarative-wire-surfaces.md), [ADR-0266](0266-declarative-remote-catalog-descriptors.md)
- **Supersedes:** Ad-hoc provider fields in `ResolvedAuth`, `TokenSet`, and `ConnectionAuth` enum variants (`ChatGptOAuth`, `CopilotOAuth`, `AntigravityOAuth`, `XaiOAuth`, `QoderOAuth`)

---

## Context and Problem Statement

As muta expands to support enterprise gateways, reverse-engineered subscription IDE surfaces (e.g., Alibaba Qoder), and cloud platforms (ChatGPT Codex, GitHub Copilot, Google Antigravity, xAI SuperGrok), a destructive pattern of **abstraction leakage and ad-hoc specialization** has accumulated across the framework:

1. **LLM Client Execution Branching**:
   In `muta-llm-client/src/protocol/openai/chat_completions/mod.rs`, request dispatch hardcodes provider branches (`if qoder { self.build_qoder_request(...) }`), mixing HTTP transport details with cryptography, custom base64 encodings, and JSON manipulation.

2. **Core Domain Struct Pollution**:
   `muta_contracts::ResolvedAuth` and `muta_providers::TokenSet` accumulated provider-specific fields (`account_id`, `project_id`, `copilot`, `qoder`). Every new non-standard provider mutated foundational contracts, forcing workspace-wide re-compilations.

3. **Enum Proliferation in `ConnectionAuth`**:
   `ConnectionAuth` contained an ever-growing list of variants: `ApiKey`, `XaiOAuth`, `ChatGptOAuth`, `CopilotOAuth`, `AntigravityOAuth`, `QoderOAuth`, leaking into TUI setup, connection resolution, and CLI flows.

4. **Inference vs. Catalog Divergence (Drift)**:
   Inference and model catalog sync previously maintained separate header and signing stacks, causing silent signature drifts and production 403 authorization failures (ADR-0265, ADR-0266).

If muta is to support dozens of heterogeneous model endpoints without collapsing under regression and maintenance costs, this coupling must be completely eradicated under **zero-legacy, uncompromised architectural purity**.

---

## Decision Drivers

- **Open-Closed Principle**: Core engine, generic LLM client, and connection manager are closed to modification when adding new providers.
- **Compiler-Guaranteed Topological Ordering**: Middleware execution must not rely on fragile runtime lists; ordering must be guaranteed by execution phases.
- **Fail-Fast Preflight Validation**: Missing credential metadata must be detected at connection assembly/initialization, never mid-flight during user inference turns.
- **Transport Purity**: HTTP signing and encoding must operate on general `http::Request` bytes, unifying inference, catalog sync, and userinfo under one identical signing stack.
- **Zero Legacy Burden (Clean Break)**: No transitional backward-compatibility aliases, no deprecation shims, no dual-write storage adapters.

---

## Considered Options

- **Option 1: Ad-hoc Hardcoding (Status Quo)**
  Add new enum variants to `ConnectionAuth` and `if provider` branches to clients.
  *Rejected*: Degenerates the framework into an unmaintainable tangle of edge cases.

- **Option 2: Unstructured Runtime Interceptor List (`Vec<Box<dyn Transformer>>`)**
  Maintain a flat list of request transformers executed in iteration order.
  *Rejected*: Order-blind; if a developer registers the signer before the body codec, signature verification silently fails at runtime with no compiler protection.

- **Option 3 (Chosen): Type-Level Phased Pipeline + Extensible Credential & Preflight Architecture**
  - Factor request transformation into 3 strictly ordered phases: Envelope → BodyCodec → Signer.
  - Sinks cryptographic signing to the transport layer, shared between inference and catalog sync.
  - Structure streaming frames as an Algebraic Data Type (`TransformedFrame`).
  - Collapse `ConnectionAuth` into `ApiKey` and `Subscription { provider }`.
  - Validate required credential extensions at preflight connection setup.

---

## Decision Outcome

Chosen option: **Option 3**.

```text
┌─────────────────────────────────────────────────────────────────────────────┐
│                       1. PHASED TRANSPORT PIPELINE                          │
│                                                                             │
│  Model Request AST ──► [ Phase 1: EnvelopePhase ] ──► Reshaped JSON Body    │
│                                                              │              │
│                                  ▼                           │              │
│                       [ Phase 2: BodyCodecPhase ] ──► Encoded Payload Bytes │
│                                                              │              │
│                                  ▼                           │              │
│                       [ Phase 3: RequestSignerPhase ] ──► Signed HTTP Req   │
│                       (Shared by Inference & Catalog)                       │
│                                                                             │
│  Raw SSE Wire Stream ──► [ StreamTransformer ] ──► Typed TransformedFrame   │
│                            - Delta(ChatChunkDelta)                          │
│                            - Skip (Heartbeat / Internal False [DONE])       │
│                            - Terminal(StreamMetrics: Durations)             │
│                            - UpstreamFault(ProviderFault: Inner 4xx/5xx)    │
└─────────────────────────────────────────────────────────────────────────────┘
                                      ▲
                                      │ Preflight Contract Validation
┌─────────────────────────────────────┴───────────────────────────────────────┐
│                    2. EXTENSIBLE CREDENTIAL CONTRACTS                       │
│                                                                             │
│  ResolvedAuth { token, user_email, extensions: ExtensionMap }               │
│  TokenSet     { access, refresh, expires_ms, attributes: ExtensionMap }     │
└─────────────────────────────────────────────────────────────────────────────┘
                                      ▲
                                      │ Managed By
┌─────────────────────────────────────┴───────────────────────────────────────┐
│                    3. UNIFIED AUTH DRIVER REGISTRY                          │
│                                                                             │
│  ConnectionAuth::ApiKey                                                     │
│  ConnectionAuth::Subscription { provider: Cow<'static, str> }               │
│                                                                             │
│  AuthProviderDriver:                                                        │
│    - start_login() -> AuthChallenge                                         │
│    - complete_login() -> Result<TokenSet>                                   │
│    - refresh() -> Result<TokenSet>                                          │
│    - populate_request_extensions() -> Result<()>                            │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

### 1. Type-Level Phased Pipeline (`muta-llm-client`)

In cryptographic signing protocols, execution order is a correctness property. A flat `Vec<Box<dyn RequestTransformer>>` is an order-blind runtime hazard. The pipeline is strictly partitioned into three ordered execution phases:

#### 1.1 Phase Definitions

```rust
/// Phase 1: Structural JSON body transformation (e.g. wrapping standard ChatCompletions into AgentChat envelope).
pub trait EnvelopePhase: Send + Sync {
    fn reshape_body(&self, body: &serde_json::Value) -> Result<serde_json::Value, ProviderError>;
}

/// Phase 2: Payload byte encoding or compression (e.g. QoderEncoding custom alphabet + thirds swap).
pub trait BodyCodecPhase: Send + Sync {
    fn encode_body(&self, body_bytes: &[u8]) -> Result<Vec<u8>, ProviderError>;
}

/// Phase 3: Final immutable request inspection and cryptographic signing (e.g. COSY RSA/AES/MD5).
/// Operates on finalized payload bytes and request path. Never mutates the body.
pub trait RequestSignerPhase: Send + Sync {
    fn sign_request(
        &self,
        req: &mut http::request::Builder,
        signed_path: &str,
        body_bytes: &[u8],
        auth: &ResolvedAuth,
    ) -> Result<(), ProviderError>;
}
```

#### 1.2 Compile-Time Typestate Builder

A fluent builder ensures compile-time ordering:
```rust
TransportPipeline::builder()
    .with_envelope(QoderAgentEnvelope::new(spec))
    .with_codec(QoderBodyCodec)
    .with_signer(CosyTransportSigner::new(surface))
    .build();
```

---

### 2. Preflight Contract Validation (`muta-contracts`)

Dynamic extension maps provide decoupled open-world extensibility, but risk late mid-turn failures if required fields are missing. All transformers and drivers declare their required extension types, enabling **Fail-Fast assertions during connection assembly and refresh**:

```rust
pub trait PreflightValidator: Send + Sync {
    /// Declare the exact TypeIds required by this pipeline component.
    fn required_extensions(&self) -> &'static [std::any::TypeId];

    /// Validate credential contracts before sending in-flight requests.
    fn preflight_assert(&self, auth: &ResolvedAuth) -> Result<(), ProviderError> {
        for type_id in self.required_extensions() {
            if !auth.extensions.contains_id(type_id) {
                return Err(ProviderError::contract_violation(*type_id));
            }
        }
        Ok(())
    }
}
```

---

### 3. Transport-Level Interceptor (Unified Inference & Catalog Transport)

COSY signing and vendor-specific authentication headers are not LLM-specific concepts; they are transport-level HTTP concerns.

- `CosyTransportSigner` implements `RequestSignerPhase` and operates directly on `http::request::Builder`, `signed_path: &str`, and raw `body_bytes: &[u8]`.
- Both the **Inference Pipeline** (`POST /algo/api/v2/service/pro/sse/agent_chat_generation`) and the **Catalog Fetcher** (`GET /algo/api/v2/model/list?Encode=1`) share the **exact same `CosyTransportSigner` instance**.
- Neither surface maintains private copies of header lists or signature algorithms. Drift between inference and catalog synchronization is rendered impossible by construction.

---

### 4. Typed Stream Frame Dissection (`TransformedFrame` ADT)

Replacing lossy `Result<Option<String>, ProviderError>` with a rich Algebraic Data Type:

```rust
/// Upstream execution and latency metrics emitted by terminal events.
#[derive(Debug, Clone, Default)]
pub struct StreamMetrics {
    pub first_token_duration_ms: Option<u64>,
    pub total_duration_ms: Option<u64>,
    pub server_duration_ms: Option<u64>,
}

/// Upstream fault returned inside a 200 HTTP SSE envelope.
#[derive(Debug, Clone)]
pub struct ProviderFault {
    pub status_code: u16,
    pub message: String,
}

/// Algebraic outcome of parsing an inbound raw SSE event.
#[derive(Debug, Clone)]
pub enum TransformedFrame {
    /// Standard LLM chunk payload (canonical JSON string).
    Delta(String),
    /// Control / heartbeat / false `[DONE]` frame to safely skip.
    Skip,
    /// Authoritative stream close with upstream duration and performance telemetry.
    Terminal(StreamMetrics),
    /// Upstream error wrapped inside a 200 envelope (e.g. quota, auth expired).
    UpstreamFault(ProviderFault),
}

pub trait StreamTransformer: Send + Sync {
    /// Transforms an inbound raw SSE event into a canonical frame outcome.
    fn transform_event(&self, event_type: Option<&str>, data: &str) -> Result<TransformedFrame, ProviderError>;
}
```

---

### 5. Orthogonal Connection Authentication & Unified Storage

- `ConnectionAuth` is simplified to:
  ```rust
  pub enum ConnectionAuth {
      ApiKey,
      Subscription { provider: Cow<'static, str> },
  }
  ```
- `TokenSet` holds open-world attributes via `#[serde(default, flatten)] pub attributes: serde_json::Map<String, serde_json::Value>`.

---

## Invariants & Behavioral Boundaries

- **`[INV-TRANSPORT-01]` Zero Provider Branches**: generic LLM clients never contain conditional branches on provider names or dialects.
- **`[INV-PIPE-01]` Strict Phase Topology**: pipeline execution must strictly follow Envelope → BodyCodec → Signer. No phase may mutate artifacts of a subsequent phase.
- **`[INV-PREFLIGHT-01]` Fail-Fast Contract Assertion**: missing required credentials must be flagged at connection assembly/refresh time, never deferred to mid-generation failure.
- **`[INV-SIGN-01]` Unified Signer Transport**: inference, catalog, and auxiliary HTTP requests for a signed provider must route through the same transport signer component.
- **`[INV-FRAME-01]` Lossless Stream Dissection**: stream transformations must preserve metadata and distinguish true terminal events from internal marker frames.

---

## Positive Consequences

1. **Provable Order Correctness**: Compile-time typestate prevents signature-ordering defects.
2. **Deterministic Error Localization**: Preflight assertions isolate configuration errors immediately upon connection setup.
3. **Zero Signature Drift**: Unifying catalog and inference signing under the same transport interceptor permanently resolves ADR-0265/0266 failure modes.
4. **Rich Telemetry & Lossless Errors**: Inner 4xx/5xx payload envelopes and duration metrics are captured as first-class domain values.
