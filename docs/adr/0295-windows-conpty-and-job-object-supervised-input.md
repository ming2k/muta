---
id: ADR-0295
title: "Windows ConPTY and Job-Object-Contained Supervised-Input Seam: PseudoConsole Stdio Virtualization and Interactive Wait Detection"
status: proposed
date: 2026-09-29
scope: platform/supervised, platform/process, platform/windows
superseded_by: null
negative_knowledge: true
---

# 0295. Windows ConPTY and Job-Object-Contained Supervised-Input Seam: PseudoConsole Stdio Virtualization and Interactive Wait Detection

- **Status:** Proposed
- **Date:** 2026-09-29
- **Scope:** `muta-platform` (`supervised`, `process`), `platform/windows`
- **Deciders:** Muta Architecture Team
- **Refines:** [ADR-0293](0293-single-owner-supervised-input-capability-seam.md) — completes the Windows platform half of the single-owner capability seam, advancing Windows from `Unsupported` to `Supervised`.
- **References:** [ADR-0292](0292-command-input-execution-contract.md), [ADR-0286](0286-hermetic-headless-execution-and-interactive-process-containment.md)

---

## Context and Problem Statement

ADR-0293 established the single-owner capability seam for supervised command input in `muta-platform::supervised`. On Windows, the capability was initialized to `InputSupervision::Unsupported`:
- Windows lacks POSIX pseudo-terminals (`/dev/ptmx`, `posix_openpt`, `TIOCSCTTY`).
- Windows lacks POSIX process group sessions (`setsid`, `killpg`), making orphan-safe process tree termination dependent on Win32 Job Objects.
- Windows does not expose a Linux-like `/proc` or Darwin-like `libproc` for kernel wait-channel inspection.

Consequently, any interactive command on Windows currently fails fast under `InputContract::Sealed`. To achieve long-term platform parity and complete the ADR-0293 vision without technical debt, Windows requires a first-class implementation of:
1. **Terminal Virtualization**: Harnessing the Windows PseudoConsole API (`ConPTY`) introduced in Windows 10 (1809+).
2. **Process Tree Containment**: Deterministic tree lifecycle management using Win32 Job Objects with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
3. **Interactive Wait State Examiner**: Detecting console input blockage without relying on fragile heuristic window scraping.

---

## Decision Drivers

- **Leak-Proof Process Lifetime**: Subprocesses, background helpers, and grandchild processes spawned under Windows must never leak when the harness drops `SupervisedChild` or terminates the command.
- **Purity of the Capability Seam (ADR-0293)**: Windows implementation must fit behind `SupervisedChild` and `InputSupervision`. `muta-agent` must remain 100% platform-agnostic.
- **No Console Host Windows**: Execution must be entirely headless; no visible conhost or terminal windows may pop up during execution.
- **Accurate Quiescence Detection**: Compilations, network downloads, and non-interactive scripts on Windows must not be falsely flagged as waiting for input.

---

## Considered Options

- **Option 1: Fallback via Windows Stdio Redirection with `winpty` or external wrapper binaries.**
  Bundle third-party binaries (like `winpty.exe`) to bridge Windows console programs.
- **Option 2: Native Windows ConPTY Virtualization + Win32 Job Object Tree + NT Wait Telemetry.**
  Implement native ConPTY (`CreatePseudoConsole`) with `STARTUPINFOEXW` attribute lists, wrap the tree in a Win32 Job Object, and detect input waits via Job Object CPU accounting and pipe state.
- **Option 3: Emulated Stdin via Anonymous Pipes Only (No ConPTY).**
  Pass regular anonymous pipes to `hStdInput`.

---

## Decision Outcome

Chosen option: **Option 2 (Native Windows ConPTY + Job Object + Telemetry)**.

Option 1 introduces external binary distribution burdens, security audit issues, and version incompatibilities. Option 3 fundamentally fails on Windows console applications (like PowerShell, `cmd.exe`, `ssh`, or password prompts) that open `CONIN$` directly or call `ReadConsoleW`, failing immediately on non-console pipe handles. Option 2 provides a native, modern, and in-tree architectural solution.

### 1. PseudoConsole (ConPTY) Infrastructure

Windows 10 (build 17763 / version 1809) and later natively support PseudoConsoles via `kernel32.dll`.

```text
┌─────────────────────────────────────────────────────────────┐
│                       SupervisedChild                       │
│                                                             │
│   ┌────────────────┐                  ┌─────────────────┐   │
│   │ Stdin Pipe     │───(write)───►    │ PseudoConsole   │   │
│   │ (Parent Write) │                  │ (HPCON)         │   │
│   └────────────────┘                  └────────┬────────┘   │
│                                                │            │
│                                                ▼            │
│   ┌─────────────────────────────────────────────────────┐   │
│   │ Child Process (STARTUPINFOEXW)                      │   │
│   │ PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE                 │   │
│   │ Standard Handles Virtualized to ConPTY              │   │
│   └─────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘
```

1. **Pipe Setup**:
   - `CreatePipe(&hPipeInRead, &hPipeInWrite, &sec_attrs, 0)`: Stdin channel.
   - `CreatePipe(&hPipeOutRead, &hPipeOutWrite, &sec_attrs, 0)`: Stdout/Terminal channel.
2. **PseudoConsole Creation**:
   ```c
   COORD size = { .X = 80, .Y = 24 };
   HPCON hpcon = NULL;
   HRESULT hr = CreatePseudoConsole(size, hPipeInRead, hPipeOutWrite, 0, &hpcon);
   ```
3. **Process Spawning via `CreateProcessW`**:
   - Allocate `STARTUPINFOEXW`.
   - Initialize attribute list via `InitializeProcThreadAttributeList`.
   - Update attribute `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE` with `hpcon`.
   - Spawn with flags `EXTENDED_STARTUPINFO_PRESENT | CREATE_NO_WINDOW | CREATE_SUSPENDED`.

### 2. Leak-Proof Containment via Win32 Job Objects

To provide the exact containment guarantees of Unix `killpg(pgid)` and `PR_SET_PDEATHSIG`:
1. Create a private anonymous Job Object:
   ```c
   HANDLE hJob = CreateJobObjectW(NULL, NULL);
   ```
2. Configure cascading kill on close:
   ```c
   JOBOBJECT_EXTENDED_LIMIT_INFORMATION info = { 0 };
   info.BasicLimitInformation.LimitFlags = 
       JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | 
       JOB_OBJECT_LIMIT_BREAKAWAY_OK;
   SetInformationJobObject(hJob, JobObjectExtendedLimitInformation, &info, sizeof(info));
   ```
3. Assign child process before execution begins:
   ```c
   AssignProcessToJobObject(hJob, pi.hProcess);
   ResumeThread(pi.hThread);
   ```
4. `OwnedProcessTree::terminate` and `Drop`:
   - Calling `TerminateJobObject(hJob, 1)` or closing the `hJob` handle instantly and unconditionally terminates all descendant processes in the tree, even if child processes spawned detached workers.

### 3. Windows Interactive Wait Detection Mechanism

Because Windows lacks `/proc/<pid>/wchan`, `GroupState` is derived from an unprivileged 3-factor evidence model:

```text
┌────────────────────────────────────────────────────────────────────────┐
│ 1. Job Object CPU Stagnation (Aggregate Tree CPU Delta == 0)           │
│    QueryInformationJobObject(hJob, JobObjectBasicAccountingInformation)│
│    Δ(TotalUserTime + TotalKernelTime) == 0                             │
└───────────────────────────────────┬────────────────────────────────────┘
                                    │ AND
┌───────────────────────────────────▼────────────────────────────────────┐
│ 2. Pipe Quiescence (Parent input buffer empty)                         │
│    PeekNamedPipe(hPipeInWrite, ...) == 0 pending bytes                 │
└───────────────────────────────────┬────────────────────────────────────┘
                                    │ AND
┌───────────────────────────────────▼────────────────────────────────────┐
│ 3. Output Stream Silence                                               │
│    Stdout/Stderr stream quiet for >= examiner floor duration           │
└───────────────────────────────────┬────────────────────────────────────┘
                                    │ AND
┌───────────────────────────────────▼────────────────────────────────────┐
│ 4. NT Thread Wait State Interrogation                                  │
│    NtQueryInformationThread / GetExitCodeProcess live check            │
│    Thread in Wait:Executive or Wait:UserRequest                        │
└────────────────────────────────────────────────────────────────────────┘
```

When all conditions are met across **two consecutive polling cycles** without CPU advancement, the examiner returns `InputWait::Awaiting`.

### 4. Answering and Buffer Flush

When `SupervisedChild::answer(text)` is invoked:
1. The text is written to `hPipeInWrite` followed by `\r\n`.
2. `FlushFileBuffers(hPipeInWrite)` guarantees byte delivery into the ConPTY engine.
3. The examiner state and stability hit counter are reset.

---

## Invariants & Behavioral Boundaries

- **`[INV-WIN-01]` Zero Orphan Guarantee**: Every Windows supervised process MUST be enrolled in an owned Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` prior to resuming the main thread.
- **`[INV-WIN-02]` Modern OS Floor**: ConPTY requires Windows 10 Build 1809 (10.0.17763) or newer. On legacy Windows builds (pre-1809 or Windows Server 2016), `input_supervision()` MUST return `InputSupervision::Unsupported`.
- **`[INV-WIN-03]` No Window Flashing**: Spawning must strictly specify `CREATE_NO_WINDOW`, preventing desktop flashes in interactive user environments.

---

## Positive Consequences

- **Full Windows Supervised Parity**: Windows achieves first-class supervised command execution, enabling interactive tools (e.g., PowerShell credential prompts, CLI confirmation dialogs) to be handled seamlessly.
- **Robust Tree Termination**: Eliminates leaky background child processes on Windows via OS-enforced Job Object boundaries.
- **Single Cross-Platform Interface**: Zero drift in `muta-agent`; `SupervisedChild` on Windows conforms to the exact same Rust interface as Linux and Darwin.

## Negative Consequences & Trade-offs

- **Win32 Platform Code Footprint**: Adds `windows-sys` FFI bindings for ConPTY and Job Objects in `crates/muta-platform`. Mitigated by gating strictly under `#[cfg(windows)]`.
- **ANSI/VT Output Virtualization**: ConPTY introduces terminal VT sequences into the output stream for console apps. Handled by existing stream drain clean-up.

---

## Rejected Alternatives & Negative Knowledge

### 1. Polling Console Window Titles or WinEvents (Rejected)
- *Why considered:* Some Windows automations watch for console window title changes.
- *Why rejected:* Flaky, race-prone, requires an active desktop window, and fails under headless services or remote SSH sessions.

### 2. Standard Anonymous Pipes without ConPTY (Rejected)
- *Why considered:* Avoids Windows 10 1809+ ConPTY requirement.
- *Why rejected:* Console applications calling `ReadConsoleW` fail immediately with `ERROR_INVALID_HANDLE` (code 6) when stdin is a pipe rather than a console handle.

### 3. Calling `taskkill /F /T /PID` on Teardown (Rejected)
- *Why considered:* Common shell workaround for killing process trees on Windows.
- *Why rejected:* Spawns external processes, has severe race conditions if PIDs are recycled, and misses grandchild processes that broke away from the console group. Job Objects are the only kernel-guaranteed mechanism.

---

## Links

- [ADR-0293: Single-Owner Supervised-Input Capability Seam](0293-single-owner-supervised-input-capability-seam.md)
- [ADR-0294: Darwin libproc Kernel-Evidence Supervised-Input Examiner](0294-darwin-libproc-supervised-input-examiner.md)
- [ADR-0292: Command Input-Execution Contract](0292-command-input-execution-contract.md)
