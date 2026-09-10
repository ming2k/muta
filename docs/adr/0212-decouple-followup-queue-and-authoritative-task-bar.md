# 0212. Decouple user follow-up queue from background task fabric and introduce authoritative task bar

- **Status:** Proposed
- **Date:** 2026-09-09
- **Scope:** `runtime/task-fabric`, `contracts`, `tui/composer`, `orchestration`
- **Builds on / Amends:** ADR-0126 (Queue affordances), ADR-0190 (Agent as actor: unified task fabric), ADR-0197 (One frontend truth: shell as client of engine)

---

## Context and Problem Statement

ADR-0126 introduced the client-side follow-up outbox, and ADR-0197 M4 migrated queue authority to the daemon driver loop (`FollowUpQueue`), establishing the rule: *the daemon decides, the frontend projects*.

Simultaneously, ADR-0190 D3 specified that completion events from background tasks (Interactive detach, Services, Timers) must wake the agent loop at round boundaries. However, in implementing this wake turn, the runtime made an expedient compromise: `crates/muta-runtime/src/task_mailbox.rs:request_wake_turn` synthesizes a fake `muta_contracts::AgentRequest::FollowUp` containing the verbatim background job completion digest (`[background task finished: ...]`):

```rust
// crates/muta-runtime/src/task_mailbox.rs (Defective wake path)
let queued = muta_contracts::QueuedMessage {
    id: uuid::Uuid::new_v4().to_string(),
    text: digest,
    display_text: None,
    sent_at_ms: None,
    images: Vec::new(),
};
let _ = env.req_tx.send(muta_contracts::AgentRequest::FollowUp {
    session_id: session_id.to_string(),
    message: queued,
}).await;
```

This implementation shortcut creates severe architectural defects:

1. **Semantic Pollution of the Follow-Up Queue**:
   The user follow-up queue is authoritative human intent. Smuggling machine-authored notifications through `AgentRequest::FollowUp` causes the TUI to render internal harness crash dumps and build outputs (`[background task finished: `verify-full-build` — exited 0 (ok)... BUILD FAILED...]`) as pending user messages in the follow-up bar. When the active round ends, the driver automatically dequeues this text and launches a new round, executing a machine log as if the operator typed it.

2. **Silent Mutation of In-Flight Steering into Next-Round Follow-Ups**:
   Steering (`AgentRequest::Steer`) is an ephemeral, in-round intervention aimed at turning points between tool invocations within the *active* round. When a steer arrives as the round is closing, or when unconsumed steer items are drained on round exit (`crates/muta-agent/src/orchestration.rs:819`), the engine emits `RoundEvent::SteerUnavailable`. The TUI frontend (`apps/tui/crates/mutx/src/lib.rs:598`) handles this by dispatching `AppMutation::DispatchRequeued`, which silently pushes the steer text into `pending_dispatch` as a next-round follow-up.
   This silent mutation violates human intent: a transient instruction intended for a running turn (e.g., *"stop searching and check src/lib.rs"*) is re-executed across a round boundary as an out-of-context fresh prompt after the previous round has already finished.

3. **Invisible Task Execution & Lack of Observability**:
   The background fabric runs processes and services without an authoritative, persistent visual representation in the user interface. Users have no visibility into active tasks, elapsed execution time, or settlement outcomes without relying on conversational transcript pollution.

A clean break is required. We must purge all system event smuggling from the follow-up pipeline, establish strict lifecycle invariants for steering, and introduce a first-class, authoritative **Task Bar** in the TUI backed by dedicated task-fabric protocol events.

---

## Decision Drivers

- **Semantic Exclusivity of User Intent**: The follow-up queue belongs exclusively to the operator. Zero machine-generated, system-authored, or fallback messages may enter it.
- **Fail-Dead Transient Control**: Steering is strictly scoped to the active round. If a steer misses the round admission window, it must expire safely and return to the operator's compose buffer—never silently mutate into a future round prompt.
- **First-Class Task Observability and Interaction**: Background jobs and services must have dedicated visual status lines (Task Bar) and interactive affordances (inspect logs, terminate/kill, manual outcome ingestion) decoupled from transcript chat history.
- **Strict Precedence of Human Will over System Wakes**: Automatic system wakes must never preempt, displace, or interleave with user-queued prompts.

---

## Considered Options

- **Option 1 (Chosen): Radical Clean-Break — Decoupled Task Fabric, Pure Human Follow-Up Queue, Safe Steer Restoration, and Authoritative Task Bar**
  Purge fake follow-ups from `task_mailbox`. Replace `SteerUnavailable` re-queueing with immediate restoration to the composer draft. Introduce dedicated `muta_contracts::TaskEvent` wire protocols and render an authoritative `TaskBar` above the composer for real-time task management.

- **Option 2 (Rejected): UI-Level Prefix Filtering and Visual Masking**
  Keep `AgentRequest::FollowUp` as the universal event pipeline, but prefix system items with magic headers (e.g., `[SYSTEM:TASK]`) and filter them out of the TUI follow-up bar.
  *Why rejected*: Preserves the root architectural disease. The driver loop still treats system digests as user prompts, queue serialization remains corrupted, and headless or non-TUI clients inherit the bug.

- **Option 3 (Rejected): Dual-Lane Follow-Up Queue in Runtime**
  Split `FollowUpQueue` into `human_items` and `system_items` lanes, hiding system items from the UI snapshot while dequeuing them at round boundaries.
  *Why rejected*: Unnecessary complexity. System wakes are not prompts; they are environmental state updates. Treating task completion as a prompt conflates conversation with lifecycle orchestration.

---

## Decision Outcome

Adopt **Option 1**. Break clean with zero backward-compatibility shims.

### 1. Architectural Changes & Data Flow

```text
               ┌─────────────────────────────────────────────────────────────┐
               │                         OPERATOR                            │
               └──────────────┬───────────────────────────────┬──────────────┘
                              │                               │
                      [Ctrl+Enter / Alt+Enter]         [Enter (Running)]
                              │                               │
                              ▼                               ▼
                   AgentRequest::FollowUp             AgentRequest::Steer
                              │                               │
                              ▼                               ▼
                   ┌──────────────────────┐        ┌──────────────────────┐
                   │   FollowUpQueue      │        │ Active Round Gate    │
                   │ (100% Human Prompts) │        │ (In-flight Turn Hook)│
                   └──────────┬───────────┘        └──────────┬───────────┘
                              │                               │
                      [Round Boundary]                [Missed / Round End]
                              │                               │
                              ▼                               ▼
                     Fresh User Round                RoundEvent::SteerExpired
                                                              │
                                                              ▼
                                                   Restored to Composer Draft
                                                   (Zero mutation to FollowUp)

──────────────────────────────────────────────────────────────────────────────────

               ┌─────────────────────────────────────────────────────────────┐
               │                    BACKGROUND TASK FABRIC                   │
               └──────────────┬───────────────────────────────┬──────────────┘
                              │                               │
                       [Spawn / Tick]                     [Settled]
                              │                               │
                              ▼                               ▼
                    TaskEvent::Progress             TaskEvent::Settled
                              │                               │
                              └───────────────┬───────────────┘
                                              │
                                              ▼
                             ┌─────────────────────────────────┐
                             │  Authoritative Task Fabric Bus  │
                             └────────────────┬────────────────┘
                                              │
                                              ▼
                             ┌─────────────────────────────────┐
                             │     TUI Task Bar Component      │
                             │ [⚙ test (42s)] [✘ build (exit 1)]│
                             └────────────────┬────────────────┘
                                              │
                                  ┌───────────┴───────────┐
                                  ▼                       ▼
                           [Space / Enter]             [Dismiss]
                                  │                       │
                       Explicit Human Ingestion      Purged from Bar
                       (Inject into Composer /
                        Voluntary Round Dispatch)
```

### 2. Invariants & Behavioral Boundaries

- **`[INV-TASK-01]` Human Exclusivity of FollowUpQueue**:
  The `FollowUpQueue` in `crates/muta-runtime` and `pending_dispatch` in `apps/tui` MUST only accept prompts explicitly submitted by the user. Runtime mailboxes, task completion digests, system supervisors, and error handlers are strictly forbidden from emitting `AgentRequest::FollowUp`.

- **`[INV-TASK-02]` Fail-Dead Steering & Composer Draft Restoration**:
  A steering input (`AgentRequest::Steer`) is valid solely within the dynamic lifetime of the targeted active round. If a steer cannot be admitted into an in-flight turn boundary before the round completes:
  1. The engine MUST emit `RoundEvent::SteerExpired { input_id, text, sent_at_ms }`.
  2. The frontend MUST NOT re-queue the input into `pending_dispatch` or `FollowUpQueue`.
  3. The frontend MUST restore the unadmitted text verbatim into the active composer editor with an ephemeral status banner (*"Round completed before steer could be admitted; restored to draft"*).

- **`[INV-TASK-03]` Dedicated Task Fabric Protocol**:
  Task lifecycle states MUST ride dedicated wire protocol events:
  - `AgentResponse::TaskSnapshot { tasks: Vec<TaskState> }`
  - `AgentResponse::TaskUpdated { task: TaskState }`
  No task output or settlement notice may be converted into a `RoundEvent` or `TranscriptMessage` unless explicitly requested by the user or admitted through a deliberate tool query (`process_logs`, `process_poll`).

- **`[INV-TASK-04]` Authoritative Task Bar**:
  The TUI MUST render a dedicated `TaskBar` component situated immediately above the composer:
  - **Active State**: Displays task label, animated spinner, and elapsed runtime (e.g. `[⚙ verify-full-build 1m 12s]`).
  - **Settled State**: Displays outcome badge with color coding (e.g. `[✓ cargo check ok]` or `[✘ verify-full-build exit 1]`).
  - **Interaction**:
    - `Space` / `Click`: Ingest task summary into composer draft for explicit submission.
    - `Esc`: Dismiss the settled task notification.
    - `Ctrl+B`: Open the Task Inspector overlay to view streaming output or kill tasks.

- **`[INV-TASK-05]` Human Will Precedence Over Autonomous Wakes**:
  If autonomous task wake turns are configured for headless agents:
  1. An autonomous wake turn MUST use `RoundDriver::SystemWake { task_id }`, distinct from `RoundDriver::Fresh` and `RoundDriver::FollowUp`.
  2. If the `FollowUpQueue` contains any human-queued items, the human item MUST be dispatched first. Autonomous wakes MUST yield until all human follow-up items are exhausted.

---

## Technical Specifications

### A. Protocol Contracts (`crates/muta-contracts`)

```rust
/// Task state broadcast for frontend TaskBar projection.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskSummary {
    pub task_id: String,
    pub label: String,
    pub state: TaskExecutionState,
    pub started_at_ms: u64,
    pub settled_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TaskExecutionState {
    Running,
    Success { exit_code: i32 },
    Failed { exit_code: Option<i32>, error: String },
    Cancelled,
}

/// Dedicated task lifecycle responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TaskResponse {
    Snapshot(Vec<TaskSummary>),
    Upsert(TaskSummary),
    Removed(String),
}

/// Round steering outcome replacement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RoundEvent {
    // ...
    // REPLACES: SteerUnavailable { input_id: String }
    SteerExpired {
        input_id: String,
        text: String,
        sent_at_ms: Option<u64>,
    },
    // ...
}
```

### B. Runtime Mailbox Purge (`crates/muta-runtime`)

1. **Delete fake FollowUp dispatch**:
   In `crates/muta-runtime/src/task_mailbox.rs`, completely remove the `running` branch that sent `AgentRequest::FollowUp`.
2. **Wire authoritative Task Fabric broadcasts**:
   When a task spawns, ticks, or settles in `BackgroundJobManager`, emit `AgentResponse::Task(TaskResponse::Upsert(...))` directly to the client channel.

### C. Orchestration & Steering Guard (`crates/muta-agent`)

In `crates/muta-agent/src/orchestration.rs`, close of `session_queues` drains unadmitted steers:
```rust
let (pending_steer, _pending_follow_up) = context.agent.close_session_queues(generation);
for pending in pending_steer {
    let _ = context.tx.send(round_response(
        &context.session_id,
        RoundEvent::SteerExpired {
            input_id: pending.id,
            text: pending.text,
            sent_at_ms: pending.sent_at_ms,
        },
    ));
}
```

### D. TUI Shell Restoration & Task Bar (`apps/tui/crates/mutx`)

1. **Handle `SteerExpired`**:
   `apps/tui/crates/mutx/src/lib.rs` receives `SteerExpired`:
   - Triggers `AppMutation::RestoreDraftFromExpiredSteer { text }`.
   - Populates composer text and attachments.
   - Zero insertions into `pending_dispatch`.
2. **Mount `TaskBar` Component**:
   - Integrated into `src/render/composer.rs` and `src/app/mod.rs`.
   - Sits directly on top of the composer input area when active tasks exist.
   - Cleans up visual noise and returns transcripts to pure conversation.

---

## Positive Consequences

- **Purity of Dialogue History**: No more random command exit codes, stack traces, or build failure digests injected as fake user utterances.
- **Predictable Steering Semantics**: Operators never have to worry about a missed steer silently executing as an unintended next round.
- **Operational Clarity**: Background builds, tests, and watchers become visible and controllable via the new Task Bar.
- **Zero Hallucination Triggers**: Models no longer receive confusing `[background task finished: ...]` prompts that derail ongoing reasoning chains.

---

## Negative Consequences & Trade-offs

- **Loss of Unsolicited Autonomous Turn Triggering in TUI**:
  By default, background task settlement will no longer automatically wake the LLM if the session is idle unless the user explicitly presses Space on the Task Bar or an explicit autonomous wake mode is enabled.
  *Mitigation*: This is the desired behavior for interactive TUI sessions. Users do not want the model burning tokens autonomously on background build completions unless explicitly instructed. For headless subagents, `SystemWake` remains available under strict precedence rules.

---

## Rejected Alternatives & Negative Knowledge

### Retaining Steer Re-queueing Behind a Configuration Flag
- *Why considered*: Allow users who prefer "never lose my typing" to keep the old behavior.
- *Why rejected*: A steer is inherently turn-contextual. Re-running a turn-level intervention as a top-level prompt is almost always semantically incorrect and produces subtle agent errors. Restoring the text directly to the composer draft preserves the user's typing without corrupting execution semantics.

### Storing Background Notifications in Transcript as System Cards
- *Why considered*: Easier than building a stateful Task Bar.
- *Why rejected*: Pollutes the transcript timeline and creates visual clutter. Background tasks are concurrent processes, not sequential conversational events. They belong in an orthogonal operational plane.
