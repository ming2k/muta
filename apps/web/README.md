# neenee web panel

Browser client for the neenee session daemon's WebSocket control plane
(`docs/reference/server-api.md`). Svelte 5 + TypeScript + Vite; no routing,
no server — one static bundle that talks to the daemon directly.

## What it does

- **Session fleet sidebar** — attaches as a `Monitor` client (ADR-0093),
  renders every hosted session with live status (`idle` / `running` /
  `needs_approval` / …), current tool, and context-token count. Sessions can
  be deleted (two-click confirm → `DeleteSession`).
- **Session chat** — attaches to a hosted session, replays the transcript
  from `Welcome`, streams assistant output (`StreamDelta` → `StreamEnd`),
  renders tool executions with live stdout/stderr, and sends `Chat` /
  `SlashCommand` / `Interrupt` requests. Slash-command replies render as
  distinct command blocks (`RoundEvent::CommandResult`, ADR-0091).
- **Blocking prompts** — permission approvals (`Once` / `Always` / `Reject`),
  `ask_user` questions, and interactive-command input are answered inline,
  including requests raised by **envoys** (`RoundEvent::Envoy`, ADR-0029):
  the reply's `parent_call_id` is routed back to the parked child agent, so a
  session never hangs silently on an approval wall.
- **Envoy nesting** — a `task` tool card expands into the child agent's live
  profile, activity, streaming text, and nested tool calls.
- **Model switching** — the header shows the active provider/model and opens
  a picker rendered from the `ProviderPicker` snapshot (favorites,
  effort/thinking flags, key readiness); selecting a model sends
  `SetDefaultModel`.
- **Todos and round stats** — `TodosUpdated` renders as a sticky task list
  above the composer; the header tracks round/turn, live activity, context
  tokens, autopilot state, and the last completed round's throughput.
- **Images** — paste or attach images into the composer (base64 `ImagePart`)
  and see them in the transcript; `UnsentInput` restores an interrupted
  prompt (images included) back into the composer.
- **Notices** — provider retries, compactions, review alerts (retained
  banner), command acks, and errors surface as toasts/banners instead of
  vanishing into the console.
- **Resilience** — both channels reconnect with capped exponential backoff;
  the transcript is rebuilt from the `Welcome` replay after a reattach, and
  markdown is sanitized with DOMPurify before reaching `{@html}`.

## Connecting

The daemon serves the panel itself (ADR-0105): the TCP listener splits plain
HTTP from WebSocket on one port, so the whole setup is

```sh
neenee daemon start          # serves the panel on http://127.0.0.1:9800
neenee panel                 # prints the URL, token included
neenee panel open            # ...and launches the browser
```

The daemon requires a bearer token by default (`[daemon] local_auth`); the
`neenee panel` URL carries it as `?token=`, which the panel persists to
localStorage. Browsers cannot set WebSocket headers, so the token travels as
a `bearer.<token>` subprotocol; handshakes from non-loopback browser origins
are refused outright.

Manual configuration (connection dialog — click the Online/Offline badge), in
resolution order:

1. Query params: `?ws=ws://host:port` (or `?host=` + `?port=`), `?project=`,
   `?token=`
2. The persisted dialog settings (localStorage)
3. The `ws://127.0.0.1:9800` default

`project` scopes session creation/monitoring; empty uses the daemon's own
project root. When the requested port is taken the daemon falls back to an
ephemeral port — the discovery file (`$XDG_RUNTIME_DIR/neenee/daemon.json`,
owner-only) always carries the actual port and token.

The client sends its `package.json` version (injected at build time) in the
`Select` handshake; the daemon enforces exact equality with its workspace
version (ADR-0100 rule 4), so a skewed panel is refused with a clear error.
CI asserts the two versions match.

## Development

```sh
pnpm install
pnpm run dev      # vite dev server
pnpm run check    # svelte-check + tsc
pnpm run test     # vitest (store protocol behavior + markdown sanitization)
pnpm run build    # static bundle in dist/
```

Note: `pnpm` commands inside `apps/web` resolve the root
`pnpm-workspace.yaml` and materialize the virtual store at the repository
root; the committed lockfile is the root `pnpm-lock.yaml`.

## Contract coupling

`src/lib/types.ts` is transcribed from `crates/neenee-contracts`
(`events.rs`, `monitor.rs`, `message.rs`, `todos.rs`, `command.rs`) and
`crates/neenee-runtime/src/serve.rs` (`Wire`, `AttachAction`,
`ControlRequest`). Any wire-visible change on the Rust side must update this
file in the same change — the same rule `docs/reference/server-api.md`
§ "Contract maintenance" applies to the AsyncAPI contract and the server
contract tests.
