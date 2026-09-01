# ADR-0164: First-class client profiles and connection emulation contracts

- **Status:** Accepted
- **Date:** 2026-09-01

## Context

Upstream LLM endpoints, enterprise gateways, and specialized coding platforms frequently inspect caller identities. Certain providers tailor token quotas, feature access (such as code completions, search grounding, or tool availability), or specialized routing based on HTTP `User-Agent` strings and companion identification headers (e.g. `X-Title`, `X-ZCode-Agent`, `x-app`, `x-goog-api-client`).

Previously, muta's client identification suffered from three fundamental architectural deficiencies:

1. **Lossy reverse-guessing heuristic**: `Transport` and `Endpoint` structs only stored a raw `user_agent: String`. At request build time, protocol drivers called `ClientIdentity::from_user_agent(&self.user_agent)` to reconstruct preset companion headers. Any custom client profile specifying bespoke HTTP headers (`extra_headers`) silently dropped those headers on the wire.
2. **Ambiguous and sensitive terminology**: Informal or legacy terms (such as "caller identity", "spoofing", or "impersonation") introduced misleading connotations, lacked engineering precision, and raised false alarms in security reviews and AI prompt handling.
3. **Implicit capability assumptions**: Upstream compatibility traits (such as whether a client configuration satisfied coding-platform prerequisites or required companion client headers) were scattered across disparate match statements rather than declared in an authoritative capability model.

## Decision

### 1. Unified, First-Class Domain Contracts

Establish an authoritative, four-part type hierarchy in `muta-contracts`:

- **`ClientPreset`**: An exhaustive closed enum covering all standard client environments (`Native`, `OpenCode`, `ClaudeCode`, `Codex`, `Cline`, `Cursor`, `KiloCode`, `RooCode`, `Windsurf`, `Aider`, `ZCode`, `Copilot`, `Antigravity`).
- **`ClientProfileSpec`**: A static specification defining each preset's canonical ID, human-readable display label, default version, User-Agent format, static companion headers, and capability characteristics.
- **`ClientCapabilities`**: An explicit capability struct declaring compatibility flags (such as `coding_platform_compatible` and `has_client_headers`).
- **`ClientProfile`**: The runtime client profile representation, supporting both presets and parameterized custom definitions (`Custom { user_agent: String, extra_headers: Vec<(String, String)> }`). Provide `pub type ClientIdentity = ClientProfile` as a backward-compatible alias.

### 2. End-to-End Type and Header Propagation

Propagate `ClientProfile` as a first-class value through all system layers:

- **Contracts**: `muta_contracts::catalog::Transport` variants (`Google`, `Anthropic`, `OpenAi`, `OpenAiResponses`) directly hold `client_profile: ClientProfile`.
- **LLM Client**: `muta_llm_client::Endpoint` directly holds `pub client_profile: ClientProfile`, exposing `Endpoint::headers(&self) -> Vec<(&str, &str)>`.
- **Protocol Request Drivers**: All four wire protocol builders (`google`, `anthropic`, `openai-chat-completions`, `openai-responses`) read headers directly from `endpoint.headers()`. Reverse User-Agent heuristics are completely eliminated. Custom headers are preserved without state loss.
- **Provider Registry**: `build_provider_for_channel` and provider constructors receive and pass `client_profile` directly into protocol endpoints.
- **Catalog Derivation**: Route derivation functions attach the resolved `ClientProfile` to generated transports.

### 3. Systematic Terminology Standardization

Standardize naming across the entire codebase, configuration, and user interface:

- Retire all adversarial or sensitive phrasing ("spoofing", "bypass", "impersonate", "fake").
- Standardize on industry-standard engineering terms: `ClientProfile`, `ClientPreset`, `ClientProfileSpec`, `ClientCapabilities`, and `ClientEmulation`.
- In user-facing interfaces (including the `mutx` TUI `/connections` inspector), rename the section card to **Client Profile**, showing the preset label, User-Agent, and dynamically listing all active client headers.

## Alternatives considered

### Retain string-only User-Agent with expanded regex guessing

Rejected. Attempting to deduce custom HTTP headers from a User-Agent string is fundamentally impossible when users supply custom headers. Carrying the typed `ClientProfile` preserves complete intent with zero guesswork.

### Separate HTTP middleware for header injection

Rejected. Header injection is tightly coupled to the endpoint's target protocol and channel credentials. Embedding `ClientProfile` into `Endpoint` keeps request assembly deterministic, testable in isolation, and free of global state.

## Consequences

- **Zero Header Loss**: Custom headers configured for private relays or specialized enterprise gateways reliably reach the wire.
- **Predictable Behavior**: Upstream provider adaptations behave identically across Google, Anthropic, and OpenAI protocol families.
- **Audit-Ready & AI-Friendly**: Standardized, neutral software engineering nomenclature ensures clarity for human developers, automated linters, and AI assistants alike.
- **Full Backward Compatibility**: Serialized configurations seamlessly accept kebab-case strings, CamelCase aliases, and structured JSON representations.
