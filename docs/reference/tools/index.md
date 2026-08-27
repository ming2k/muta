# Built-in tools

The muta agent exposes a fixed set of built-in tools to the model on every
round. MCP server tools are appended at runtime. This is the lookup
surface — one page per tool category. For how tools are gated (access tiers,
capability axes, the permission broker), see [Tool access](access.md).

Most built-in tools live in `muta-agent`'s `tools` module; skill adapters live in
`muta-skills`, MCP adapters in `muta-agent`'s `mcp` module, and `envoy` in
`muta-agent` proper.
The `Tool` trait is defined in
`crates/muta-contracts/src/capability.rs`.

## Registry

Most tools self-register through `inventory` and are collected into a
`ToolSet` by the application. Agent construction automatically adds `todo` and
`todo_update`, bound to that instance's live task-list context. `RunnerTool` is
assembled explicitly because it captures a snapshot of the other tools.

| Tool | Access | Permission scope | Reference page |
|------|--------|------------------|----------------|
| `execute_command` | `Execute` | `command` argument | [execute_command](execute_command.md) |
| `read_text` | `Read` | `*` | [filesystem](filesystem.md) |
| `read_image` | `Read` | `*` | [filesystem](filesystem.md) |
| `write_file` | `Write` | `path` argument | [filesystem](filesystem.md) |
| `edit_file` | `Write` | `path` argument | [filesystem](filesystem.md) |
| `find_files` | `Read` | `*` | [filesystem](filesystem.md) |
| `list_dir` | `Read` | `*` | [filesystem](filesystem.md) |
| `search_text` | `Read` | `*` | [filesystem](filesystem.md) |
| `ask_user` | `Read` | `*` | [interaction](interaction.md) |
| `todo` | `Read` | `*` | [interaction](interaction.md) |
| `todo_update` | `Read` | `*` | [interaction](interaction.md) |
| `webfetch` | `Read` | `*` | [web](web.md) |
| `websearch` | `Read` | `*` | [web](web.md) |
| `spawn_runner` / `runner` | `Read` (spawns runner) | `*` | [runner](envoy.md) |
| `runner_code` | `Read` (spawns runner) | `*` | [runner](envoy.md) |
| `runner_mcp` | `Read` (spawns runner) | `*` | [runner](envoy.md) |
| `use_skill` | `Read` | `*` | [skills](skills.md) |
| `list_skills` | `Read` | `*` | [skills](skills.md) |
| `mcp__<server>__<tool>` | `Read` if server `read_only = true`, else `Write` | `*` | [mcp](mcp.md) |

`permission_scope` defaults to `"*"`. Only `write_file`, `edit_file`, and
`execute_command` override it; their scope string is what a cached `Always` rule matches
against.

Parameters are exposed to the model as JSON Schema via
`Tool::to_openai_function()` (`crates/muta-contracts/src/capability.rs`), which
wraps `Tool::parameters()`.

## See also

- [Tool access](access.md) — access tiers, capability axes, permission broker
- [How to add a tool](../../how-to/add-a-tool.md) — implementing the `Tool` trait
- [Rounds and turns](../../explanation/agent-design/rounds-and-turns.md) — how schemas are
  injected, streamed, and fell back to text
