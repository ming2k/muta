# 0115. Credential placement under XDG: token auth is config, OAuth tokens are state

- **Status:** Accepted
- **Date:** 2026-08-19

## Context

Two on-disk credential stores exist, and both had been informally described as
"sibling secrets files," inviting the question of whether *both* belong in
`$XDG_STATE_HOME` (or neither does):

- `~/.config/neenee/credentials.toml` — API keys the **user supplies** (pasted
  into the TUI or hand-written), keyed by provider instance
  (`[builtins.<id>]` / `[user.<id>] api_key`) since the channel-label map was
  removed.
- `~/.local/state/neenee/auth.toml` — OAuth access/refresh token sets,
  **program-generated**, rewritten by the daemon on every refresh, keyed by
  provider instance (`[tokens.<provider>]`).

The XDG Base Directory Specification v0.8 defines `$XDG_STATE_HOME` as data
that "should persist between (application) restarts, but that is **not
important or portable enough to the user** that it should be stored in
`$XDG_DATA_HOME`", enumerating action history and reusable application state
(view, layout, undo history). Nothing in the spec places user-*supplied*
secrets anywhere, because secrets are not state — they are configuration the
user chose.

## Decision

The two files are classified by their **provenance and churn**, not by the
fact that both happen to be secrets:

| Store | XDG category | Why |
|---|---|---|
| `credentials.toml` (API keys) | **Config** | User-supplied, long-lived, portable (a new machine wants them), and manually editable by design — the load path (`env > credentials.toml > config inline`) treats the file as a user configuration input. Every dimension contradicts State's "not important or portable enough" test. |
| `auth.toml` (OAuth token sets) | **State** | Program-generated, high-churn (rewritten on each refresh), and recoverable by re-login rather than by user re-collection — behaviorally identical to the histories it sits beside. |

Security is **not** a factor in this split: the spec grants `$XDG_STATE_HOME`
no extra protection over `$XDG_CONFIG_HOME`. The security properties that
matter — owner-only file mode 0600, private temp files during atomic writes —
are provided by `fsutil::create_private_file` for both files equally. Moving
either file to State would buy nothing and hide a user-editable input inside a
program-owned directory.

Any future "export/backup credentials" feature must include `auth.toml`
alongside `credentials.toml` despite their different categories: the refresh
token is the durable secret of an OAuth login, and losing only that half
forces re-authentication for every OAuth provider.

## Alternatives considered

- **Both files in `XDG_STATE_HOME`.** Rejected: contradicts the spec's own
  importance/portability test for user-supplied keys, and diverges from the
  established practice of cargo (`~/.cargo/credentials.toml`), gh
  (`~/.config/gh/hosts.yml`), gcloud, aws, npm, kubectl, and docker — none of
  which place user credentials under `~/.local/state`.
- **Both files in `XDG_CONFIG_HOME`.** Rejected for `auth.toml`: it is
  daemon-rewritten on every token refresh; a config directory that mutates
  several times per hour misleads both users and backup tooling about what is
  hand-edited.
- **A secret service / keyring as the only store.** Not rejected on merit but
  out of scope: it changes the operational model (headless hosts, containers)
  and does not remove the need for a file fallback. The XDG placement decision
  is orthogonal and stands either way.

## Consequences

- `paths::credentials_file()` stays on `config_dir`; `paths::auth_file()` on
  `state_dir` — both already correct, now with a recorded rationale instead of
  an accident.
- `docs/reference/paths.md` and `docs/explanation/persistence.md` document the
  split with the spec-based reasoning, so the next "shouldn't this be in
  state?" question has an answer to point at.
- Migration note for a hypothetical future move of `auth.toml`: the legacy
  `config_dir` fallback read (`legacy_auth_file`) already exists and would be
  retired in the same change.

## References

- [XDG Base Directory Specification v0.8](https://specifications.freedesktop.org/basedir-spec/latest/)
- [ADR-0014](0014-xdg-persistence-architecture.md) — the four-category model
  this decision interprets
- [ADR-0072](0072-type-level-secret-redaction.md) — 0600 on-disk secrets
- [ADR-0077](0077-rename-neenee-auth-to-neenee-oauth.md) — the
  `credentials.toml` / `auth.toml` ownership split
