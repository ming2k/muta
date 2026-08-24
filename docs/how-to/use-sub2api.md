# How to use sub2api relays

Use a sub2api relay when the service exposes an OpenAI, Anthropic, or Google
compatible HTTP surface and gives you a token plus a relay URL. muta's
provider editor creates a named provider instance, stores the token, and seeds
the model list from the selected template.

For templates with live model discovery, muta queries the relay's `/models`
endpoint at startup and keeps only ids that are also present in muta's model
registry for that wire protocol. Unknown or incompatible ids are hidden. A
failed request or an empty intersection keeps the last valid model list.
Discovery never replaces the provider's token, token environment variable,
base URL, user agent, or authentication mode.

Trusted first-party templates (currently only Kimi Code) are the exception:
every advertised model is materialized, and ids missing from the registry are
fitted with the capability metadata the platform advertises — see
[ADR-0065](../adr/0065-runtime-fitted-model-capability-overlay.md).

For the provider schema and field meanings, see
[How to add a provider](add-a-provider.md) and
[Providers](../reference/providers.md).

## Add an OpenAI sub2api relay

1. Open `/connections`.
2. Select `＋ Add connection`.
3. Select `OpenAI (sub2api)`.
4. Fill the fields:

   | Field | Value |
   |-------|-------|
   | `Name` | Any display name, such as `Example OpenAI` |
   | `Base URL` | The full chat endpoint, such as `https://relay.example.com/v1/chat/completions` |
   | `Token` | The relay token |

5. Press `Enter` to save and activate the provider.

Do not enter only the root URL, such as `https://relay.example.com/v1`, for an
OpenAI relay. The OpenAI provider posts directly to the configured `Base URL`.
Use the full `/chat/completions` endpoint.

The `OpenAI (sub2api)` template seeds common OpenAI text models, including
the GPT-5.6 tier-named family (`gpt-5.6-sol`, `gpt-5.6-terra`,
`gpt-5.6-luna`), plus `gpt-5.5`, `gpt-5.4`, `gpt-5.4-mini`,
`gpt-5.3-codex-spark`, `gpt-5.2`, `gpt-5.2-chat-latest`, and `gpt-5.2-pro`.
The `gpt-5.6` alias (routes to Sol) is also registered, so you can type it
via `＋ Add model` if your relay exposes it. GPT-5.6 honors the `max`
reasoning-effort level. If your relay advertises another chat model, open
the provider's model list and use `＋ Add model`.

## Add an Anthropic sub2api relay

1. Open `/connections`.
2. Select `＋ Add connection`.
3. Select `Anthropic (sub2api)`.
4. Fill the fields:

   | Field | Value |
   |-------|-------|
   | `Name` | Any display name, such as `Example Claude` |
   | `Base URL` | The full Messages endpoint, such as `https://relay.example.com/v1/messages` |
   | `Token` | The relay token |

5. Press `Enter` to save and activate the provider.

Anthropic relays use the `/messages` endpoint, not
`/chat/completions`. The template seeds the Claude model family. Add a custom
model from the provider's model list when the relay exposes a Claude alias that
is not listed.

## Add the Antigravity Google relay

1. Open `/connections`.
2. Select `＋ Add connection`.
3. Select `Antigravity (sub2api)`.
4. Fill `Name` and `Token`.
5. Keep the pre-filled `Base URL` unless your relay uses a different host:
   `https://relay.example.com/antigravity/v1beta`.
6. Press `Enter` to save and activate the provider.

Google-native relays use the versioned base URL. muta appends
`/models/{model}:generateContent` for each request.

## Configure a relay instance

Edit the state store when you want a reproducible provider definition without
using the TUI. Instances live in `providers.toml`
(`$XDG_STATE_HOME/muta/`), the selection in `config.toml`, and tokens in the
credentials file or an environment variable.

```toml
# $XDG_CONFIG_HOME/muta/config.toml — behavior only
default_provider = "example-openai"
```

```toml
# $XDG_STATE_HOME/muta/providers.toml — instances
[[providers]]
id = "example-openai"
name = "Example OpenAI"
transport = "OpenAi"
base_url = "https://relay.example.com/v1/chat/completions"
models = ["gpt-5.5"]
```

```toml
# $XDG_CONFIG_HOME/muta/credentials.toml — secrets (or set RELAY_API_KEY in the env)
[providers]
example-openai = "sk-..."
```

For Anthropic:

```toml
[[providers]]
id = "example-claude"
name = "Example Claude"
transport = "Anthropic"
base_url = "https://relay.example.com/v1/messages"
models = ["claude-sonnet-5"]
```

For Google-native Antigravity:

```toml
[[providers]]
id = "antigravity"
name = "Antigravity"
transport = "Google"
base_url = "https://relay.example.com/antigravity/v1beta"
models = ["gemini-3-flash"]
```

## Check a relay endpoint

Query the model list when the relay supports OpenAI's `/models` route:

```bash
curl -fsS \
  -H "Authorization: Bearer $RELAY_API_KEY" \
  https://relay.example.com/v1/models
```

Then test the exact chat endpoint configured in muta:

```bash
curl -fsS \
  -H "Authorization: Bearer $RELAY_API_KEY" \
  -H "Content-Type: application/json" \
  https://relay.example.com/v1/chat/completions \
  -d '{"model":"gpt-5.5","messages":[{"role":"user","content":"Reply with OK only."}],"stream":false}'
```

Use the endpoint that returns JSON chat-completion data. A relay root that
returns HTML is not a valid muta `Base URL`.
