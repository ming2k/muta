# 0182. Route-Projected Capabilities, Distributed Single Source of Truth, and Elimination of Client-Side Static Registry

- **Status:** Accepted
- **Date:** 2026-09-06

## Context

The system's model catalog architecture has evolved through two major architectural milestones:
1. **ADR-0123 (State-Derived Routes):** Decoupled connectivity into behavior (`config.toml`), principals (`providers.toml`), credentials (`credentials.toml`), and runtime-derived routes (`Channel`).
2. **ADR-0149 (Three-Layer Capability Resolution):** Established that effective model capabilities resolve in a deterministic hierarchy:
   $$\text{EffectiveCapabilities} = \text{UserOverrides} \oplus \text{RemoteAdvertisement} \oplus \text{StaticBaseline}$$
   This resolution is evaluated on the daemon side via `Channel::capabilities()`.

Despite these advances, a fundamental architectural defect persists across the daemon–client process boundary: **the dual-brain capability split**.

### The Concrete Pathology: The `opencode-go` Silent Display Failure

When a user adds or uses the `opencode-go` preset (which relies on `LiveCatalog::ModelsDev`, ADR-0171), the relay advertises both baseline-known models (e.g., `deepseek-v4-flash`, `glm-5.2`) and dynamic catalog models (e.g., `glm-5.3`, `glm-5.3-flash`, `qwen3.8-max`, `hy3`, `kimi-k3`):

1. **The Ingest Layer Works Flawlessly:**
   `crates/muta-models-dev` correctly deserializes `limit.context` from `models.dev` (`crates/muta-models-dev/src/schema.rs:77`). The bridge `from_dev_model` (`crates/muta-providers/src/models_dev.rs:48`) maps it into `DiscoveredModel.context_window = Some(1_000_000)`. Discovery persists it to `DiscoveryCache.remote_metadata` and `DiscoveryCache.fitted_models` (`crates/muta-agent/src/catalog/discovery.rs:322`).
2. **The Daemon Evaluates Truth Correctly:**
   In the daemon runtime, `derive_channel` creates a `Channel` with `remote: Some(...)`. Calling `channel.capabilities().context_window` correctly yields `1_000_000`.
3. **The Presentation Layer Collapses to Zero:**
   In the terminal interface (`mutx`), the model bar (`apps/tui/crates/mutx/src/chrome/model_bar.rs:117`) and telemetry overlays (`apps/tui/crates/mutx/src/event_loop/render.rs:1015`) attempt to render context usage by calling:
   ```rust
   let context_max = crate::providers::model_context_window(current_model);
   ```
   which evaluates:
   ```rust
   pub fn model_context_window(model: &str) -> usize {
       muta_contracts::model::resolve(model).context_window
   }
   ```
4. **The Silent Failure Invariant:**
   `mutx` runs in a separate client process (or attached over IPC). The client process has **no access** to the daemon's internal in-memory overlay. Because dynamic models like `glm-5.3` do not exist in the client's compiled-in `BaselineModels`, `muta_contracts::model::resolve("glm-5.3")` falls back to `fallback_model()`, returning `context_window = 0`.
   At line 173 of `model_bar.rs`:
   ```rust
   if context_max > 0 {
       context_spans = context_usage_spans(used, context_max, theme, bg);
   }
   ```
   Because `context_max == 0`, the entire context gauge is omitted from the UI. The user sees token usage without any context window boundary or percentage gauge, assuming discovery failed.

### Root Cause Analysis: Architectural Anti-Patterns

This bug is not an isolated omission; it is the direct symptom of three structural design compromises:

```text
               ┌─────────────────────────────────────────────────────────────┐
               │                        DAEMON PROCESS                       │
               │                                                             │
               │  ADR-0149 Three-Layer Engine:                               │
               │  User Overrides ⊕ Remote Metadata ⊕ Baseline                │
               │                        │                                    │
               │                        ▼                                    │
               │         Channel::capabilities() [AUTHORITY]                 │
               │                        │                                    │
               └────────────────────────┼────────────────────────────────────┘
                                        │
                         IPC Boundary   │  ProviderPickerSnapshot
                                        │  (Incomplete DTO: vision only)
                                        │
               ┌────────────────────────┼────────────────────────────────────┐
               │                        ▼                                    │
               │  ProviderModelInfo { model, protocol, vision, ... }         │
               │  [MISSING: context_window, max_output_tokens, thinking]     │
               │                                                             │
               │  mutx TUI / Web Client:                                     │
               │  model_context_window(model_id: &str)                       │
               │                        │                                    │
               │                        ▼                                    │
               │  muta_contracts::model::resolve(bare_model_id)              │
               │  [Empty Local FITTED_MODELS] -> Fallback -> context: 0      │
               │                                                             │
               │                        CLIENT PROCESS                       │
               └─────────────────────────────────────────────────────────────┘
```

1. **Context-Stripping Function Signatures:**
   `model_context_window(model: &str)` assumes that model capabilities are an intrinsic, global scalar property of a bare identifier string. Under ADR-0149, capabilities are strictly **route-scoped**: `glm-5.2` served by official Zhipu vs. `glm-5.2` served by `opencode-go` have different rate limits, context windows, and wire formats. Any function signature that discards the `connection_id` context is mathematically incapable of returning the true capability.
2. **Hidden In-Process Global Mutable State:**
   `crates/muta-contracts/src/model.rs` maintains a process-wide `static FITTED_MODELS: OnceLock<RwLock<HashMap<&'static str, Model>>>` populated via `Box::leak`. In a multi-process Client/Daemon architecture, memory-leaked static overlays are an anti-pattern: they create split-brain state where the daemon's memory has updated but attached clients remain blind.
3. **Piecemeal DTO Patchwork:**
   When this exact problem previously broke image pasting in the composer, an ad-hoc field `vision: bool` was appended to `ProviderModelInfo` (`crates/muta-contracts/src/events.rs:1615`), while `active_model_supports_vision` was patched in `apps/tui/crates/mutx/src/clipboard_ops.rs:28` to inspect the snapshot. However, because this was treated as a local bug rather than an architectural violation, `context_window`, `max_output_tokens`, and `thinking` were left unprojected.

---

## Theoretical & Industrial Foundations

To achieve an uncompromising, industrial-grade solution, this decision aligns with four core computer science and software architecture principles:

1. **Command Query Responsibility Segregation (CQRS) Read-Model Projections:**
   The evaluation of ADR-0149's 3-layer capability resolution is a Domain Write/Resolution concern owned exclusively by the Daemon Core. The client UI is a pure Read Model. The client must never re-evaluate, shadow, or infer domain invariants; it must consume fully resolved, materialized Read DTOs.
2. **The "Dumb Terminal / Smart Host" Invariant:**
   Frontends (`mutx` TUI, `apps/web`) must remain thin presentation engines. Pushing business-logic resolution (e.g., fallback tables, heuristic capability deduction) into the client violates boundary separation and leads to distributed divergence.
3. **Single Source of Truth (SSOT) & Distributed Consistency:**
   In any distributed system (including local IPC), derived facts must have exactly one authority. `Channel::capabilities()` is that authority. Replicating partial capability logic in client-side static tables violates SSOT.
4. **Zero-Leakage Memory Hygiene:**
   Eliminating runtime string interning via `Box::leak` for dynamically discovered models in static maps restores deterministic resource cleanup and eliminates unbounded memory growth in long-running daemons.

---

## Decision

We establish an authoritative, **Route-Projected Capability Contract** across the entire workspace.

### 1. Elevate Route Capabilities to a First-Class Canonical Contract

In `crates/muta-contracts/src/model.rs`, define the canonical, wire-serializable representation of evaluated route capabilities:

```rust
/// Materialized, route-scoped capabilities evaluated daemon-side via ADR-0149.
/// Projected to frontends as the infallible, single source of truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = concat!(env!("CARGO_MANIFEST_DIR"), "/../../apps/web/src/lib/generated/wire.gen.ts"))]
pub struct RouteCapabilities {
    /// Context window size in tokens. Guaranteed > 0 for all routed channels.
    pub context_window: usize,
    /// Maximum generation tokens, when advertised or configured.
    pub max_output_tokens: Option<u32>,
    /// Whether the route accepts image attachments.
    pub vision: bool,
    /// Whether the route supports tool/function calling.
    pub tool_call: bool,
    /// Extended thinking / reasoning support mode.
    pub thinking: ThinkingSupport,
}
```

### 2. Complete the Cross-Process DTO Contract

Update `ProviderModelInfo` in `crates/muta-contracts/src/events.rs` to carry the authoritative route capabilities directly:

```rust
pub struct ProviderModelInfo {
    pub model: String,
    pub protocol: String,
    pub effort: Option<String>,
    pub thinking: Option<bool>,
    pub favorite: bool,
    pub last_used_ms: Option<u64>,
    /// Fully resolved route capabilities (ADR-0149: User ⊕ Remote ⊕ Baseline).
    pub capabilities: RouteCapabilities,
}
```

For backward wire compatibility with older persisted snapshots or clients, `capabilities` is serialized inline or with `#[serde(default)]`.

In `crates/muta-agent/src/catalog/picker.rs`, update `channel_model_info` to stamp the evaluated `channel.capabilities()` directly:

```rust
pub fn channel_model_info(channel: &Channel) -> ProviderModelInfo {
    let caps = channel.capabilities();
    let route_caps = RouteCapabilities {
        context_window: caps.context_window,
        max_output_tokens: caps.max_output_tokens,
        vision: caps.vision,
        tool_call: caps.tool_call,
        thinking: caps.thinking,
    };
    ...
}
```

### 3. Eliminate Parameterless Client Capability Inferences

1. **Deprecate and Remove `model_context_window(model: &str)`:**
   Delete `crate::providers::model_context_window` from `apps/tui/crates/mutx/src/providers.rs`.
2. **Introduce Route-Aware Capability Accessor on `App`:**
   In `apps/tui/crates/mutx/src/app/`:
   ```rust
   impl App {
       /// Return the authoritative capabilities of the currently active route
       /// (current_provider, current_model). Falls back to static baseline only
       /// if the daemon snapshot has not yet mounted.
       pub fn active_route_capabilities(&self) -> RouteCapabilities {
           self.provider_picker
               .rows
               .iter()
               .find(|row| row.id == self.current_provider)
               .and_then(|row| {
                   row.model_info
                       .iter()
                       .find(|info| info.model == self.current_model)
               })
               .map(|info| info.capabilities)
               .unwrap_or_else(|| {
                   let m = muta_contracts::model::resolve(&self.current_model);
                   RouteCapabilities {
                       context_window: m.context_window,
                       max_output_tokens: None,
                       vision: m.vision,
                       tool_call: m.tool_call,
                       thinking: m.thinking,
                   }
               })
       }
   }
   ```
3. **Pass Resolved Limit into View Components:**
   Update `ModelBarView` in `apps/tui/crates/mutx/src/chrome/model_bar.rs` to take `context_window: usize` directly as an input parameter rather than calculating it inside the render loop:
   ```rust
   pub struct ModelBarView<'a> {
       pub current_model: &'a str,
       pub model_available: bool,
       pub provider_name: Option<&'a str>,
       pub reasoning_effort: Option<&'a str>,
       pub context_tokens: Option<usize>,
       pub context_window: usize,  // 👈 Explicit invariant input
       pub last_turn_tps: Option<f64>,
       pub ignition_elapsed_ms: Option<u64>,
   }
   ```

### 4. Sunset the In-Process Leaked `FITTED_MODELS` Overlay

Deprecate `muta_contracts::model::register_fitted_models` and the process-wide `FITTED_MODELS` static.
- Dynamic capabilities are held in `DiscoveryCache` (persisted on disk) and projected in `Channel` and `ProviderPickerSnapshot` (in memory / IPC).
- `muta_contracts::model::resolve(id: &str)` is strictly scoped as a compile-time static baseline query (`baseline_models().find(...)`), eliminating the fake promise that a global function can resolve dynamic runtime models without connection context.

---

## Alternatives Considered

### 1. IPC Synchronization of `FITTED_MODELS` Map
- **Proposal:** Keep `FITTED_MODELS` in `muta-contracts`, but broadcast a new `FittedModelsUpdated` IPC event from daemon to clients whenever discovery runs, populating the client's local static table via `register_fitted_models`.
- **Rejected:** Replicating mutable global state across IPC creates race conditions during client startup, requires distributed cache-invalidation logic, maintains the unsafe `Box::leak` memory-leak pattern, and fails to solve the semantic flaw that a bare `model_id` cannot represent different capabilities across different connections.

### 2. File-Watcher on `DiscoveryCache` in Client Process
- **Proposal:** Make `mutx` watch `discovery.json` and call `sync_fitted_model_registry()` locally in the TUI process.
- **Rejected:** Violates the client–server architecture. The client would be bypassing the daemon IPC to read internal cache files directly, re-introducing file contention and lock overhead. Furthermore, remote client sessions (e.g., `mutx attach` over SSH or Unix socket) do not share the filesystem with the daemon.

### 3. Ad-Hoc `context_window` Field Addition (The "Patch-on-Patch" Approach)
- **Proposal:** Simply add `pub context_window: usize` next to `pub vision: bool` on `ProviderModelInfo`, leave `model_context_window(&str)` as-is, and patch it with an `app` lookup.
- **Rejected:** Fails the standard of architectural rigor. Adding isolated fields one by one whenever a UI component breaks is how the codebase accumulated this debt. Encapsulating all route capabilities in `RouteCapabilities` provides a unified, future-proof contract for `context_window`, `max_output_tokens`, `tool_call`, `vision`, and `thinking`.

---

## Consequences

### Positive Consequences
- **Deterministic UI Invariant:** Discovered models from relays like `opencode-go` (e.g., `glm-5.3`, `qwen3.8-max`, `hy3`) immediately display their correct context limits, percentages, and telemetry in the TUI model bar and modals.
- **Elimination of Split-Brain State:** Both Daemon and Client consume the exact same capabilities derived from the authoritative ADR-0149 resolution.
- **Zero Client Guesswork:** Frontends are completely relieved of capability inference, baseline searching, or fallback heuristics.
- **Universal Multi-Client Support:** The web frontend (`apps/web`) automatically receives TypeScript-typed route capabilities via `wire.gen.ts`, enabling instant context bar accuracy in browser interfaces.
- **Clean Memory Model:** Retires `Box::leak` interning for dynamic runtime models in `muta-contracts`.

### Negative Consequences & Mitigations
- **Contract Migration:** `ProviderModelInfo` changes shape.
  *Mitigation:* Retain backward-compatible serialization with `#[serde(default)]`, mapping legacy `vision` and `thinking` fields during deserialization.
- **Call-Site Refactoring:** All client call sites using `model_context_window(model)` must be updated to reference the active route context.
  *Mitigation:* The compiler guarantees completeness: deleting the old parameterless function produces immediate, exhaustively checkable compiler errors for all obsolete invocations.

---

## References

- **ADR-0123:** Provider instances are state; routes are derived, never persisted.
- **ADR-0149:** Three-layer model capability resolution order (User ⊕ Remote ⊕ Baseline).
- **ADR-0171:** Three-layer model catalog and pluggable network sources (`LiveCatalog::ModelsDev`).
- **Code Locations:**
  - `crates/muta-contracts/src/events.rs:1590` (`ProviderModelInfo`)
  - `crates/muta-contracts/src/model.rs:424` (`resolve`)
  - `crates/muta-providers/src/models_dev.rs:48` (`from_dev_model`)
  - `crates/muta-agent/src/catalog/picker.rs:242` (`channel_model_info`)
  - `apps/tui/crates/mutx/src/chrome/model_bar.rs:117` (`model_bar` render loop)
  - `apps/tui/crates/mutx/src/clipboard_ops.rs:28` (`active_model_supports_vision`)
