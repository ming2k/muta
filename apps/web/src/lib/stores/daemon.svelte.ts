import type {
  AgentRequest,
  AgentResponse,
  LiveToolExecution,
  Message,
  MonitorEvent,
  MonitoredSession,
  TodoItem,
} from "../types.js";

class DaemonStore {
  public connected = $state<boolean>(false);
  public wsUrl = $state<string>("ws://127.0.0.1:9800");
  public sessions = $state<MonitoredSession[]>([]);
  public activeSessionId = $state<string | null>(null);
  public messages = $state<Message[]>([]);
  public streamingAssistantText = $state<string>("");
  public streamingReasoningText = $state<string>("");
  public liveTools = $state<Record<string, LiveToolExecution>>({});
  public todos = $state<TodoItem[]>([]);

  private monitorWs: WebSocket | null = null;
  private sessionWs: WebSocket | null = null;

  public activeSession = $derived(
    this.sessions.find((s) => s.id === this.activeSessionId)
  );

  public isBusy = $derived(
    this.activeSession?.status === "running" || this.streamingAssistantText.length > 0
  );

  public init(url: string = "ws://127.0.0.1:9800") {
    this.wsUrl = url;
    this.connectMonitor();
  }

  private connectMonitor() {
    if (this.monitorWs) {
      this.monitorWs.close();
    }

    try {
      this.monitorWs = new WebSocket(this.wsUrl);

      this.monitorWs.onopen = () => {
        this.connected = true;
        const selectFrame = {
          type: "Select",
          action: { monitor: { watch: true, include_idle: true } },
        };
        this.monitorWs?.send(JSON.stringify(selectFrame));
      };

      this.monitorWs.onmessage = (event) => {
        try {
          const data = JSON.parse(event.data);
          if (data.type === "Monitor") {
            this.handleMonitorEvent(data.event);
          }
        } catch (err) {
          console.error("Monitor parse error:", err);
        }
      };

      this.monitorWs.onclose = () => {
        this.connected = false;
        setTimeout(() => this.connectMonitor(), 2500);
      };
    } catch {
      this.connected = false;
      setTimeout(() => this.connectMonitor(), 2500);
    }
  }

  private handleMonitorEvent(event: MonitorEvent) {
    if (event.type === "Snapshot") {
      this.sessions = event.sessions;
      if (!this.activeSessionId && event.sessions.length > 0) {
        this.attach(event.sessions[0].id);
      }
    } else if (event.type === "SessionAdded") {
      this.sessions = [event.session, ...this.sessions.filter((s) => s.id !== event.session.id)];
    } else if (event.type === "SessionUpdated") {
      this.sessions = this.sessions.map((s) =>
        s.id === event.session.id ? event.session : s
      );
    } else if (event.type === "SessionRemoved") {
      this.sessions = this.sessions.filter((s) => s.id !== event.session_id);
    }
  }

  public attach(sessionId: string) {
    if (this.activeSessionId === sessionId && this.sessionWs?.readyState === WebSocket.OPEN) {
      return;
    }

    if (this.sessionWs) {
      this.sessionWs.close();
    }

    this.activeSessionId = sessionId;
    this.messages = [];
    this.streamingAssistantText = "";
    this.streamingReasoningText = "";
    this.liveTools = {};

    try {
      this.sessionWs = new WebSocket(this.wsUrl);

      this.sessionWs.onopen = () => {
        const selectFrame = {
          type: "Select",
          action: { attach: sessionId },
        };
        this.sessionWs?.send(JSON.stringify(selectFrame));
      };

      this.sessionWs.onmessage = (event) => {
        try {
          const data = JSON.parse(event.data);
          if (data.type === "Welcome") {
            this.messages = data.messages || [];
          } else if (data.type === "Response") {
            this.handleAgentResponse(data.response);
          }
        } catch (err) {
          console.error("Session parse error:", err);
        }
      };
    } catch (err) {
      console.error("Failed to connect to session:", err);
    }
  }

  private handleAgentResponse(resp: AgentResponse) {
    if (resp.type === "Round") {
      const ev = resp.event;
      if (ev.type === "AssistantDelta") {
        this.streamingAssistantText += ev.delta;
      } else if (ev.type === "AssistantEnd") {
        if (this.streamingAssistantText.trim().length > 0) {
          this.messages.push({
            role: "assistant",
            content: this.streamingAssistantText,
            timestamp: Date.now(),
          });
        }
        this.streamingAssistantText = "";
        this.streamingReasoningText = "";
      } else if (ev.type === "ReasoningDelta") {
        this.streamingReasoningText += ev.delta;
      } else if (ev.type === "ToolCall") {
        this.liveTools[ev.id] = {
          id: ev.id,
          name: ev.name,
          arguments: ev.arguments,
          status: "running",
        };
      } else if (ev.type === "ToolResult") {
        if (this.liveTools[ev.id]) {
          this.liveTools[ev.id].status = "completed";
          this.liveTools[ev.id].output = ev.output;
          this.liveTools[ev.id].durationMs = ev.duration_ms;
        }
      } else if (ev.type === "TodosUpdated") {
        this.todos = ev.todos;
      }
    } else if (resp.type === "ConversationReplaced") {
      this.messages = resp.messages;
    } else if (resp.type === "ConversationCleared") {
      this.messages = [];
    }
  }

  public send(req: AgentRequest) {
    if (this.sessionWs && this.sessionWs.readyState === WebSocket.OPEN) {
      this.sessionWs.send(JSON.stringify({ type: "Request", request: req }));
    }
  }

  public sendChat(text: string) {
    if (!text.trim()) return;
    this.messages.push({
      role: "user",
      content: text,
      timestamp: Date.now(),
    });
    this.send({ type: "Chat", text });
  }

  public interrupt() {
    this.send({ type: "Interrupt" });
  }

  public newSession() {
    const ws = new WebSocket(this.wsUrl);
    ws.onopen = () => {
      ws.send(JSON.stringify({ type: "Select", action: "new" }));
    };
    ws.onmessage = (e) => {
      try {
        const frame = JSON.parse(e.data);
        if (frame.type === "Welcome") {
          ws.close();
          this.attach(frame.session_id);
        }
      } catch {
        // ignore
      }
    };
  }
}

export const daemon = new DaemonStore();
