# How to use OpenRouter

OpenRouter is a built-in model provider in Muta. One connection uses one
OpenRouter API key and discovers the current model catalog from OpenRouter;
you do not need to create a custom endpoint for each model.

## Add the connection

1. Create an API key in the OpenRouter dashboard.
2. Open `/connections` in `mutx`.
3. Press `a`, choose **OpenRouter**, enter a connection name and the API key,
   and save.
4. Open `/models` and select `nex-agi/nex-n2.5-pro:free`, or search for any
   other model returned by the live OpenRouter catalog.

Before the first successful catalog refresh, Muta seeds
`nex-agi/nex-n2.5-pro:free` with its tool-calling, image-input, 262,144-token
context, and reasoning-effort capabilities. A successful refresh replaces the
available model list with OpenRouter's current catalog. If a later refresh
fails, Muta keeps the last valid catalog; the seed is only the pre-refresh
fallback.

## Configure an environment credential

Connections normally store their token in `credentials.toml`. To source it
from the environment instead, set the connection's `api_key_env` field:

```toml
[[connections]]
name = "openrouter"
provider = "openrouter"
api_key_env = "OPENROUTER_API_KEY"
```

Then export the key before starting Muta:

```bash
export OPENROUTER_API_KEY="sk-or-..."
```

Do not add `base_url` or `protocol` for the normal service. The built-in route
uses `https://openrouter.ai/api/v1/chat/completions` and the OpenRouter Chat
Completions dialect, including unified reasoning controls and replayable
reasoning details across tool calls.

## Free-model limits

The `:free` suffix selects a zero-price model variant, but it does not remove
OpenRouter request and availability limits. A `429` response is an upstream
quota signal; wait for the limit window or select another model. Use the
connection details view to inspect the key's reported usage and rate limits.

For the complete provider and capability contract, see
[Providers](../reference/providers.md).
