# 0136. Muta core and Mutx terminal app

- **Status:** Accepted
- **Date:** 2026-08-24
- **Supersedes:** ADR-0080, ADR-0102; the embedded-web portions of ADR-0105
- **Revises:** ADR-0098, ADR-0119, ADR-0121

## Context

The single `neenee` binary combined two independent products: a durable
session daemon and an interactive terminal frontend. The web frontend already
lived under `apps/` and consumed the daemon control protocol as a client, but
the TUI remained both a library under `crates/` and the command shell that
hosted the daemon. This made the frontend boundary asymmetric and let daemon
startup depend on re-entering the current executable.

The product also needed one project-wide name. Keeping the old package, state,
environment, protocol, and command names would leave the rename partial and
make new architecture documentation ambiguous.

## Decision

1. Rename the project and all active package namespaces from `neenee` to
   **Muta**. Rust packages use the `muta-*` prefix, persistent paths use
   `muta`, and product environment variables use the `MUTA_*` prefix. This is
   a hard rename with no command or path aliases.
2. Make `muta` the core binary. Its public command surface contains daemon and
   service control only: `daemon`, `session`, `config`, `auth`, `mcp`,
   `skill`, and `doctor`. A bare invocation prints core help. It does not
   launch or distribute either frontend.
3. Make `mutx` the terminal app and place it under `apps/tui`, beside
   `apps/web`. It owns interactive sessions, headless prompt execution,
   attachment, the dashboard, terminal clipboard behavior, TUI rendering,
   and shell completions. `apps/tui` is one app subproject containing the
   related `crates/mutx` executable and its private `crates/mutx-engine`
   rendering crate; neither crate is scattered into the root `crates/` tree.
4. Forbid a dependency from `muta` to `mutx` or `mutx-engine`. Daemon identity
   and service assembly remain in the core; terminal capabilities remain in
   the app.
5. Keep on-demand startup. A local `mutx` operation first discovers the Muta
   daemon. If none is ready, it starts `muta daemon start --fg`, resolving
   `muta` from an explicit `MUTA_BIN`, then beside `mutx`, then through
   `PATH`. Compatibility checks compare the running daemon to the resolved
   `muta` image, never to the distinct `mutx` image.
6. Ship `muta` and `mutx` together in release archives and install both.
7. Make composer completion a backend capability. The daemon publishes the
   command catalog on `Welcome` and answers `CompleteInput` with race-tagged
   `InputCompletions` edits. Slash matching, intent steering, aliases, trusted
   project commands, and project/explicit `@path` discovery live in
   `muta-runtime`; TUI and Web translate native cursor offsets only at their
   final presentation boundary. These wire additions are protocol version 2.
8. Build and deploy `apps/web` independently. The daemon embeds no Web bundle
   and has no Web-specific asset crate or `panel` command. Its plain-HTTP
   surface is the application-neutral `/healthz` probe; Web connects over the
   same authenticated WebSocket protocol as any other client. The generic
   `muta daemon token` command lets an operator retrieve the local TCP bearer
   credential without introducing a Web-specific service API.

## Alternatives considered

- **Rename the command but retain one executable.** Rejected because the TUI
  would still contain daemon implementation and remain architecturally unlike
  the web app.
- **Keep the TUI library under `crates/`.** Rejected because the top-level
  layout would continue to describe it as core infrastructure rather than an
  app.
- **Embed the Web build through a `muta-web-assets` crate.** Rejected because
  it makes a generic daemon depend on one frontend's toolchain and release
  lifecycle. A static Web app can be hosted independently and connect through
  the public control protocol.
- **Let each client derive completion from a shared command list.** Rejected
  because matching, aliases, intent rules, and filesystem discovery would
  still diverge. The backend returns ready-to-apply edits instead.
- **Let `mutx` re-enter itself with a hidden daemon mode.** Rejected because a
  hidden implementation path still collapses the process boundary and makes
  packaging errors hard to detect.
- **Keep the old state and environment names as aliases.** Rejected because
  two names for one instance would make discovery and support diagnostics
  ambiguous. Migration, if later required, must be explicit rather than an
  indefinite alias layer.

## Consequences

- The daemon can run, build, and ship without compiling terminal UI code.
- TUI and web are peer applications over the same daemon protocol.
- Completion behavior is implemented and tested once in the backend; a new
  frontend receives it by implementing the request/result presentation seam.
- The Web app can release and deploy without rebuilding the daemon, while the
  daemon no longer carries frontend assets.
- Development and releases must build two binaries. Installing only `mutx`
  makes on-demand startup fail with an error naming the missing `muta` binary.
- Existing `neenee` state is not read automatically after the hard rename.
- Clipboard export initiated inside the daemon cannot call into TUI code. It
  reports the client-capability boundary until clipboard delivery is carried
  explicitly over the control protocol.

## References

- [Command line reference](../reference/cli.md)
- [Crate layering](../explanation/crate-layering.md)
- [Instance paths](../reference/paths.md)
- [ADR-0098](0098-crate-renames-and-library-extractions.md)
- [ADR-0102](0102-unified-binary-and-runtime-rename.md)
- [ADR-0105](0105-one-port-two-protocols.md)
