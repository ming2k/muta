# Built-in tools

The neenee agent exposes a fixed set of built-in tools to the model on every
round. MCP server tools are appended at runtime. This is the lookup
surface — one page per tool category. For how tools are gated (access tiers,
capability axes, the permission broker), see [Tool access](access.md).

Most built-in tools live in `neenee-agent`'s `tools` module; skill adapters live in
`neenee-skills`, MCP adapters in `neenee-agent`'s `mcp` module, and `envoy` in
`neenee-agent` proper.
The `Tool` trait is defined in
`crates/neenee-core/src/capability.rs`.

## Registry

Most tools self-register through `inventory` and are collected into a
`ToolSet` by the application. Agent construction automatically adds `todo` and
`todo_update`, bound to that instance's live task-list context. `EnvoyTool` is
assembled explicitly because it captures a snapshot of the other tools.

| Tool | Access | Permission scope | Reference page |
|------|--------|------------------|----------------|
| `bash` | `Execute` | `command` argument | [bash](bash.md) |
| `read_file` | `Read` | `*` | [filesystem](filesystem.md) |
| `read_image` | `Read` | `*` | [filesystem](filesystem.md) |
| `write_file` | `Write` | `path` argument | [filesystem](filesystem.md) |
| `edit_file` | `Write` | `path` argument | [filesystem](filesystem.md) |
| `grep` | `Read` | `*` | [filesystem](filesystem.md) |
| `glob` | `Read` | `*` | [filesystem](filesystem.md) |
| `list_dir` | `Read` | `*` | [filesystem](filesystem.md) |
| `ask_user` | `Read` | `*` | [interaction](interaction.md) |
| `todo` | `Read` | `*` | [interaction](interaction.md) |
| `todo_update` | `Read` | `*` | [interaction](interaction.md) |
| `webfetch` | `Read` | `*` | [web](web.md) |
| `websearch` | `Read` | `*` | [web](web.md) |
| `envoy` | `Read` (spawns envoy) | `*` | [envoy](envoy.md) |
| `envoy_code` | `Read` (spawns envoy) | `*` | [envoy](envoy.md) |
| `search_history` | `Read` | `*` | [skills](skills.md) |
| `use_skill` | `Read` | `*` | [skills](skills.md) |
| `list_skills` | `Read` | `*` | [skills](skills.md) |
| `mcp__<server>__<tool>` | `Read` if server `read_only = true`, else `Write` | `*` | [mcp](mcp.md) |

`permission_scope` defaults to `"*"`. Only `write_file`, `edit_file`, and
`bash` override it; their scope string is what a cached `Always` rule matches
against.

Parameters are exposed to the model as JSON Schema via
`Tool::to_openai_function()` (`crates/neenee-core/src/capability.rs`), which
wraps `Tool::parameters()`.

## See also

- [Tool access](access.md) — access tiers, capability axes, permission broker
- [How to add a tool](../../how-to/add-a-tool.md) — implementing the `Tool` trait
- [Rounds and turns](../../explanation/agent-design/rounds-and-turns.md) — how schemas are
  injected, streamed, and fell back to text
