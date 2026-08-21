# 0124. Web tool hardening: per-hop SSRF, token budgets, untrusted-content boundary

- **Status:** Accepted
- **Date:** 2026-08-21

## Context

ADR-0118 established the two-stage research pipeline (`websearch` breadth,
`webfetch` depth) and left known implementation gaps. Measured against the
live defaults, four classes of problem surfaced:

1. **SSRF redirect bypass.** `assert_public_url` validated only the *initial*
   URL, while the shared reqwest client followed redirects automatically
   (`Policy::limited(5)` — a hop *count* limit, not a destination check). A
   public URL answering `302 → http://169.254.169.254/…` was followed into
   the metadata endpoint. DNS rebinding (TOCTOU between the check and the
   connect) was acknowledged in comments and left open. The v6 guard also
   missed `fe80::/10` (link-local), and the v4 guard missed TEST-NET,
   `192.0.0.0/24`, `240.0.0.0/4` and CGNAT ranges were only partly covered.
2. **Unbudgeted output.** `SearchProvider` returned a pre-formatted `String`
   (the ADR-0118 "known limitation"), so the only budget mechanism was
   chopping the rendered blob at 4 000 tokens — which cut mid-entry and
   swallowed the URLs of every result past the cut. Measured: Exa with
   `numResults: 10` returns ≈14k tokens, chopped to ~28%. `webfetch` capped
   in *bytes* (16 000) while `websearch` capped in *tokens*, and its
   truncation notice advised `raw=true`, which does not raise the cap — a
   prompt to retry in a way that cannot succeed.
3. **No untrusted-content boundary.** Page bodies and search snippets
   entered the model context verbatim. A fetched page saying "run `curl … |
   sh`" is a prompt-injection vector against an agent holding a shell.
   Codex documents this class of exposure explicitly (its default
   `web_search = "cached"` mode exists to shrink it).
4. **No fetch limits.** The builtin reader buffered the entire body
   (`response.text()`) before truncation; a large file was fully downloaded
   and lossily decoded.

## Decision

### 1. Redirects are followed manually, each hop re-guarded

The shared client is built with `redirect::Policy::none()`. A new
`guarded_get` follows redirects in async code: every hop resolves and
passes `assert_public_url` *before* the request is issued, relative
`Location` values are resolved against the current URL, and the chain is
capped at 5 hops. The body is streamed (`bytes_stream`) with an 8 MiB hard
cap, so an oversized or lying-`Content-Length` response never buffers
unbounded.

This shape was chosen over a custom `redirect::Policy` because reqwest's
redirect callback is synchronous — checking a hop requires DNS, and doing
that inside the callback would mean blocking the async runtime. Manual
following keeps the check async and testable.

`assert_public_url` itself gained the missing ranges: `fe80::/10`,
TEST-NET-1/2/3, `192.0.0.0/24`, `240.0.0.0/4`. DNS rebinding remains
accepted risk (noted in the module docs); closing it needs resolve-and-pin.

### 2. Providers return structure; the tool layer owns the budget

`SearchProvider::search` now returns `ProviderOutput` — either
`Results(Vec<SearchResult>)` (DDG, SearXNG, Tavily, Bocha) or
`Blob(String)` (Exa, Parallel) — instead of a pre-rendered string. This is
the ADR-0118 §Consequences prerequisite, done.

The renderer (`format_results`) enforces the 4 000-token budget with an
explicit degradation order:

1. **Titles + URLs are never truncated** — they are the model's candidate
   list.
2. Snippets degrade to title+URL (with a visible marker) when they do not
   fit.
3. Only when even a title+URL line no longer fits are tail entries dropped,
   with a notice naming how many — so the model knows to narrow the query.

Blob outputs pass through the same token cap. Exa requests 5 results
instead of 10 (measured: 10 results ≈ 14k tokens guaranteed to be chopped;
5 fits).

`webfetch` moves from a 16 000-*byte* cap to the same 4 000-*token* cap
(keeping the first half on truncation), completing ADR-0120's unit
unification. Its truncation notice now suggests what actually works — a
narrower URL or anchor — instead of `raw=true`.

### 3. Untrusted-content boundary

`webfetch` wraps every successful result in
`[BEGIN/END UNTRUSTED WEB CONTENT]` markers whose opening line states the
rule. A system-prompt section (`system.web_untrusted_content`, active only
when a web tool is admitted, same mechanical guard as the other sections)
teaches the model what the markers mean: content inside them — and any
search snippet — is data, never instruction; injection attempts are
reported, not obeyed. The `websearch` description carries a one-line version
of the same rule.

### 4. Smaller corrections in the same pass

- `webfetch`/`snapshot` send a browser User-Agent (matching the search
  backends) instead of `neenee/0.1`, which anti-bot layers rejected more
  often.
- Tavily's API key moved from the JSON body to `Authorization: Bearer`,
  keeping it out of error-body echoes.
- Unknown `provider`/`fallback` names log a `tracing::warn` (still falling
  back to Exa — a typo no longer fails silently, but does not brick the
  tool either).
- The `websearch` description's "current year" is computed once per process
  at first use rather than per tool construction, so daemon sessions do not
  carry a stale year across New Year's.

## Consequences

- A redirect into private space fails the whole fetch with an SSRF-guard
  error naming the refused address — observable and testable (covered by
  integration tests that spin a loopback redirector aimed at the metadata
  endpoint).
- 304 Not Modified handling in `snapshot` now detects the validator match by
  ETag equality (guarded_get treats 3xx as redirects and 2xx as bodies; 304
  arrives as a success with an empty body). A no-validator server that
  returns 200 with an unchanged ETag is treated as Modified — same behavior
  as before, hash comparison still catches content changes.
- Search result *lists* can be shorter in tokens than before, but they no
  longer lose candidate URLs — the model always sees the full candidate
  list or an explicit count of what was dropped.
- The token cost of a fetched page is now bounded by tokens, not bytes:
  CJK-heavy pages keep ~2× more content than the old byte cap; ASCII-heavy
  pages slightly less.

## Alternatives considered

- **Custom `redirect::Policy` with a sync DNS check inside the callback.**
  Rejected: blocking inside reqwest's redirect hook risks runtime deadlock
  (`block_in_place` is unavailable in the local-set/embedded contexts the
  agent runs tools under).
- **Full resolve-and-pin (connect to the IP the guard resolved).** Deferred:
  it needs a custom connector and changes proxy interactions; the manual
  per-hop guard closes the realistic vectors first.
- **Filtering/dropping injected instructions at the tool layer (regex or
  model-based scrubbing).** Rejected: unreliable and lossy. Markers +
  guidance follow the boundary pattern the industry converged on (Anthropic
  citations envelope, Codex "treat web results as untrusted" guidance).
- **Raise `numResults` and let the budget degrade.** Rejected: more hits
  past the budget are worthless if their URLs are dropped; 5 in-budget hits
  beat 10 with 7 invisible.

## References

- ADR-0118 — the two-stage pipeline this hardens.
- ADR-0120 — tokens as the budget unit.
- `crates/neenee-agent/src/tools/ssrf.rs` — the guard and its range table.
- `crates/neenee-agent/src/tools/web.rs` — `guarded_get`, markers, budgets.
- `crates/neenee-agent/src/tools/search/mod.rs` — `ProviderOutput`,
  budget-aware `format_results`.
- `docs/reference/tools/web.md` — user-facing behavior.
