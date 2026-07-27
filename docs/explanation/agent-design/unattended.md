# Unattended operation

`unattended` is the mode that lets an agent run **without human intervention**:
no permission confirmations, no questions — it decides and acts on its own
authority. The point of the flag is autonomy, not speed: an unattended agent is
one the human can walk away from.

This page is about the *design intent* and how that intent maps onto the
mechanisms that actually enforce it. For the operational surfaces — the slash
command, the CLI flag, the toggle's wire behaviour — see
[Slash commands](../../reference/commands.md); for the one-line definition, see
the [Glossary](../../reference/glossary.md#tools-and-capabilities).

## Intent vs. enforcement

The design intent of unattended is broad: a session in which the agent never
stops to wait on a human. There are two distinct surfaces a round can stop on:

| Surface | What stops the round | Who answers |
|---------|----------------------|-------------|
| **Permission broker** | A `Write`/`Execute` tool fires; the broker parks a oneshot and emits `PermissionRequest` | The user, via the once/always/reject modal |
| **User question** | The model calls `ask_user`; the harness parks a oneshot and emits `UserQuestionRequest` | The user, via the question modal |
| **Interactive stdin** | The model emits a command the interactive classifier matches; the harness parks a oneshot and emits `InputRequest` | The user, via the inline input panel |

`unattended` **enforces the whole posture**, not just one gate. With the flag on
every one of these surfaces is reclaimed: the broker auto-approves every
side-effecting tool, `ask_user` is dropped from the advertised toolset (and a
stale call short-circuits with a refusal rather than parking), and an
interactive command's stdin is closed instead of prompting the operator. The
flag is now a guaranteed floor for the "no confirmations, no questions" target
posture, not merely an expression of it.

This convergence is deliberate. A round that can stop on any of the three
surfaces is not truly walk-away-able; suppressing only the broker left two
deadlock paths that a model-driven `ask_user` or an interactive command could
still trip. Reclaiming all three when the flag is on makes the autonomy
contract honest: with `unattended` on, nothing the model does will pause for a
human.

## The broker gate

The only code path the flag actually controls is the permission broker
([Harness architecture → Permission broker](harness.md#permission-broker)).
After the write-scope gate clears a side-effecting tool, the broker branch
decides whether to park:

```text
tool with real ScopeTarget (Path/Command)
  └─ unattended OR always-allow rule matches?
        ├─ yes → skip the prompt, run the tool
        └─ no  → emit PermissionRequest, park oneshot, await decision
```

Two things follow from where this check sits:

- **Reads never consult it.** A tool whose `ScopeTarget` is `Unspecified`
  (`read_file`, `grep`, `glob`, …) bypasses the broker regardless of the flag —
  a read is not a side effect the user must approve. So unattended changes
  nothing for the read-heavy exploration phases of a round; it only removes the
  pauses before *actions*.
- **It composes with the allowlist, it does not replace it.** The condition is
  `unattended || always_allowed`. With unattended off, a cached `Always` rule
  still lets a tool through without prompting; with unattended on, the rule set
  is simply irrelevant. Unattended is the broader dial; `/permissions` is the
  narrow, per-tool one.

Under unattended the write-scope gate is also load-bearing in a second way: an
out-of-scope write cannot be elevated (there is no human to answer the broker's
prompt), so the gate blocks it outright before the broker ever sees it.

## Reclaiming ask_user and interactive stdin

The broker gate covers side-effecting tools, but two more surfaces can also stop
a round on a human. Under unattended both are reclaimed so the "no questions"
half of the posture is enforced, not just expressed:

```text
ask_user
  └─ unattended? → schema dropped from the advertised toolset; a stale call
                   (name carried over from an earlier turn) short-circuits with
                   a refusal instead of parking a oneshot.

interactive command (bash stdin)
  └─ unattended? → stdin closed instead of emitting InputRequest; the command
                   fails fast with a non-interactive remedy.
```

The system prompt is also told the session is unattended — that no human is
reachable, that the question tool is gone, and that the model must decide and
act on its own authority. This pairs the mechanical reclaim (the harness cannot
deadlock) with the behavioral one (the model is steered away from deferring).
See [User questions](user-questions.md) for the interactive counterpart.

The flag is **live and process-local**. It is not persisted to the session, not
carried across `/resume`, and not part of any envoy profile that reloads from
disk. Toggling it mid-round takes effect on the very next broker check.

## What unattended is not

Three neighbouring ideas that are easy to mistake for unattended:

- **The `always` allowlist** is durable and per-rule (`/permissions`), not a
  mode. Unattended is the live, blanket version of the same relaxation — on for
  every side-effecting tool at once, off again with a single toggle. A session
  that wants a permanent "always allow `bash`" uses the allowlist; one that
  wants "don't bother me for the rest of this task" uses unattended.
- **The `WriteScope` boundary** is a *soft* capability limit. A write tool
  outside the agent's scope is routed to the permission broker so an attended
  user can approve the elevation (or reject it); it is blocked outright only
  under unattended, where no human can answer the prompt. See
  [Rounds and turns](rounds-and-turns.md).
- **Headless rejection** is the opposite posture. The headless entry point
  *automatically rejects* write permissions rather than suppressing the prompt.
  Unattended says "act without asking"; headless says "refuse because no one can
  answer." They solve different problems and never coexist in one client.

## Where it is forced on

Three places set unattended automatically rather than at the user's request:

| Site | Why | Reference |
|------|-----|-----------|
| **Envoy children** | A spawned agent has historically had no guaranteed path to surface a reply back down; defaulting to unattended preserves the autonomous contract. Full-duplex (ADR-0029) now wires that path, so an interactive profile can opt out, but the built-in profiles stay `true`. | [Envoys → Full-duplex](envoys.md#full-duplex) |
| **Side conversations** (`/btw`) | A side `Agent` shares the principal's permission channel; a modal it raised could not be routed back to the right child, so the aside runs unattended to stay deadlock-free. | [ADR-0017](../../adr/0017-side-conversations.md) |
| **`--unattended` at startup / `/unattended on`** | The user explicitly elevates the whole session to the no-intervention posture. | [Slash commands](../../reference/commands.md) |

In every case the rationale is the same: the agent is running in a context
where a prompt either *cannot* be answered or the human has declared it
shouldn't be.

## Visibility

Because an unattended session silently executes actions that would otherwise
pause, the harness makes the state impossible to miss without making it loud:

- The status bar shows a flat `unattended` label in the warning tone for the
  whole session — plain text, not a raised pill, because it is a persistent
  state flag rather than a momentary mode. See
  [TUI status bar](../../reference/tui/status-bar.md).
- Toggling emits a `RoundEvent::UnattendedChanged` so the TUI refreshes the badge
  immediately, mid-turn, without flushing the activity bar.

The elevated state is always visible; it never needs to interrupt.

## Safety bounds still apply

Unattended removes the *interactive* backstop, not the *automatic* ones. A round
running unattended is still subject to every execution bound the harness enforces
on its own — the repeated-read guard, the optional `hard_stop_turns` budget,
and the user's `Esc` interrupt. The agent is
freer to act without asking, not freed from the guards that keep an uncapped
autonomous loop honest. See [Harness architecture → Safety bounds](harness.md).

## See also

- [Harness architecture](harness.md) — the permission broker and safety bounds
  unattended sits inside.
- [User questions](user-questions.md) — the `ask_user` surface, the other thing
  a round can stop on, and why it is governed separately.
- [Envoys → Full-duplex](envoys.md#full-duplex) — why envoy children default to
  unattended.
- [Tool access and capability axes](../../reference/tools/access.md) — the
  factual reference for which tools hit the broker.
- [Slash commands](../../reference/commands.md) — `/unattended [on|off]`.
