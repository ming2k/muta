---
id: ADR-0293
title: "Single-Owner Supervised-Input Capability Seam: One Atomic Capability, Opaque Handle, No Platform Leakage"
status: accepted
date: 2026-09-29
scope: platform/supervised, platform/process, agent/tools, contracts/execution
superseded_by: null
negative_knowledge: true
---

# 0293. Single-Owner Supervised-Input Capability Seam: One Atomic Capability, Opaque Handle, No Platform Leakage

- **Status:** Accepted
- **Date:** 2026-09-29
- **Scope:** `muta-platform` (`supervised`, `process`), `muta-agent`
  (`execute_command`, `agent/execution`), `muta-contracts` (`capability`,
  `tool_output`)
- **Deciders:** Muta Architecture Team
- **Refines:** [ADR-0292](0292-command-input-execution-contract.md) — keeps its
  kernel-evidence detection and controlling-terminal injection, but moves the
  mechanism behind one cohesive platform seam.

---

## Context and Problem Statement

ADR-0292 introduced runtime-supervised command input: a child whose stdin is a
private pty slave (also its controlling terminal), with a kernel-evidence
examiner (`/proc/<pid>/wchan` + `fd 0`) that detects a genuine input wait and
either injects an operator answer or fast-fails. The behavior is right, but the
**seam was misplaced**: `muta-agent`'s command runner orchestrated spawn,
detection, injection, and termination by reaching directly into platform
primitives. Review found six defects, each a symptom of that misplacement:

1. **Two capabilities conflated into one flag.**
   `controlling_terminal_supported()` reported `cfg!(unix)` (can provide a
   terminal) while the examiner was `#[cfg(target_os = "linux")]` (can detect a
   wait). On macOS the terminal exist but detection is absent, so the dispatch
   chose `Supervised` and the examiner could never fire — a prompting command
   neither receives an answer nor fast-fails; it stalls to the idle budget (up
   to 8 minutes). **Worse than Windows**, which at least refuses cleanly.
2. **A documented claim that was false.** The ADR-0292 changelog asserted
   non-Linux now reports `Sleeping`; the code returns
   `ProcessActivitySample::default()`, whose `wait_state` is `Gone`
   (`#[default]`). The macOS defect was described as fixed while shipping
   unchanged.
3. **Duplicated vocabulary.** Two `InputChannel` enums (contracts and platform)
   joined by hand-written mapping in the agent.
4. **Abstraction leakage.** The public platform surface exposed `PtyMaster`,
   `PtyChildSetup` (`Box<dyn Fn() -> io::Result<()>>` — a fork/`pre_exec`
   concept), and `SupervisedSpawn`, so the agent saw pty and fork internals.
5. **Contract inconsistency.** `spawn_supervised` omitted the
   `rollback_failed_attach` path that `spawn_owned` has, and left
   `kill_on_drop` to the caller — breaking the "configure + spawn + contain is
   one operation" contract that `spawn_owned` established.
6. **Dead surface.** `InputChannel` and `InputPrompt::channel` had no consumer:
   `channel` was never read anywhere.

The root cause is one: **mechanism lived in the agent.** Whether a platform can
supervise is a platform fact; detecting a wait is a platform mechanism; the
agent should own only the loop and the policy.

## Decision

The supervised-input mechanism moves wholesale into a new
`muta-platform::supervised` module exposing **one atomic capability, one opaque
handle, and one observation enum**. The agent never sees pty, `pre_exec`, or
`/proc`.

### 1. One atomic capability

```rust
pub enum InputSupervision {
    /// No controlling terminal — the caller MUST use `Sealed` (immediate EOF).
    Unsupported,
    /// A terminal is available but a wait cannot be told apart from legitimate
    /// quiet computation — the caller MUST NOT auto-inject (it would misfire on
    /// a compiling build) and MUST fall back to `Sealed` fast-fail.
    TerminalOnly,
    /// Terminal + reliable wait detection — full supervision.
    Supervised,
}

pub const fn input_supervision() -> InputSupervision;
```

The dispatch layer consults this **once**. `Unsupported` and `TerminalOnly`
both fall back to `Sealed`. "Platform independence" becomes a structural
property: there is one value to check, not three `cfg`s to keep consistent. A
platform can only reach the `Supervised` arm if it genuinely implements both
halves; there is no way to arm a terminal without detection.

### 2. One opaque handle

```rust
pub struct SupervisedChild { /* private: child, tree guard, tty, detector */ }

impl SupervisedChild {
    pub fn spawn(command: &mut Command) -> io::Result<Self>;
    pub fn stdout(&mut self) -> io::Result<ChildStdout>;
    pub fn stderr(&mut self) -> io::Result<ChildStderr>;
    pub async fn wait(&mut self) -> Option<i32>;
    pub fn terminate(&self) -> io::Result<()>;
    /// Advance the examiner one step; `Awaiting` only after the wait has been
    /// observed stable across consecutive samples with no CPU progress.
    pub fn poll_input_wait(&mut self) -> InputWait;
    /// Write one line to the child's terminal and reset the examiner.
    pub fn answer(&self, data: &str) -> io::Result<()>;
}
// Drop terminates the whole process tree.

pub enum InputWait { Idle, Awaiting }
```

Spawn, containment, rollback, `kill_on_drop`, pty ownership, and the stability
de-bounce all live behind this type. `spawn` reuses the corrected
`process::spawn_contained` (which now performs rollback on attach failure), so
the containment contract is identical to `spawn_owned`.

### 3. Channel identity collapses to "the terminal"

Because the child's stdin *is* the controlling terminal (ADR-0292 §2), a stdin
read and a `/dev/tty` read are one channel. The examiner therefore reports only
`Awaiting`/`Idle` — no channel tag — and the answer always goes to the single
pty master. The duplicated channel vocabulary is deleted.

### 4. Platform matrix (honest, each half gated by what is implemented)

| Platform | Terminal | Detection | `input_supervision()` |
|---|---|---|---|
| Linux | `posix_openpt` + `dup2` + `TIOCSCTTY` | `/proc/<pid>/wchan` + `fd 0` | `Supervised` |
| macOS | same pty path | not implemented yet | **`TerminalOnly`** → `Sealed` fast-fail |
| Windows | not implemented (ConPTY is future work) | — | `Unsupported` → `Sealed` |

macOS lands at `TerminalOnly` — an honest boundary that removes the 8-minute
stall. It is not `Supervised` because a `libproc` detector that cannot
distinguish a `read` from a `sleep` would auto-inject into the wrong wait.
Windows keeps its `Unsupported` stub until a ConPTY implementation satisfies the
same trait.

### 5. Deletions (no legacy burden)

- `muta_contracts::InputChannel` and `InputPrompt::channel` (dead).
- `Tool::interactive_input_supported` (the dispatch path already branches on the
  tool; a trait method to discover one platform boolean added indirection).
- From `muta-platform::process`'s public surface: `SupervisedSpawn`, `PtyMaster`,
  `PtyChildSetup`, `spawn_supervised`, `WaitState`, `InputChannel`,
  `ProcessActivitySample`, `sample_activity`, `controlling_terminal_supported`.
- The agent's `RunningChild`, channel mapping, and double-sampling stability
  gate (now platform-internal).

Wire is unchanged: the TUI's `StdinRequest{id, command, prompt, secret}` is
untouched.

## Alternatives Considered

- **Leave the mechanism in the agent and only fix the macOS flag.** Rejected:
  it repairs one symptom (defect 1) while leaving the misplacement (defects
  3–6) that caused it. The next platform addition would re-open the same class
  of bug.
- **A `trait SupervisedBackend` with runtime `dyn` selection.** Rejected: the
  capability is compile-time per target, and a trait object buys nothing over a
  `const fn` for a seam with one implementation per platform. It would also add
  a `Box<dyn>` hop inside the per-tick examiner.
- **Report `Sleeping` on non-Linux to force fast-fail (the ADR-0292 changelog's
  stated fix).** Rejected: it would fast-fail every quiet-but-fine command on
  macOS — killing legitimate compiles. The honest answer is `TerminalOnly` →
  `Sealed`, not a fabricated "sleeping" verdict.
- **Write the macOS `libproc` detector now.** Deferred: it cannot be compiled or
  exercised from this development environment (Linux), and a detector that
  cannot separate an input wait from a sleep would misfire. Shipping it blind is
  exactly the "unverifiable mechanism" this ADR removes. It belongs in a
  follow-up ADR proven on macOS.
- **Keep `InputPrompt::channel` for future ConPTY polymorphism.** Rejected: no
  consumer exists and the terminal model is channel-free; carrying an unused
  field against a hypothetical is speculative machinery.
- **Fold the mechanism into `process.rs` rather than a new module.** Rejected:
  `process.rs` is already 1000+ lines of general lifecycle concern; the
  supervised-input concern is distinct and belongs in its own module alongside
  the other capability modules.

## Consequences

**Positive.** "Platform independence" is enforced by one capability value
rather than three coordinated `cfg`s; macOS no longer stalls for 8 minutes; the
agent's command runner is reduced to a drain loop plus policy; the answer
channel is single and unambiguous; and the containment contract is uniform.

**Negative.** A new module and a small amount of platform plumbing to maintain.
`TerminalOnly` means macOS cannot yet auto-answer prompts — it fast-fails
instead, which is a capability gap made visible rather than hidden.

**Migration.** `StdinPolicy`/`InputContract` are unchanged (ADR-0292); only the
mechanism behind them moves. `controlling_terminal_supported` →
`input_supervision`. `Tool::interactive_input_supported` is removed. No
serialized state changes.

## References

- [ADR-0292](0292-command-input-execution-contract.md) — the supervised-input
  contract this refines; keeps its kernel-evidence detection and terminal
  injection, relocates the mechanism.
- [ADR-0286](0286-hermetic-headless-execution-and-interactive-process-containment.md)
  — physical terminal decoupling the pty path builds on.
- [ADR-0005](0005-strict-layering-and-renames.md) — the layering doctrine this
  restores (mechanism in `muta-platform`, policy in `muta-agent`).
