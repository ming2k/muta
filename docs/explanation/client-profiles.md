# Client Profiles and Connection Emulation

How muta represents, manages, and injects caller client identities across upstream
inference providers.

## Why Client Profiles Exist

Large language model providers, corporate API gateways, and specialized coding platforms
often treat client identities as an operational dimension. Upstream endpoints use HTTP
`User-Agent` strings and companion identification headers to:

- Route requests to specialized inference clusters (e.g. IDE-optimized backends);
- Unlock capability tiers such as prompt caching, code completion models, or search grounding;
- Track client ecosystem distribution across coding assistants and CLI tools;
- Enforce gateway access policies for authorized developer environments.

To interoperate seamlessly with these upstream environments, muta models client identity
as an authoritative, first-class configuration attribute termed a **Client Profile**.

## The Profile Model

Muta structures client identities into four orthogonal layers:

```
┌─────────────────────────────────────────────────────────────┐
│                       ClientProfile                         │
│   (Enum: Presets OR Parameterized Custom Header Map)        │
└──────────────┬──────────────────────────────┬───────────────┘
               │                              │
        Preset Selection                Custom Config
               │                              │
               ▼                              ▼
┌─────────────────────────────┐   ┌───────────────────────────┐
│     ClientProfileSpec       │   │   Raw User-Agent +        │
│  - Static ID & Label        │   │   Custom extra_headers    │
│  - Standard User-Agent      │   └───────────────────────────┘
│  - Preset Companion Headers │
│  - ClientCapabilities       │
└─────────────────────────────┘
```

### Standard Presets

Muta provides built-in specifications for standard AI coding and development environments:

| Preset | Canonical ID | Display Label | Characteristics |
|--------|--------------|---------------|-----------------|
| `Native` | `native` | muta (Native) | Default identity (`User-Agent: muta/<version>`) |
| `OpenCode` | `opencode` | OpenCode | OpenCode coding assistant identity |
| `ClaudeCode` | `claude-code` | Claude Code | Anthropic Claude Code CLI (`x-app: claude-code`) |
| `Codex` | `codex` | OpenAI Codex | OpenAI Codex CLI headers |
| `Cline` | `cline` | Cline | Cline extension companion headers (`X-Title: Cline`) |
| `Cursor` | `cursor` | Cursor | Cursor IDE client identity (`X-Title: Cursor`) |
| `KiloCode` | `kilo-code` | Kilo Code | Kilo Code extension identity |
| `RooCode` | `roo-code` | Roo Code | Roo Code extension identity |
| `Windsurf` | `windsurf` | Windsurf | Windsurf editor companion headers |
| `Aider` | `aider` | Aider | Aider pair-programming CLI |
| `ZCode` | `zcode` | Z Code | Zhipu ZCode headers (`X-Title`, `X-ZCode-Agent`) |
| `Copilot` | `copilot` | GitHub Copilot | VS Code Chat & GitHub Copilot headers |
| `Antigravity`| `antigravity`| Antigravity (Google)| Cloud Code / Antigravity client metadata |

### Parameterized Custom Profiles

Users and enterprise deployments can define custom client profiles that pair arbitrary
`User-Agent` strings with arbitrary key-value HTTP headers. This enables connections through
custom reverse proxies, security authentication gateways, or proprietary internal tooling
without modifying engine source code.

## Lossless Wire Request Pipeline

A foundational invariant of muta's client profile subsystem is **zero header loss**.

Rather than storing only a `User-Agent` string and attempting to reverse-engineer companion
headers at request time, muta preserves the full `ClientProfile` struct throughout the entire
transport lifecycle:

1. **Connection Resolution**: A connection configuration resolves its assigned `ClientProfile`.
2. **Channel Derivation**: The catalog binds the resolved `ClientProfile` directly to the `Transport`.
3. **Endpoint Construction**: The LLM client endpoint stores the profile as a first-class field.
4. **Wire Emission**: When assembling HTTP requests across any of the four supported protocols
   (`Google generateContent`, `Anthropic messages`, `OpenAI chat completions`, `OpenAI responses`),
   the protocol builder extracts all declared headers directly via `endpoint.headers()`.

This eliminates heuristic guesswork, ensures predictable testability, and guarantees that custom
headers configured for enterprise gateways are faithfully transmitted on every request.

## Design Invariants and Clean Nomenclature

Muta adheres strictly to neutral, precise software engineering terminology:

- **Client Profile & Emulation**: The system models compatibility profiles rather than adversarial bypasses. Terms like "spoofing", "faking", or "bypassing" are rejected in favor of `ClientProfile`, `ClientPreset`, `ClientProfileSpec`, and `ClientEmulation`.
- **Authoritative Capabilities**: Compatibility attributes (such as whether a profile meets the prerequisites for coding platforms) are queried via explicit `ClientCapabilities` properties rather than hardcoded pattern-matching downstream.
- **Transparent User Visibility**: Interfaces and inspection modals (such as the TUI Connection Inspector) clearly display the active client profile, its underlying `User-Agent`, and every companion header injected into outbound requests.
