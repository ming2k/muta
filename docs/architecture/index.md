# Architecture blueprints

Living subsystem blueprints: how a subsystem works today, in prose, with the
constraints an engineer or an AI assistant must observe. These pages absorb the
invariants from accumulated decision records and cite the founding records for
history; the decisions themselves stay in
[Architecture Decision Records](../adr/index.md).

| Blueprint | Subsystem |
|-----------|-----------|
| [Session IR architecture](session-ir.md) | Canonical in-memory Session IR (history, state, policy), clean-break persistence schema, forensic scene preservation, and multi-pass request compilation |
| [Web tools architecture](web-tools.md) | `search_web` / `read_url` provider selection, credential readiness, and configuration revisioning |
| [Model catalog architecture](model-catalog.md) | Model membership, capability resolution, remote catalog sources, and what the model picker renders |

Blueprints are **HOT**: they are updated in the same change that alters the
behavior they describe. A blueprint that disagrees with the code is a defect in
the blueprint.
