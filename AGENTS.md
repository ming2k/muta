Before writing, modifying, or archiving any documentation, please read and follow `docs/dev/documentation/index.md` in the project root. AI assistants may read `docs/dev/documentation/` and suggest changes, but must not directly modify files in that directory.

## Testing Rules & AI Behavioral Boundaries
- **Runner Tool**: Always use `cargo nextest run` instead of `cargo test` for unit and integration tests.
- **No Pre-test Baseline on Trivial Changes**: For obvious, deterministic edits (config adjustments, constants, localized fixes), DO NOT run test suites before editing. Edit directly.
- **Fast Feedback First**: When verifying code changes:
  1. Prefer `cargo check` (or package-level check) for instant syntax/type validation over full compilation.
  2. For tests, ALWAYS use targeted filters (e.g. `cargo nextest run -p <package> -E 'test(<filter>)'`) instead of running full package or workspace suites.
- **Latency Consciousness**: Prioritize developer waiting time and iterative speed. Avoid triggering redundant, long-running compilation or test tasks.

