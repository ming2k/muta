# 0077. Rename `neenee-auth` → `neenee-oauth`

- **Status:** Accepted
- **Date:** 2026-07-23
- **Companion to:** [ADR-0076](0076-rename-session-and-store-crates.md) (the
  session/store rename) — this ADR renames the last crate whose name overstates
  its scope, continuing the vocabulary cleanup from ADR-0073 / 0075 / 0076.

## Context

After ADR-0073 flattened the workspace and ADR-0075 / 0076 renamed the
application and session/store crates, one crate still carried a name that
describes more than what it contains. Like ADR-0076 this is a vocabulary
problem, not an architecture problem — the strict-DAG topology from ADR-0005 is
correct and unchanged, and no responsibility or dependency edge moves.

### `neenee-auth` — a name that covers a concept the crate does not own

The crate does exactly one thing: **OAuth2 credential acquisition**. Its own
`lib.rs:1` opens "OAuth2 + PKCE authentication for providers that need it"; the
facade struct is `OAuth`; the modules are `pkce`, `device`, `chatgpt_device`,
`browser`, `token`, `store`. Every line of it — PKCE S256, the RFC 8628
device-code grant, the ChatGPT JSON device variant, the browser loopback
callback server, single-flight refresh, the on-disk `auth.toml` store — is
OAuth-specific machinery.

API-key authentication is **not** here. It is config resolution
(`api_key_env` → `credentials.toml` → inline) in `neenee-persistence` /
`neenee-core`, and the crate says so explicitly in its own `lib.rs:3-5`. So
`neenee-auth` overstates its scope: the name implies "all authentication", but
the crate is only the OAuth variant.

Meanwhile the word "auth" is already load-bearing elsewhere as a **concept**:
`neenee-core::ChannelAuth` is the discriminator enum
(`ApiKey | XaiOAuth | ChatGptOAuth | CopilotOAuth`) that decides *which*
authentication scheme a channel uses, and `neenee-persistence::config` carries
the `auth` TOML field. A crate named "auth" collides with that concept — a
reader reaching for `neenee-auth` expecting to find *how channels declare their
auth scheme* finds none of it; that lives in core.

### The naming criterion this crate already meets

ADR-0074 recorded the project's crate-naming test: a crate earns its name from
its **headline job / artifact** (it rejected `neenee-protocol` as underselling
the transport substrate). By that test the headline job here is unambiguous —
the facade is literally `OAuth` — so the exact name is `neenee-oauth`, nothing
broader.

The rename also makes the provider-adjacent pair read accurately:

```text
neenee-oauth  (obtains the credential)  ──►  catalog  ──►  neenee-llm-client  (spends it)
```

## Decision

Rename one crate, pure rename — no responsibility, dependency, or topology
change:

| Old | New | Rationale |
|-----|-----|-----------|
| `neenee-auth` (crate) | **`neenee-oauth`** | Matches the crate's own self-description ("OAuth2 + PKCE authentication"), its facade (`OAuth`), and ADR-0074's "name the job" test. Frees "auth" to mean only the `ChannelAuth` concept in `neenee-core`. |
| `crates/neenee-auth/` (dir) | **`crates/neenee-oauth/`** | Directory matches package name. `git mv` preserves history. |

Internal type names that carry "Auth" (`AuthStore`, `AuthError`) are **not**
renamed by this ADR. Their shapes are not OAuth-specific: `AuthStore` is a
provider-keyed `auth.toml` map that could hold any token set, and `AuthError`
covers transport / decode / device-code / timeout failures generally. They are
referenced widely and, once qualified by `neenee_oauth::AuthStore`, no longer
claim the whole concept — a crate named "auth" does, a type named `AuthStore`
inside a crate named "oauth" does not. Renaming them is a separate, larger
decision and is not required to fix the crate-level overstatement this ADR
targets.

Nothing else about the topology changes. The strict-DAG property from ADR-0005,
the crate's responsibilities, and every dependency edge are unchanged. The two
consumers (`neenee-transport`, which runs the login flow, and `neenee-agent`,
which reads tokens from the catalog) keep their edges; `neenee-llm-client`
still does not depend on it (the resolved bearer is handed in, never pulled).

## Alternatives considered

- **Keep `neenee-auth`.** Rejected. The name describes more than the crate
  contains (it omits API-key auth, which lives in persistence/core) and
  collides with the `ChannelAuth` concept in core. ADR-0074's "name the job"
  test points unambiguously at `oauth`.

- **`neenee-credentials`.** Rejected. Too broad: it would imply this crate owns
  the `credentials.toml` (API-key) path too, which it does not — that lives in
  `neenee-persistence`. "credentials" also collides with the existing
  `Credentials` config type. The crate is OAuth-specific, and `oauth` says so.

- **`neenee-identity` / `neenee-login`.** Rejected. "identity" is fuzzier than
  OAuth and is not a word the rest of the workspace uses. "login" undersells
  the crate: it also owns proactive refresh, single-flight, and the durable
  token store, not just the initial login. `oauth` is the one word that covers
  the whole PKCE + device + refresh + store lifecycle.

- **Merge into `neenee-llm-client`.** Rejected. The two crates share no
  internal substrate, have independent consumer sets (`neenee-llm-client` is
  consumed by `neenee-providers` + `neenee-agent`; this crate by
  `neenee-transport` + `neenee-agent`), and sit on opposite sides of the
  credential boundary — one mints a bearer, the other spends it. ADR-0074's
  framework (merge only when single-consumer + shared-substrate + lockstep)
  keeps them apart; `neenee-llm-client` does not even depend on this crate
  today. Merging would couple two currently-disjoint jobs.

## Consequences

- **Positive.** The crate name now matches its facade (`OAuth`) and its own
  `lib.rs` self-description. A reader no longer expects to find API-key auth
  or the channel-auth discriminator inside it.

- **Positive.** The word "auth" now has one clear owner across the workspace:
  the `ChannelAuth` concept in `neenee-core`. The provider-adjacent pair reads
  accurately: `neenee-oauth` obtains the credential, `neenee-llm-client`
  spends it.

- **Negative (one-time, breaking).** Every `neenee-auth` path dependency and
  every `use neenee_auth::` reference must update to the new name. This is a
  workspace-internal rename; the crates are not published, so the blast radius
  is this repository plus any out-of-tree embedding. Recorded under
  `[Unreleased]` → `Changed` in `CHANGELOG.md`.

- **Neutral.** `Cargo.lock` is reconciled by `cargo build`. The internal
  `Auth*` type names (`AuthStore`, `AuthError`) and `TokenSet` are unchanged,
  so the rename is mechanical at the crate boundary.

## Migration mechanics

| What | Files | Notes |
|------|-------|-------|
| `git mv` directory | `crates/neenee-auth/` → `crates/neenee-oauth/` | history preserved |
| package name | `crates/neenee-oauth/Cargo.toml` | `name = "…"` |
| path dependencies | consuming `Cargo.toml`s (`neenee-transport`, `neenee-agent`) | `neenee-auth` key + `path = "…"` |
| `use` + path references | `.rs` files (`neenee-transport`, `neenee-agent`, doc comments in `neenee-core`, `neenee-persistence`) | `neenee_auth` → `neenee_oauth` |
| lockfile | `Cargo.lock` | package name; reconciled by `cargo build` |
| doc comments | across crates | prose mentions of `neenee_auth` |
| living docs | `docs/dev/`, `docs/how-to/`, `docs/reference/` (excl. `docs/dev/documentation/` policy) | mechanical rename + prose fixes |
| glossary | `docs/reference/glossary.md` | new `neenee-oauth` term; legacy-terms row for `neenee-auth` |
| ADR index | `docs/adr/index.md` | new row only |

ADR decision bodies (0052, 0072, 0076) still contain `neenee-auth` references.
Per ADR workflow they are immutable historical records and are left unchanged;
this ADR and the glossary carry the current truth.

## References

- [ADR-0052](0052-xai-supergrok-provider.md) — created this crate as the OAuth
  acquisition layer; stated it "is reusable for any future OAuth-only provider".
- [ADR-0074](0074-consolidate-llm-client-crate.md) — the "name the job" test
  this rename applies, and the framework that keeps this crate separate from
  `neenee-llm-client`.
- [ADR-0076](0076-rename-session-and-store-crates.md) — the rename precedent
  (vocabulary cleanup, not architecture) this ADR follows.
- [Crate layering](../explanation/crate-layering.md)
- [Workspace layout](../dev/workspace-layout.md)
