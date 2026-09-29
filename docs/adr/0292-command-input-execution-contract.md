---
id: ADR-0292
title: "Command Input-Execution Contract: Kernel-Evidence Supervision and Controlling-Terminal Injection"
status: accepted
date: 2026-11-04
scope: contracts/execution, platform/process, agent/tools, agent/execution
superseded_by: null
negative_knowledge: true
---

# 0292. Command Input-Execution Contract: Kernel-Evidence Supervision and Controlling-Terminal Injection

- **Status:** Accepted
- **Date:** 2026-11-04
- **Scope:** `muta-contracts` (`capability`, `tool_output`), `muta-platform`
  (`process`), `muta-agent` (`execute_command`, `shell_input`, `agent/execution`)
- **Deciders:** Muta Architecture Team
- **Supersedes:** the PTY rejection in [ADR-0043](0043-bash-stdin-execution-contract.md)
  §Alternatives ("A PTY for every command") and the `state == 'S'` heuristic in
  [ADR-0286](0286-hermetic-headless-execution-and-interactive-process-containment.md) §3.
- **Refined by:** [ADR-0293](0293-single-owner-supervised-input-capability-seam.md)
  — keeps this contract (kernel-evidence detection + controlling-terminal
  injection) but relocates the mechanism behind one platform capability seam.
  Where this ADR says "the tool cannot give the child a controlling terminal",
  read the corrected, atomic gate
  `muta_platform::supervised::input_supervision()`.

---

## Context and Problem Statement

Two prior decisions bounded how the harness handles a command that wants input,
and each left a gap that only becomes visible when they are read together.

**ADR-0043** established that shell steps are non-interactive by construction:
stdin is decided before spawn, a command that blocks on `read(stdin)` gets an
immediate EOF, and a pre-spawn **classifier** (`sudo`/`gpg`/`passwd`/…) decides
whether to ask the operator for one line of input first. It explicitly
**rejected** giving children a PTY, on the grounds that a PTY drags raw terminal
control sequences into output and would require a full VT100 state machine to
clean them.

**ADR-0286** added a runtime circuit breaker: an unattended command with closed
stdin that stops producing output is sampled for kernel activity, and if every
process is in interruptible sleep (`state == 'S'`) with zero CPU progress it is
killed as an interactive stall.

Three defects survive this combination:

1. **The stall detector is a heuristic, not a classifier.** `state == 'S'` with
   zero CPU progress is true of *every* sleeping wait — `hrtimer_nanosleep`
   (`sleep`), `do_wait` (a waiting shell, a blocked `flock`), and
   `poll_schedule_timeout` (an idle socket) all match. The circuit breaker
   therefore risks fast-failing legitimate quiet computation.
2. **The detector is dead on non-Linux.** `sample_activity` returns
   `ProcessActivitySample::default()` off Linux, so `process_count == 0` and the
   `process_count > 0` guard can never trip — macOS silently falls back to the
   8-minute idle budget.
3. **The classifier is pre-spawn and advisory, so it cannot see the wait that
   actually happens.** A command the classifier did not recognize (a custom
   binary, a nested script, a `git` invocation that reaches a credential
   helper) gets no supervision at all; a command it *did* recognize is answered
   before spawn, into stdin, even when the program would read `/dev/tty`
   instead (`sudo`, `gpg`, `pinentry`) — which, per ADR-0286, has been
   `setsid`-detached and fails with `ENXIO`.

The load-bearing question is whether "is this process waiting for input?" can be
answered directly rather than guessed. It can:

- `/proc/<pid>/wchan` names the kernel function a sleeping task is parked in.
  Measured on Linux 6.x: `pipe_wait_readable` / `anon_pipe_read` / `pipe_read`
  for a stdin pipe read, `wait_woken` for a `/dev/tty` read, versus
  `hrtimer_nanosleep`, `do_wait`, and `poll_schedule_timeout` for non-input
  sleeps.
- `/proc/<pid>/fd/0` says *which* channel fd 0 resolves to (`pipe:[…]`,
  `/dev/tty`, `/dev/pts/N`), disambiguating a prompt from a socket wait.

And the reason ADR-0043 gave for rejecting PTYs does not hold: the PTY is only
polluting if it is the **output** channel. Giving the child a controlling
terminal while stdout/stderr remain pipes yields `/dev/tty` prompt support at
**zero** output-pollution cost — verified directly (a `read < /dev/tty` child
answered through the pty master while its stdout stayed a clean pipe).

## Decision

A command's input handling becomes a **runtime-supervised contract**, with three
mutually exclusive modes decided before spawn and enforced by kernel evidence at
runtime.

### 1. `InputContract` — the input axis of the execution contract

`StdinPolicy` is replaced by `InputContract`, whose variants cover the whole
space:

- **`Sealed`** — `/dev/null` stdin, no controlling terminal, no runtime
  supervision. The default hard floor. A `read(stdin)` gets instant EOF, so an
  input block is structurally impossible, and the child gets no terminal, so it
  cannot *induce* interactive behaviour by branching on `isatty`.
- **`Prefilled { data }`** — a harness-held pipe preloaded from a declared
  source (operator or opt-in model), then closed.
- **`Supervised { expectation }`** — a held-open stdin pipe **and** a
  child-owned controlling terminal, with a runtime examiner.

### 2. Physical terminal decoupling is preserved; the child's stdin *is* its terminal

`spawn_supervised` gives the child a private pty pair: `setsid` (already
required by ADR-0286) is followed, in `pre_exec`, by an open of the slave,
`dup2` of the slave onto fd 0, and `TIOCSCTTY` — so the child's **stdin is the
terminal slave** and `open("/dev/tty")` resolves to the same terminal, while
stdout/stderr remain ordinary pipes. Two consequences make this strictly better
than a "held-open stdin pipe plus a separate controlling terminal":

- A `read(stdin)` and a `read("/dev/tty")` are the *same* channel, so the
  examiner's `fd 0` identity is always the right one to report. The
  `sudo`/`gpg`/`pinentry` shape — which opens `/dev/tty` on a *separate* fd
  while fd 0 is an unrelated pipe — is detected and answered, which a pipe-plus-
  separate-terminal design gets wrong.
- There is exactly one place an answer goes: a write to the pty master. No
  second channel to keep in sync, and no way for a child to read input the
  harness cannot observe.

The child never sees the operator's real terminal, and no terminal control
sequences ever reach the transcript (output is on pipes).

### 3. Detection by kernel evidence, not stream content

`ProcessActivitySample` gains `wait_state: WaitState`
(`AwaitingInput { channel }` / `Running` / `Sleeping` / `Gone`), derived from
`wchan` + `fd/0`. A **terminal-family** wait (`n_tty_read`, `wait_woken`) is
authoritative on its own — fd 0 must not gate it, because the tty read may be on
a separate fd; a **pipe** wait (`pipe_wait_readable`, `anon_pipe_read`,
`pipe_read`, `wait_for_partner`) is an input wait only when fd 0 really is a
pipe, which keeps a socket read (`wait_woken` on an fd-0 socket) from being
misclassified. `Sleeping` (a compile, a `sleep`, an idle socket) falls through
to the idle budget, and a restricted `wchan` (kernel hardening) degrades to
`Sleeping` rather than fast-failing. Human-readable output is never consulted —
a password prompt and a compiling build are byte-identical in silence.

Sampling is **not** on the per-line path. A kernel sample scans `/proc` (cost
O(host processes)); taking one per output line would slow a many-line command by
seconds. Arrival of a line is itself proof of progress, so the examiner samples
only after the command has been quiet past the floor (at most a few samples per
second), and the stability gate counts consecutive quiet-path samples rather
than wall-clock deltas. A regression test asserts multi-line commands stay in
the millisecond range.

### 4. One drain loop, two postures

A single loop runs for every contract. On a detected wait it **injects** when an
`InputHandler` is present (the supervised, attended case) and **fast-fails** with
`ShellTermination::InputUnanswered` when it is not (the sealed/unattended case).
`skip_interactive_input` and unattended mode simply decline to arm the handler.

The runtime supervisor crosses the `Tool` boundary as
[`ToolInvocation::input_handler`] — a per-invocation `&dyn InputHandler` built by
the dispatch layer (it captures the live event channel), so the command tool owns
the spawn/examination while the agent owns the human channel.

### 5. Platform honesty

`Tool::interactive_input_supported()` + `process::controlling_terminal_supported()`
let the dispatch layer choose `Supervised` only where a controlling terminal
exists (Unix). Where it does not (Windows, today), the layer chooses `Sealed` —
clean immediate-EOF semantics — rather than fabricating a terminal, and the
classifier's pre-spawn refusal still applies.

## Alternatives Considered

- **Keep `state == 'S'` and add command-name heuristics.** Rejected: the defect
  is the *evidence*, not its breadth. Kernel wait-reason telemetry is strictly
  more precise and tool-agnostic; broadening a string matcher reproduces the
  leaky special-casing ADR-0286 already rejected.
- **Give every child a PTY as its output channel** (the ADR-0043 rejection).
  Rejected again, for the same reason — but the *rejection no longer blocks* the
  input goal, because the terminal is used only as the controlling terminal and
  as stdin while output stays on pipes. This is the insight ADR-0043 lacked when
  it conflated "a PTY" with "the output channel".
- **A held-open stdin pipe *plus* a separate controlling terminal (no stdin
  hijack).** This was the first design, and testing rejected it: a program that
  opens `/dev/tty` on a separate fd while fd 0 is an unrelated pipe presents a
  `wait_woken` wait with a *pipe* `fd 0`, which the discriminator then
  misreports. Making the child's stdin *be* the terminal slave collapses stdin
  and `/dev/tty` into one channel and removes the ambiguity. (`read x < /dev/tty`
  in `sh` does **not** reproduce the bug — `sh` `dup2`s the redirect onto fd 0 —
  so the discriminating test must open `/dev/tty` on a fresh fd, e.g. via
  `python3 -c "open('/dev/tty')"`.)
- **Do the whole thing on stdout/stderr.** Rejected: undecidable. A prompt may
  emit nothing, or text indistinguishable from ordinary output; whether input is
  awaited is a process-state fact, not a stream fact.
- **Mid-execution stdin-only injection (no terminal), refused by ADR-0043.**
  Retained only for the `Sealed`/`Prefilled` paths. It cannot serve
  `sudo`/`gpg`/`pinentry`, which read `/dev/tty`; those require the controlling
  terminal. So it is neither removed nor sufficient.
- **`wait_woken` alone as the input signal.** Rejected as insufficient: it also
  covers `poll_schedule_timeout`-style socket waits. `fd/0` identity is the
  second factor that separates a prompt from a network wait.
- **Windows ConPTY in the same change.** Rejected for this ADR: it is
  unattributable from the development environment (a Linux sandbox), and
  shipping unverifiable platform code violates the "no speculative machinery"
  stance. Windows reports "unsupported" and runs `Sealed` — an honest boundary,
  not a silent hang. A future ADR may add ConPTY behind the same trait.
- **A live subscription to a "prompt detected" channel (inotify/`pidfd`).** A
  250ms poll is already well inside the human-perceptible budget, and every
  subscription mechanism reintroduces kernel-version coupling for no measurable
  gain over a cheap `wchan` read on an already-running tick.

## Consequences

**Positive.** "Waiting for input" is answered from kernel semantics, so
`sleep`/`wait`/`flock`/idle sockets are no longer at risk of a false
interactive-stall kill; a genuine `sudo`/`gpg` prompt on either channel is
answered through an operator panel and the command resumes; unattended sessions
fast-fail precisely; and unsupported platforms fail honestly.

**Negative.** A supervised run holds an extra pipe and a pty master per
command — negligible, but real. `wchan` readability depends on kernel
`kptr_restrict`; when restricted the examiner degrades to `Sleeping` (idle
budget) rather than fast-failing, so a restricted host is safe but slower.
Windows cannot supervise at all.

**Migration.** `StdinPolicy` → `InputContract`; `Tool::call_structured_with_events`
takes a `ToolInvocation`; tools gain `interactive_input_supported()`; the
`ShellTermination` enum gains `InputUnanswered`; the recovery sentinel is
`Sealed` (the old `Closed` default). No serialized state changes — the contract
is decided per call and never persisted.

## References

- [ADR-0043](0043-bash-stdin-execution-contract.md) — the stdin execution
  contract this supersedes on the PTY question and extends with runtime
  supervision.
- [ADR-0286](0286-hermetic-headless-execution-and-interactive-process-containment.md)
  — the interactive-stall circuit breaker whose `state == 'S'` heuristic this
  replaces with kernel wait-reason telemetry.
- [ADR-0141](0141-human-request-control-protocol.md) — the one broker/oneshot
  protocol the runtime input request rides, exactly like `ask_user`/permission.
- [ADR-0257](0257-unbounded-stream-guard-and-finite-execution-contract.md) —
  finite-execution contract the drain loop still enforces.
