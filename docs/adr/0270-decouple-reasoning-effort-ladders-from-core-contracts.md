# 0270. Decouple Reasoning Effort Ladders from Core Contracts

- **Status:** Accepted
- **Date:** 2026-09-21
- **Implementation:** `muta-contracts`, `muta-providers`, `muta-llm-client`
- **Builds on:** [ADR-0046](0046-reasoning-is-opt-in-per-model.md), [ADR-0065](0065-runtime-fitted-model-capability-overlay.md), [ADR-0203](0203-remote-catalog-overlay-and-connection-gated-pipeline.md), [ADR-0260](0260-provider-dialect-inheritance.md)
- **Decouples:** `muta-contracts::effort` from vendor-specific and model-specific capability baseline constants

---

## Context and Problem Statement

Reasoning effort controls the depth of reasoning token generation ("how hard should the model think").
In `muta-contracts`, effort was originally partitioned into two layers:
- **Layer A**: Abstract vocabulary (`Effort` enum: `None`, `Minimal`, `Low`, `Medium`, `High`, `Xhigh`, `Max`, `Ultra`) and open runtime companion (`EffortLevel`).
- **Layer B**: Model-specific baseline value sets (`EFFORT_CLAUDE_FULL`, `EFFORT_CLAUDE_NO_XHIGH`, `EFFORT_OPENAI_GPT`, `EFFORT_OPENAI_GPT_5_6`, `EFFORT_GLM_5`, `EFFORT_XAI_GROK`, `EFFORT_LOW_HIGH_MAX`, etc.).

Placing Layer B in `muta-contracts` created an architectural abstraction leak:
1. **Core Contract Contamination**: `muta-contracts` is the foundational crate defining universal domain types, interfaces, and traits. Hardcoding vendor-specific and model-specific constants (such as Claude Sonnet 4.6 rejecting `xhigh`, or GLM-5 depth tiers) directly into the contracts crate meant that adding or updating any vendor model required mutating the core contracts.
2. **Aggregator Provider Aberration**: When defining aggregator or multi-model proxy providers (such as `opencode`, `opencode_go`, or `custom_baselines`), developers were forced to import multiple vendor-specific constants from `muta_contracts::effort`:
   ```rust
   use muta_contracts::effort::{EFFORT_CLAUDE_NO_XHIGH, EFFORT_GLM_5, EFFORT_LOW_HIGH_MAX};
   ```
   Having an OpenCode provider implementation import Anthropic- and Z.AI-branded symbols from the core contracts crate made the conceptual boundary failure blatantly apparent.
3. **Confusion Across Three Orthogonal Concerns**:
   - **Wire Protocol / LLM Client**: Dictates wire serialization (e.g. `reasoning_effort` for OpenAI ChatCompletions, `output_config.effort` for Anthropic Messages, `thinkingLevel`/`thinkingBudget` for Google Gemini). The client does not know or restrict which discrete tiers a model accepts.
   - **Model Capability / Architecture**: The model architecture and version dictate valid effort rungs (e.g. GPT-5 tops out at `xhigh`; GPT-5.6 adds `max`; Sonnet 4.6 rejects `xhigh`; DeepSeek accepts `low, high, max`). This is an intrinsic capability of the model.
   - **Model Provider / Catalog**: Dictates metadata discovery and routing. Upstream `/models` endpoints rarely advertise effort tiers (only Moonshot Kimi and Copilot do; others return bare `{id}` lists), requiring clients to supply compiled-in baseline seeds to prevent upstream 400 errors.

---

## Decision Drivers

- **Zero Vendor Knowledge in Contracts**: `muta-contracts` must only define universal domain abstractions, algorithms, and vendor-neutral fallback ladders.
- **Model Capability Baselines Belong to Provider Registry**: The compiled baseline ladders are model capability seeds used by providers to populate catalog defaults. They belong in `muta-providers::registry::effort_ladders`.
- **Uncompromised Hygiene**: Remove all vendor-specific constants (`EFFORT_CLAUDE_*`, `EFFORT_OPENAI_*`, `EFFORT_GLM_*`, etc.) from `muta-contracts` completely. No legacy aliases or deprecated shims.
- **Clear Separation of Concerns**:
  - `muta-contracts`: `Effort`, `EffortLevel`, universal operations (`rank`, `clamp_to`, `gemini_thinking_budget`), and neutral fallback `COMMON_LADDER`.
  - `muta-llm-client`: Serialization of `Effort` onto wire shapes. Protocol tests define local fixtures or use neutral ladders.
  - `muta-providers`: Provider catalog specifications, baseline model definitions, and model capability effort ladders (`muta_providers::registry::effort_ladders`).

---

## Decision

1. **Purge Vendor Effort Constants from `muta-contracts`**:
   - Delete `EFFORT_CLAUDE_FULL`, `EFFORT_CLAUDE_NO_XHIGH`, `EFFORT_OPENAI_GPT`, `EFFORT_OPENAI_GPT_5_6`, `EFFORT_OPENAI_GPT_6`, `EFFORT_XAI_GROK`, `EFFORT_LOW_HIGH_MAX`, `EFFORT_GLM_5`, `EFFORT_GEMINI_LEVEL`, `EFFORT_GEMINI_BUDGET` from `muta_contracts::effort`.
   - Retain only `COMMON_LADDER = &[Effort::Low, Effort::Medium, Effort::High]` as the universal vendor-neutral fallback ladder for models declaring reasoning support without specified rungs.
   - Update `muta_contracts::lib` to re-export only `Effort`, `EffortLevel`, and `COMMON_LADDER`.

2. **Introduce `muta_providers::registry::effort_ladders`**:
   - Group baseline model family ladders within `crates/muta-providers/src/registry/effort_ladders.rs`:
     - `COMMON` (`low, medium, high`)
     - `CLAUDE_FULL` (`low, medium, high, xhigh, max`)
     - `CLAUDE_NO_XHIGH` (`low, medium, high, max`)
     - `OPENAI_GPT` (`none, minimal, low, medium, high, xhigh`)
     - `OPENAI_GPT_5_6` (`none, minimal, low, medium, high, xhigh, max`)
     - `OPENAI_GPT_6` (`low, medium, high, xhigh, max, ultra`)
     - `XAI_GROK` (`none, low, medium, high`)
     - `LOW_HIGH_MAX` (`low, high, max` - DeepSeek, Kimi K3)
     - `GLM_5` (`low, high, xhigh, max` - Z.AI GLM-5)
     - `GEMINI_LEVEL` (`minimal, low, medium, high` - Gemini 3.x)
     - `GEMINI_BUDGET` (`low, high, max` - Gemini 2.5)
   - Provider registry modules (`anthropic.rs`, `openai.rs`, `deepseek.rs`, `opencode.rs`, `opencode_go.rs`, etc.) source their model capability ladders from `effort_ladders::*`.

3. **Decouple `muta-llm-client` Test Baselines**:
   - `muta-llm-client` test baselines and protocol test suites use local test fixtures and `muta_contracts::effort::COMMON_LADDER`, eliminating any test-only reliance on vendor constants previously hosted in contracts.

---

## Consequences

### Positive
- **Clean Architecture**: `muta-contracts` is strictly vendor-agnostic. New model families or version ladder variations do not trigger changes to fundamental contract types.
- **Natural Aggregator Provider Syntax**: Aggregators like `opencode` import model capability ladders from `crate::registry::effort_ladders`, reflecting that they are using capability definitions from the provider registry.
- **Architectural Clarity**: The three axes (Wire Protocol vs Model Capability vs Provider Catalog) are cleanly separated into their respective layers.

### Negative / Neutral
- Provider files and tests that referenced `muta_contracts::effort::EFFORT_*` must be updated to reference `crate::registry::effort_ladders::*`.
