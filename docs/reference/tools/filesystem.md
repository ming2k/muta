# Filesystem tools

Read and mutate files and directory listings. `read_text`, `read_image`,
`find_files`, `list_dir`, `code_query`, and `search_text` are `Read`;
`write_file` and `edit_text` are `Write`. Source:
`crates/muta-agent/src/tools/`.

Relative paths resolve from the primary workspace. An absolute path is
accepted only when it is inside the primary or an explicitly admitted
additional workspace root — or the implicit platform temp roots
(`$TMPDIR` and `/tmp` on Unix, both raw and canonical spellings), which are
always admitted so scratch workflows (spill files, staging dirs, probes)
work without configuring `[workspace].additional_roots`.

## `read_text`

| Parameter | Type | Required | Default | Notes |
|-----------|------|----------|---------|-------|
| `path` | string | yes | — | File path |
| `offset` | integer | no | — | 1-based start line |
| `limit` | integer | no | — | Max lines |

## `read_image`

| Parameter | Type | Required | Default | Notes |
|-----------|------|----------|---------|-------|
| `path` | string | yes | — | Image file path |

Reads an image file (PNG, JPEG, GIF, WebP) and delivers it inline so a
vision-capable model can see it. Large images are auto-resized to a sensible
resolution before sending.

The image is returned as a structured `ToolOutput::Image` and delivered to the
model out-of-band: the tool result message carries a short text placeholder,
and the harness injects the actual image into a follow-up user-role message.
This mirrors how opencode lowers images out of tool results for OpenAI Chat
Completions providers (whose tool messages only accept string content), so it
works across kimi / GLM / OpenAI / Gemini.

## `write_file`

| Parameter | Type | Required | Notes |
|-----------|------|----------|-------|
| `path` | string | yes | File path |
| `content` | string | yes | Full content; overwrites |

## `edit_text`

| Parameter | Type | Required | Notes |
|-----------|------|----------|-------|
| `path` | string | yes | Path to text file; relative paths use primary workspace |
| `old_string` | string | yes | Exact verbatim text block to replace; must match uniquely |
| `new_string` | string | yes | Replacement text to insert in place of `old_string` |

### Mutation outcomes and syntax warnings

Both mutation tools write through the execution-environment filesystem before
reporting syntax diagnostics. Malformed JSON, TOML, and supported source-language
content is written successfully, including new files, overwrites, and partial
repairs of already-invalid files. Intermediate invalid states are allowed; no
skip parameter is required or provided.

All successful results retain `ToolOutput::Patch` and its rich diff. Parser
diagnostics populate its `warnings` string array (omitted when empty; older
payloads default to empty), with `Write succeeded` and an explicit
`Warning (non-blocking syntax diagnostic)`. The UI displays warnings alongside
the diff; model-facing and legacy text output include them with the patch summary.
Warnings describe the committed candidate and do not roll it back. Repair the file in subsequent edits. Unsupported
formats are not claimed to be validated; syntax checks are not compilation or
type checking.

Argument validation, path authorization, unique matching, and filesystem failures
remain hard errors. Failed writes do not produce success warnings. The
code-intelligence extension does not veto syntax errors. Policy:
[ADR-0233](../../adr/0233-non-blocking-mutation-syntax-diagnostics.md).

## `find_files`

| Parameter | Type | Required | Default | Notes |
|-----------|------|----------|---------|-------|
| `patterns` | string or string array | no | `["*"]` | Path globs relative to `path` (e.g. `["*.rs"]`); alternatives are ORed. Accepts `include` as alias. Defaults to all files if omitted |
| `path` | string | no | `.` | Directory to search; relative paths use primary workspace |
| `exclude` | string or string array | no | `[]` | Path globs to exclude (e.g. `["target/**"]`) |
| `max_depth` | integer | no | unlimited | Maximum depth below `path` (>= 1) |
| `limit` | integer | no | `200` | Result cap; maximum `1000` |

Globs use ripgrep-compatible gitignore semantics. A slashless glob matches a
file name at any depth; a leading `/` anchors it to `path`. Pass alternatives
as separate `patterns` items instead of a brace-packed glob. The walker reads
`.gitignore` and `.ignore`, searches hidden paths unless ignored, and always
prunes repository metadata, dependency, and build-output directories.

## `search_text`

| Parameter | Type | Required | Default | Notes |
|-----------|------|----------|---------|-------|
| `query` | string | yes | — | Exact text to search for (default), or regex pattern when `regex` is true |
| `path` | string | no | `.` | Directory or file to search; relative paths use primary workspace |
| `include` | string or string array | no | `[]` | File globs relative to `path` (e.g. `["*.rs"]`). Accepts `patterns` as alias |
| `exclude` | string or string array | no | `[]` | File globs to exclude |
| `regex` | boolean | no | `false` | Treat query as regular expression instead of literal text |
| `context` | integer | no | `0` | Context lines per match; maximum `10` |
| `limit` | integer | no | `200` | Returned-line cap; maximum `1000` |

Runs in-process with Rust's `regex` engine (escaped by default for safe literal matching) and ripgrep's `ignore` traversal
library; it does not spawn an `rg` executable. Output is capped at about 32 KB,
and each file contributes at most 50 matches.

## `code_query`

One tool, three modes, one parser. `outline` summarises a file; `symbol`
returns a declaration's source; `find` locates declarations by kind and name
across a scope.

| Parameter | Type | Required | Default | Notes |
|-----------|------|----------|---------|-------|
| `mode` | string | yes | — | `outline`, `symbol`, or `find` |
| `path` | string | no | `.` | File for `outline`/`symbol`; file or directory scope for `find` |
| `symbol` | string | for `symbol` | — | Declaration name, optionally container-qualified (`Service::run`) |
| `pattern` | string | for `find` | — | `kind[:name-glob]` clauses, comma/space separated and ORed |
| `limit` | integer | no | `200` | Entry cap; maximum `1000` |
| `budget` | integer | no | `16384` | Result byte cap; maximum `131072` |

```text
pattern := clause ( ( ',' | whitespace ) clause )*
clause  := kind [ ':' name_glob ]
kind    := fn | method | struct | enum | trait | impl | class
         | interface | type | const | static | mod | macro
```

The kind vocabulary is **closed**: the model never writes a tree-sitter query,
so a wrong kind is rejected with the legal list named rather than failing as a
malformed S-expression. `name_glob` supports `*` and `?` only. The `fn` clause
also matches `method`, so a caller need not know whether a function sits inside
an `impl` or `class`; `method` stays exact.

Results are a syntactic summary — never a complete AST, type analysis, or
dependency proof:

- **Bounded.** Input is capped at 2 MiB per file and a scope-wide `find` scans
  at most 2000 files / 64 MiB; output is capped by `limit` and `budget`. Every
  truncation is disclosed, and each file from `find`/`symbol` is capped at 400
  lines.
- **Versioned.** Every result carries the content-addressed version of the
  bytes it described. That version is the value `edit_text` and `write_file`
  accept as `expected_version`.
- **Honest.** Unsupported extensions, oversized inputs, and empty scopes are
  reported as such; nothing is silently clipped or silently empty.

Relative paths resolve against the primary workspace, and a scope-wide query
walks through the shared ignore rules (`find_files` / `search_text`), so it
never wades into build output or vendored trees.

Code structure enters model context only through this scoped, on-demand query:
no facet projects an automatic repository-wide map, and a returned result is
history-bearing evidence that is not rewritten when the source later changes.
See [Model context](../../explanation/agent-design/model-context.md).

## `list_dir`

| Parameter | Type | Required | Default | Notes |
|-----------|------|----------|---------|-------|
| `path` | string | no | `.` | Directory |
| `limit` | integer | no | `200` | Entry cap; maximum `1000` |

Returns only immediate children in stable order. Use `find_files` for
recursive or filtered discovery.
