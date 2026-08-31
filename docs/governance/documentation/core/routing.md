# Routing

Use this page to decide where new documentation belongs. Documentation in
adopting repositories is routed through **four sequential gates**, followed
by a fixed root-file location exception.

The gates are not parallel axes. They are a priority-ordered cascade:
each gate closes a question before the next gate is considered, so every
document lands in exactly one location. Apply the gates in order and stop at
the first match.

## Gate 1 — Time: durable decision records

Question: *Is this a durable technical decision and its historical context,
recorded for the lifetime of the project?*

If yes: `docs/adr/NNNN-<slug>.md`. Stop.

Architectural Decision Records are immutable once accepted. They are not
learning material and not reference material; they are a historical record of
*why* a decision was made. Once a document enters this gate, the
cognitive-mode questions of Gate 4 do not apply to it.

See the [Architecture Profile](../profiles/architecture/profile.md) for ADR lifecycle rules.

## Gate 2 — Governance: architectural charter, review gates, and guidelines

Question: *Is this document a project-wide architectural constitution, an API
design guideline, or a multi-domain intake/review gate SOP?*

If yes: `docs/governance/`. Stop.

Top-level governance documents define the mandatory rules, review filters, and
lifecycle standards that all code in the repository must obey. They sit above
internal developer tooling (`docs/dev/`) and are accessible to both core
maintainers and external contributors. Sub-types include:
- `docs/governance/index.md` — The governance charter, review gates overview, and contributor PR checklist.
- `docs/governance/api-design-guidelines.md` — Interface rules, naming invariants, and memory safety contracts.
- `docs/governance/<domain>-governance.md` — Subsystem-specific intake SOPs.
- `docs/governance/documentation/` — This documentation governance standard.

## Gate 3 — Audience: contributor-only

Question: *Is the only intended reader a project contributor (setup, build,
test, release, internal maintenance)?*

If yes: `docs/dev/`. Stop.

`docs/dev/` is the firewall between contributor knowledge and user knowledge.
User-facing documentation must never link into it. Within `docs/dev/`,
documents include setup, testing, acceptance journeys, experience scenarios,
project layout, postmortems, and release processes.

Explanation documents under `docs/explanation/` are dual-audience:
contributors read them for architectural context, but they live on the user
side of the firewall because users need them too. Contributor docs may link
out to explanation docs; the reverse direction is forbidden.

## Gate 4 — Cognitive mode: user-facing Diátaxis

Question: *How is the user engaging with the material?*

[Diátaxis](https://diataxis.fr/) is a documentation framework that splits
content by cognitive mode: learning, doing, looking up, and understanding.
Apply it to everything that passes through Gates 1, 2, and 3:

| Directory | Mode | Style |
|-----------|------|-------|
| `docs/tutorials/` | Learning | Second person. Guarantee success. State the expected outcome at every step. |
| `docs/how-to/` | Doing | Imperative mood. Titles start with "How to". Assume the reader knows the basics. |
| `docs/reference/` | Lookup | Prefer tables and lists. Keep prose minimal and factual. Completeness over narrative flow. |
| `docs/explanation/` | Understanding | Discursive; opinionated when useful. Link to ADRs for specific decision history. |

If content seems to belong in two Diátaxis directories, split it into two
documents rather than blending the styles.

## Root files (location exception)

Some files must live at the repository root because tooling, hosting
platforms, or community convention look for them there. This is a **location
constraint**, orthogonal to content routing. Root files are not a fifth gate;
they are a fixed list with a fixed purpose each:

| File | Conceptual home | Why it is at root |
|------|-----------------|-------------------|
| `README.md` | User pitch (Gate 3 adjacent) | Hosting platforms render it by default |
| `CHANGELOG.md` | Time-adjacent: user-facing release history | Community convention |
| `CONTRIBUTING.md` | Gate 2: contributor workflow | Hosting platforms surface it on PR/issue prompts |
| `AGENTS.md` | Gate 2: contributor / AI workflow | AI agents and tooling inspect root instructions |
| `SECURITY.md` | Mixed audience: users and researchers | Hosting platforms surface it |
| `LICENSE` | Not classified | Legal requirement |
| `CODE_OF_CONDUCT.md` | Not classified | Community requirement |

Route content into a root file only when it matches that file's fixed
purpose. Do not invent new root markdown files to escape the gates; route into
`docs/` instead.

## Decision order (summary)

1. Durable technical decision? → `docs/adr/`
2. Project-wide governance or charter? → `docs/governance/`
3. Contributor-only? → `docs/dev/`
4. User-facing: learning → `docs/tutorials/`; doing → `docs/how-to/`; lookup
   → `docs/reference/`; understanding → `docs/explanation/`
5. Matches one of the fixed root files above? → repository root
6. Otherwise: it does not belong on this repository's documentation surface.

## Hard boundaries

- Do not create monolithic documentation pages such as `Documentation.md` or `Guide.md`.
- Do not duplicate the `README.md` quick start inside `docs/`; link to it.
- Do not put design rationale in reference pages; move it to explanation docs or ADRs.
- Do not put implementation coordinates in explanation docs — no source file paths or line numbers, no struct field listings, no API signatures, no fenced source blocks. Keep explanation conceptual; move the detail to `docs/reference/` and link from there.
- Do not put option tables in tutorials; link to reference docs.
- Do not mix user docs and contributor docs; `docs/dev/` is the firewall.
- Do not create an empty documentation directory without an `index.md`.
- Do not hide project-specific assumptions inside generic governance pages; record them as repository contracts in [Repository Contracts](../contracts.md).
