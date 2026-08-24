# Model Metadata

This page defines how muta determines a model's capability and route for one
provider channel. For provider availability and endpoints, see
[Providers](providers.md). For the decision history, see
[ADR-0070](../adr/0070-provider-scoped-remote-model-metadata.md).

## Sources

| Source | Scope | Used when | Authority |
|--------|-------|-----------|-----------|
| Per-provider baseline table | Model id | Every provider has no trusted value for a field | Baseline and offline fallback |
| Fitted overlay | Unknown model id | A trusted provider discovered an id not in any baseline table | Bare-id fallback outside a channel |
| Remote channel metadata | Provider instance and model | A trusted provider's live list explicitly supplies a field | Effective behavior for that channel |

Each provider's baseline table lives beside its other registry data (e.g.
`crates/muta-providers/src/registry/openai.rs`) and is submitted to
`muta_contracts`'s lookup machinery at link time via
`inventory::submit!(BaselineModels(...))`. `muta_contracts` owns only the
`resolve()` / `baseline_models()` mechanism — the data itself is distributed
per provider. See [`muta_contracts::model`](../../crates/muta-contracts/src/model.rs)
for the lookup precedence rules.

Remote metadata never changes another provider's channel. A model id can have
different routes or limits at different providers and accounts.

## Merge rules

The effective model capability view starts from the baseline table or its
conservative unknown-model fallback. Each non-empty remote field then replaces
the corresponding value.

| Field | Remote behavior |
|-------|-----------------|
| Family | Replaces the baseline family |
| Context window and output limit | Replaces the baseline value when present |
| Reasoning representation | Replaces the baseline thinking type when present |
| Tool calling and vision | An explicit `true` or `false` replaces the baseline |
| Effort levels | A present list replaces the baseline; an empty list disables effort control. See [Reasoning effort](effort.md) for the per-provider ladder and resolution chain |
| Endpoint | Selects the provider channel's inference surface only |

An omitted remote field is not a negative capability. It retains the static
baseline so partial provider responses do not erase useful local knowledge.

## Discovery modes

`model_source` applies only to a provider instance created from a built-in
template.

| Value | Behavior |
|-------|----------|
| `Fixed` | Uses the template's compiled-in seed list; no network request |
| `Api` | Fetches the provider's model list at startup; the last valid result remains when the request fails or yields no usable models |

Most templates treat a live list as availability only. Their advertised ids are
intersected with locally supported models, and static metadata remains active.
Templates marked trusted may fit remote capability metadata and materialize
provider-native model ids.

## GitHub Copilot

The Copilot login template is a trusted `Api` source. Its model list controls
the selectable set and each selectable model's route.

| Remote field | muta behavior |
|--------------|-----------------|
| `model_picker_enabled` | `false` excludes the model from the picker and channel set |
| `supported_endpoints` with `/chat/completions` | Uses the OpenAI Chat Completions adapter |
| `supported_endpoints` with `/responses` | Uses the OpenAI Responses adapter |
| `supported_endpoints` with `/v1/messages` | Uses the Anthropic Messages adapter with Copilot authentication |
| `capabilities.limits` | Supplies context and output limits |
| `capabilities.supports` | Supplies tools, vision, reasoning, and effort controls |

Copilot discovery sends the OAuth bearer and Copilot client identity headers.
The response therefore reflects the logged-in account's entitlements rather
than a generic static plan assumption.

## Persistence

Trusted remote metadata is stored in the matching
`[[providers.channels]]` entry as the optional `remote` table. It is managed by
discovery; do not edit it to force unsupported provider behavior. A successful
refresh replaces the snapshot. A failed refresh leaves the previous snapshot
and its channel set intact.

```toml
[[providers.channels]]
label = "gpt-5"
model = "gpt-5"
auth = "CopilotOAuth"

  [providers.channels.remote]
  endpoint = "responses"
  context_window = 200000
  max_output_tokens = 16384
  tool_call = true
  vision = true
  effort_levels = ["low", "medium", "high"]
```
