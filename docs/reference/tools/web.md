# Web tools

Muta exposes two independent read-only tools:

- `search_web` discovers sources and returns titles, URLs, and snippets.
- `read_url` extracts one URL selected by the model as Markdown.

This breadth/depth split is defined by
[ADR-0118](../../adr/0118-two-stage-web-research-search-breadth-fetch-depth.md).
Provider selection and configuration ownership are defined by
[ADR-0202](../../adr/0202-singleton-web-provider-selection.md).

## Configuration

Each axis selects one compiled provider or `disabled`. There are no web
connection instances, presets to create, or automatic fallback routes.

```toml
[web]
provider = "exa"
reader = "disabled"
timeout_secs = 20
# proxy = "socks5h://127.0.0.1:1080"
# searxng_url = "https://search.example.com/search"
```

The default search provider is Exa. The default reader is disabled, so the UI
must not report a reader as active until one is selected. The old values
`reader = "builtin"` and `reader = "none"` load as `disabled`; saves emit only
the canonical value.

### Search providers

| Provider | Credential | Endpoint |
|---|---|---|
| `exa` | Optional (`EXA_API_KEY`) | Fixed hosted service |
| `parallel` | Optional (`PARALLEL_API_KEY`) | Fixed hosted service |
| `duckduckgo` | None | Fixed hosted service |
| `searxng` | None | `web.searxng_url` required for readiness |
| `tavily` | Required (`TAVILY_API_KEY`) | Fixed hosted service |
| `bocha` | Required (`BOCHA_API_KEY`) | Fixed hosted service |
| `disabled` | None | Tool disabled |

### Reader providers

| Provider | Credential | Endpoint |
|---|---|---|
| `jina` | Optional (`JINA_API_KEY`) | `https://r.jina.ai`; anonymous use is supported |
| `disabled` | None | Tool disabled |

Only implemented adapters are advertised. In particular, `builtin`,
Firecrawl, and arbitrary custom readers are not selectable providers.

## Credentials

Secrets live in `credentials.toml`, keyed by axis and provider:

```toml
[web.search]
tavily = "..."

[web.reader]
jina = "..."
```

The fixed environment variable shown in the provider tables takes precedence
over the stored value. Configuration responses expose only whether the
effective credential is stored, provided by the environment, optional and
missing, required and missing, or not required. Secret text is never returned.

Selecting a provider before entering its required token or endpoint is allowed;
Settings shows **Needs setup** and the tool remains unavailable. This is not an
active or silently substituted route.

## Live settings protocol

- `QueryWebSearchConfig` returns the authoritative selection, readiness,
  revision, and capability catalog.
- `UpdateWebSearchConfig` applies a partial update with `expected_revision`.
  A stale update is rejected so two frontends attached to the same hosted
  session cannot silently overwrite each other. A credential mutation carries
  `axis`, `provider_id`, and `value`; an
  empty value clears the stored credential. Behavior and credential changes
  use separate updates so every acknowledgement covers one atomic file write.

After persistence, the daemon replaces one resolved runtime snapshot. The next
tool call rebuilds its provider/client when the snapshot revision changed; a
restart is not required.

## Legacy migration

`[websearch]` still loads as an alias for `[web]`. A former
`web_connections.toml` file is read only during migration:

- a selected connection backed by a known provider becomes that provider;
- its connection-keyed credential is copied to the provider-keyed map;
- `builtin` becomes `disabled`;
- unsupported custom or unimplemented routes become `disabled`;
- a one-shot marker prevents a deliberately cleared token from being reimported.

The legacy file and unmatched credentials are not deleted. They are never used
for normal routing after migration.

## Output and security

Both tools cap output at 4,000 tokens. Web content is wrapped as untrusted data.
`read_url` rejects loopback, private, link-local, metadata, and reserved targets
before connecting, and revalidates every redirect hop. Response bodies have an
8 MiB hard cap.

Hosted providers receive the query or target URL. Use a self-hosted SearXNG
provider when query privacy requires infrastructure you control.
