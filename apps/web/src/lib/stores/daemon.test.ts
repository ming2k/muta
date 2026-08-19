import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DaemonStore, requestFrame, resolveConfig } from "./daemon.svelte.js";
import { FakeWebSocket, installFakeWebSocket } from "../test/fake-websocket.js";

describe("resolveConfig", () => {
  const storage = () => {
    const map = new Map<string, string>();
    return {
      getItem: (k: string) => map.get(k) ?? null,
      setItem: (k: string, v: string) => void map.set(k, v),
      removeItem: (k: string) => void map.delete(k),
      map,
    };
  };

  it("defaults to the loopback endpoint with no project and no token", () => {
    expect(resolveConfig("", null)).toEqual({
      wsUrl: "ws://127.0.0.1:9800",
      project: null,
      token: null,
    });
  });

  it("prefers the ?ws= query param and persists it", () => {
    const store = storage();
    const cfg = resolveConfig("?ws=ws://10.0.0.2:1234", store);
    expect(cfg.wsUrl).toBe("ws://10.0.0.2:1234");
    expect(store.map.get("neenee.ws-url")).toBe("ws://10.0.0.2:1234");
  });

  it("builds a URL from ?host= and ?port=", () => {
    expect(resolveConfig("?host=example.com&port=9000", null).wsUrl).toBe("ws://example.com:9000");
  });

  it("falls back to stored settings", () => {
    const store = storage();
    store.map.set("neenee.ws-url", "ws://stored:1");
    store.map.set("neenee.project", "/srv/proj");
    store.map.set("neenee.ws-token", "stored-token");
    expect(resolveConfig("", store)).toEqual({
      wsUrl: "ws://stored:1",
      project: "/srv/proj",
      token: "stored-token",
    });
  });

  it("rejects non-ws schemes", () => {
    expect(resolveConfig("?ws=http://evil.example", null).wsUrl).toBe("ws://127.0.0.1:9800");
  });

  it("accepts bare host:port in the ws field", () => {
    expect(resolveConfig("?ws=localhost:7777", null).wsUrl).toBe("ws://localhost:7777");
  });

  it("reads ?token= and persists it (the `neenee panel` URL flow)", () => {
    const store = storage();
    const cfg = resolveConfig("?token=abc123", store);
    expect(cfg.token).toBe("abc123");
    expect(store.map.get("neenee.ws-token")).toBe("abc123");
  });
});

describe("DaemonStore smoke", () => {
  beforeEach(() => {
    installFakeWebSocket();
    FakeWebSocket.reset();
    window.localStorage.clear();
  });

  it("sends a monitor Select on connect, with the handshake version", () => {
    const store = new DaemonStore();
    store.init({ wsUrl: "ws://test:1" });
    const ws = FakeWebSocket.latest();
    expect(ws.url).toBe("ws://test:1");
    // No token configured: no subprotocol offer.
    expect(ws.protocols).toBeUndefined();
    ws.open();
    const select = ws.sentJson(0);
    expect(select.type).toBe("Select");
    expect(select.action).toEqual({ monitor: { watch: true, include_idle: true } });
    // Injected by vite define from package.json — must equal the workspace
    // version or the daemon refuses the connection (ADR-0100).
    expect(typeof select.version).toBe("string");
    expect(select.version).toBe(__NEENEE_CLIENT_VERSION__);
  });

  it("carries the token as a bearer. subprotocol (ADR-0105)", () => {
    const store = new DaemonStore();
    store.init({ wsUrl: "ws://test:1", token: "sekret" });
    const ws = FakeWebSocket.latest();
    expect(ws.protocols).toEqual(["bearer.sekret"]);
  });

  it("ends a session via the kill_session control verb (ADR-0112)", () => {
    const store = new DaemonStore();
    store.init({ wsUrl: "ws://test:1" });
    store.endSession("sess-9");
    const ws = FakeWebSocket.latest();
    ws.open();
    // The Select carries the control verb, not an attach.
    expect(ws.sentJson(0).action).toEqual({
      control: { verb: "kill_session", session_id: "sess-9" },
    });
    // The reply (ok or error) closes the one-shot control connection.
    ws.message({ type: "ControlReply", ok: true, session_id: "sess-9" });
    expect(ws.readyState).toBe(FakeWebSocket.CLOSED);
  });

  it("attaches to the first session from the monitor snapshot and loads the transcript", () => {
    const store = new DaemonStore();
    store.init({ wsUrl: "ws://test:1" });
    const monitor = FakeWebSocket.latest();
    monitor.open();
    monitor.message({
      type: "Monitor",
      kind: "snapshot",
      project_root: "/srv/proj",
      daemon_started_at: 1,
      sessions: [
        {
          id: "sess-1",
          overview: "demo",
          created_at: 1,
          updated_at: 2,
          message_count: 1,
          hosting: "hosted",
          status: "idle",
          round: 1,
          output_tokens: 0,
          elapsed_ms: 0,
          project_root: "/srv/proj",
        },
      ],
    });
    expect(store.sessions).toHaveLength(1);

    const session = FakeWebSocket.latest();
    expect(session).not.toBe(monitor);
    session.open();
    expect(session.sentJson(0).action).toEqual({ attach: "sess-1" });

    session.message({
      type: "Welcome",
      session_id: "sess-1",
      round_counter: 3,
      provider: "kimi-code",
      model: "k2",
      messages: [
        { role: "User", content: "hi", timestamp: 100 },
        { role: "System", content: "secret", hidden: true },
        { role: "Assistant", content: "hello", timestamp: 101 },
      ],
    });
    expect(store.sessionAttached).toBe(true);
    expect(store.roundCounter).toBe(3);
    expect(store.providerInfo).toEqual({ provider: "kimi-code", model: "k2" });
    // Hidden harness-injected messages never reach the feed.
    expect(store.feed.map((i) => (i.kind === "message" ? i.message.content : ""))).toEqual([
      "hi",
      "hello",
    ]);
  });
});

// ---------------------------------------------------------------------------
// Wire-protocol coverage: one store + scripted fake WS per test, attached to
// "sess-1" via a Welcome replay. Round events go through `roundEvent`.
// ---------------------------------------------------------------------------

describe("DaemonStore wire protocol", () => {
  beforeEach(() => {
    installFakeWebSocket();
    FakeWebSocket.reset();
    window.localStorage.clear();
  });

  /** Init the store, attach to sess-1, and replay an empty Welcome. */
  function attachSession(store: DaemonStore, sessionId = "sess-1"): FakeWebSocket {
    store.init({ wsUrl: "ws://test:1" });
    FakeWebSocket.latest().open(); // monitor socket
    store.attach(sessionId);
    const session = FakeWebSocket.latest();
    session.open();
    session.message({
      type: "Welcome",
      session_id: sessionId,
      round_counter: 0,
      provider: "kimi-code",
      model: "k2",
      messages: [],
    });
    return session;
  }

  function roundEvent(ws: FakeWebSocket, event: unknown, sessionId = "sess-1") {
    ws.message({ type: "Response", Round: { session_id: sessionId, event } });
  }

  describe("stream folding", () => {
    it("folds StreamDelta×2 + StreamEnd into one Assistant message", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      roundEvent(session, "StreamStart");
      roundEvent(session, { StreamDelta: "Hello, " });
      roundEvent(session, { StreamDelta: "world" });
      expect(store.streamingAssistantText).toBe("Hello, world");
      expect(store.feed).toHaveLength(0);

      roundEvent(session, { StreamEnd: "Hello, world" });
      expect(store.streamingAssistantText).toBe("");
      expect(store.feed).toHaveLength(1);
      const item = store.feed[0];
      expect(item.kind).toBe("message");
      if (item.kind === "message") {
        expect(item.message.role).toBe("Assistant");
        expect(item.message.content).toBe("Hello, world");
      }
    });

    it("appends a Text event as an Assistant message (non-streamed fallback)", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      roundEvent(session, { Text: "plain reply" });
      expect(store.feed).toHaveLength(1);
      const item = store.feed[0];
      expect(item.kind).toBe("message");
      if (item.kind === "message") {
        expect(item.message.role).toBe("Assistant");
        expect(item.message.content).toBe("plain reply");
      }
      expect(store.streamingAssistantText).toBe("");
    });
  });

  describe("CommandResult", () => {
    it("appends command feed items; Error variant → error, Text/Ack → success", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      roundEvent(session, {
        CommandResult: { name: "help", args: "", result: { Text: "help text" } },
      });
      roundEvent(session, {
        CommandResult: { name: "bogus", args: "x", result: { Error: { message: "bad" } } },
      });
      roundEvent(session, {
        CommandResult: { name: "compact", args: "", result: { Ack: { title: "Done" } } },
      });

      expect(store.feed.map((i) => i.kind)).toEqual(["command", "command", "command"]);
      const records = store.feed.map((i) => (i.kind === "command" ? i.record : null));
      expect(records[0]).toMatchObject({ name: "help", status: "success" });
      expect(records[1]).toMatchObject({ name: "bogus", status: "error" });
      expect(records[2]).toMatchObject({ name: "compact", status: "success" });
    });
  });

  describe("tool folding", () => {
    it("tracks ToolCall/ToolStream/ToolResult/ToolCancelled in liveTools", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      roundEvent(session, { ToolCall: { id: "t1", name: "bash", arguments: "{}" } });
      expect(store.liveTools["t1"]).toMatchObject({
        id: "t1",
        name: "bash",
        status: "running",
        stdout: "",
        stderr: "",
      });

      roundEvent(session, { ToolStream: { id: "t1", stream: { Stdout: "out1 " } } });
      roundEvent(session, { ToolStream: { id: "t1", stream: { Stdout: "out2" } } });
      roundEvent(session, { ToolStream: { id: "t1", stream: { Stderr: "err1" } } });
      expect(store.liveTools["t1"].stdout).toBe("out1 out2");
      expect(store.liveTools["t1"].stderr).toBe("err1");

      roundEvent(session, {
        ToolResult: {
          id: "t1",
          name: "bash",
          output: "done",
          structured: { Text: "done" },
          duration_ms: 5,
        },
      });
      expect(store.liveTools["t1"].status).toBe("completed");
      expect(store.liveTools["t1"].output).toBe("done");
      expect(store.liveTools["t1"].durationMs).toBe(5);

      roundEvent(session, { ToolCall: { id: "t2", name: "bash", arguments: "{}" } });
      roundEvent(session, {
        ToolResult: {
          id: "t2",
          name: "bash",
          output: "",
          structured: { Error: { message: "boom" } },
          duration_ms: 1,
        },
      });
      expect(store.liveTools["t2"].status).toBe("failed");

      roundEvent(session, { ToolCall: { id: "t3", name: "bash", arguments: "{}" } });
      roundEvent(session, { ToolCancelled: { id: "t3", name: "bash" } });
      expect(store.liveTools["t3"].status).toBe("cancelled");
    });
  });

  describe("envoy flow", () => {
    const envoyEvent = (ws: FakeWebSocket, event: unknown, parentCallId = "call-1") =>
      roundEvent(ws, { Envoy: { parent_call_id: parentCallId, event } });

    it("folds envoy Started/Stream/Tool events into the parent tool", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      roundEvent(session, { ToolCall: { id: "call-1", name: "task", arguments: "{}" } });
      envoyEvent(session, { Started: { profile: "explore" } });
      envoyEvent(session, { StreamDelta: "partial " });
      envoyEvent(session, { StreamDelta: "text" });
      expect(store.liveTools["call-1"].envoy?.streamingText).toBe("partial text");

      envoyEvent(session, { StreamEnd: "partial text" });
      envoyEvent(session, {
        ToolCall: { id: "e1", name: "read", arguments: "{}", round: 1, turn: 0 },
      });
      envoyEvent(session, {
        ToolResult: { id: "e1", name: "read", output: "file contents", duration_ms: 1 },
      });
      envoyEvent(session, { Activity: "reading files" });

      const envoy = store.liveTools["call-1"].envoy;
      expect(envoy?.profile).toBe("explore");
      expect(envoy?.text).toBe("partial text");
      expect(envoy?.streamingText).toBe("");
      expect(envoy?.activity).toBe("reading files");
      expect(envoy?.tools).toHaveLength(1);
      expect(envoy?.tools[0]).toMatchObject({
        id: "e1",
        name: "read",
        status: "completed",
        output: "file contents",
        durationMs: 1,
      });
    });

    it("routes envoy PermissionRequest replies with parent_call_id", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      roundEvent(session, { ToolCall: { id: "call-1", name: "task", arguments: "{}" } });
      envoyEvent(session, { Started: { profile: "explore" } });
      envoyEvent(session, {
        PermissionRequest: {
          id: "p1",
          tool: "bash",
          label: "",
          description: "d",
          arguments: "{}",
          scope: "x",
          elevation: false,
          one_off: false,
        },
      });
      expect(store.pendingPermission?.request.id).toBe("p1");
      expect(store.pendingPermission?.origin.parentCallId).toBe("call-1");
      expect(store.pendingPermission?.origin.label).toBe("explore");

      store.resolvePermission("Once");
      expect(store.pendingPermission).toBeNull();
      // sent[0] is the attach Select frame.
      expect(session.sentJson(1)).toEqual({
        type: "Request",
        PermissionReply: { request_id: "p1", decision: "Once", parent_call_id: "call-1" },
      });
    });

    it("routes top-level PermissionRequest replies with parent_call_id null", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      roundEvent(session, {
        PermissionRequest: {
          id: "p2",
          tool: "bash",
          label: "",
          description: "d",
          arguments: "{}",
          scope: "x",
          elevation: false,
          one_off: false,
        },
      });
      expect(store.pendingPermission?.origin.parentCallId).toBeNull();

      store.resolvePermission("Once");
      expect(session.sentJson(1)).toEqual({
        type: "Request",
        PermissionReply: { request_id: "p2", decision: "Once", parent_call_id: null },
      });
    });

    it("routes envoy UserQuestionRequest replies with parent_call_id", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      roundEvent(session, { ToolCall: { id: "call-1", name: "task", arguments: "{}" } });
      envoyEvent(session, {
        UserQuestionRequest: {
          id: "q1",
          questions: [{ question: "?", options: [{ label: "a" }], multi_select: false }],
        },
      });
      expect(store.pendingQuestion?.request.id).toBe("q1");
      expect(store.pendingQuestion?.origin.parentCallId).toBe("call-1");

      store.answerQuestion([["a"]]);
      expect(store.pendingQuestion).toBeNull();
      expect(session.sentJson(1)).toEqual({
        type: "Request",
        UserQuestionReply: { request_id: "q1", answers: [["a"]], parent_call_id: "call-1" },
      });
    });

    it("routes envoy InputRequest replies with parent_call_id", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      roundEvent(session, { ToolCall: { id: "call-1", name: "task", arguments: "{}" } });
      envoyEvent(session, {
        InputRequest: { id: "i1", command: "sudo x", prompt: "password", secret: true },
      });
      expect(store.pendingInput?.request.id).toBe("i1");
      expect(store.pendingInput?.origin.parentCallId).toBe("call-1");

      store.replyInput("x");
      expect(store.pendingInput).toBeNull();
      expect(session.sentJson(1)).toEqual({
        type: "Request",
        InputReply: { request_id: "i1", text: "x", parent_call_id: "call-1" },
      });
    });
  });

  describe("input recovery", () => {
    it("UnsentInput drops the optimistic echo and restores the draft once", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      store.sendChat("hello");
      expect(store.feed).toHaveLength(1);

      roundEvent(session, { UnsentInput: { prompt: "hello", images: [] } });
      expect(store.feed).toHaveLength(0);
      expect(store.restoredDraft).toEqual({ text: "hello", images: [] });

      expect(store.takeRestoredDraft()).toEqual({ text: "hello", images: [] });
      expect(store.takeRestoredDraft()).toBeNull();
    });

    it("UserInputInserted dedupes the optimistic echo but appends new text", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      store.sendChat("hello");
      roundEvent(session, {
        UserInputInserted: { id: "q1", text: "hello", sent_at_ms: Date.now() },
      });
      expect(store.feed).toHaveLength(1); // deduped against the echo

      roundEvent(session, {
        UserInputInserted: { id: "q2", text: "world", sent_at_ms: Date.now() },
      });
      expect(store.feed).toHaveLength(2);
      const item = store.feed[1];
      expect(item.kind).toBe("message");
      if (item.kind === "message") {
        expect(item.message.role).toBe("User");
        expect(item.message.content).toBe("world");
      }
    });
  });

  describe("reconnect", () => {
    afterEach(() => {
      vi.useRealTimers();
    });

    it("reattaches with backoff after a server-side close", () => {
      vi.useFakeTimers();
      try {
        const store = new DaemonStore();
        const session = attachSession(store);
        expect(store.sessionAttached).toBe(true);

        const socketsBefore = FakeWebSocket.instances.length;
        session.serverClose();
        expect(store.sessionAttached).toBe(false);
        // Backoff: no immediate reattach.
        expect(FakeWebSocket.instances.length).toBe(socketsBefore);

        vi.advanceTimersByTime(1000);
        const reattached = FakeWebSocket.latest();
        expect(reattached).not.toBe(session);
        reattached.open();
        expect(reattached.sentJson(0).type).toBe("Select");
        expect(reattached.sentJson(0).action).toEqual({ attach: "sess-1" });
      } finally {
        vi.useRealTimers();
      }
    });
  });

  describe("state events", () => {
    it("folds HarnessState/AutopilotChanged/RoundCompleted/Activity/Compacted", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      roundEvent(session, { Activity: "waiting for model" });
      expect(store.activity).toBe("waiting for model");

      roundEvent(session, {
        HarnessState: { loop_status: "idle", round_counter: 2, autopilot: true },
      });
      expect(store.roundCounter).toBe(2);
      expect(store.autopilot).toBe(true);
      expect(store.activity).toBeNull(); // idle clears the activity line

      roundEvent(session, { AutopilotChanged: false });
      expect(store.autopilot).toBe(false);

      roundEvent(session, { Activity: "thinking" });
      roundEvent(session, {
        RoundCompleted: {
          round: 1,
          output_tokens: 100,
          duration_ms: 2000,
          paused_ms: 0,
          generation_ms: 1500,
        },
      });
      expect(store.lastRound).toEqual({
        round: 1,
        output_tokens: 100,
        duration_ms: 2000,
        paused_ms: 0,
        generation_ms: 1500,
      });
      expect(store.roundCounter).toBe(1);
      expect(store.activity).toBeNull();

      roundEvent(session, {
        Compacted: { archived_messages: 3, before_chars: 1000, after_chars: 200 },
      });
      expect(store.toasts.length).toBeGreaterThan(0);
      const toast = store.toasts[store.toasts.length - 1];
      expect(toast.severity).toBe("info");
      expect(toast.title).toBe("Context compacted");
    });
  });

  describe("global responses", () => {
    it("stores ProviderPicker/ProviderKeys and applies ProviderSwitched", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      session.message({
        type: "Response",
        ProviderPicker: {
          default_id: "kimi-code",
          rows: [
            {
              id: "kimi-code",
              name: "Kimi Code",
              model: "k2",
              models: ["k2"],
              builtin: true,
              protocol: "openai",
              base_url: "https://api.example",
              key_ready: true,
            },
          ],
        },
      });
      expect(store.providerPicker?.default_id).toBe("kimi-code");
      expect(store.providerPicker?.rows).toHaveLength(1);

      session.message({ type: "Response", ProviderSwitched: { provider: "p", model: "m" } });
      expect(store.providerInfo).toEqual({ provider: "p", model: "m" });

      session.message({ type: "Response", ProviderKeys: [["kimi-code", true]] });
      expect(store.providerKeys).toEqual([["kimi-code", true]]);
    });

    it("ConversationReplaced merges messages and commands sorted by timestamp", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      session.message({
        type: "Response",
        ConversationReplaced: {
          session_id: "sess-2",
          messages: [
            // Message timestamps are epoch SECONDS.
            { role: "User", content: "first", timestamp: 100 },
            { role: "Assistant", content: "third", timestamp: 101 },
          ],
          commands: [
            // Command timestamps are epoch MILLISECONDS — between the two.
            {
              name: "help",
              args: "",
              status: "success",
              result: { Text: "help text" },
              timestamp: 100_500,
            },
          ],
        },
      });

      expect(store.activeSessionId).toBe("sess-2");
      expect(
        store.feed.map((i) => (i.kind === "message" ? i.message.content : `cmd:${i.record.name}`)),
      ).toEqual(["first", "cmd:help", "third"]);
    });

    it("ConversationCleared blanks state, keeps the attachment, and adopts the handoff id", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      // Seed some session-scoped state the switch must wipe.
      roundEvent(session, "StreamStart");
      roundEvent(session, { StreamDelta: "partial" });
      store.roundCounter = 3;

      // The wire variant is a unit — `{"ConversationCleared":null}`; the fresh
      // session's id is NOT carried here (unlike ConversationReplaced).
      session.message({ type: "Response", ConversationCleared: null });

      // `/new` blanked the transcript, zeroed the round counter, and dropped
      // the streaming tail — while the socket stays bound to the harness, so
      // the composer keeps working.
      expect(store.activeSessionId).toBeNull();
      expect(store.feed).toHaveLength(0);
      expect(store.streamingAssistantText).toBe("");
      expect(store.roundCounter).toBe(0);
      expect(store.sessionAttached).toBe(true);

      // The next Round event carries the fresh session's id: the handoff.
      roundEvent(session, { Text: "fresh session reply" }, "sess-fresh");
      expect(store.activeSessionId).toBe("sess-fresh");
      expect(store.feed).toHaveLength(1);
    });
  });

  describe("outgoing frames", () => {
    it("sendChat/interrupt/setDefaultModel/deleteSession send flattened Request frames", () => {
      const store = new DaemonStore();
      const session = attachSession(store);
      // sent[0] is the attach Select frame.

      store.sendChat("/help");
      expect(session.sentJson(1)).toEqual({ type: "Request", SlashCommand: "/help" });

      store.sendChat("hi", [{ mime: "image/png", data: "AA==" }]);
      expect(session.sentJson(2)).toEqual({
        type: "Request",
        Chat: {
          text: "hi",
          images: [{ mime: "image/png", data: "AA==" }],
          sent_at_ms: expect.any(Number),
        },
      });

      store.interrupt();
      expect(session.sentJson(3)).toEqual({ type: "Request", Interrupt: null });

      store.setDefaultModel("k2");
      expect(session.sentJson(4)).toEqual({ type: "Request", SetDefaultModel: { id: "k2" } });

      store.deleteSession("sess-1");
      expect(session.sentJson(5)).toEqual({ type: "Request", DeleteSession: { id: "sess-1" } });
    });

    it("requestFrame flattens the request next to the type tag", () => {
      const frame = requestFrame({ Chat: { text: "hi", images: [] } });
      expect(frame.startsWith('{"type":"Request","Chat"')).toBe(true);
      const parsed = JSON.parse(frame) as Record<string, unknown>;
      expect(parsed).not.toHaveProperty("request");
      expect(parsed.Chat).toEqual({ text: "hi", images: [] });
    });
  });

  describe("session routing", () => {
    it("ignores Round events for a different session_id", () => {
      const store = new DaemonStore();
      const session = attachSession(store);

      roundEvent(session, { Text: "stray" }, "other");
      expect(store.feed).toHaveLength(0);

      roundEvent(session, { Text: "mine" });
      expect(store.feed).toHaveLength(1);
    });
  });
});
