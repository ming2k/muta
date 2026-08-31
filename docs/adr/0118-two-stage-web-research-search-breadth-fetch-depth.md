# 0118. Two-stage web research: search is breadth, fetch is depth

- **Status:** Accepted
- **Date:** 2026-08-19

## Context

The web tools grew organically into two independent axes, and a proposal
arose to support "multi-stage, multi-provider" websearch — e.g. configure
Tavily for a semantic breadth search plus Jina Reader for deep reading,
fused into one pipeline.

The current code (before this ADR):

- `websearch` delegates to a pluggable `SearchProvider`
  (`crates/neenee-agent/src/tools/search/`) selected by `[websearch] provider`
  with a single `fallback`. Six backends exist: exa (default, anonymous MCP),
  parallel (anonymous MCP), duckduckgo (scraping), searxng (self-hosted),
  tavily and bocha (keyed REST).
- `webfetch` is a hard-coded direct fetch: SSRF guard, then a naive
  hand-rolled HTML state machine (`html_to_text`), then a 16,000-byte →
  8,000-byte truncate. No DOM parsing, no readability extraction, no
  JavaScript rendering — SPA pages strip to a near-empty shell, and
  boilerplate (nav/footer/cookie banners) pollutes the first bytes the model
  actually sees.

A tempting design is to make `websearch` itself multi-stage: search, then
automatically fetch the top-N hits' full text, and merge everything into one
tool result. This ADR argues against that fusion and for a different split.

## Decision

The two stages are **the two existing tools**, and the multi-provider
capability is expressed as **two orthogonal config axes**, not one pipeline:

1. **`websearch` stays breadth-only.** It returns titles, URLs, and snippets.
   It does not auto-read pages. The model — not the tool — schedules deep
   reads, because the model is the only component that knows which hit is
   worth reading. (Backends that already embed content server-side, e.g.
   Exa's `livecrawl: fallback`, remain free to do so; that is the backend's
   own breadth quality, not our pipeline.)

2. **`webfetch` becomes the pluggable depth stage** via a new `Reader`
   abstraction (`crates/neenee-agent/src/tools/reader/`), mirroring
   `SearchProvider`: one module + one match arm in `build_reader`, selected
   by a new `[websearch] reader` key.
   - `reader = "jina"` (default): delegate to `https://r.jina.ai/<url>` (optional
     `jina_api_key`), which renders JavaScript and extracts the main content
     as Markdown.
   - `reader = "disabled"` / `"none"`: disable webfetch tool.

   So "Tavily + Jina" is expressed as `provider = "tavily"` **plus**
   `reader = "jina"` — the two axes compose freely with all six search
   backends.

3. **SSRF runs before any reader.** The guard resolves the target host
   before a reader executes, so a private URL is never relayed to an
   external reader service either.

4. **Truncation is unified in bytes.** `webfetch` shares the 16,000-byte cap
   (keeping half on truncation) with the rest of the web tools, replacing
   the old chars-vs-bytes mismatch between `cap_output` and the fetch path.

### Why not fuse reading into websearch

- **Context economics.** The 16k output cap holds roughly two full pages.
  Auto-reading the top 3 hits means 3–10 extra HTTP round trips and burns
  the budget on hits the model would have skipped. A snippet list costs
  ~1/20th of that and preserves the model's ability to choose.
- **Latency.** Search is one round trip; search-plus-read is a fan-out of
  dependent requests through the same proxy. Failures compound.
- **The dispatch already exists.** The model already chains `websearch` →
  `webfetch` (the tools' descriptions nudge it), and envoy sub-agents can
  run the same loop for parallel research. A fused tool would *remove*
  scheduling freedom to save one tool call the model is happy to make.
- **Failure isolation.** With two tools, a search outage and a reader
  outage degrade independently. Fused, one flaky reader poisons search.

## Alternatives considered

- **Fuse breadth+depth in `websearch` (auto-read top-N).** Rejected for the
  reasons above. Tavily's own `include_raw` / Exa's `livecrawl` cover the
  "search with embedded content" middle ground where it genuinely helps, at
  the backend's discretion.
- **A third tool (`webresearch`?) that does both.** Rejected: tool-count
  inflation for zero new capability, and it duplicates the argument surfaces
  of both existing tools.
- **A local readability port (DOM crate + extraction heuristics).** Deferred:
  it would fix boilerplate but not the SPA case, and adds a heavy dependency.
  The reader abstraction keeps this door open — a `readability` reader is a
  new module, nothing else changes.
- **Reusing Exa's `crawled_pages` / Parallel content as the reader.** These
  are search-side conveniences, not general URL readers; they cannot read an
  arbitrary URL the model already knows (docs links, GitHub issues, pasted
  URLs).

## Consequences

- `Tavily + Jina` (the proposal that motivated this) is one config table:
  `provider = "tavily"` + `tavily_api_key` + `reader = "jina"` (+ optional
  `jina_api_key`). Tavily now requests `search_depth: "advanced"` so the
  breadth stage returns richer snippets worth reading.
- `webfetch` with `reader = "jina"` sends the target URL to a third party
  and adds one network hop — opt-in, off by default, and documented.
- Known limitation, deliberately out of scope here: `SearchProvider` returns
  a pre-formatted `String`, so the tool layer cannot dedupe URLs, cap
  per-domain results, or budget full text across hits. If fused reading is
  ever revisited, first refactor providers to return structured
  `SearchResult`s so the *tool layer* owns formatting and budgets.
- Live-network E2E tests (`tests/webtool_e2e.rs`, `#[ignore]`d) pin the
  two-stage contract: search returns URLs; fetch reads one of them.

## References

- `crates/neenee-agent/src/tools/search/mod.rs` — the `SearchProvider`
  pattern this ADR mirrors for readers.
- `crates/neenee-agent/src/tools/reader/` — the new `Reader` abstraction.
- `docs/reference/tools/web.md` — user-facing configuration.
- ADR-0115 — why web tool keys live inline in `config.toml`'s `[websearch]`
  table rather than `credentials.toml` (unchanged by this ADR; `jina_api_key`
  follows the existing convention).