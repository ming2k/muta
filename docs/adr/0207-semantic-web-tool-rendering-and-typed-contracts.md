# 0207. Semantic Web Tool Rendering and Typed Contracts

- Status: Accepted
- Date: 2026-09-13
- Scope: `muta-contracts`, `muta-agent`, `apps/tui/mutx`, `apps/web`
- Deciders: Muta maintainers
- Consulted: -
- Informed: -
- Amends: [0001](0001-tool-rendering-redesign.md), [0118](0118-two-stage-web-research-search-breadth-fetch-depth.md), [0124](0124-web-tool-hardening-per-hop-ssrf-token-budgets-untrusted-boundary.md)

---

## Context and Problem Statement

ADR-0001 established the principle of typed `ToolOutput` to replace string-sniffing in tool rendering. ADR-0118 established the two-stage web architecture separating search breadth (`search_web`) from deep reader retrieval (`read_url`). ADR-0124 introduced strict per-hop SSRF validation, token budgeting, and untrusted-content boundaries (`[BEGIN UNTRUSTED WEB CONTENT]`).

Despite these foundations, the presentation pipeline for web tools suffered from four fundamental issues:

1. **Premature Stringification & Semantic Erasure**: In `muta-agent`, structured search hits (title, url, snippet, domain) and reader markdown bodies were flattened into monolithic text blobs before crossing the contract boundary.
2. **Visual Fallback Degradation**: In `mutx`, `WebSearchPresenter` and `WebReaderPresenter` lacked dedicated `ResultKind` variants, falling back to `ResultKind::Code`. This resulted in search results and articles being drawn on a monospaced code surface with artificial, misleading line-number gutters (`1, 2, 3...`).
3. **Internal Security Guard Leakage**: The `[BEGIN UNTRUSTED WEB CONTENT]` boundary defined in ADR-0124 was designed exclusively for LLM prompt-injection defense. Flattening tool outputs forced this internal system framing directly into the terminal UI, cluttering the user transcript.
4. **Broken Hyperlink Interaction**: While `mutx` provides native `LinkHit` hit-testing and OS browser dispatching (`open_browser`), search hits and read URLs remained inert text lines.

---

## Decision

We adopt a full-stack, typed contract architecture for web tools across all layers:

### 1. First-Class Contracts in `muta-contracts`

Add typed variants to `ToolOutput`:

```rust
pub enum ToolOutput {
    // ...
    WebSearch {
        query: String,
        provider: String,
        results: Vec<WebSearchHit>,
        truncated: bool,
    },
    WebArticle {
        url: String,
        title: Option<String>,
        domain: String,
        markdown: String,
        reader: String,
        tokens: usize,
        truncated: bool,
    },
}

pub struct WebSearchHit {
    pub title: String,
    pub url: String,
    pub domain: String,
    pub snippet: String,
}
```

All types export to TypeScript (`apps/web/src/lib/generated/wire.gen.ts`) via `ts-rs`.

### 2. Model-Facing vs UI-Facing Decoupling

`ToolOutput::to_text(&self)` composes model-facing prompts adhering to ADR-0124 (injecting `[BEGIN UNTRUSTED WEB CONTENT]` boundaries, token counts, and truncation notices). The TUI and web frontends consume the unpolluted typed fields directly.

### 3. Structured Execution in `muta-agent`

`WebSearchTool` and `WebReaderTool` implement `Tool::call_structured`.
- `search_web` budgets hits via `results_to_hits` and `budget_web_hits`, extracting the host domain for each hit.
- `read_url` parses page titles and host domains, preserving the pure markdown body in `WebArticle.markdown`.

### 4. Semantic Presentation & Interaction in `mutx`

Add `ResultKind::WebSearch` and `ResultKind::WebArticle`.
- **Search SERP Layout** (`draw_web_search_content`): Card-based flow with index pills (`[1]`), bold headings, domain tags (`🌐 domain`), wrapped snippets, and truncation notices.
- **Article Reader Layout** (`draw_web_article_content`): Replaces raw code gutters with a clean reading mode. The security boundary is rendered as an elegant provenance banner (`🛡️ Untrusted External Content`). Headings, lists, blockquotes, and code blocks are rendered in clean prose.
- **Mouse Link Interaction**: Every search result URL and article target registers a `LinkHit` with `LayoutMap`, enabling direct mouse-click opening in the default browser via `open_browser`.
- **Backward Compatibility**: `parse_fallback_web_search` and `parse_fallback_web_article` restore card and article layouts from legacy sessions.

---

## Consequences

- **Positive**: Eliminates string-sniffing for web tools; decouples human UI from AI safety prompts; provides modern card/reader UI in terminal; enables instant URL click-through; synchronizes types to web frontends.
- **Negative**: Adds two new variants to `ToolOutput`, requiring match arm updates in exhaustive consumers.
