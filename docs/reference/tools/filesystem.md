# Filesystem tools

Read and mutate files and directory listings. `read_text`, `read_image`,
`find_files`, `list_dir`, and `search_text` are `Read`; `write_file` and
`edit_text` are `Write`. Source: `crates/muta-agent/src/tools/`.

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

## `list_dir`

| Parameter | Type | Required | Default | Notes |
|-----------|------|----------|---------|-------|
| `path` | string | no | `.` | Directory |
| `limit` | integer | no | `200` | Entry cap; maximum `1000` |

Returns only immediate children in stable order. Use `find_files` for
recursive or filtered discovery.
