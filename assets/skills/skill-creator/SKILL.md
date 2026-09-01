---
name: skill-creator
description: Create new skills, modify and improve existing skills, and measure skill performance for muta. Use when users want to create a skill from scratch, edit, or optimize an existing skill, run evals to test a skill, benchmark skill performance with variance analysis, or optimize a skill's description for better triggering accuracy.
---

# Skill Creator for muta

A skill for creating new muta skills and iteratively improving them.

At a high level, the process of creating a skill goes like this:

- **Decide what the skill should do** and roughly how it should do it
- **Write a draft of the skill** adhering to muta's skill architecture and frontmatter specs
- **Create a few test prompts** and run muta agent / sub-runners with access to the skill on them
- **Help the user evaluate results** both qualitatively and quantitatively
  - While runs happen, draft quantitative evals if applicable, then explain them to the user
  - Use `eval-viewer/generate_review.py` (or conversation review) to inspect results and metrics
- **Rewrite and refine the skill** based on user feedback and benchmark flaws
- **Repeat until satisfied**
- **Optimize the skill description** to ensure reliable triggering in muta discovery
- **Install/Deploy** into project scope (`.muta/skills/<name>/`) or user scope (`~/.local/share/muta/skills/<name>/`)

Your job when using this skill is to figure out where the user is in this process and then jump in and help them progress through these stages.

---

## Communicating with the user

Skill creator may be used by developers across a wide range of experience. Pay attention to context cues:

- Clarify terms if you're in doubt, and explain the "why" behind design choices.
- Keep the user in the loop when choosing between project-local (`.muta/skills/`) vs user-global (`~/.local/share/muta/skills/`) storage.
- If the user prefers a fast iteration ("just vibe with me, no formal evals"), adapt flexibly to their preference.

---

## Muta Skill Architecture

Before authoring, understand how muta discovers, loads, and executes skills:

### 1. Scopes and Priority Order

When two skills share the same name, higher-priority scopes override lower ones:

| Scope | Location | Priority | Typical Use |
|-------|----------|----------|-------------|
| **Repo** | `.muta/skills/<name>/` or `skills/<name>/` | Highest (3) | Workspace/repo-specific conventions, project build workflows |
| **Extra** | Configured in `[skills] paths` | Higher (2) | Custom local skill directories |
| **User** | `~/.local/share/muta/skills/<name>/` (XDG) | Normal (1) | Global personal skills available across all workspaces |
| **Remote** | Configured in `[skills] urls` | Lowest (0) | Team or community shared remote skill bundles |

*Note on Workspace Trust:* Project-local skills (`.muta/skills/`) are protected by muta's workspace security attestation. In un-trusted workspaces, they remain quarantined until approved with `/trust`.

### 2. Anatomy of a Muta Skill

```
<skill-name>/
├── SKILL.md (required)
│   ├── YAML frontmatter (name, description required)
│   └── Markdown instructions
└── Bundled Resources (optional)
    ├── scripts/    - Executable scripts or automation tools
    ├── references/ - Domain manuals, API docs, schemas loaded as needed
    └── assets/     - Templates, configurations, boilerplates
```

### 3. Progressive Disclosure Model

Muta skills use a three-tier loading model to conserve context window and cost:

1. **Discovery / Metadata** (~50-100 words): Only `name` and `description` are loaded during catalog listing and system prompts.
2. **Skill Body (`SKILL.md`)** (< 500 lines recommended): Loaded lazily when invoked via `use_skill` or triggered by mention (`@<skill-name>`).
3. **Bundled Resources**: Loaded on demand only when referenced or executed.

### 4. Frontmatter Specification

Muta parses YAML frontmatter in `SKILL.md`. Supported fields:

```yaml
---
name: my-skill                         # Required: kebab-case identifier (max 64 chars)
description: Detailed trigger guidance # Required: What it does & when to use (max 1024 chars)
short-description: Brief summary       # Optional: fallback for compact UI/catalog
version: 0.1.0                         # Optional: semantic version string
tags: [backend, testing, rust]        # Optional: categorization tags
policy:
  allow_implicit_invocation: true      # Optional: true to allow @mention auto-loading
dependencies:                          # Optional: required tools or MCP servers
  - type: mcp
    value: postgres-mcp
    description: Database query tool
---
```

---

## Step-by-Step Skill Creation Guide

### Step 1: Capture Intent & Scope

Extract answers from the conversation or interview the user:

1. **What should this skill enable the agent to do?** (Domain knowledge, multi-step workflow, style guide, code generation)
2. **When should it trigger?** (Keywords, file types, user problem contexts)
3. **Where should it live?**
   - Project-local: `.muta/skills/<skill-name>/`
   - User-global: `~/.local/share/muta/skills/<skill-name>/`
4. **Does it need test cases?**
   - Deterministic workflows (code transformations, schemas, packaging) benefit from quantitative test cases.
   - Subjective workflows (writing style, design advice) work best with qualitative review.

### Step 2: Write the `SKILL.md`

Follow these best practices:

- **Crafting `description`**: The description is the single most important factor for accurate triggering. Include both **what** the skill does AND **specific trigger scenarios** (e.g. *"Use when the user asks to refactor database queries, optimize SQL performance, or create migrations"*).
- **Imperative & Action-Oriented**: Write clear, direct instructions for the agent.
- **Explain the "Why"**: Give reasoning behind rules rather than relying solely on arbitrary caps or constraints.
- **Structure Reference Pointers**: If detailed documentation exceeds 300 lines, extract it to `references/<topic>.md` and instruct the model when to read it using `read_text`.
- **Provide Concrete Examples**: Include input/output snippets and common edge cases.

### Step 3: Set Up Test Cases (Optional but Recommended)

For skills with testable outcomes, create test cases in `evals/evals.json`:

```json
{
  "skill_name": "my-skill",
  "evals": [
    {
      "id": 1,
      "prompt": "Refactor the user repository to use connection pooling",
      "expected_output": "Updated repository code with pool initialization and error handling",
      "files": []
    }
  ]
}
```

### Step 4: Run Evals & Compare Baselines

Organize evaluation workspace into `<skill-name>-workspace/iteration-<N>/`:

1. **Run with-skill test**: Execute the prompt with access to the new skill. In muta, you can use `spawn_runner` to execute in an isolated environment.
2. **Run baseline test**:
   - For new skills: run without the skill (`without_skill/`).
   - For improved skills: run against the snapshot of the previous version (`old_skill/`).
3. **Grade against assertions**:
   - Programmatic checks (file exists, compiles, matches regex).
   - Use grader agent instructions (`agents/grader.md`) for detailed verification.
4. **Aggregate results**:
   ```bash
   python3 -m scripts.aggregate_benchmark <workspace>/iteration-1 --skill-name <name>
   ```

### Step 5: Review & Feedback Loop

1. **Present Results**: Launch the eval review viewer or present side-by-side outputs to the user:
   ```bash
   python3 eval-viewer/generate_review.py <workspace>/iteration-1 --skill-name "my-skill"
   ```
2. **Gather Feedback**: Identify gaps, edge-case failures, or prompt ambiguities.
3. **Refine `SKILL.md`**: Update instructions, extract repeated patterns into `scripts/` or `references/`, and iterate until quality criteria are met.

### Step 6: Validate & Optimize Description

1. **Validate format**:
   ```bash
   python3 scripts/quick_validate.py <path-to-skill-directory>
   ```
2. **Description Optimization**: Check that description triggers on representative positive queries and avoids triggering on near-miss negative queries.

### Step 7: Packaging & Distribution

If distributing or sharing the skill:
```bash
python3 scripts/package_skill.py <path/to/skill-folder>
```
Or simply place the skill folder in:
- `.muta/skills/<skill-name>/` (Project repository)
- `~/.local/share/muta/skills/<skill-name>/` (User global)

Then verify discovery with:
```bash
muta skill ls
```

---

## Reference Resources & Helpers

- `agents/grader.md` — Detailed instructions for assertion verification and grading
- `agents/comparator.md` — Blind A/B comparison between skill versions
- `agents/analyzer.md` — Identifying variance and benchmark trends
- `references/schemas.md` — JSON specifications for evals, metrics, and benchmarks
- `scripts/quick_validate.py` — SKILL.md validator
- `scripts/package_skill.py` — Skill packaging tool
