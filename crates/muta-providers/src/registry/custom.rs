//! The `custom` model provider: bring your own endpoint.
//!
//! `custom` is an ordinary [`super::ModelProviderSpec`] with an **open model
//! universe** (ADR-0201 §6). It replaces the pre-ADR-0201 "pure-custom
//! connection kind": a relay, gateway, or local runtime is now just a
//! connection to this provider with its own `base_url` and `protocol`.
//!
//! It declares no endpoint, no user agent, no live catalog, and no baseline
//! models. The connection supplies the endpoint; when it omits one, the route
//! falls back to the protocol's localhost default
//! (`crate::registry::route_for_model`). Baselines for case-sensitive
//! third-party ids live in `super::custom_baselines` and are submitted to the
//! global model registry independently of this spec.

use muta_contracts::WireProtocol;

use super::{ModelProviderSpec, RemoteCatalogSource};

pub(crate) const MODEL_PROVIDER_SPEC: ModelProviderSpec = ModelProviderSpec {
    prompt_cache: super::unsupported_prompt_cache,
    id: "custom",
    baselines: &[],
    // No default endpoint: the connection must supply one (or accept the
    // protocol's localhost fallback).
    base_url: "",
    user_agent: None,
    // A prefill for the common case, not an authority — the connection's
    // `protocol` override wins.
    protocol: WireProtocol::OpenAiChatCompletions,
    // Open model universe: the connection declares whatever it serves.
    models: &[],
    catalog_source: RemoteCatalogSource::Endpoint(crate::DiscoveryProtocol::OpenAi),
    default_client_profile: muta_contracts::ClientPreset::Native,
    client_profile_sensitive: false,
    wire_overrides: &[],
};
