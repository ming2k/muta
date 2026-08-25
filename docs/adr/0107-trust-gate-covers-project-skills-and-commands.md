# 0107. Trust gate covers project skills and slash commands

- **Status:** Superseded
- **Date:** 2026-08-15
- **Superseded by:** ADR-0140
- **Builds on:** ADR-0085 (config-time tool scoping + trust model), ADR-0096 (unified daemon)

## Context

ADR-0085 §5 gated a project's `.neenee/config.toml` — `[mcp.*]` servers and
`[[hooks]]` — behind a one-time `/trust`, because they execute
project-supplied processes: loading them automatically from a cloned repo is
the npm-`postinstall` hazard. But the gate stopped at config. Two other
project-local capabilities kept loading unconditionally:

- **Project skills** (`.neenee/skills`, `.agents/skills`, `.claude/skills` —
  `SkillScope::Repo`), which outrank every other scope by priority, and
- **project slash commands** (`.neenee/commands/`).

Both are prompt text, not executables — but for an agent that holds tools,
project-supplied prompt text is execution by proxy: a malicious repo can
plant a skill that shadows a same-named user skill (Repo priority is the
highest) and instruct the agent to run `bash`, exfiltrate context, or edit
files. The threat model ADR-0085 already articulates applies verbatim;
the boundary was simply incomplete. ADR-0096's unified daemon makes it
sharper still: scans now run under the daemon's cwd-anchored project root,
not whichever directory a TUI happened to start in, so scan-time gating must
be pinned to the *session's* project root.

## Decision

Extend the ADR-0085 §5 trust decision to every project-supplied capability:

1. **Repo-scope skills and project slash commands load only for trusted
   projects.** The check lives inside the scan path itself
   (`discover_all` / `discover_commands_trusted`), consulted from the
   on-disk trust store at every scan — startup, the hourly skill-catalog
   refresh, `/skills reload`, and `/trust`/`/untrust` all share the one
   gate. Command discovery is anchored to the session's project root (the
   daemon has no meaningful cwd of its own).
2. **`/trust` enables them mid-session**: the skills registry rescans
   immediately, and project commands become runnable through a
   trust-checked dispatcher fallback. `/untrust` drops them at once.
3. **Shadowing is never silent.** When a project skill or command overrides
   a same-named user/system entry by priority, the harness emits a
   one-time-per-name warning notice naming the winner and suggesting
   inspection or `/untrust`. (Surfaced with `NoticeKind::ReviewAlert` —
   the closed enum's only "needs attention" warning kind; a dedicated
   variant is a deliberate non-goal for now.)
4. The untrusted-project startup notice now names every disabled class:
   MCP servers, hooks, project skills, project slash commands.

## Alternatives considered

- **Gate only executables, treat prompt text as benign.** Rejected: with a
  tool-holding agent the distinction is cosmetic — a planted skill *is* a
  confused-deputy exploit.
- **Gate `AGENTS.md`-style auto-injected context too.** Rejected for now:
  every coding agent treats the root context file as trusted-by-open, and
  gating it would break the universal workflow. The boundary is recorded
  here explicitly instead of by omission.
- **Priority flip (user scope wins over project).** Rejected: nearest-to-work
  wins is the useful semantics; the answer to abuse is the trust gate plus
  shadow alerts, not a weaker precedence model.

## Consequences

- Opening an untrusted repo can no longer inject prompt text or commands
  into sessions; the first `/trust` is the single, explicit escalation.
- `neenee skill list` and completion listings hide repo-scope entries for
  untrusted projects (they were never scanned).
- Web/TUI clients surface shadow alerts through the existing notice
  channel without protocol changes.

## References

- ADR-0085 (trust model), ADR-0096 (unified daemon; daemon-cwd scanning).
- npm `postinstall` / git-hook auto-execution as the analogous hazard class.
