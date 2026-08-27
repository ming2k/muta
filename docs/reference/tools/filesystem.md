# Filesystem tools

Read and mutate files and directory listings. `read_text`, `read_image`,
`find_files`, `list_dir`, and `search_text` are `Read`; `write_file` and
`edit_file` are `Write`. Source: `crates/muta-agent/src/tools/`.

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
resolution before sending. For plain-text files use `read_text` instead.

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

## `edit_file`

| Parameter | Type | Required | Notes |
|-----------|------|----------|-------|
| `path` | string | yes | File path |
| `old_string` | string | yes | Must exist verbatim |
| `new_string` | string | yes | Replacement text |

## `find_files`

| Parameter | Type | Required | Default | Notes |
|-----------|------|----------|---------|-------|
| `patterns` | string array | yes | — | Path globs relative to `path`; alternatives are separate array items (OR) |
| `path` | string | no | `.` | Directory to search |
| `exclude` | string array | no | `[]` | Path globs to exclude |
| `max_depth` | integer | no | unlimited | Maximum depth below `path` |
| `limit` | integer | no | `200` | Result cap; maximum `1000` |

Globs use ripgrep-compatible gitignore semantics. A slashless glob matches a
file name at any depth; a leading `/` anchors it to `path`. Pass alternatives
as separate `patterns` items instead of a brace-packed glob. The walker reads
`.gitignore` and `.ignore`, searches hidden paths unless ignored, and always
prunes repository metadata, dependency, and build-output directories.

## `search_text`

| Parameter | Type | Required | Default | Notes |
|-----------|------|----------|---------|-------|
| `query` | string | yes | — | Regular expression, or exact text with `literal` |
| `path` | string | no | `.` | File or directory to search |
| `include` | string array | no | `[]` | File globs relative to `path`; alternatives are separate array items (OR) |
| `exclude` | string array | no | `[]` | File globs to exclude |
| `literal` | boolean | no | `false` | Disable regular-expression parsing |
| `context` | integer | no | `0` | Context lines per match; maximum `10` |
| `limit` | integer | no | `200` | Returned-line cap; maximum `1000` |

Runs in-process with Rust's `regex` engine and ripgrep's `ignore` traversal
library; it does not spawn an `rg` executable. Output is capped at about 32 KB,
and each file contributes at most 50 matches.

## `list_dir`

| Parameter | Type | Required | Default | Notes |
|-----------|------|----------|---------|-------|
| `path` | string | no | `.` | Directory |
| `limit` | integer | no | `200` | Entry cap; maximum `1000` |

Returns only immediate children in stable order. Use `find_files` for
recursive or filtered discovery.
