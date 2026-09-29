---
id: ADR-0294
title: "Darwin libproc Kernel-Evidence Supervised-Input Examiner: Resolving Terminal Wait States via XNU Process Introspection"
status: proposed
date: 2026-09-29
scope: platform/supervised, platform/process, platform/darwin
superseded_by: null
negative_knowledge: true
---

# 0294. Darwin libproc Kernel-Evidence Supervised-Input Examiner: Resolving Terminal Wait States via XNU Process Introspection

- **Status:** Proposed
- **Date:** 2026-09-29
- **Scope:** `muta-platform` (`supervised`, `process`), `platform/darwin`
- **Deciders:** Muta Architecture Team
- **Refines:** [ADR-0293](0293-single-owner-supervised-input-capability-seam.md) — completes the Darwin half of the unified supervised-input capability seam, enabling macOS to promote from `TerminalOnly` to `Supervised`.
- **References:** [ADR-0292](0292-command-input-execution-contract.md), [ADR-0286](0286-hermetic-headless-execution-and-interactive-process-containment.md)

---

## Context and Problem Statement

ADR-0293 established the single-owner capability seam for supervised command input, consolidating pty management, kernel wait detection, and answer injection behind `muta-platform::supervised`. Under that contract:
- Linux implements full kernel-evidence supervision via `/proc/<pid>/wchan` (disclosing `n_tty_read`/`tty_read`) and `/proc/<pid>/fd/0` pointing to `/dev/pts/*`, reporting `InputSupervision::Supervised`.
- macOS was intentionally gated at `InputSupervision::TerminalOnly` (falling back to `InputContract::Sealed` fast-fail). While the controlling terminal mechanism (`posix_openpt`, `grantpt`, `unlockpt`, `TIOCSCTTY`) works identically across POSIX targets, Darwin lacks `/proc`. Without reliable kernel wait detection, auto-injecting an answer or fast-failing during a quiet build risks misfiring on legitimate compute.

To promote macOS from `TerminalOnly` to full `Supervised` status, `muta-platform::supervised` requires a Darwin-native kernel evidence examiner capable of distinguishing an interactive terminal read prompt (e.g. `sudo`, `read`, password prompts) from:
1. Active computation (compilers, bundlers, test suites).
2. Non-interactive sleeping (timer sleep, `sleep 10`).
3. Socket / IPC blocking (network fetches, language servers).

Because Darwin (XNU kernel) enforces Kernel ASLR (KASLR) and does not expose symbolic wait-channel names (like Linux `/proc/<pid>/wchan`) to unprivileged userspace, naive wait-state heuristics fail. This ADR defines the Darwin kernel-evidence architecture using `libproc` APIs, descriptor introspection, and terminal buffer telemetry.

---

## Decision Drivers

- **Zero False-Positives (Chesterton's Fence)**: Under no circumstances may a quiet compilation, link step, or background timer sleep be misclassified as an interactive input wait.
- **Architectural Seam Alignment**: The Darwin examiner must sit strictly behind `SupervisedChild` in `muta-platform::supervised`. `muta-agent` must observe only `InputWait::{Idle, Awaiting}` and `input_supervision() == Supervised`.
- **No Unprivileged Breakdown**: Must work without `root` / `sudo` privileges and without disabling System Integrity Protection (SIP).
- **Verification Honesty**: Implementation code must remain gated behind explicit compile/target guards and target verification harnesses, preventing blind inclusion of unverifiable binaries on non-Darwin build hosts.

---

## Considered Options

- **Option 1: Naive Thread State Polling via `proc_pidinfo(PROC_PIDTHREADINFO)` alone.**
  Poll whether threads in the target process group have `pth_run_state == TH_STATE_WAITING`.
- **Option 2: Multi-Factor XNU Evidence Chain: `libproc` Task/Thread Inspection + Descriptor Vnode Resolution + Master TTY Buffer `ioctl(FIONREAD)`.**
  Combine task-level CPU usage deltas, thread execution states, file descriptor vnode matching on `fd 0`, and the pty master's pending input buffer telemetry.
- **Option 3: External DTrace / `dtruss` or `lldb` Process Attachment.**
  Attach a tracer to the child process to monitor the `read(0, ...)` syscall directly.

---

## Decision Outcome

Chosen option: **Option 2 (Multi-Factor XNU Evidence Chain)**.

Option 1 causes catastrophic false-positives on standard timer sleeps and disk I/O. Option 3 requires elevated privileges (`root`/SIP bypass) and imposes unacceptable process-attachment latency and instability. Option 2 provides deterministic, unprivileged kernel evidence with sub-millisecond polling cost.

### 1. The Multi-Factor Darwin Kernel-Evidence Chain

A Darwin process group is classified as `GroupState::Awaiting` if and only if **all four independent factors** hold simultaneously across consecutive polling cycles:

```text
┌────────────────────────────────────────────────────────────────────────┐
│ 1. Descriptor Topology (fd 0 == harness slave pty vnode)              │
│    proc_pidfdinfo(pid, PROC_PIDFDVNODEPATHINFO, 0, &vnode_info)        │
│    vnode_info.pvi_cdir.pv_path matches allocated "/dev/ttysNNN"       │
└───────────────────────────────────┬────────────────────────────────────┘
                                    │ AND
┌───────────────────────────────────▼────────────────────────────────────┐
│ 2. Master Buffer Depletion (no unread input waiting in pty queue)      │
│    ioctl(master_fd, FIONREAD, &pending_bytes) == 0 && pending == 0     │
└───────────────────────────────────┬────────────────────────────────────┘
                                    │ AND
┌───────────────────────────────────▼────────────────────────────────────┐
│ 3. Task CPU Stagnation (zero compute progress across interval)         │
│    proc_pidinfo(pid, PROC_PIDTASKINFO, &task_info)                     │
│    Δ(task_info.pti_total_user + task_info.pti_total_system) == 0       │
└───────────────────────────────────┬────────────────────────────────────┘
                                    │ AND
┌───────────────────────────────────▼────────────────────────────────────┐
│ 4. Thread Wait State & Sleep Non-Zero                                  │
│    proc_pidinfo(pid, PROC_PIDTHREADINFO, &thread_info)                 │
│    No thread in TH_STATE_RUNNING; leader/worker in TH_STATE_WAITING    │
│    with pth_sleep_time > 0                                             │
└────────────────────────────────────────────────────────────────────────┘
```

### 2. Kernel Introspection Architecture (`libproc` Details)

#### A. Task CPU Telemetry (`PROC_PIDTASKINFO`)
`muta-platform` queries task metrics without Mach ports:
```c
struct proc_taskinfo taskinfo;
int ret = proc_pidinfo(pid, PROC_PIDTASKINFO, 0, &taskinfo, sizeof(taskinfo));
```
- `taskinfo.pti_total_user`: Total user time in nanoseconds.
- `taskinfo.pti_total_system`: Total system time in nanoseconds.
- Aggregate CPU ticks = `(taskinfo.pti_total_user + taskinfo.pti_total_system) / NS_PER_TICK`.
- If aggregate ticks increase between poll steps, the process group is actively computing; wait state is immediately demoted to `GroupState::Running`.

#### B. Stdio Slave Verification (`PROC_PIDFDVNODEPATHINFO`)
To verify that `fd 0` has not been redirected away to `/dev/null`, a pipe, or a regular file:
```c
struct proc_vnodepathinfo vpi;
int ret = proc_pidfdinfo(pid, 0, PROC_PIDFDVNODEPATHINFO, &vpi, sizeof(vpi));
```
The vnode path `vpi.pvi_cdir.pv_path` must match the assigned slave path (e.g. `/dev/ttys003`). If `fd 0` is not the harness-allocated terminal slave, the process is not waiting on the harness terminal, yielding `GroupState::Otherwise`.

#### C. Thread State and Wait Channel Resolution (`PROC_PIDINFO_THREADINFO`)
Darwin exposes thread state via:
```c
struct proc_threadinfo thinfo;
int ret = proc_pidinfo(pid, PROC_PIDTHREADINFO, thread_id, &thinfo, sizeof(thinfo));
```
- `thinfo.pth_run_state`:
  - `TH_STATE_RUNNING` (1): Thread is executing or runnable. State is `GroupState::Running`.
  - `TH_STATE_STOPPED` (2): Thread suspended by signal/job control.
  - `TH_STATE_WAITING` (3): Thread is blocked waiting on an event.
  - `TH_STATE_UNINTERRUPTIBLE` (4): Blocked in page-fault or disk I/O.
- `thinfo.pth_sleep_time`: Number of seconds the thread has been sleeping. A value $\ge 1$ confirms the block is settled rather than a transient context switch.

#### D. Terminal Input Queue Telemetry (`ioctl(FIONREAD)`)
On Darwin, querying the master pty via `ioctl(master_fd, libc::FIONREAD, &mut nbytes)` discloses how many bytes remain unconsumed in the line discipline.
- If `nbytes > 0`, the child has unread input available; it is either processing or deliberately ignoring stdin. State cannot be `Awaiting`.
- If `nbytes == 0` AND the thread is blocked on `fd 0`, the line discipline has exhausted all input and the child is blocked on the terminal read.

### 3. Process Group Enumeration on Darwin

Linux uses directory enumeration over `/proc`. Darwin achieves group enumeration via `proc_listpids`:

```rust
pub fn enumerate_process_group(pgid: i32) -> Vec<i32> {
    let mut pids = vec![0i32; 1024];
    let bytes = unsafe {
        libc::proc_listpids(
            libc::PROC_PPID_ONLY, // or iterate all pids and filter by pgrp
            0,
            pids.as_mut_ptr().cast(),
            (pids.len() * std::mem::size_of::<i32>()) as i32,
        )
    };
    // filter by proc_pidinfo(pid, PROC_PIDTASKALLINFO).pbsd.pbi_pgid == pgid
    ...
}
```
All live processes belonging to the session/process group are aggregated:
- Total group CPU ticks = $\sum \text{task ticks}$.
- If **any** process in the group is `TH_STATE_RUNNING`, the group state is `GroupState::Running`.
- If all live processes are quiescent and the active leader/worker satisfies the 4-factor check, the group state is `GroupState::Awaiting`.

### 4. Stability De-bounce Gate

Identical to Linux (ADR-0293 §2):
- Consecutive `Awaiting` samples required: **$\ge 2$ consecutive polls** (with agent tick floor, minimum 500ms).
- Zero CPU progress across the entire group across the consecutive polls.
- An injection via `answer()` immediately resets the gate counter and cleared CPU baselines.

---

## Invariants & Behavioral Boundaries

- **`[INV-DARWIN-01]` Zero Speculative Auto-Injection**: No answer may be injected into a Darwin pty slave based on stream silence or time alone. `InputWait::Awaiting` requires explicit descriptor verification, CPU non-progress, and thread wait state.
- **`[INV-DARWIN-02]` Target Build Gating**: Darwin-specific FFI structures and `libproc` calls MUST be strictly isolated under `#[cfg(target_os = "macos")]`. Under Linux and Windows, compilation of `muta-platform` must remain 100% agnostic to Darwin headers.
- **`[INV-DARWIN-03]` Promotion Gate**: `input_supervision()` on `target_os = "macos"` shall remain `InputSupervision::TerminalOnly` until the implementation passes the Darwin Acceptance Test Suite on native Darwin hardware or macOS CI runners.

---

## Positive Consequences

- **Platform Parity**: macOS becomes a first-class supervised platform, lifting commands out of `Sealed` fast-fail without introducing hangs or false-positives.
- **Zero Daemon Privilege Required**: Uses standard, unprivileged `libproc` APIs accessible to standard developer shells without `sudo` or SIP exceptions.
- **Uniform Consumer API**: `muta-agent` and downstream CLI/TUI layers require zero changes; the capability enum transparently transitions from `TerminalOnly` to `Supervised`.

## Negative Consequences & Trade-offs

- **Kernel Introspection Cost**: `proc_listpids` and `proc_pidinfo` require system calls per poll. Mitigated by the ADR-0293 design: polling only occurs during command silence after the quiet floor (minimum 500ms), introducing negligible ($< 0.1\%$) CPU overhead.
- **XNU Internal Evolution**: While `libproc` is stable and used by tools like `lsof` and `ps`, Apple considers parts of it private/semi-private API. Mitigated by using standard `libc::proc_pidinfo` definitions packaged in standard crates (`mach2` / `libc`).

---

## Rejected Alternatives & Negative Knowledge

### 1. Simple `TH_STATE_WAITING` Check (Rejected)
- *Why considered:* Minimal code, checks if `thinfo.pth_run_state == 3`.
- *Why rejected:* `sleep 5`, `nanosleep`, background locks, and socket polling also put threads in `TH_STATE_WAITING`. Auto-injecting into a compiling build or a timed script would corrupt command execution and break builds.

### 2. DTrace / `dtruss` Syscall Tracing (Rejected)
- *Why considered:* Direct observation of `read(0, ...)` entry.
- *Why rejected:* DTrace on macOS requires root privileges and is restricted by System Integrity Protection (SIP). Totally incompatible with developer workstation usage.

### 3. Reading Terminal Slave via Non-Blocking Read from Harness (Rejected)
- *Why considered:* Testing whether the slave has characters available.
- *Why rejected:* Reading from the pty master steals bytes meant for the child; reading from the slave requires parent ownership and causes race conditions with the child's own `read()` syscall.

---

## Verification Protocol (Acceptance Matrix on Darwin Target)

Prior to promoting `input_supervision()` to `Supervised` on `macos`:
1. **Compilation Test**: Build `cargo check --target aarch64-apple-darwin` and `x86_64-apple-darwin` cleanly.
2. **False-Positive Gate (Negative Tests)**:
   - Run `python3 -c "import time; time.sleep(3)"`: Must yield `InputWait::Idle` continuously until exit.
   - Run multi-threaded build simulation (CPU + thread locks): Must yield `InputWait::Idle`.
3. **True-Positive Gate (Interactive Test)**:
   - Run `sh -c 'printf "Prompt: "; read val; echo "Got: $val"'`: Must transition to `InputWait::Awaiting` within 1000ms.
   - Send answer `"muta_verified\n"`: Child must wake immediately, receive answer, and exit 0.

---

## Links

- [ADR-0293: Single-Owner Supervised-Input Capability Seam](0293-single-owner-supervised-input-capability-seam.md)
- [ADR-0292: Command Input-Execution Contract](0292-command-input-execution-contract.md)
- [ADR-0286: Hermetic Headless Execution and Interactive Process Containment](0286-hermetic-headless-execution-and-interactive-process-containment.md)
