# Workflow patterns

How developers, teams, and automation pipelines interact with muta across
different operational modes.

## Overview

muta is organized around a single unified binary that supports multiple
interaction models. Rather than enforcing a rigid pairing loop, the system
adapts to five distinct workflow patterns depending on whether the task
requires direct human guidance, background execution, subagent delegation,
external tool expansion, or automated scheduling.

```text
┌─────────────────────────────────────────────────────────────┐
│                    Workflow Archetypes                      │
├──────────────────────────────┬──────────────────────────────┤
│ 1. Interactive Pairing Loop  │ Direct conversational coding │
│ 2. Multi-Session Daemon      │ Detached background tasks    │
│ 3. Subagent & WIP Consensus  │ Delegated multi-agent work   │
│ 4. Ecosystem & Skills        │ MCP and domain extensions    │
│ 5. Headless Automation       │ Cron scheduling & CI monitor │
└──────────────────────────────┴──────────────────────────────┘
```

## Interactive pairing loop

The primary interactive workflow operates in a fullscreen terminal user
interface (TUI). When a developer launches `muta` in a workspace, the client
transparently discovers or spawns a background session daemon, binds to a
local Unix domain socket, and opens the session transcript.

In this workflow, the developer and the agent work in an iterative loop:

1. **Prompt & Context Injection**: The user describes a goal (e.g. bug fix,
   feature implementation, or test suite addition).
2. **Tool Execution & Safety Gating**: The agent reads project files,
   analyzes dependencies, executes test commands, and proposes code edits.
   Workspace authority and the physical sandbox decide what may run.
   Attended mode can request a missing grant; `/autopilot on` makes the same
   decision non-interactive and fails immediately when authority is missing.
3. **Isolated Side Inquiries**: When the user has a tangent question that
   would otherwise pollute the main conversation context, the `/btw` command
   spawns an isolated side inquiry, answers it, and cleanly returns to the
   active task.

## Multi-session daemon management

For long-running tasks such as full-codebase refactorings or comprehensive
build validations, the multi-session daemon allows the user to detach and
re-attach without interrupting active work.

The daemon process hosts all active sessions across different projects under
one runtime. The developer can:

- **Detach cleanly**: Closing the terminal or disconnecting leaves the
  session driving its execution pipeline safely in the background.
- **Supervise via Dashboard**: The full-screen `/dashboard` view gives the
  developer a real-time monitor console over all running sessions across
  every repository, with instant keyboard switching and prompt injection.
- **Resume anytime**: Running `mutx attach <session-id>` or selecting the
  session in the dashboard reconnects the interactive TUI directly to the
  persisted event stream.

## Delegated subagent execution and work coordination

Complex engineering tasks often benefit from dividing responsibilities
between a primary orchestrator and specialized subagents.

muta supports two complementary delegation patterns:

- **Research Delegation (`envoy`)**: The principal agent delegates broad
  exploration, documentation indexing, or log analysis to a read-only child
  envoy. The child operates in a separate context window and returns a
  synthesized summary without bloating the primary turn context.
- **Implementation Delegation (`envoy_code`)**: The principal delegates
  concrete coding and testing tasks to an autonomous subagent.

Note: the WIP-coordination tools (`declare_wip`/`check_wip`/`wip_done`) that
once lived here as session-facing tools were removed; workspace-exclusivity is
now enforced structurally (one session owns a workspace, peers coordinate
through the orchestrator console) rather than through voluntary declarations.

## Extensible capability integration

When an engineering task requires capabilities outside built-in filesystem
and shell operations, muta integrates external tools through two
extension surfaces:

- **Model Context Protocol (MCP)**: Local stdio MCP servers expose external
  databases, browser automation drivers (e.g. Playwright), or issue trackers
  directly as callable tools. The daemon manages server process lifecycles
  and dynamic tool discovery transparently.
- **Domain Skills**: Markdown and instruction bundles placed in project or
  user skill directories supply domain-specific guidelines, testing rules,
  and specialized prompt fragments on demand.

## Headless automation and observability

In addition to interactive sessions, muta serves automated workflows in
CI/CD environments and background developer machines:

- **Scheduled Prompts (`/schedule`)**: Developers can configure recurring
  cron triggers or countdown timers for periodic health checks, dependency
  audits, or release verification reports.
- **Observability Streams (`muta daemon status --json`)**: External dashboards, monitoring
  scripts, and supervisor units observe live session states through
  stream-oriented JSON output or watch tables without interfering with
  running turns.
- **Supervised Services**: The daemon can run under systemd user supervision
  with configurable idle-exit windows, bounded shutdown drains, and graceful
  teardown guarantees.

## Related decisions

- [ADR-0096: Unified session daemon and control plane](../adr/0096-unified-session-daemon.md)
- [ADR-0097: Session addressing and orchestrator console](../adr/0097-session-addressing-and-orchestrator-console.md)
- [ADR-0100: Daemon lifecycle standard](../adr/0100-daemon-lifecycle-standard.md)
- [ADR-0101: Daemon shutdown correctness](../adr/0101-daemon-shutdown-correctness.md)
- [ADR-0102: Unified single-binary architecture and runtime rename](../adr/0102-unified-binary-and-runtime-rename.md)
