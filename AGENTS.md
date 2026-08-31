Before writing, modifying, or archiving any documentation, please read and follow `docs/governance/documentation/core/index.md` in the project root. AI assistants may read `docs/governance/documentation/` and suggest changes, but must not directly modify files in that directory.

## Testing Rules & AI Behavioral Boundaries
- **Runner Tool**: Always use `cargo nextest run` instead of `cargo test` for unit and integration tests.
- **No Pre-test Baseline on Trivial Changes**: For obvious, deterministic edits (config adjustments, constants, localized fixes), DO NOT run test suites before editing. Edit directly.
- **Fast Feedback First**: When verifying code changes:
  1. Prefer `cargo check` (or package-level check) for instant syntax/type validation over full compilation.
  2. For tests, ALWAYS use targeted filters (e.g. `cargo nextest run -p <package> -E 'test(<filter>)'`) instead of running full package or workspace suites.
- **Latency Consciousness**: Prioritize developer waiting time and iterative speed. Avoid triggering redundant, long-running compilation or test tasks.

## Non-interactive Git Discipline

The AI shell has no TTY. Any git command that opens an interactive editor or pager will hang until the command timeout — never let that happen.

- **Annotated tags**: ALWAYS inline the message: `git tag -a vX.Y.Z -m "..."`. Never run `git tag -a` without `-m`.
- **Commits/merges**: use `git commit -m "..."` and `git merge --no-edit`. Avoid `git rebase -i`; prefer non-interactive equivalents (or set `GIT_SEQUENCE_EDITOR=:` when unavoidable).
- **Paged output**: prefer `git --no-pager log|diff|show ...`, piped through `head` when long.
- **If a command appears to wait for editor/pager input**: kill it immediately and retry with an inline message or `--no-edit` instead of waiting.

