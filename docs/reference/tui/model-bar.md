# Model bar

Single-row strip pinned directly below the input box. It is **ambient
gauge–focused**: the row reads — left to right — the **model** (with its
reasoning-effort tier and `@<instance>` provenance suffix), the **context
usage**, and the latest-turn **stream speed**.

Everything *actionable* about the composer moved inside the box itself: the
next-`Enter` action sentence and the `as:` target row are composer-owned meta
rows (see [Input box](input-box.md#meta-rows)). Long-lived **session** state
(the workspace path, the delegated autonomous flag) lives on its own dedicated
[head row](status-bar.md) below this one.

## Appearance

Reasoning model, fixed-effort (Kimi K3's single `max` tier always shows; the
id-first policy renders the wire id `k3`):

```text
 k3 max @kimi-code  89.2k (8%)   47.8 tok/s
```

Non-reasoning model (effort tag absent):

```text
 kimi-k2.7-code @kimi-code  89.2k (8%)   47.8 tok/s
```

Before a defensible sample exists, the rate segment renders as `– tok/s`
instead of dropping.

On narrow terminals the row degrades in a fixed order: the instance suffix
drops first (pure provenance), then the reasoning tag, then the rate segment,
then the context meter; the model name is the last ambient item to disappear.

The three segments keep their relative order — model → context → speed — at
every width that still shows more than one of them.

| Attribute | Value |
|-----------|-------|
| Location | 1 row below the input box (bottom of the footer stack) |
| Model name | `brand` + BOLD |
| Instance suffix | `@<instance>` in `muted`, after the effort tag — the provider instance's display name, so identical models served by different instances stay attributable (mirrors the `· <provider>` suffix in the Models picker) |
| Reasoning effort | `{effort}` in `info` + BOLD, right after the model name — only while the active model is actually reasoning (Anthropic: thinking opted in; OpenAI: model exposes effort) |
| Stream rate | Rightmost segment: latest completed principal turn's client-observed rate, `47.8 tok/s` in `text_muted` (or `– tok/s` before a defensible sample); click opens the Performance report |
| Context usage | Middle segment between model and rate — committed AI-visible context only: `89.2k` in `text_muted`; `(8%)` in threshold color (green/yellow/red); live composer drafts are excluded; click opens the token-source report |
| Background | `surface` |

## Visibility

Hidden when overlay modals are open, and suppressed while the permission
sheet is open (the sheet takes over the input-box, model-bar, and status
rows).

## Source

`draw_model_bar` / `ModelBarView` in `chrome.rs`; the returned rects are the
context meter's click target (opens the token-source report) and the rate
segment's click target (opens the Performance report).
