# muta-providers

Concrete LLM provider implementations and the `build_provider_for_channel`
factory consumed by the orchestration layer.

The crate is organized as:

- `registry` — the provider template table, the OpenAI-compatible endpoint
  specs, the built-in model lists, and `build_provider_for_channel`, the
  single place that turns a `muta_core::catalog::Channel` into a concrete
  `dyn Provider`;
- `list_models` — live model-list discovery (`GET /v1/models` and peers);
- `oauth` — OAuth2 + PKCE credential acquisition for the subscription
  providers (xAI SuperGrok, ChatGPT/Codex, GitHub Copilot): PKCE S256, the
  RFC 8628 device flow and OpenAI's JSON variant, browser loopback login,
  single-flight refresh, and the on-disk `auth.toml` token store. Per-vendor
  client constants live in `oauth::config`;
- `mock` — trivial in-memory provider used as the default channel.

A keyless OpenAI-compatible relay reaches the same `OpenAiChatCompletionsProvider` as a
cloud endpoint (an empty key suppresses the auth header), so there is no
separate local provider module.
