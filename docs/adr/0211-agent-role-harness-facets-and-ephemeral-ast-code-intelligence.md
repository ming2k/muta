# 0211. Agent Roles, Harness Facets, and Ephemeral AST Code Intelligence

- **Status:** Proposed
- **Date:** 2026-03-31
- **Extends/Builds on:** ADR-0137 (server-side KV-cache alignment and three-zone architecture), ADR-0167 (worker-station agent model), ADR-0183 (spatiotemporal aspect engine)
- **Supersedes/Refines:** deprecates `AgentPreset` in favor of `AgentRole` (refining ADR-0053/ADR-0167); supersedes `CognitiveTask`/`CognitivePipeline` terminology with `HarnessTask` (completing ADR-0167 §2's de-stewarding trajectory); refines ADR-0183 Phase 2 turn intake from remote LLM generation to 0ms local synthesis.

## Context

Muta's positioning is a high-performance, controllable AI Harness for software engineering. While earlier decisions established the worker-station model (ADR-0167), five-phase spatiotemporal aspects (ADR-0183), and three-zone KV-cache alignment (ADR-0137), three critical architectural and terminology gaps have emerged:

1. **Active Tools vs. Ambient Harness Facets Conflation:**
   - In current contracts, all capabilities exposed to an agent are modeled as `Tool`s.
   - However, structural AST code intelligence (such as syntax error checking, repo outline projection, and symbol tracking) operates primarily as **passive, ambient harness mechanics** (pre-turn environmental projection and post-mutation invariant gating).
   - Forcing code intelligence into an explicit `Tool` forces the LLM to spend extra ReAct turns calling `get_repo_map` instead of seeing the topology at turn onset. Conversely, hardcoding Tree-sitter globally into the daemon violates session isolation, leaks memory across long-lived daemons, and forces AST overhead onto roles that do not write code (e.g. general conversational agents, supervisors, or MCP routers).

2. **Terminology Drift: The Lingering Ghost of "Steward" and the Misleading "Preset":**
   - ADR-0167 explicitly announced the "de-stewarded" architecture, but renamed the substrate to `CognitiveTask`/`CognitivePipeline`. The "cognitive" branding carries pseudo-philosophical pretension; in reality, these are single-shot, timeout-bounded, fail-open **harness internal auxiliary tasks** (session titling, stream loop detection).
   - Concurrently, the vocabulary `AgentPreset` (ADR-0053) suggests static IDE configuration templates or macro files, obscuring the domain truth: in the Worker-Station model (ADR-0167), when a user launches a session, they are staffing a **Session Station** with a specific **Agent Role** (e.g., `Developer`, `CodeAnalyst`).

3. **KV-Cache Invalidation via Volatile Injections:**
   - ADR-0137 established the three-zone cache model: Zone 1 (Static Invariant Prefix), Zone 2 (Monotonic History), and Zone 3 (Ephemeral Tail).
   - Putting volatile repository maps into Zone 1 is catastrophic: a single code edit alters Zone 1, which completely invalidates the cached KV blocks for all subsequent 50,000+ tokens of multi-turn conversation history.
   - Furthermore, current Phase 2 Turn Intake (`orchestration.rs`) invokes a remote LLM (`EnvironmentSensorTask`) with 1500ms network timeout and commits the transient reminder directly into the persistent transcript, polluting Zone 2 across subsequent turns.

## Decision

### 1. Terminological Hygiene: De-Cognitivize to `HarnessTask`

We eradicate "cognitive" branding across contracts and runtime:
- `CognitiveTask` $\rightarrow$ **`HarnessTask`**
- `CognitivePipeline` $\rightarrow$ **`HarnessTaskPipeline`**
- A `HarnessTask` is strictly defined as a stateless, tool-free, timeout-bounded ($t \le 2000\text{ms}$), fail-open internal mechanic executed by the Harness state machine to maintain invariants, titles, and loop diagnostics. It represents no persona and no agency.

### 2. Dual-Wing Agent Model: `AgentRole` = `Tools` + `Facets`

We deprecate `AgentPreset` in favor of **`AgentRole`**. A Session Station is staffed by an `AgentRole` that defines two orthogonal capability sets:

```text
┌────────────────────────────────────────────────────────────────────────┐
│                        Session Station                                 │
│                                                                        │
│   Staffed by: AgentRole (e.g. "Developer", "CodeAnalyst", "General")   │
│                                                                        │
│   ┌───────────────────────────────────┬────────────────────────────┐   │
│   │ 1. Action Space (Tools)           │ 2. Ambient Space (Facets)  │   │
│   │    - Model-driven active RPC      │    - Harness-driven ambient│   │
│   │    - Evaluated inside ReAct loop  │    - Hooks & Invariants    │   │
│   ├───────────────────────────────────┼────────────────────────────┤   │
│   │ • read_text, edit_text            │ • CodeIntelligenceFacet    │   │
│   │ • write_file, execute_command     │ • GitWorkspaceFacet        │   │
│   │ • ask_user                        │ • DockerSandboxFacet       │   │
│   └───────────────────────────────────┴────────────────────────────┘   │
└────────────────────────────────────────────────────────────────────────┘
```

1. **`Tool` (Action Space):** Active capabilities invoked explicitly by the LLM via structured JSON-RPC function calls.
2. **`HarnessFacet` (Ambient & Gating Space):** Passive environmental scaffolding equipped by the role, hooking deterministically into the execution pipeline:
   ```rust
   #[async_trait]
   pub trait HarnessFacet: Send + Sync {
       /// Unique facet identifier (e.g. "code_intelligence").
       fn name(&self) -> &'static str;

       /// Zone 3 Projection: Contribute true-ephemeral context to request tail.
       fn project_ephemeral_context(&self, env: &TurnContext) -> Option<String> { None }

       /// Phase 4 Gating: Intercept and validate tool mutations before disk writes.
       fn intercept_file_mutation(&self, path: &Path, content: &str) -> Result<(), FacetViolation> { Ok(()) }

       /// Optional companion read-only tools registered to the agent's tool catalog.
       fn accompanying_tools(&self) -> Vec<Arc<dyn Tool>> { Vec::new() }
   }
   ```

### 3. Instance-Bound `CodeIntelligenceFacet` (No Global Tree-sitter)

Tree-sitter is **never** instantiated as a global daemon singleton. It is encapsulated within `CodeIntelligenceFacet` and bound strictly to the `AgentRole` instance:

- **Targeted Binding:** Only developer/coding roles (e.g. `Role::Developer`, `Role::CodeRunner`) instantiate `CodeIntelligenceFacet`. General conversational or supervisor roles carry zero AST overhead.
- **Polyglot Parsing via Tree-sitter:** Embedded C/Rust grammar parsers (`tree-sitter-rust`, `tree-sitter-typescript`, `tree-sitter-python`, `tree-sitter-c`, `tree-sitter-cpp`, `tree-sitter-go`). Parsing operates purely on in-memory UTF-8 buffers with zero build-system or LSP daemon dependencies.
- **Resource Lifecycle:** When the session is suspended or destroyed, all parser arenas and syntax trees are immediately reclaimed.

### 4. Zone 3 True-Ephemeral Repo Map (1024 Token Budget)

To guarantee 100% KV-cache retention on prior conversation history:

1. **Strict Placement in Zone 3 (Request Tail):**
   - The repository skeleton is **never placed in Zone 1 (System Prompt)**.
   - The repository skeleton is **never committed to SQLite transcripts**.
   - It is appended to the request exclusively at dispatch time (`assemble_prepared`) at the physical tail of the latest user message, and discarded from memory upon response receipt.
2. **Token Budget & Packing Policy:**
   - **Baseline Budget:** 1024 tokens (covering ~70–100 public types, traits, and exported function signatures).
   - **Adaptive Monorepo Ceiling:** Scaled up to 1500–2048 tokens for large workspaces.
   - **Contract-First Greedy Fit:** Only public signatures (`pub struct/trait/fn`) are indexed; internal helpers are omitted. Active dirty files receive boosted full expansion; distant crates display one-line module summaries.
3. **Cache Invariant:**
   - Multi-turn conversation history in Zone 2 enjoys **100% KV-cache hit rate**.
   - The 1024-token Zone 3 tail computes via prefill in $\le 30\text{ms}$ on modern inference hardware.

### 5. 1ms Incremental Syntax Guard (Pre/Post Mutation Gating)

`CodeIntelligenceFacet` intercepts `edit_text` and `write_file` invocations during Phase 4 (Tool Gating):
- Modified file content is parsed incrementally via Tree-sitter (`parser.parse(new_source, Some(&old_tree))`) in $\le 2\text{ms}$.
- If the parsed root node contains `(ERROR)` syntax nodes (e.g., unclosed delimiters, broken indentation), the tool execution **aborts immediately without writing to disk**.
- The tool returns structured line/column syntax diagnostics to the model, forcing same-turn self-correction and eliminating syntax-broken commits.

### 6. Phase 2 Turn Intake: 0ms Local Synthesis

We eliminate the remote LLM invocation in Phase 2 Turn Intake (ADR-0183):
- `EnvironmentSensorTask` is deleted.
- Turn Intake executes purely in local Rust: checks Git status, extracts AST symbol deltas for dirty files, formats a $\le 100$-token environmental reminder in 0ms, and routes it into Zone 3.

## Alternatives Considered

1. **Global Daemon-wide Tree-sitter / LSP Singleton:**
   *Rejected*: A global background LSP or Tree-sitter daemon consumes hundreds of megabytes of resident memory, risks cross-project cache leaks, and penalizes non-coding sessions.
2. **Placing Repo Map in Zone 1 (System Prompt):**
   *Rejected*: File edits modify code symbols, which mutates the Repo Map. Altering Zone 1 invalidates the entire left-to-right KV cache, turning 50,000+ tokens of multi-turn history into 100% cache misses on every code change.
3. **Treating AST as a Model-Invoked Tool Only (`get_repo_map`):**
   *Rejected*: Forces the model to spend an entire ReAct turn and network round-trip asking for project topology before writing code. Ambient Zone 3 projection provides zero-turn instant orientation.
4. **Vector Database / Long-Term Semantic Memory:**
   *Rejected*: Codebases and compilers represent absolute deterministic ground truth. Dynamic LLM-generated vector memory introduces stale cache poisoning, hallucination loops, and attention dilution. Project rules remain strictly file-backed in version-controlled `AGENTS.md`.

## Consequences

### Positive
- **Architectural Coherence:** Clean separation between model-invoked actions (`Tools`), ambient environment scaffolding (`Facets`), and stateless internal utilities (`HarnessTasks`).
- **Zero-Turn Code Orientation:** 1024-token Repo Map in Zone 3 gives coding roles immediate structural comprehension without exploratory tool calls.
- **Rock-Solid KV Caching:** Confining volatile AST skeletons to Zone 3 preserves 95%+ prompt cache hits across long multi-turn sessions.
- **Fail-Fast Syntax Defense:** Incremental 1ms Tree-sitter validation prevents broken syntax from touching the disk.
- **Zero Latency Turn Intake:** Eliminates 1500ms remote cognitive overhead on round admission.

### Negative & Mitigations
- **Binary Footprint:** Linking Tree-sitter core and grammar crates adds ~5MB to binary size.
  *Mitigation*: Compile only mainstream tier-1 grammars (Rust, TS, Python, Go); unsupported languages gracefully fall back to text mode without errors.

## References

- [ADR-0137](0137-server-side-kv-cache-alignment-and-zoning.md): Server-side KV-cache alignment and three-zone architecture
- [ADR-0150](0150-two-axis-agent-architecture-and-harness-steward.md): Two-axis agent architecture
- [ADR-0167](0167-worker-station-agent-model-and-hypervisor.md): Worker-station agent model and hypervisor station placement
- [ADR-0183](0183-homogeneous-agent-kernel-and-spatiotemporal-aspect-engine.md): Homogeneous agent kernel and spatiotemporal aspect engine
- Tree-sitter Incremental Parsing Architecture (Brunsfeld et al.)
- Aider Repository Map Architecture (Gauthier, 2023)
