# Dev and test isolation

How a checkout runs its own builds and tests without touching the installed
muta — its daemon, its sessions, its config, its credentials, its logs, or
its port. The decision record is
[ADR-0121](../adr/0121-instance-isolation-for-development-and-testing.md);
this page is the workflow.

## Why isolation is required

The installed daemon and a checkout's debug build resolve **the same
paths** by default: the same `daemon.lock` (single-instance `flock`), the
same `daemon.sock`, the same `daemon.json` discovery record, the same
`~/.config/muta` and `~/.local/share/muta`, and the same TCP port 9800.
Without isolation:

- a dev daemon contends for the host daemon's lock, and whichever daemon
  writes its discovery record last routes *every* client — installed or
  dev — to itself;
- when 9800 is taken the daemon silently falls back to an ephemeral port,
  so the two daemons coexist while each client population discovers only
  one of them;
- dev sessions, `/reload` edits, OAuth refreshes, and logs land in the real
  user data — a debugging run can pollute or corrupt it;
- a test that forgets its own tempdir sandbox writes into the real home the
  same way (this has happened; isolated path capabilities and the
  `test-path-override` feature prevent recurrence).

## The mechanism: one selector

`--home <dir>` (the flag) and `MUTA_HOME=<dir>` (the environment
variable) are the same switch — the **instance root**. Both move the entire
footprint:

```text
<dir>/muta/
  config/     config.toml, credentials.toml, themes/
  data/       projects/<bucket>/sessions/, skills/, commands/
  state/      auth.toml, history.json, log/
  cache/      models_discovery.json, skills/remote/
  instance/   daemon.json, daemon.sock, daemon.lock, serve/
```

`MUTA_PORT=<port>` takes a default TCP port that cannot contend with the
host daemon's 9800. Use the flag for one-off invocations and the variable
for a whole process tree (CI steps, `cargo test`, a shell session); the
flag wins when both are present. The flag is sugar over the variable in one
important way: at startup it is restated as `MUTA_HOME` in the process
environment, so every child — the auto-spawned daemon, a detached
`daemon start` — inherits the sandbox without having to re-pass the flag.

## Running the debug build isolated

Build, then point every invocation at one instance root. A stable root
under `/tmp` keeps the dev daemon and its sessions alive between runs:

```bash
cargo build -p mutx
./target/debug/muta --home /tmp/muta-dev daemon start
./target/debug/muta --home /tmp/muta-dev daemon status
./target/debug/muta --home /tmp/muta-dev            # TUI session
```

`cargo run` is equally fine here (ADR-0134): it links the same stable
`target/debug/muta` path the auto-spawned daemon records, and the wire
window serves clients whatever their product build. The one gate that
survives is the **dev-drift** check — after a rebuild, a still-running
daemon of the *same version* is refused with a binary-mismatch error
until you stop it:

```bash
cargo run -q -p mutx -- daemon start
# …edit code, rebuild (cargo build / cargo run)…
cargo run -q -p mutx -- daemon status
# → error: "client/daemon binary mismatch … Stop it with
#   `muta daemon stop` and rerun — the daemon restarts on demand."
cargo run -q -p mutx -- daemon stop   # then rerun; a fresh daemon spawns
```

`daemon stop` itself never hits the gate — it addresses the daemon by pid
from the discovery record, so the fix it names always works.

Export the variable once to drop the flag from every command:

```bash
export MUTA_HOME=/tmp/muta-dev
export MUTA_PORT=9801
./target/debug/muta daemon status --diagnostic   # confirms the instance
./target/debug/mutx attach
```

The diagnostic's first line names the resolved instance root and port, so
"which daemon am I talking to" is a one-command check.

Tear down (stops the dev daemon and wipes the dev root):

```bash
./target/debug/muta --home /tmp/muta-dev daemon stop
rm -rf /tmp/muta-dev
```

## Running the test suites isolated

`cargo test` cannot take a per-invocation `--home`, so the variable carries
the sandbox into every test binary — even one that forgets its own tempdir
then resolves inside the throwaway root instead of `~`:

```bash
export MUTA_HOME=$(mktemp -d /tmp/muta-test.XXXXXX)
cargo test --workspace --locked
./target/debug/muta daemon stop   # if any suite spawned a daemon
rm -rf "$MUTA_HOME"
```

Keep the root for a post-mortem by deferring the last two lines. The
runtime integration suites install the variable themselves before any path
resolves, so they are isolated even without the export; the export is the
belt-and-braces layer for everything else in the tree.

## What CI does

The `test` job sets a process-wide `MUTA_HOME` under `runner.temp`, and
the daemon-smoke e2e job starts its daemon under `MUTA_HOME` +
`MUTA_PORT` — one variable instead of the four `XDG_*` exports it used to
hand-assemble. The same job then runs
[`apps/web/e2e/cli-smoke.sh`](../../apps/web/e2e/cli-smoke.sh), which pins
the CLI's own contract against a live, isolated daemon: retired spellings
are unrecognized commands, the noun-verb shapes parse, `--remote`
rejects a missing port/token, and a real `--remote` connection reaches the
daemon (protocol-level coverage stays in `daemon-smoke.mjs`).

## Running the smokes locally

```bash
cargo build -p mutx
bash apps/web/e2e/cli-smoke.sh          # CLI surface (isolated instance)
DAEMON_URL=http://127.0.0.1:9800 DAEMON_TOKEN=… node apps/web/e2e/daemon-smoke.mjs
```

Both confine themselves to a throwaway instance root; a host daemon is
never touched.

## Invariants to preserve when touching this area

- Daemon runtime paths derive from `Dirs::instance_dir()` only — never
  from `runtime_dir` directly — so every call site observes one override
  stack.
- `client::spawn_daemon` must keep inheriting the environment (no
  `env_clear`), and the CLI's `--home` must keep restating itself as
  `MUTA_HOME` at startup: sandbox inheritance for spawned daemons depends
  on both.
  `lifecycle_integration::spawned_daemon_inherits_the_muta_home_sandbox`
  fails if this regresses.
- Default port selection goes through `startup::env_default_port()`, not
  the raw constant, wherever the "no `--port` given" default is resolved.
- There is deliberately no separate runtime-dir selector: one root, one
  concept (see ADR-0121's alternatives).
