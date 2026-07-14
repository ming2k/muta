# neenee-intelligence

Reusable public-information monitoring and structured AI expert review for
neenee application surfaces.

`OpinionHub` reuses the configured neenee web tools to rank topic searches and
to observe explicit links with HTTP validators plus SHA-256 fallback
fingerprints. `ExpertPanel` runs five perspectives through an independent
round, a cross-examination round, and a separate meeting-manager synthesis.

Both services persist through the shared XDG State policy and expose injectable
ports for deterministic tests. The crate has no brokerage dependency and no
order-submission path. `neenee-quant-gui` composes it with `neenee-quant`, but
expert conclusions remain advisory.

See [How to use the intelligence workbench](../../../docs/how-to/use-intelligence-workbench.md)
and [ADR-0063](../../../docs/adr/0063-intelligence-workbench-and-expert-council.md).
