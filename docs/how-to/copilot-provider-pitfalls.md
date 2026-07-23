# How to avoid GitHub Copilot provider pitfalls

The `copilot-oauth` provider logs in through GitHub's OAuth device flow and then
reads the live model list from `api.githubcopilot.com/models`. Because GitHub's
Copilot backend behaves differently depending on the OAuth client, token type,
and account plan, the model picker can end up missing models you expect. This
guide explains the common causes and how to verify them.

## OAuth App client id controls the model allowlist

GitHub maintains a **per-client-id model allowlist** on the Copilot backend.
Different OAuth Apps see different `/models` responses even when the same
GitHub account has a Copilot Pro/Plus subscription.

neenee uses the public Copilot OAuth App client id (`Ov23li8tweQw6odWQebz`)
that opencode and several community integrations also use. If you replace it
with your own GitHub OAuth App, the backend will usually return only the
always-available GPT-4o family, not the account's full subscription catalog.

**Check:** the client id is compiled into `crates/platform/neenee-auth/src/config.rs`.
If you fork neenee and change `COPILOT.client_id`, you are also changing the
model set GitHub exposes.

## Token type matters

The device flow can produce two token prefixes, and they are not
interchangeable:

| Prefix | Source | How neenee uses it |
|--------|--------|--------------------|
| `gho_` | GitHub OAuth App token (the default for neenee's built-in client id) | Sent directly as `Authorization: Bearer <token>`. Works for Individual Copilot subscribers and the GA model catalog. |
| `ghu_` | GitHub App user token (for example from VS Code's Copilot extension client id `Iv1.b507a08c87ecfe98`) | Must be exchanged through `GET /copilot_internal/v2/token` for a short-lived bearer before calling the Copilot API. neenee currently does not perform this exchange, so `ghu_` tokens will not work correctly. |

If you paste a token obtained from VS Code or the GitHub CLI into neenee, it
will probably fail or show only a handful of models.

## Copilot Business / Enterprise accounts

Business and Enterprise plans are routed to dedicated Copilot API endpoints such
as `api.business.githubcopilot.com`. The exact endpoint is returned by the
`/copilot_internal/v2/token` exchange. neenee currently hardcodes
`api.githubcopilot.com`, so Business/Enterprise users may see an incomplete
model list or 404 errors even with a valid token.

The fix is to implement the token exchange and use the `endpoints.api` value
from the exchange response. Until then, the built-in `copilot-oauth` template is
best suited for Individual subscribers.

## `model_picker_enabled` filters the picker

The `/models` endpoint lists more than just user-selectable chat models. It also
includes internal utility models, embeddings, and preview entries. neenee keeps
only entries where `model_picker_enabled` is `true`. This is intentional: the
remaining entries are the models GitHub considers usable for interactive chat
under the current account.

If a model id appears in a raw `/models` response but not in neenee's picker,
check the `model_picker_enabled` field first.

## When discovery fails

After login, neenee fetches the live `/models` list automatically. If that
fetch fails (network error, an expired token, a Copilot backend hiccup), the
previous model subset is kept and a **warning notice** appears in the
transcript explaining which provider failed and why. A silently seed-only list
(gpt-4o-mini alone) usually means one of:

- the fetch failed — re-read the warning notice, then re-login;
- the fetch succeeded but the client id / token type only unlocks the base
  GPT-4o family (see the sections above).

## Troubleshooting checklist

1. **Re-run device login.** Delete the copilot entry from `auth.toml` and log in
   again through the provider picker. This ensures the token was minted for
   neenee's built-in public client id.
2. **Check the token prefix.** Open `auth.toml` and look at the copilot
   `access` value. If it starts with `ghu_`, it came from a GitHub App flow and
   will not work until token exchange is implemented.
3. **Compare with opencode.** Log in to opencode with the same GitHub account.
   If opencode shows more models, the difference is almost certainly the OAuth
   client id or token type.
4. **Inspect `/models` directly.** With a valid token, run:
   ```bash
   curl https://api.githubcopilot.com/models \
     -H "Authorization: Bearer <token>" \
     -H "X-GitHub-Api-Version: 2026-06-01"
   ```
   Count the entries and check `model_picker_enabled` for the missing model.
5. **Check the account plan.** Business/Enterprise plans may need a different
   API host and token exchange, which neenee does not yet support.

## See also

- [Providers](../reference/providers.md) — capability matrix and `copilot-oauth`
  notes
- [Model Metadata](../reference/model-metadata.md) — how discovered remote
  metadata overrides static model facts
- [ADR-0065](../adr/0065-runtime-fitted-model-capability-overlay.md) — runtime
  fitted-model capability overlay
- [ADR-0070](../adr/0070-provider-scoped-remote-model-metadata.md) —
  provider-scoped remote model metadata
