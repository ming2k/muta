# 0179. Tool Modality Orthogonality, Pattern Consistency, and Parameter Ergonomics

- **Status:** Accepted
- **Date:** 2026-09-16
- **Builds on:** ADR-0135 (retirement deletes, no teaching errors), ADR-0143 (filesystem search tool boundaries), ADR-0146 (tool hazard model), ADR-0148 (content-aware mutation signatures)

## Context

Muta's model-facing tool surface evolved incrementally across several milestones:
1. ADR-0143 established single-responsibility boundaries for filesystem search: `list_dir` for shallow traversal, `find_files` for recursive glob discovery, and `search_text` for content matching.
2. In parallel, reading tools bifurcated by content modality: `read_text` for paginated text inspection and `read_image` for visual artifact inspection.

However, three significant friction points persisted across real-world agent interactions:

1. **Modality Asymmetry and Category Confusion (`edit_file` vs `read_text`/`read_image`)**:
   While reading was categorized by target content modality (`read_text`, `read_image`), editing retained the container-level verb `edit_file`. Yet `edit_file` strictly operates on textual strings via unique exact-match replacement (`old_string` -> `new_string`); it cannot edit images, binaries, or audio streams. This asymmetry creates cognitive confusion for the model and blocks clean multimodal tool expansion (e.g. `edit_image` for crop/resize/mask operations).
2. **Unix Mental-Model Collision in `find_files`**:
   `find_files` treated `patterns` as a mandatory JSON field. But in standard developer practice and Unix conventions (`find <dir>`), path is the primary target and filtering is optional. When agents invoked `find_files` with `{"path": "crates"}` to recursively inspect a subtree, parameter deserialization failed immediately (0ms failure: `missing required field: patterns`), forcing an avoidable, costly error-retry loop.
3. **Pattern Matching Dialect Ambiguity**:
   The boundaries between Glob matching, Regular Expressions, and Exact Literal matching lacked a single authoritative contract. Models occasionally attempted regexes in file path finders, or worried that code search with punctuation (`fn new()`) would fail due to regex compilation errors without knowing whether literal search was the default.

Under Muta's zero-compromise architectural discipline (ADR-0135: "Retirement deletes; no teaching errors; no legacy aliases"), we resolve these issues cleanly and comprehensively at the root.

## Decision

We establish an authoritative, orthogonal tool architecture governing modality, pattern dialects, and parameter ergonomics:

```
┌────────────────────────────────────────────────────────────────────────────┐
│ 1. Text Modality Family (*_text)                                           │
│    - read_text   : Range-addressed text reading (offset/limit)             │
│    - search_text : In-file text discovery (Literal by default, optional Regex)│
│    - edit_text   : Deterministic, exact-literal text replacement (Atomic)  │
├────────────────────────────────────────────────────────────────────────────┤
│ 2. Visual Modality Family (*_image)                                        │
│    - read_image  : Image inspection (resolution/formats/downsampling)      │
│    - [Reserved] edit_image : Visual transformations (crop/resize/filter)   │
├────────────────────────────────────────────────────────────────────────────┤
│ 3. Filesystem Node & Container Primitives                                  │
│    - list_dir    : Shallow directory listing (no recursion, no filter)     │
│    - find_files  : Recursive file discovery (Glob patterns, default ["*"]) │
│    - write_file  : Whole-file container creation and overwrite            │
└────────────────────────────────────────────────────────────────────────────┘
```

### 1. Rename `edit_file` to `edit_text` (Zero Legacy Aliases)
- The tool is renamed to `edit_text` across all contracts, agents, prompts, permissions, doom guards, TUI presenters, and documentation.
- Per ADR-0135, no compatibility alias is retained in the model-facing tool catalog. Tool guidance prompts, runner manifests, and execution engines strictly expose and recognize `edit_text`.
- The semantics remain strictly deterministic: exact unique `old_string` matching replaced with `new_string`.

### 2. Three-Tier Pattern Isolation Boundary

Each search/mutation primitive enforces an unbreachable pattern paradigm:

| Paradigm | Supported Syntax | Admitted Operations | Prohibited Operations |
|:---|:---|:---|:---|
| **Glob (Wildcard)** | `*`, `**`, `?`, `[...]` | Path matching only (`find_files.patterns`, `search_text.include`, `search_text.exclude`). Ripgrep/gitignore standard. | Prohibited in file edits; prohibited in raw text matching. |
| **Exact Literal** | UTF-8 exact substring | Text editing (`edit_text.old_string`) and default content search (`search_text.query` with `regex: false`). | Prohibited from expanding wildcards or regex groups. |
| **Regular Expression** | Rust `regex` engine | Content search ONLY when explicitly requested (`search_text.regex = true`). | Prohibited as default; strictly prohibited in file/text mutations. |

### 3. Robust Parameter Ergonomics (Postel's Law)
- **`find_files` patterns defaults to `["*"]`**:
  `patterns` in `FindFilesArgs` becomes `Option<Vec<String>>`. If omitted by the caller (or empty), it automatically defaults to matching all files (`["*"]`). Calling `find_files(path="crates")` succeeds immediately and recursively returns all files under `crates`.
- **String or Array Polymorphism for Globs**:
  While schema definitions declare array types, deserialization transparently accepts either a single string (`"patterns": "*.rs"`) or an array (`"patterns": ["*.rs"]`). This eliminates brittle schema rejections when models omit array brackets.
- **Inter-Tool Parameter Aliasing**:
  `find_files.patterns` accepts `include` as a deserialization alias, and `search_text.include` accepts `patterns` as an alias. Whichever vocabulary the model reaches for, the call succeeds without schema friction.
- **Actionable Self-Healing Error Diagnostics**:
  When `search_text` encounters an invalid regex with `regex: true`, the error response includes explicit guidance:
  `"Invalid regular expression: <details>. If you intended to search for exact literal text with special characters, omit 'regex' or set 'regex': false."`
- **Explanatory Parameter Descriptions**:
  Every tool and parameter schema carries unambiguous usage instructions, explicit defaults, and cross-tool boundaries (e.g. `list_dir` notes `find_files` for recursion, `read_text` points to `read_image` for non-text, `edit_text` explicitly prohibits regex/globs).

## Consequences

### Positive
- **Symmetric and Orthogonal**: Text operations form a clean, intuitive triplet: `read_text`, `search_text`, `edit_text`. Container-level operations remain `write_file`, `list_dir`, `find_files`.
- **Multimodal Readiness**: Future multimodal additions (`edit_image`, `edit_audio`) fit naturally without colliding with generic file tools.
- **Elimination of 0ms Agent Failures**: Models calling `find_files` with only a target path now succeed reliably instead of failing with schema validation errors.
- **KV Cache & Token Economy**: Model prompts and system instructions consistently reinforce a predictable, unified naming convention.

### Negative & Mitigations
- **Code & Test Churn**: Requires updating all call sites, test fixtures, TUI configurations, and documentation referencing `edit_file`.
  - *Mitigation*: Executed in a single atomic migration across all workspace crates and verified via comprehensive test suites.
