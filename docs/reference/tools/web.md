# Web tools

Fetch URLs and search the web. Both are `Read`. Source:
`crates/neenee-agent/src/tools/web.rs`. Provider configuration lives in
`config.toml` under `[websearch]`.

Research is a two-stage pipeline ([ADR-0118](../../adr/0118-two-stage-web-research-search-breadth-fetch-depth.md)):
`websearch` is the **breadth** stage (pluggable search provider), `webfetch`
is the **depth** stage (pluggable reader). The two axes are configured
independently and compose freely — e.g. `provider = "tavily"` for semantic
search plus `reader = "jina"` for clean full-page reading.

## Untrusted content

Everything the web tools return — page bodies, search snippets, summaries —
is third-party content and a prompt-injection surface. `webfetch` wraps its
output in `[BEGIN/END UNTRUSTED WEB CONTENT]` markers and a system-prompt
section (`system.web_untrusted_content`, active whenever a web tool is
admitted) teaches the model to treat everything inside those markers (and in
search snippets) as data, never as instructions.

## SSRF and redirect safety

Fetches resolve the target host and refuse non-public addresses (loopback,
RFC1918, link-local, the cloud metadata endpoint, reserved ranges) *before*
connecting. Redirects are followed manually — each hop re-runs the same
check — so a public URL answering `302 → http://169.254.169.254/` is refused
mid-chain instead of being followed into the metadata endpoint. Response
bodies are streamed with an 8 MiB hard cap.

## `webfetch`

| Parameter | Type | Required | Default | Notes |
|-----------|------|----------|---------|-------|
| `url` | string | yes | — | `http` or `https` |
| `raw` | boolean | no | `false` | Skip text extraction |

Output is capped at 4 000 tokens (keeping the first half on truncation) —
the same budget `websearch` uses. The truncation notice suggests a narrower
URL or anchor; `raw=true` only disables HTML stripping and does not raise
the cap.

HTML pages are converted to text by the configured **reader**:

| `reader` | Behavior |
|----------|----------|
| `builtin` (default) | Direct fetch (SSRF-guarded, redirect-rechecked, body-capped) + local HTML stripping. Zero third-party dependency; naive extraction that keeps page boilerplate. Sends a browser User-Agent. |
| `jina` | Delegate to `r.jina.ai`: server-side JavaScript rendering, readability-style main-content extraction, Markdown output. Handles SPA pages the builtin reader cannot. Anonymous use works at a modest rate limit; `jina_api_key` raises it. If the reader fails, `webfetch` falls back to the builtin path and annotates the result. |

## `websearch`

| Parameter | Type | Required | Notes |
|-----------|------|----------|-------|
| `query` | string | yes | Search query |

Structured backends render results under the shared 4 000-token budget with
titles and URLs never truncated: snippets degrade to title+URL first, and
only when even that no longer fits are tail entries dropped with an explicit
notice. Blob-style backends (Exa, Parallel) pass through the same cap. The
description's "current year" is computed per session, so long-lived daemon
sessions keep the right year after New Year's.

The default backend is **Exa** (`provider = "exa"`) with **Parallel** as the
fallback; both are hosted MCP endpoints usable anonymously (an API key is
optional and raises the quota). Other backends — `searxng` (self-hosted,
keyless), `tavily` (hosted, needs a key; uses `advanced` search depth),
`bocha` (hosted AI search, needs a key, directly reachable from mainland
China without a proxy), and `duckduckgo` (keyless scraping, frequently
blocked) — are configurable in `[websearch]` (`config.toml`). An unknown
`provider` or `fallback` name logs a warning and falls back to Exa (it does
not fail silently).

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

> **Privacy note:** the default (anonymous) Exa and Parallel backends send
> your search queries to their hosted services. Switch to a self-hosted
> `searxng` (or any keyed backend you trust) if that matters.
