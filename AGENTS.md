Before writing, modifying, or archiving any documentation, please read and follow `docs/dev/documentation/index.md` in the project root. AI assistants may read `docs/dev/documentation/` and suggest changes, but must not directly modify files in that directory.

## Testing Rules & AI Behavioral Boundaries
- **Runner Tool**: Always use `cargo nextest run` instead of `cargo test` for unit and integration tests. Use `cargo test --doc` only when verifying documentation tests.
- **Tiered Verification (Do Not Over-test)**:
  - During intermediate edits, use `cargo check -p <crate>` for fast type checking, or `cargo nextest run -p <crate> --lib` / `cargo nextest run -E 'test(name)'` for targeted test validation.
  - Run full workspace tests (`cargo nextest run --workspace`) ONLY at the final delivery stage of a task.
- **Async Test Safety**:
  - Never write unbounded `rx.recv().await` on open channels. Always wrap with `tokio::time::timeout` or use non-blocking `try_recv()` to prevent infinite hangs/deadlocks.
  - Always use `#[tokio::test(start_paused = true)]` for tests involving timers, timeouts, or sleep to advance virtual time instantly.
- **Environment & State Isolation**:
  - Never hardcode ports (always bind to `:0` for ephemeral ports).
  - Never touch user home or global state paths; always isolate using `tempfile::tempdir()` and local sandbox roots.

