#!/usr/bin/env python3
"""A tiny, dependency-free MCP Streamable-HTTP server used to validate muta's
MCP HTTP transport.

Speaks the POST + JSON/SSE subset of the Streamable HTTP transport muta uses:

  - POST /mcp with an `initialize` request  -> JSON body, `Mcp-Session-Id` header
  - POST /mcp with `tools/list` / `tools/call` -> `text/event-stream` body with
    a single `data:` frame carrying the JSON-RPC reply (the harder of the two
    legal content shapes, so muta's SSE parsing is exercised)

Exposes the same two trivial tools as the stdio fixture (`echo`, `add`). Also
honors the config-time tool filter: a tool named `hidden` is advertised but
should never be published when `deny_tools`/`allow_tools` exclude it.

Run standalone to experiment:

    python3 mock_mcp_http_server.py [port]
"""

import json
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

TOOLS = [
    {
        "name": "echo",
        "description": "Echo back the provided text.",
        "inputSchema": {
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"],
        },
    },
    {
        "name": "hidden",
        "description": "A tool config should filter out.",
        "inputSchema": {"type": "object", "properties": {}},
    },
]

SESSION_COUNTER = 0


def text_result(text):
    return {"content": [{"type": "text", "text": text}]}


def handle(request):
    method = request.get("method")
    params = request.get("params") or {}

    if method == "initialize":
        return {
            "protocolVersion": params.get("protocolVersion", "2024-11-05"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "mock-mcp-http", "version": "0.1.0"},
        }
    if method == "tools/list":
        return {"tools": TOOLS}
    if method == "tools/call":
        name = params.get("name")
        args = params.get("arguments") or {}
        if name == "echo":
            return text_result(str(args.get("text", "")))
        raise ValueError(f"unknown tool: {name}")
    raise ValueError(f"unknown method: {method}")


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        global SESSION_COUNTER
        length = int(self.headers.get("Content-Length", "0"))
        request = json.loads(self.rfile.read(length) or b"{}")

        # The first request is always initialize; issue a session id.
        session = self.headers.get("Mcp-Session-Id")
        is_initialize = request.get("method") == "initialize"

        response = {"jsonrpc": "2.0", "id": request.get("id")}
        try:
            response["result"] = handle(request)
        except Exception as error:  # noqa: BLE001 - any failure becomes JSON-RPC error
            response["error"] = {"code": -32000, "message": str(error)}
        body = json.dumps(response)

        # initialize replies plain JSON; later requests reply SSE-framed so
        # both legal content types are exercised.
        if is_initialize:
            SESSION_COUNTER += 1
            session = f"sess-{SESSION_COUNTER}"
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Mcp-Session-Id", session)
            self.end_headers()
            self.wfile.write(body.encode())
        else:
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            if session:
                self.send_header("Mcp-Session-Id", session)
            self.end_headers()
            self.wfile.write(f"data: {body}\n\n".encode())

    def log_message(self, *_args):
        # Silence per-request stderr noise; muta discards it anyway.
        pass


def main():
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 9876
    HTTPServer(("127.0.0.1", port), Handler).serve_forever()


if __name__ == "__main__":
    main()
