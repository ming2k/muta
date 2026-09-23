# Agent Directives & Documentation Governance

Before writing, modifying, or archiving any documentation, follow Protocol v6.0.0 (`docs/governance/documentation/core/index.md`).

## 1. Documentation Governance & Invariant Directives
- **`[INV-LINT-01]` Location Sanitization**: Never create unapproved Markdown files at the repository root; permit only whitelisted root entries (`README.md`, `README.zh-CN.md`, `CHANGELOG.md`, `AGENTS.md`, `LICENSE.md`, etc.).
- **`[INV-LINT-02]` Contributor Firewall**: Public documentation (`docs/{tutorials,how-to,reference,explanation}/`) must NEVER link into internal documentation (`docs/dev/**`).
- **`[INV-LINT-03]` Frontmatter & Lifecycle Integrity**: ADRs must declare standardized frontmatter (`id`, `title`, `status`, `date`) and evolve in-place.
- **`[INV-AGENT-01]` Negative Knowledge Mandate**: Every ADR must detail rejected alternatives and why they failed.
- **`[INV-AGENT-02]` Context Routing & Chesterton's Fence**:
  - In feature generation: never recommend patterns marked `status: superseded` or `status: rejected`.
  - In refactoring/investigation: retrieve `superseded`/`rejected` records as negative constraints.
- **`[INV-AGENT-03]` Blameless Postmortems**: Postmortems in `docs/dev/postmortems/` must focus strictly on systemic defense failures and detection gaps; human blame is prohibited.
- **Verification Tool**: Run `docgov check` to verify repository governance compliance.

## Testing Rules & AI Behavioral Boundaries
- **Runner Tool**: Always use `cargo nextest run` instead of `cargo test` for unit and integration tests.
- **No Pre-test Baseline on Trivial Changes**: For obvious, deterministic edits (config adjustments, constants, localized fixes), DO NOT run test suites before editing. Edit directly.
- **Fast Feedback First**: When verifying code changes:
  1. Prefer `cargo check` (or package-level check) for instant syntax/type validation over full compilation.
  2. For tests, ALWAYS use targeted filters (e.g. `cargo nextest run -p <package> -E 'test(<filter>)'`) instead of running full package or workspace suites.
- **Latency Consciousness**: Prioritize developer waiting time and iterative speed. Avoid triggering redundant, long-running compilation or test tasks.
- **Finite Execution Only (ADR-0263)**: All shell commands executed via `run_command` MUST be finite and self-terminating. Do not attempt to run background daemons or dev servers (`background` and `service` modes have been completely eradicated). Long-running services belong outside the agent loop in the user's terminal; ephemeral test servers must be run as self-terminating composite subshell scripts (e.g. `(server & PID=$!; test; kill $PID)`). Any command that streams continuously or goes silent without terminating is killed early by StreamGuard (ADR-0257/0263).
- **Finite Foreground Execution (ADR-0257)**: The AI shell has no TTY. Foreground commands MUST be finite and self-terminating. NEVER run unbounded streaming or monitoring tools (`top`, `intel_gpu_top`, `tail -f`, `ping`, `watch`) directly without explicit bounds (`timeout 2s <cmd>`, `| head -n 30`, `top -b -n 1`, `ping -c 3`). Continuous unbounded streaming in the foreground triggers StreamGuard early cutoff.

## Non-interactive Git Discipline

The AI shell has no TTY. Any git command that opens an interactive editor or pager will hang until the command timeout — never let that happen.

- **Annotated tags**: ALWAYS inline the message: `git tag -a vX.Y.Z -m "..."`. Never run `git tag -a` without `-m`.
- **Commits/merges**: use `git commit -m "..."` and `git merge --no-edit`. Avoid `git rebase -i`; prefer non-interactive equivalents (or set `GIT_SEQUENCE_EDITOR=:` when unavoidable).
- **Paged output**: prefer `git --no-pager log|diff|show ...`, piped through `head` when long.
- **If a command appears to wait for editor/pager input**: kill it immediately and retry with an inline message or `--no-edit` instead of waiting.

