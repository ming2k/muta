# Web tools

Fetch URLs and search the web. Both are `Read`. Source:
`crates/neenee-agent/src/tools/web.rs`. Provider configuration lives in
`config.toml` under `[websearch]`.

Research is a two-stage pipeline ([ADR-0118](../../adr/0118-two-stage-web-research-search-breadth-fetch-depth.md)):
`websearch` is the **breadth** stage (pluggable search provider), `webfetch`
is the **depth** stage (pluggable reader). The two axes are configured
independently and compose freely — e.g. `provider = "tavily"` for semantic
search plus `reader = "jina"` for clean full-page reading.

## `webfetch`

| Parameter | Type | Required | Default | Notes |
|-----------|------|----------|---------|-------|
| `url` | string | yes | — | `http` or `https` |
| `raw` | boolean | no | `false` | Skip text extraction |

HTML pages are converted to text by the configured **reader**:

| `reader` | Behavior |
|----------|----------|
| `builtin` (default) | Direct fetch + local HTML stripping. Zero third-party dependency; naive extraction that keeps page boilerplate. |
| `jina` | Delegate to `r.jina.ai`: server-side JavaScript rendering, readability-style main-content extraction, Markdown output. Handles SPA pages the builtin reader cannot. Anonymous use works at a modest rate limit; `jina_api_key` raises it. If the reader fails, `webfetch` falls back to the builtin path and annotates the result. |

## `websearch`

| Parameter | Type | Required | Notes |
|-----------|------|----------|-------|
| `query` | string | yes | Search query |

The default backend is **Exa** (`provider = "exa"`) with **Parallel** as the
fallback; both are hosted MCP endpoints usable anonymously (an API key is
optional and raises the quota). Other backends — `searxng` (self-hosted,
keyless), `tavily` (hosted, needs a key; uses `advanced` search depth),
`bocha` (hosted AI search, needs a key, directly reachable from mainland
China without a proxy), and `duckduckgo` (keyless scraping, frequently
blocked) — are configurable in `[websearch]` (`config.toml`).

## `[websearch]` reference

```toml
[websearch]
provider = "exa"          # breadth: exa | parallel | duckduckgo | searxng | tavily | bocha
fallback = "parallel"     # tried when provider fails; "" disables
reader = "builtin"        # depth:  builtin | jina
proxy = "socks5h://127.0.0.1:1080"  # applies to both tools
timeout_secs = 20
# keys, all optional unless the selected backend requires them:
# exa_api_key, parallel_api_key, tavily_api_key, bocha_api_key, jina_api_key
# searxng_url (required for searxng)
```

A backend that requires a key reports a pointing error at call time
(e.g. `` Bocha backend selected but `[websearch].bocha_api_key` is not set ``)
— check the `provider` value matches the key you configured, and that
`fallback` is not `""` if you want automatic failover.
