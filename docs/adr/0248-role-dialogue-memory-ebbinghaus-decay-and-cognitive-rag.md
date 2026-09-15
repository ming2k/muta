# 0248. Role-Scoped Dialogue Memory, Ebbinghaus Forgetting Curve, and Cognitive RAG Architecture

- Status: Accepted
- Date: 2026-04-15
- Scope: persistence/memory, contracts/roles, agent/tools, cognitive
- Deciders: Core Architecture Team
- Consulted: Agent Runtime Team, Persona & Steering Working Group
- Informed: All Contributors
- Extends/Refines: ADR-0244, ADR-0246
- Related Tools: `recall_memory`

---

## Context and Problem Statement

As agents transition from single-turn task executors into persistent cognitive entities, maintaining coherent context across sessions and long spans of time is essential. This is particularly crucial for reflective, conversational, or inquiry-oriented roles such as `philosophist` (ADR-0244, ADR-0246).

Historically, Muta lacked a dedicated dialogue-level memory architecture:
1. **Context Window Saturation**: Standard context compaction (ADR-0019, ADR-0186) discards older transcript turns or aggregates them into lossy session-level summaries. Once a session ends or is reset, prior conversational insights, user philosophies, and shared reflections vanish from the model's awareness.
2. **Session-Lineage Silos**: Dialogue history was strictly partitioned per-session. When a user opened a new session with the `philosophist` role, the agent had zero recall of discussions held days or weeks prior.
3. **The Unbounded Growth Anti-Pattern**: Naive RAG implementations log every message indiscriminately into a vector database or flat index. Over months of interaction, this leads to catastrophic memory bloat, high retrieval latency, dilution of relevant context, and unbounded disk footprint on terminal end-user workstations.
4. **Noise Contamination**: Indiscriminate session indexing captures shell execution outputs, tool results, file diffs, and system injections. A conversational role needs recall over *interpersonal dialogue* (what the user stated and what the role contemplated), not intermediate bash tool traces.
5. **Heavy Native Stack Dependencies**: Standard industry RAG pipelines rely on Python subprocesses, external vector databases (Qdrant, Milvus), or heavyweight ONNX Runtime bindings (`fastembed`) that mandate downloading hundreds of megabytes of model weights on startup—unacceptable for a zero-dependency, local-first terminal engine.

## Decision Drivers

- **Role Isolation & Clean Boundary**: Memory must strictly belong to the specific role (`philosophist`). No cross-role bleed (e.g., developer shell sessions must not pollute philosophical inquiry).
- **Pure Dialogue Intake**: Capture only authentic user prompt $\leftrightarrow$ role response turns, completely free from tool execution results, AST queries, or internal system prompts.
- **Human Cognitive Fidelity**: Mirror biological memory dynamics:
  - Immediate/Working Memory (high activation for recent exchanges, zero initial decay).
  - Long-term Memory with Ebbinghaus exponential forgetting curves.
  - Spaced Repetition (retrieval practice): recalling a memory strengthens its neural/cognitive stability and retards future forgetting.
- **Zero-Dependency Native Rust Stack**: Leverage embedded SQLite with FTS5 BM25 and multiscale CJK/lexical fallback. Zero external daemons, zero Python, zero required weight downloads, instant startup.
- **Strict Bounded Footprint**: Self-pruning and capacity bounding to ensure the on-disk database remains compact, performant, and permanently maintainable for terminal end users.

## Considered Options

- **Option 1**: Heavyweight Vector Embedding Store (ONNX Runtime / fastembed-rs + sqlite-vec).
- **Option 2**: Ephemeral Session-Only Context Injection (replaying past session summaries into system prompt).
- **Option 3**: **Role-Scoped Cognitive Dialogue Memory via Embedded SQLite FTS5, Ebbinghaus Decay Scoring, Spaced Repetition Reinforcement, and Self-Pruning Bounded Capacity.**

## Decision Outcome

Chosen option: **Option 3**.

### 1. Storage & Tech Stack: `role_memory.db`

Durable role memory lives in an independent SQLite database located at `$XDG_DATA_HOME/muta/role_memory.db` (`Dirs::role_memory_db`), isolated from `muta.db` to prevent schema migration interference.
- Configured with WAL mode, `synchronous = NORMAL`, and relational integrity.
- Stores relational memory metadata in `role_memories` with full-text indexing via SQLite `fts5` virtual table `fts_role_memories` (Porter + unicode61 tokenizer).
- Employs hybrid search: FTS5 BM25 ranking combined with substring/term fallback for non-spaced CJK (Chinese, Japanese, Korean) queries.

### 2. Biological Memory Dynamics (Ebbinghaus Forgetting & Reinforcement)

Memories are modeled with a continuous cognitive retention score $R \in [0.0, 1.0]$:

$$R(\Delta t) = \begin{cases} 1.0 & \text{if } \Delta t \le \tau_{\text{working}} \\ \exp\left( -\frac{\Delta t_{\text{days}}}{S \times \tau_{\text{base}}} \right) & \text{if } \Delta t > \tau_{\text{working}} \end{cases}$$

Where:
- $\Delta t$: Elapsed time since the last retrieval/access timestamp (`last_accessed_at_s`).
- $\tau_{\text{working}}$: Working memory window (12 hours / 43,200 seconds). Interactions within this threshold exhibit 100% retention.
- $\tau_{\text{base}}$: Baseline decay half-life (7.0 days).
- $S$: Memory strength / stability (initial default $S = 1.0$).

#### Spaced Repetition Reinforcement
Whenever the agent successfully recalls a memory via `recall_memory`:
1. `access_count` increments by 1.
2. `last_accessed_at_s` resets to the current UNIX timestamp.
3. `strength` increases ($S \leftarrow S + 1.0$).

This ensures that frequently revisited philosophical principles and discussions decay progressively slower, cementing into permanent long-term memory.

### 3. Composite Cognitive Retrieval Scoring

When querying memories, candidate entries are ranked by a composite cognitive score:

$$\text{Score} = \text{Relevance} \times \left( w_{\text{baseline}} + (1 - w_{\text{baseline}}) \times R(\Delta t) \right) \times \text{Importance}$$

Where:
- $\text{Relevance}$: FTS5 BM25 weight combined with keyword density.
- $w_{\text{baseline}} = 0.2$: Ensures high-relevance older memories are not completely shadowed, while fresh/reinforced memories are prioritized.
- $\text{Importance} \ge 1.0$: Salience weighting for foundational insights.

### 4. Bounded Capacity and Automatic Pruning

To prevent unbounded database growth on user devices:
- Each role is constrained to a default capacity cap ($N_{\text{max}} = 1000$ dialogue turns).
- Automated pruning runs upon dialogue insertion and maintenance:
  1. Evicts forgotten memories: $R(\Delta t) < 0.05$ with `access_count <= 1`.
  2. If the active entry count still exceeds $N_{\text{max}}$, entries with the lowest composite cognitive weight are pruned first.

### 5. Role Boundary & Dialogue Ingestion

- **Ingestion Hook**: In `muta-agent::orchestration::execute_round`, upon successful round completion, if `agent.active_role()` is set (e.g. `"philosophist"`), `!input.hidden`, and both user prompt and role response are non-empty, the pair is recorded into `RoleMemoryStore`.
- **Pure Dialogue**: Intermediate tool operations (bash, file reads, web fetches) and internal system reminders are strictly excluded.
- **Role Isolation**: The store indexes and filters strictly by `role`. `developer` turns are never visible to `philosophist`, preserving conversational purity.

### 6. Built-in Tool: `recall_memory`

A read-only capability registered via `muta_contracts::register_tool!` and admitted to the `philosophist` role:
- Parameters: `query: string` (required), `limit: integer` (optional, default 5, max 10).
- Output: Markdown-rendered recollection displaying elapsed time, cognitive category (`Working Memory` vs. `Long-term Memory (Retained XX%)`), access count, and verbatim dialogue turns.
- Role Prompt Alignment: `AgentRoleProfile::philosophist()` identity directive is updated to explicitly instruct the model to call `recall_memory` when referencing past perspectives, philosophical positions, or shared reflections.

---

## Invariants & Behavioral Boundaries

- **`[INV-MEM-01]` Pure Dialogue Ingestion**: The memory ingestion pipeline must record only authentic `(Role::User, Role::Assistant)` prompt-response pairs. Tool execution outputs, system prompts, AST symbols, and hidden injection messages must never be written to `role_memories`.
- **`[INV-MEM-02]` Strict Role Partitioning**: Memory queries and storage must be strictly isolated by the active agent role identifier. A role must never observe, query, or mutate memories belonging to another role.
- **`[INV-MEM-03]` Working Memory Invariance**: Any dialogue recorded within `WORKING_MEMORY_WINDOW_SECS` (12 hours) must yield a retention score of $1.0$ without decay.
- **`[INV-MEM-04]` Retrieval Practice Reinforcement**: Every memory recalled and returned by `recall_memory` must have its `access_count` incremented, `last_accessed_at_s` updated, and `strength` boosted ($S \ge S + 1.0$).
- **`[INV-MEM-05]` Bounded Capacity Guarantee**: The memory store must enforce a per-role capacity ceiling. Pruning must prioritize low-retention, single-access entries before evicting reinforced memories.
- **`[INV-MEM-06]` Zero External Service Dependency**: Role memory operations must run entirely within the embedded SQLite engine. No network requests, external Python runtimes, or ONNX weight downloads may be required for memory storage or recall.

---

## Positive Consequences

- **Conversational Longevity**: `philosophist` maintains meaningful continuity across multiple sessions, referencing past discussions naturally.
- **Realistic Cognitive Feel**: Recent topics are readily accessible in working memory; older topics require explicit conceptual cues to recall, accurately mimicking human memory.
- **Zero Maintenance for Users**: Capacity bounding and forgetting pruning ensure the database never grows unbounded or slows down.
- **Fast & Portable**: Sub-millisecond SQLite queries with WAL mode; works out of the box on macOS, Linux, and Windows without setup.

## Negative Consequences & Trade-offs

- **Keyword/Lexical Bound**: While FTS5 BM25 + CJK fallback performs well for topical and conceptual search, it lacks full vector latent semantic interpolation for drastically paraphrased concepts.
  - *Mitigation*: The role prompt instructs the model to query topical keywords and concepts; CJK character tokenization ensures high recall for non-Latin languages.

---

## Rejected Alternatives & Negative Knowledge

### Option 1 (Heavyweight Vector Store with ONNX/fastembed)
- *Why considered*: Dense embeddings provide semantic paraphrase matching.
- *Why rejected*: Violates the zero-dependency CLI principle. Bundling ONNX runtime adds 200MB+ to distribution binaries, requires downloading 100MB+ model weights at runtime, introduces cross-compilation friction on ARM/Linux targets, and adds noticeable latency to startup.

### Option 2 (Ephemeral Session-Only Summaries)
- *Why considered*: Simple to implement without new persistent storage.
- *Why rejected*: Fails the core requirement of multi-session conversational recall. When a user begins a new session with `/role philosophist`, all historical context is lost.

---

## Links

- Extends: [ADR-0244: Immutable Session Role](0244-immutable-session-role-and-truthful-identity-projection.md)
- Extends: [ADR-0246: Symmetric Role Architecture](0246-symmetric-role-architecture-and-canonical-roles-schema.md)
- Invariants: `docs/governance/documentation/core/invariants.md`
