# Web tools architecture

`search_web` and `read_url` form two independent stages. Search discovers
sources; the reader extracts one URL chosen by the model. Neither stage calls
the other implicitly.

The configuration has cardinality one per axis:

```text
config.toml [web] ── behavior ──┐
                                ├─ resolve ─► versioned WebRuntimeConfig ─► tools
credentials.toml [web.*] ───────┘
                 environment ───┘

daemon capability catalog ─► validation ─► TUI and web provider selectors
```

`WebSearchProvider` and `WebReaderProvider` are closed enums. Their capability
records declare display metadata, credential policy, endpoint policy, and the
default environment variable. The daemon sends this catalog to frontends;
frontends do not maintain their own provider lists.

Behavior and readiness are different states. Selecting Tavily without a token,
or SearXNG without an endpoint, is valid persisted setup but the corresponding
tool is unavailable. The configuration response exposes `RequiredMissing`,
`OptionalMissing`, `Stored`, `Environment`, or `NotRequired`, never a token.
Environment values override stored credentials.

Every update carries the frontend's observed revision. After validation and
persistence, the daemon resolves behavior plus credentials into one snapshot
and swaps it into the shared runtime handle. Tool clients and adapters rebuild
against its revision on their next call. Unknown provider values never choose
a fallback provider. One update changes either behavior or one credential;
mixed patches are rejected rather than pretending two files can be committed
atomically.

The former `web_connections.toml` store is not a runtime dependency. At load,
known legacy connection selections and credentials are copied to their provider
equivalents. Unsupported custom records become `disabled`; the source file and
unmatched credentials remain intact for recovery. A persisted one-shot marker
prevents those preserved values from being imported again after the user clears
a migrated credential.

The binding rules and rejected alternatives are recorded in
[ADR-0202](../adr/0202-singleton-web-provider-selection.md).
