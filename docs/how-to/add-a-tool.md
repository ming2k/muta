# How to add a built-in tool

This guide walks through implementing a new tool that the agent can call. It
assumes familiarity with the `Tool` trait. For the existing tool catalog,
see [Built-in tools](../reference/tools/index.md). For the protocol the model uses
to call tools, see
[Rounds and turns](../explanation/agent-design/rounds-and-turns.md).

Most built-in tools live in `muta-agent`'s `tools` module. Pick the module that
matches the tool's domain: filesystem and web tools go in
`crates/muta-agent/src/tools/`, slash-command discovery and project
scaffolding live in `crates/muta-runtime/src/` (`commands` and `project`),
MCP adapters live in `crates/muta-mcp/src`,
and skill tools live in `crates/muta-skills`. `envoy` likewise
lives in `crates/muta-agent/src/` because it constructs agents.
The todo tools in `crates/muta-agent/src/tools/todo.rs` receive their
agent-owned state through `TodoToolContext`, injected by the agent's private
integration module.

## Implement the `Tool` trait

Define a struct and implement `Tool`
(`crates/muta-contracts/src/capability.rs`). The four required members are
`name`, `description`, `parameters`, and `call`.

```rust
pub struct CountLinesTool;

#[async_trait]
impl Tool for CountLinesTool {
    fn name(&self) -> &str {
        "count_lines"
    }

    fn description(&self) -> &str {
        "Count the number of lines in a file."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"}
            },
            "required": ["path"],
            "additionalProperties": false
        })
    }

    async fn call(&self, arguments: &str) -> Result<String, String> {
        let path = serde_json::from_str::<serde_json::Value>(arguments)
            .ok()
            .and_then(|v| v.get("path").and_then(|p| p.as_str()).map(str::to_string))
            .ok_or("missing \"path\"")?;
        let content = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())?;
        Ok(content.lines().count().to_string())
    }
}
```

`parameters()` returns a JSON Schema. It is forwarded verbatim to the model
through `Tool::to_openai_function()`; no tool overrides that
default. Keep the schema strict: set `additionalProperties: false` and list
every required field so the model cannot invent extra keys.

## Return structured output (`ToolOutput`)

Implement `call()` for the model-facing string result, then override
`call_structured()` (`crates/muta-contracts/src/capability.rs`) to return a typed
[`ToolOutput`](../adr/0001-tool-rendering-redesign.md) so the UI renders from
data instead of a sniffed string. The default `call_structured()` just wraps
`call()`'s string as `ToolOutput::Text`, so this is optional but recommended
for any tool whose result has structure (a shell exit code, a file listing, a
diff, …). `call()` should delegate back through `to_text()` so both paths stay
consistent:

```rust
async fn call(&self, arguments: &str) -> Result<String, String> {
    self.call_structured(arguments).await.map(|o| o.to_text())
}

async fn call_structured(&self, arguments: &str) -> Result<crate::ToolOutput, String> {
    // …do the work…
    Ok(crate::ToolOutput::Code {
        lang: Some("rs".into()),
        text,
        start_line: 0,
        prefix: None,
        suffix: None,
    })
}
```

The variants (`Text`, `Error`, `Shell`, `Code`, `Listing`, `Matches`) live in
`crates/muta-contracts/src/tool_output.rs`. `bash` is the reference example — it
also overrides `call_structured_with_events` to stream stdout live via
`ToolStream`.

> **Note on `call_structured_with_events`.** If you override it (rare — only
> `bash` and the envoy `task` tool do), the signature now takes a final
> `stdin: StdinPolicy` argument. Non-shell tools ignore it (it defaults to
> `StdinPolicy::Closed`, which gives a child no stdin); the default
> `call`/`call_structured` delegations pass `Closed` for you, so most tools
> are unaffected. See ADR-0043 for the full stdin execution contract.

## Choose a `ToolAccess`

Override `access()` (`crates/muta-contracts/src/capability.rs`) only when the
tool is read-only. The default is `ToolAccess::Write`, which is the safe
choice for any tool with side effects.

```rust
fn access(&self) -> ToolAccess {
    ToolAccess::Read
}
```

`Read` tools bypass the permission broker and pass the write-scope gate. `Write`
tools prompt the user once per `(tool, scope)` pair unless an `Always` rule
is cached. See [Built-in tools](../reference/tools/access.md) for the
full gating matrix.

## Override `permission_scope` for write tools

A `Write` tool should override `permission_scope`
(`crates/muta-contracts/src/capability.rs`) so cached `Always` rules match the
smallest stable resource identifier. The default `"*"` causes any approval
to authorize all future calls to that tool, which is rarely what users
want.

```rust
fn permission_scope(&self, arguments: &str) -> String {
    json_string(arguments, "path")
}
```

`json_string` (`crates/muta-agent/src/tools/helpers.rs`) extracts a JSON field
from the arguments string and falls back to `"*"`. Existing scopes: file
tools use the `path` argument, `bash` uses the full `command` text. Pick a scope that distinguishes
meaningfully different invocations but is stable across retries of the same
invocation.

## Override `permission_label` / `permission_description` when needed

`Tool::description()` is sent to the model and is often written as
instruction prose ("Call this only when…", "Do not infer…"). That text is
fine for the model but confusing when the user reads it in a permission
prompt. Two trait methods control what the prompt shows instead
(`crates/muta-contracts/src/capability.rs`):

- `permission_label()` (default: `name()`) — the header title.
- `permission_description()` (default: `description()`) — the body shown
  under "Details".

Override either only when the default would puzzle a user. Keep
`permission_description()` to one or two plain sentences describing *what
the call does*, not *when the model should call it*.

```rust
fn permission_label(&self) -> String {
    "Create project".to_string()
}

fn permission_description(&self) -> String {
    "Create a new project directory with the given name and path.".to_string()
}
```

Both overrides are UI-only: they never reach the model and are not part of the
function schema sent to providers.

## Optional: stream sub-task events

If the tool spawns long-running work that should surface in the TUI,
override `call_with_events` (`crates/muta-contracts/src/capability.rs`) instead
of `call`. The default implementation delegates to `call`, so overriding
`call` alone is enough for synchronous tools.

`EnvoyTool` (`crates/muta-agent/src/envoy_tool.rs`) is currently the only
tool that overrides `call_with_events`. It forwards `SubTaskEvent`s from
the envoy so the parent harness can render live progress. Read its
implementation before adopting the same pattern; the event surface is
narrow.

## Register the tool

Register a context-free tool beside its implementation. The application
collects these submissions through `inventory`, so no central tool list needs
editing.

```rust
muta_contracts::register_tool!(CountLinesFactory => CountLinesTool);
```

If a tool needs runtime services, use the context-aware macro form and return
`None` when the service is unavailable. A tool that needs state created inside
the agent still belongs in `muta-agent`'s `tools` module when it only
consumes that state;
construct it in `muta-agent::tool_integration` so every agent lifecycle gets
the same binding. Tools that create or control agents remain in
`muta-agent` proper.

An embedding can add a runtime-selected or product-specific tool while
constructing an agent:

```rust
let agent = Agent::builder(provider, tools, identity)
    .with_tool(Arc::new(MyProductTool::new(service)))
    .with_skills(skills)
    .build();
```

Use `with_tools` for an iterator. Agent-owned tool identities take precedence
over caller-supplied tools with the same name and variant, so an embedding
cannot accidentally detach `todo` from that agent's state.

Tools collected before `EnvoyTool` construction are available for its
snapshot. Admission is by capability axis: read-only, non-interactive tools
can enter the `EXPLORE` profile, while write tools and user-interactive tools
are excluded. See
[Envoys → Tool admission](../explanation/agent-design/envoys.md#tool-admission)
and [ADR-0011](../adr/0011-subagent-profiles.md).

## Verify

Run the test suite before relying on the new tool:

```bash
cargo test -p muta-contracts
cargo test -p mutx
```

Then exercise the tool manually:

1. Start the agent with a provider that supports native function calling
   (see [Providers](../reference/providers.md)).
2. Ask the model to perform a task that should trigger the new tool.
3. Confirm the tool step renders with the right name, arguments, and
   result.
4. Switch to `GoogleProvider` (`Google`) and repeat to confirm a second
   native tool-call wire format works.
5. Switch to a provider that does not serialize `ModelRequest.tool_specs`
   (e.g. a test adapter that returns a canned reply), and repeat. The model
   should emit the universal fallback JSON and the tool should still execute
   through `parse_tool_call`.

If the tool is `Write`, also confirm the permission modal appears on first
use and that an `Always` decision is cached against the scope returned by
`permission_scope`.

## Update documentation

Update these surfaces in the same change:

- Add a row to the table in [Built-in tools](../reference/tools/index.md).
- If the tool introduces a new permission scope shape, document it under
  the tool's parameter table.
- If the tool changes how the harness behaves on a round, update
  [Harness architecture](../explanation/agent-design/harness.md).

## See also

- [Built-in tools](../reference/tools/index.md) — existing tool catalog
- [Rounds and turns](../explanation/agent-design/rounds-and-turns.md) — schema injection and
  fallback mechanics
- [Provider capabilities](../explanation/provider-capabilities.md) — why
  tool support varies across providers
