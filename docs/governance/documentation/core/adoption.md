# Adoption

Use this guide when adopting `docs-governance` in a new repository.

---

## 1. Pre-Adoption Decisions

Decide these baseline settings before adopting:

| Decision | Default | Alternatives |
|----------|---------|--------------|
| Documentation Language | American English | Bilingual / Project locale |
| Content Taxonomy | Diátaxis under `docs/` | Customized taxonomy |
| Active Profiles | `core`, `validation`, `architecture` | `core` only, or customized subset |
| Contributor Docs Path | `docs/dev/` | Project-specific dev path |
| Architecture Records Path | `docs/adr/` | Disabled if not using ADRs |

---

## 2. Adoption Steps

1. **Install Core and Profiles**:
   Run the synchronization tool to copy core governance and selected profiles:
   ```bash
   ./tools/sync.sh <target-repo-path>
   ```

2. **Configure Repository Contracts**:
   Open `<target-repo-path>/docs/governance/documentation/contracts.md` and
   declare your active profiles and repository path bindings.

3. **Establish Directory Skeletons**:
   Ensure `docs/index.md`, `docs/governance/index.md`, `docs/dev/index.md`, and
   an `index.md` for each adopted documentation directory exist.

4. **Install Root AGENTS.md**:
   Add or update the root `AGENTS.md` in the target repository to instruct AI
   agents to follow `docs/governance/documentation/`.

5. **Integrate Verification into CI**:
   Add `./tools/verify.sh .` to your CI pull request validation workflow.

6. **Verify Initial Compliance**:
   ```bash
   ./tools/verify.sh <target-repo-path>
   ```
