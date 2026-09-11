# Built-in tools

The muta agent exposes a fixed set of built-in tools to the model on every
round. MCP server tools are appended at runtime. This is the lookup
surface — one page per tool category. For how tools are gated (access tiers,
capability axes, the permission broker), see [Tool access](access.md).

Most built-in tools live in `muta-agent`'s `tools` module; skill adapters live in
`muta-skills`, MCP adapters in `muta-agent`'s `mcp` module, and `subagent` in
`muta-agent` proper.
The `Tool` trait is defined in
`crates/muta-contracts/src/capability.rs`.

## Registry

Most tools self-register through `inventory` and are collected into a
`ToolSet` by the application. Agent construction adds `todo`, bound to that
instance's live task-list context. `SubagentTool` is assembled explicitly
because it captures a snapshot of the other tools.

| Tool | Access | Permission scope | Reference page |
|------|--------|------------------|----------------|
| `execute_command` | `Execute` | `command` argument | [execute_command](execute_command.md) |
| `read_text` | `Read` | `*` | [filesystem](filesystem.md) |
| `read_image` | `Read` | `*` | [filesystem](filesystem.md) |
| `write_file` | `Write` | `path` argument | [filesystem](filesystem.md) |
| `edit_text` | `Write` | `path` argument | [filesystem](filesystem.md) |
| `find_files` | `Read` | `*` | [filesystem](filesystem.md) |
| `list_dir` | `Read` | `*` | [filesystem](filesystem.md) |
| `search_text` | `Read` | `*` | [filesystem](filesystem.md) |
| `code_query` | `Read` | `*` | [filesystem](filesystem.md) |
| `ask_user` | `Read` | `*` | [interaction](interaction.md) |
| `todo` | `Read` | `*` | [interaction](interaction.md) |
| `read_url` | `Read` | `*` | [web](web.md) |
| `search_web` | `Read` | `*` | [web](web.md) |
| `spawn_agent` | `Read` (spawns subagent) | `*` | [subagent](subagent.md) |
| `process` | `Read` | `*` | [execute_command](execute_command.md#the-process-tool) |
| `mcp__<server>__<tool>` | `Read` if server `read_only = true`, else `Write` | `*` | [mcp](mcp.md) |

`permission_scope` defaults to `"*"`. Only `write_file`, `edit_text`, and
`execute_command` override it; their scope string is what a cached `Always` rule matches
against.

`todo` is installed by `Agent::new` rather than self-registering, so it is absent
from a context built with only `collect_toolset`. `spawn_agent` selects the
child's role from the `role` enum documented in [subagent](subagent.md) — those
roles are arms of one tool, not separate tools, and the enum is also the
enforced contract (an unadvertised role is refused, not silently defaulted).

The published surface is pinned by a token-budget test
(`crates/muta-agent/src/tools/tests.rs`): every schema listed here is paid for in
every request, so adding one means raising that budget deliberately.

Parameters are exposed to the model as JSON Schema via
`Tool::to_openai_function()` (`crates/muta-contracts/src/capability.rs`), which
wraps `Tool::parameters()`.

## See also

- [Tool access](access.md) — access tiers, capability axes, permission broker
- [How to add a tool](../../how-to/add-a-tool.md) — implementing the `Tool` trait
- [Rounds and turns](../../explanation/agent-design/rounds-and-turns.md) — how schemas are
  injected, streamed, and fell back to text
