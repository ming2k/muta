/**
 * Daemon connection store — the web panel's client for the neenee
 * session-daemon WebSocket protocol.
 *
 * Contract: `docs/reference/server-api.md` + `docs/reference/server.asyncapi.yaml`
 * (mirrored in `../types.ts`). Two logical channels over one endpoint:
 *
 * - a Monitor connection (`Select{monitor}` → snapshot + diffs) driving the
 *   session list, and
 * - a per-attached-session connection (`Select{attach}` → `Welcome` +
 *   `Response` stream) driving the transcript.
 *
 * Everything here serializes/deserializes the daemon's serde shapes exactly:
 * flattened `Request`/`Response` payloads, `Monitor` frames tagged `kind`,
 * bare-string unit variants inside `Round.event`, and `Chat.images` always
 * present (the Rust field has no `#[serde(default)]`).
 */

import type {
  AgentNotice,
  AgentRequest,
  AgentResponse,
  AttachAction,
  CommandRecord,
  CommandResult,
  EnvoyEvent,
  ImagePart,
  InputRequest,
  Message,
  MonitorFrame,
  MonitoredSession,
  PermissionDecision,
  PermissionRequest,
  ProviderPickerSnapshot,
  QueuedUserInput,
  RoundEvent,
  RoundSummary,
  TodoList,
  UserQuestionRequest,
  Wire,
} from "../types.js";

/**
 * Client build identifier for the ADR-0100 version handshake. The daemon
 * enforces exact equality against its own `CARGO_PKG_VERSION` before any
 * session work, so this must be the plain workspace version (e.g. "0.24.0")
 * with no client prefix. Injected at build time from `package.json` by
 * `vite.config.ts` (`__NEENEE_CLIENT_VERSION__`); CI refuses a drift between
 * the two. Empty (tests / non-vite runtimes) omits the field, which the
 * daemon tolerates.
 */
const CLIENT_VERSION: string =
  typeof __NEENEE_CLIENT_VERSION__ === "string" ? __NEENEE_CLIENT_VERSION__ : "";

/** Reconnect base delay for both channels; doubles per failure, capped. */
const RECONNECT_BASE_MS = 1000;
const RECONNECT_MAX_MS = 15_000;

/** Default daemon endpoint when nothing is configured. */
const DEFAULT_WS_URL = "ws://127.0.0.1:9800";

/** localStorage keys for the connection settings. */
const WS_URL_STORAGE_KEY = "neenee.ws-url";
const PROJECT_STORAGE_KEY = "neenee.project";
const TOKEN_STORAGE_KEY = "neenee.ws-token";

/** Connection state, distinct from any session's status. */
export type ConnectionState = "connecting" | "connected" | "disconnected";

/** Where a blocking request originated (top-level agent or an envoy). */
export interface RequestOrigin {
  /** The envoy's parent tool-call id; `null` for top-level requests. */
  parentCallId: string | null;
  /** Display label, e.g. the envoy profile name. */
  label: string | null;
}

const TOP_LEVEL_ORIGIN: RequestOrigin = { parentCallId: null, label: null };

/** A blocking request the operator must answer before the round proceeds. */
export interface PendingPermission {
  request: PermissionRequest;
  origin: RequestOrigin;
}

export interface PendingQuestion {
  request: UserQuestionRequest;
  origin: RequestOrigin;
}

export interface PendingInput {
  request: InputRequest;
  origin: RequestOrigin;
}

/** One transient user-visible notice/error line. */
export interface Toast {
  id: number;
  severity: "info" | "warning" | "error";
  title: string;
  body?: string;
}

/** A tool run by an envoy, rendered nested inside the parent tool card. */
export interface EnvoyTool {
  id: string;
  name: string;
  arguments: string;
  status: "running" | "completed";
  output?: string;
  durationMs?: number;
}

/** UI-model envoy execution, folded from `RoundEvent::Envoy` sub-events. */
export interface EnvoyExecution {
  profile: string | null;
  activity: string | null;
  /** Completed envoy response text (accumulated across `StreamEnd`s). */
  text: string;
  streamingText: string;
  /** Completed envoy reasoning traces (accumulated across `StreamReasoningEnd`s). */
  reasoning: string[];
  streamingReasoning: string;
  tools: EnvoyTool[];
}

/** UI-model tool execution, folded from ToolCall/ToolStream/ToolResult events. */
export interface LiveToolExecution {
  id: string;
  name: string;
  arguments: string;
  status: "running" | "completed" | "failed" | "cancelled";
  stdout: string;
  stderr: string;
  output?: string;
  durationMs?: number;
  /** Nested envoy activity when this tool is a `task` spawn (ADR-0029). */
  envoy?: EnvoyExecution;
}

/**
 * The transcript feed: dialogue messages plus slash-command blocks (ADR-0091)
 * in arrival order. `key` is a stable per-session-ui id for keyed each blocks.
 */
export type FeedItem =
  | { kind: "message"; key: string; message: Message }
  | { kind: "command"; key: string; record: CommandRecord };

/** Resolved connection settings for the daemon endpoint. */
export interface DaemonConfig {
  wsUrl: string;
  /** Caller project path for the `Select` frame; `null` omits the field. */
  project: string | null;
  /**
   * Bearer token for daemons with auth on (ADR-0105: always on `--public`,
   * default on loopback via `[daemon] local_auth`). Browsers cannot set
   * headers on a WebSocket, so it travels as the `bearer.<token>` subprotocol.
   */
  token: string | null;
}

/** Minimal storage contract so `resolveConfig` stays testable without a DOM. */
export interface ConfigStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

function sanitizeWsUrl(raw: string | null | undefined): string | null {
  if (!raw) return null;
  const trimmed = raw.trim();
  if (!trimmed) return null;
  if (trimmed.startsWith("ws://") || trimmed.startsWith("wss://")) return trimmed;
  // Bare host[:port] input is common in a settings field — assume plain ws.
  if (/^[\w.-]+(:\d+)?(\/\S*)?$/.test(trimmed)) return `ws://${trimmed}`;
  return null;
}

/** What `GET /healthz` reports; lets the UI name the failure precisely. */
export interface DaemonProbe {
  version: string;
  auth: boolean;
  panel: boolean;
}

/**
 * Resolve the daemon endpoint and project scope, highest priority first:
 * URL query params (`?ws=` / `?host=`+`?port=` / `?project=` / `?token=`),
 * then persisted localStorage settings, then the loopback default.
 * Query-param values are persisted so a shared/deep-linked URL sticks across
 * reloads (the token is a loopback-scoped credential; persisting it is what
 * makes the printed `neenee panel` URL a one-click flow).
 */
export function resolveConfig(search: string, storage: ConfigStorage | null): DaemonConfig {
  const params = new URLSearchParams(search);
  const storedUrl = storage?.getItem(WS_URL_STORAGE_KEY) ?? null;
  const storedProject = storage?.getItem(PROJECT_STORAGE_KEY) ?? null;
  const storedToken = storage?.getItem(TOKEN_STORAGE_KEY) ?? null;

  let wsUrl =
    sanitizeWsUrl(params.get("ws")) ??
    sanitizeWsUrl(
      params.get("host")
        ? `${params.get("host")}${params.get("port") ? `:${params.get("port")}` : ""}`
        : null,
    ) ??
    sanitizeWsUrl(storedUrl) ??
    DEFAULT_WS_URL;

  const project = params.get("project")?.trim() || storedProject?.trim() || null;
  const token = params.get("token")?.trim() || storedToken?.trim() || null;

  if (storage) {
    if (params.get("ws") || params.get("host")) storage.setItem(WS_URL_STORAGE_KEY, wsUrl);
    if (params.get("project")) storage.setItem(PROJECT_STORAGE_KEY, project ?? "");
    if (params.get("token")) storage.setItem(TOKEN_STORAGE_KEY, token ?? "");
  }

  return { wsUrl, project, token };
}

/** Persist connection settings from the connection dialog. */
export function persistConfig(storage: ConfigStorage, config: DaemonConfig): void {
  storage.setItem(WS_URL_STORAGE_KEY, config.wsUrl);
  if (config.project) storage.setItem(PROJECT_STORAGE_KEY, config.project);
  else storage.removeItem(PROJECT_STORAGE_KEY);
  if (config.token) storage.setItem(TOKEN_STORAGE_KEY, config.token);
  else storage.removeItem(TOKEN_STORAGE_KEY);
}

/** Derive the HTTP(S) base URL for the daemon's static/health endpoints. */
export function httpBaseUrl(wsUrl: string): string {
  return wsUrl.replace(/^ws:/, "http:").replace(/^wss:/, "https:").replace(/\/+$/, "");
}

/** Probe `GET /healthz`; `null` when nothing answers (or CORS/network fails). */
export async function probeDaemon(wsUrl: string): Promise<DaemonProbe | null> {
  try {
    const resp = await fetch(`${httpBaseUrl(wsUrl)}/healthz`, { cache: "no-store" });
    if (!resp.ok) return null;
    return (await resp.json()) as DaemonProbe;
  } catch {
    return null;
  }
}

function wireEnvelope(action: AttachAction, project: string | null): string {
  const frame: { type: "Select"; action: AttachAction; project?: string; version?: string } = {
    type: "Select",
    action,
  };
  if (project !== null) {
    frame.project = project;
  }
  if (CLIENT_VERSION) {
    frame.version = CLIENT_VERSION;
  }
  return JSON.stringify(frame);
}

/** Serialize an AgentRequest into the flattened Wire::Request frame. */
export function requestFrame(req: AgentRequest): string {
  return JSON.stringify({ type: "Request", ...req });
}

/** Millisecond clock for a transcript message (sent_at_ms wins, else unix seconds). */
function messageTimeMs(message: Message): number {
  return message.sent_at_ms ?? (message.timestamp ? message.timestamp * 1000 : 0);
}

export class DaemonStore {
  public connection = $state<ConnectionState>("disconnected");
  public draining = $state<boolean>(false);
  public sessions = $state<MonitoredSession[]>([]);
  public daemonProjectRoot = $state<string>("");

  public activeSessionId = $state<string | null>(null);
  public sessionAttached = $state<boolean>(false);
  public feed = $state<FeedItem[]>([]);
  public streamingAssistantText = $state<string>("");
  public streamingReasoningText = $state<string>("");
  public liveTools = $state<Record<string, LiveToolExecution>>({});
  public todos = $state<TodoList>({ items: [], next_id: 1, updated_at_round: 0 });
  public contextTokens = $state<number | null>(null);

  /** Harness-reported state (Welcome + HarnessState/AutopilotChanged events). */
  public roundCounter = $state<number>(0);
  public autopilot = $state<boolean>(false);
  /** Last live activity line ("waiting for model", …) from Activity events. */
  public activity = $state<string | null>(null);
  /** 0-indexed model-request position within the round (TurnStarted). */
  public currentTurn = $state<number | null>(null);
  /** Per-round accounting from the last naturally completed round. */
  public lastRound = $state<RoundSummary | null>(null);

  /** Current provider/model from Welcome, updated by ProviderSwitched. */
  public providerInfo = $state<{ provider: string; model: string } | null>(null);
  /** Full provider-picker snapshot, pushed on attach and after mutations. */
  public providerPicker = $state<ProviderPickerSnapshot | null>(null);
  /** Provider key-readiness summary (header surface). */
  public providerKeys = $state<[string, boolean][]>([]);

  public pendingPermission = $state<PendingPermission | null>(null);
  public pendingQuestion = $state<PendingQuestion | null>(null);
  public pendingInput = $state<PendingInput | null>(null);

  public toasts = $state<Toast[]>([]);
  public sessionError = $state<string | null>(null);

  /**
   * Draft restored into the composer after `UnsentInput` (the round was
   * interrupted before any output; the prompt never reached the model).
   */
  public restoredDraft = $state<{ text: string; images: ImagePart[] } | null>(null);

  public wsUrl = $state<string>(DEFAULT_WS_URL);
  public project = $state<string | null>(null);
  public token = $state<string | null>(null);
  /** Last `/healthz` probe outcome (null = unreachable or never probed). */
  public daemonProbe = $state<DaemonProbe | null>(null);

  private monitorWs: WebSocket | null = null;
  private sessionWs: WebSocket | null = null;
  private monitorGeneration = 0;
  private sessionGeneration = 0;
  private reconnectDelay = RECONNECT_BASE_MS;
  private sessionReconnectDelay = RECONNECT_BASE_MS;
  private sessionReconnectTimer: number | null = null;
  private nextToastId = 1;
  private nextFeedKey = 1;
  private newSessionWs: WebSocket | null = null;

  public activeSession = $derived(
    this.sessions.find((s) => s.id === this.activeSessionId) ?? null,
  );

  public isBusy = $derived(
    this.activeSession?.status === "running" ||
      this.streamingAssistantText.length > 0 ||
      Object.keys(this.liveTools).some((id) => this.liveTools[id].status === "running"),
  );

  /** Read the persisted + query-param configuration without connecting. */
  public loadConfig(): DaemonConfig {
    const storage = typeof window !== "undefined" ? window.localStorage : null;
    const search = typeof window !== "undefined" ? window.location.search : "";
    return resolveConfig(search, storage);
  }

  public init(overrides?: Partial<DaemonConfig>) {
    const resolved = this.loadConfig();
    this.wsUrl = overrides?.wsUrl ?? resolved.wsUrl;
    this.project = overrides?.project !== undefined ? overrides.project : resolved.project;
    this.token = overrides?.token !== undefined ? overrides.token : resolved.token;
    this.connectMonitor();
  }

  /** Apply new connection settings, persist them, and reconnect everything. */
  public applyConfig(config: DaemonConfig) {
    if (typeof window !== "undefined") {
      persistConfig(window.localStorage, config);
    }
    this.wsUrl = config.wsUrl;
    this.project = config.project;
    this.token = config.token;
    // Reattach from scratch against the new endpoint: drop every live socket
    // (monitor reconnect happens below; the session channel reattaches from
    // the fresh snapshot).
    if (this.sessionWs) {
      this.detachSocketHandlers(this.sessionWs);
      this.sessionWs.close();
      this.sessionWs = null;
    }
    if (this.newSessionWs) {
      this.detachSocketHandlers(this.newSessionWs);
      this.newSessionWs.close();
      this.newSessionWs = null;
    }
    this.clearSessionState();
    this.sessions = [];
    this.daemonProjectRoot = "";
    this.draining = false;
    this.connectMonitor();
  }

  /** Open a control-plane socket, carrying the token as a `bearer.` subprotocol. */
  private openSocket(): WebSocket {
    return new WebSocket(this.wsUrl, this.token ? [`bearer.${this.token}`] : undefined);
  }

  /** Refresh the `/healthz` probe (connection state hints in the dialog). */
  public async probe(): Promise<DaemonProbe | null> {
    this.daemonProbe = await probeDaemon(this.wsUrl);
    return this.daemonProbe;
  }

  // -------------------------------------------------------------------------
  // Monitor channel
  // -------------------------------------------------------------------------

  private connectMonitor() {
    const generation = ++this.monitorGeneration;

    if (this.monitorWs) {
      this.detachSocketHandlers(this.monitorWs);
      this.monitorWs.close();
      this.monitorWs = null;
    }

    this.connection = "connecting";
    const ws = this.openSocket();
    this.monitorWs = ws;

    ws.onopen = () => {
      if (generation !== this.monitorGeneration) return;
      this.connection = "connected";
      this.reconnectDelay = RECONNECT_BASE_MS;
      ws.send(wireEnvelope({ monitor: { watch: true, include_idle: true } }, this.project));
    };

    ws.onmessage = (event) => {
      if (generation !== this.monitorGeneration) return;
      try {
        this.handleFrame(JSON.parse(event.data as string) as Wire);
      } catch (err) {
        console.error("monitor frame parse error:", err, event.data);
      }
    };

    ws.onclose = () => {
      if (generation !== this.monitorGeneration) return;
      this.connection = "disconnected";
      // A failed WS handshake is opaque to browsers; the health probe tells
      // "daemon needs a token" apart from "nothing is listening".
      if (!this.daemonProbe) void this.probe();
      if (!this.draining) {
        window.setTimeout(() => this.connectMonitor(), this.reconnectDelay);
        this.reconnectDelay = Math.min(this.reconnectDelay * 2, RECONNECT_MAX_MS);
      }
    };

    ws.onerror = () => {
      if (generation !== this.monitorGeneration) return;
      this.connection = "disconnected";
    };
  }

  private handleMonitorFrame(frame: MonitorFrame) {
    switch (frame.kind) {
      case "snapshot":
        this.daemonProjectRoot = frame.project_root;
        this.sessions = frame.sessions;
        if (!this.activeSessionId && frame.sessions.length > 0) {
          this.attach(frame.sessions[0].id);
        } else if (
          this.activeSessionId &&
          !this.sessionAttached &&
          this.sessionWs === null &&
          frame.sessions.some((s) => s.id === this.activeSessionId)
        ) {
          // The session channel is down but the daemon still hosts the
          // session — reattach (e.g. after a daemon restart).
          this.attach(this.activeSessionId);
        }
        break;
      case "session_added":
      case "session_updated": {
        const row = frame as MonitoredSession;
        const rest = this.sessions.filter((s) => s.id !== row.id);
        this.sessions =
          frame.kind === "session_added" ? [row, ...rest] : [row, ...rest].sort(
            (a, b) => b.updated_at - a.updated_at,
          );
        break;
      }
      case "session_removed":
        this.sessions = this.sessions.filter((s) => s.id !== frame.session_id);
        if (frame.session_id === this.activeSessionId) {
          this.clearSessionState();
          const next = this.sessions[0];
          if (next) this.attach(next.id);
        }
        break;
      case "daemon_draining":
        this.draining = true;
        this.pushToast("warning", "Daemon draining", "The daemon is shutting down.");
        break;
    }
  }

  // -------------------------------------------------------------------------
  // Session channel
  // -------------------------------------------------------------------------

  public attach(sessionId: string) {
    if (
      this.activeSessionId === sessionId &&
      this.sessionWs &&
      this.sessionWs.readyState === WebSocket.OPEN
    ) {
      return;
    }

    this.cancelSessionReconnect();
    const generation = ++this.sessionGeneration;
    this.clearSessionState();
    this.activeSessionId = sessionId;

    if (this.sessionWs) {
      this.detachSocketHandlers(this.sessionWs);
      this.sessionWs.close();
      this.sessionWs = null;
    }

    const ws = this.openSocket();
    this.sessionWs = ws;

    ws.onopen = () => {
      if (generation !== this.sessionGeneration) return;
      ws.send(wireEnvelope({ attach: sessionId }, this.project));
    };

    ws.onmessage = (event) => {
      if (generation !== this.sessionGeneration) return;
      try {
        this.handleFrame(JSON.parse(event.data as string) as Wire);
      } catch (err) {
        console.error("session frame parse error:", err, event.data);
      }
    };

    ws.onclose = () => {
      if (generation !== this.sessionGeneration) return;
      this.sessionWs = null;
      const wasAttached = this.sessionAttached;
      this.sessionAttached = false;
      if (this.activeSessionId !== sessionId) return;
      // Reattach with backoff while the daemon still hosts the session; the
      // Welcome replay restores the transcript, so no messages are lost.
      if (wasAttached) {
        this.pushToast("warning", "Session detached", "Reconnecting…");
      }
      this.scheduleSessionReconnect(sessionId);
    };

    ws.onerror = () => {
      if (generation !== this.sessionGeneration) return;
      this.sessionAttached = false;
    };
  }

  private scheduleSessionReconnect(sessionId: string) {
    this.cancelSessionReconnect();
    this.sessionReconnectTimer = window.setTimeout(() => {
      this.sessionReconnectTimer = null;
      if (this.activeSessionId !== sessionId || this.sessionAttached) return;
      // Attach only if the monitor still lists the session (or the monitor is
      // down too — the attach attempt is cheap and fails fast).
      this.attach(sessionId);
    }, this.sessionReconnectDelay);
    this.sessionReconnectDelay = Math.min(this.sessionReconnectDelay * 2, RECONNECT_MAX_MS);
  }

  private cancelSessionReconnect() {
    if (this.sessionReconnectTimer !== null) {
      window.clearTimeout(this.sessionReconnectTimer);
      this.sessionReconnectTimer = null;
    }
  }

  /** Create a new hosted session via the control plane and attach to it. */
  public newSession() {
    if (this.newSessionWs) {
      this.detachSocketHandlers(this.newSessionWs);
      this.newSessionWs.close();
      this.newSessionWs = null;
    }

    const ws = this.openSocket();
    this.newSessionWs = ws;

    ws.onopen = () => {
      ws.send(
        wireEnvelope(
          {
            control: {
              verb: "create_session",
              project: this.project ?? this.daemonProjectRoot ?? "/",
            },
          },
          this.project,
        ),
      );
    };

    ws.onmessage = (event) => {
      try {
        const frame = JSON.parse(event.data as string) as Wire;
        if (frame.type === "ControlReply") {
          if (frame.ok && frame.session_id) {
            this.attach(frame.session_id);
          } else {
            this.pushToast("error", "Could not create session", frame.error ?? "unknown error");
          }
          this.detachSocketHandlers(ws);
          ws.close();
          if (this.newSessionWs === ws) this.newSessionWs = null;
        }
      } catch (err) {
        console.error("control frame parse error:", err, event.data);
      }
    };

    ws.onclose = () => {
      if (this.newSessionWs === ws) this.newSessionWs = null;
    };
  }

  /**
   * End a hosted session (ADR-0112): the panel-side counterpart of the TUI's
   * `/exit` — "I am done with this session", not "detach". Reuses the
   * `kill_session` control verb (one-shot control connection, mirroring
   * `newSession`), which tears the session down server-side and publishes
   * `SessionRemoved`; the monitor stream then drops the row and, if it was
   * the active session, `handleMonitorFrame` clears the view. Disk history
   * is kept — ending is not deleting.
   */
  public endSession(id: string) {
    const ws = this.openSocket();
    ws.onopen = () => {
      ws.send(
        wireEnvelope(
          {
            control: {
              verb: "kill_session",
              session_id: id,
            },
          },
          this.project,
        ),
      );
    };
    ws.onmessage = (event) => {
      try {
        const frame = JSON.parse(event.data as string) as Wire;
        if (frame.type === "ControlReply" && !frame.ok) {
          this.pushToast("error", "Could not end session", frame.error ?? "unknown error");
        }
      } catch (err) {
        console.error("control frame parse error:", err, event.data);
      }
      this.detachSocketHandlers(ws);
      ws.close();
    };
    ws.onerror = () => {
      this.pushToast("error", "Could not end session", "control connection failed");
    };
  }

  // -------------------------------------------------------------------------
  // Frame dispatch
  // -------------------------------------------------------------------------

  private handleFrame(frame: Wire) {
    switch (frame.type) {
      case "Welcome":
        this.sessionAttached = true;
        this.sessionReconnectDelay = RECONNECT_BASE_MS;
        this.sessionError = null;
        this.roundCounter = frame.round_counter;
        this.providerInfo = { provider: frame.provider, model: frame.model };
        this.feed = frame.messages
          .filter((m) => !m.hidden)
          .map((m) => this.messageItem(m));
        break;
      case "Pick":
        this.sessionError =
          "The daemon asked this client to pick a session — not supported by the web panel yet.";
        break;
      case "Error":
        this.sessionError = frame.message;
        if (frame.code === "version_mismatch") {
          this.pushToast(
            "error",
            "Client/daemon version mismatch",
            "Run `neenee stop`, then reload this panel — the daemon restarts on demand at the new build.",
          );
        } else {
          this.pushToast("error", "Daemon error", frame.message);
        }
        break;
      case "ControlReply":
        // Handled where issued (newSession); nothing to do on other sockets.
        break;
      case "Monitor":
        this.handleMonitorFrame(frame as MonitorFrame);
        break;
      case "Response":
        this.handleResponse(frame as AgentResponse);
        break;
      case "Request":
      case "Select":
        break;
    }
  }

  private handleResponse(resp: AgentResponse) {
    if ("Round" in resp) {
      this.handleRoundEvent(resp.Round.session_id, resp.Round.event);
    } else if ("ProviderPicker" in resp) {
      this.providerPicker = resp.ProviderPicker;
    } else if ("ProviderKeys" in resp) {
      this.providerKeys = resp.ProviderKeys;
    } else if ("ProviderSwitched" in resp) {
      this.providerInfo = {
        provider: resp.ProviderSwitched.provider,
        model: resp.ProviderSwitched.model,
      };
    } else if ("ConversationCleared" in resp) {
      // `/new` blanked the transcript: the harness switched this attached
      // connection to a brand-new empty session. The variant is a unit — it
      // carries no id — so the new session id arrives with the next `Round`
      // event (see the handoff in handleRoundEvent) or a monitor row. The
      // socket stays bound, so the composer must stay usable.
      this.clearSessionState();
      this.sessionAttached = true;
    } else if ("ConversationReplaced" in resp) {
      this.activeSessionId = resp.ConversationReplaced.session_id;
      this.streamingAssistantText = "";
      this.streamingReasoningText = "";
      this.liveTools = {};
      this.feed = this.buildReplacedFeed(
        resp.ConversationReplaced.messages.filter((m) => !m.hidden),
        resp.ConversationReplaced.commands ?? [],
      );
    } else if ("Error" in resp) {
      this.sessionError = resp.Error;
      this.pushToast("error", "Agent error", resp.Error);
    } else if ("Exit" in resp) {
      this.sessionAttached = false;
    }
  }

  /**
   * Rebuild the feed after a session switch: dialogue messages plus the
   * persisted command ledger, merged by timestamp (both epoch ms).
   */
  private buildReplacedFeed(messages: Message[], commands: CommandRecord[]): FeedItem[] {
    const items: FeedItem[] = [
      ...messages.map((m) => this.messageItem(m)),
      ...commands.map((c) => this.commandItem(c)),
    ];
    const time = (item: FeedItem): number =>
      item.kind === "message" ? messageTimeMs(item.message) : item.record.timestamp;
    return items.sort((a, b) => time(a) - time(b));
  }

  // -------------------------------------------------------------------------
  // Round events
  // -------------------------------------------------------------------------

  private handleRoundEvent(sessionId: string, event: RoundEvent) {
    if (sessionId !== this.activeSessionId) {
      if (this.activeSessionId !== null) return;
      // Session handoff: after `/new` (`ConversationCleared` carries no id)
      // the daemon rebound this attached connection to the fresh session, and
      // its id first shows up tagging that session's round events. Events for
      // *other* live sessions (e.g. a `/btw` aside streaming alongside) still
      // arrive tagged; only adopt when we hold no current session.
      this.activeSessionId = sessionId;
    }

    if (typeof event === "string") {
      if (event === "StreamStart") {
        this.streamingAssistantText = "";
        this.streamingReasoningText = "";
      } else if (event === "StreamDiscard") {
        this.streamingAssistantText = "";
        this.streamingReasoningText = "";
      }
      return;
    }

    if ("StreamDelta" in event) {
      this.streamingAssistantText += event.StreamDelta;
    } else if ("StreamReasoningDelta" in event) {
      this.streamingReasoningText += event.StreamReasoningDelta;
    } else if ("StreamReasoningEnd" in event) {
      this.streamingReasoningText = event.StreamReasoningEnd;
    } else if ("StreamEnd" in event) {
      this.commitStreamingMessage(event.StreamEnd);
    } else if ("Text" in event) {
      // Non-streamed assistant reply (fallback path, "[Interrupted]",
      // hook-blocked prompts). Emitted only when nothing streamed.
      const text = event.Text;
      if (text.trim().length > 0) {
        this.pushFeed({
          kind: "message",
          key: this.feedKey(),
          message: {
            role: "Assistant",
            content: text,
            timestamp: Math.floor(Date.now() / 1000),
            hidden: false,
          },
        });
      }
    } else if ("CommandResult" in event) {
      const { name, args, result } = event.CommandResult;
      this.pushFeed({
        kind: "command",
        key: this.feedKey(),
        record: {
          name,
          args,
          status: "Error" in result ? "error" : "success",
          result,
          timestamp: Date.now(),
        },
      });
    } else if ("ToolCall" in event) {
      const call = event.ToolCall;
      this.liveTools[call.id] = {
        id: call.id,
        name: call.name,
        arguments: call.arguments,
        status: "running",
        stdout: "",
        stderr: "",
      };
    } else if ("ToolStream" in event) {
      const t = this.liveTools[event.ToolStream.id];
      if (t) {
        if ("Stdout" in event.ToolStream.stream) t.stdout += event.ToolStream.stream.Stdout;
        else t.stderr += event.ToolStream.stream.Stderr;
      }
    } else if ("ToolResult" in event) {
      const r = event.ToolResult;
      const entry = this.liveTools[r.id];
      if (entry) {
        const failed =
          typeof r.structured === "object" &&
          r.structured !== null &&
          ("Error" in r.structured || "PermissionDenied" in r.structured);
        entry.status = failed ? "failed" : "completed";
        entry.output = r.output;
        entry.durationMs = r.duration_ms;
      }
    } else if ("ToolCancelled" in event) {
      const entry = this.liveTools[event.ToolCancelled.id];
      if (entry) entry.status = "cancelled";
    } else if ("PermissionRequest" in event) {
      this.pendingPermission = { request: event.PermissionRequest, origin: TOP_LEVEL_ORIGIN };
    } else if ("UserQuestionRequest" in event) {
      this.pendingQuestion = { request: event.UserQuestionRequest, origin: TOP_LEVEL_ORIGIN };
    } else if ("InputRequest" in event) {
      this.pendingInput = { request: event.InputRequest, origin: TOP_LEVEL_ORIGIN };
    } else if ("TodosUpdated" in event) {
      this.todos = event.TodosUpdated;
    } else if ("ContextTokens" in event) {
      this.contextTokens = event.ContextTokens.tokens;
    } else if ("HarnessState" in event) {
      this.roundCounter = event.HarnessState.round_counter;
      this.autopilot = event.HarnessState.autopilot;
      if (event.HarnessState.loop_status === "idle") {
        this.activity = null;
        this.currentTurn = null;
      }
    } else if ("AutopilotChanged" in event) {
      this.autopilot = event.AutopilotChanged;
    } else if ("RoundCompleted" in event) {
      this.lastRound = event.RoundCompleted;
      this.roundCounter = event.RoundCompleted.round;
      this.activity = null;
      this.currentTurn = null;
    } else if ("TurnStarted" in event) {
      this.currentTurn = event.TurnStarted.turn;
      this.roundCounter = event.TurnStarted.round;
    } else if ("Activity" in event) {
      this.activity = event.Activity;
    } else if ("Compacted" in event) {
      const c = event.Compacted;
      this.pushToast(
        "info",
        "Context compacted",
        `${c.archived_messages} messages archived (${c.window_tokens_before} → ${c.window_tokens_after} tokens).`,
      );
    } else if ("RetryScheduled" in event) {
      const r = event.RetryScheduled;
      this.pushToast(
        "warning",
        `Retry ${r.attempt}/${r.max_attempts} in ${Math.round(r.delay_ms / 1000)}s`,
        r.message,
      );
    } else if ("UserInputInserted" in event) {
      this.appendInsertedInput(event.UserInputInserted);
    } else if ("NextRoundStarted" in event) {
      this.appendInsertedInput(event.NextRoundStarted);
    } else if ("UserInputUnavailable" in event) {
      this.pushToast(
        "info",
        "Input deferred",
        "The round stopped accepting input first; it will run next round.",
      );
    } else if ("Notice" in event) {
      this.handleNotice(event.Notice);
    } else if ("Error" in event) {
      this.sessionError = event.Error;
      this.pushToast("error", "Turn error", event.Error);
    } else if ("UnsentInput" in event) {
      this.handleUnsentInput(event.UnsentInput);
    } else if ("Envoy" in event) {
      this.handleEnvoyEvent(event.Envoy.parent_call_id, event.Envoy.event);
    }
    // UserInputCancelled / UserInputCancelFailed concern queued inserts this
    // client never issues; nothing to surface.
  }

  /**
   * A queued insert crossed the turn boundary and joined the live transcript.
   * Dedupe against the composer's optimistic echo: a Chat we painted moments
   * ago may be re-reported here when the daemon admitted it mid-round.
   */
  private appendInsertedInput(input: QueuedUserInput) {
    const text = input.display_text ?? input.text;
    const now = Date.now();
    const tail = this.feed[this.feed.length - 1];
    if (
      tail?.kind === "message" &&
      tail.message.role === "User" &&
      tail.message.content === text &&
      now - messageTimeMs(tail.message) < 5_000
    ) {
      return;
    }
    this.pushFeed({
      kind: "message",
      key: this.feedKey(),
      message: {
        role: "User",
        content: text,
        images: input.images,
        sent_at_ms: input.sent_at_ms ?? now,
        hidden: false,
      },
    });
  }

  /** The round died before any output: drop the optimistic echo, restore the draft. */
  private handleUnsentInput(unsent: { prompt: string; images: ImagePart[] }) {
    for (let i = this.feed.length - 1; i >= 0; i--) {
      const item = this.feed[i];
      if (item.kind === "message" && item.message.role === "User") {
        if (item.message.content === unsent.prompt) this.feed.splice(i, 1);
        break;
      }
    }
    this.restoredDraft = { text: unsent.prompt, images: unsent.images };
    this.pushToast(
      "warning",
      "Prompt not sent",
      "Interrupted before any output; your prompt was restored to the composer.",
    );
  }

  // -------------------------------------------------------------------------
  // Envoy events (nested under a parent `task` tool call; ADR-0029)
  // -------------------------------------------------------------------------

  private handleEnvoyEvent(parentCallId: string, event: EnvoyEvent) {
    const parent = this.liveTools[parentCallId];
    const origin: RequestOrigin = {
      parentCallId,
      label: parent?.envoy?.profile ?? null,
    };

    if ("PermissionRequest" in event) {
      this.pendingPermission = { request: event.PermissionRequest, origin };
      return;
    }
    if ("UserQuestionRequest" in event) {
      this.pendingQuestion = { request: event.UserQuestionRequest, origin };
      return;
    }
    if ("InputRequest" in event) {
      this.pendingInput = { request: event.InputRequest, origin };
      return;
    }
    if (!parent) return; // stray envoy event for a tool we never saw
    if (!parent.envoy) {
      parent.envoy = {
        profile: null,
        activity: null,
        text: "",
        streamingText: "",
        reasoning: [],
        streamingReasoning: "",
        tools: [],
      };
    }
    const envoy = parent.envoy;

    if ("Started" in event) {
      envoy.profile = event.Started.profile;
    } else if ("Notice" in event) {
      this.handleNotice(event.Notice);
    } else if ("StreamStart" in event) {
      envoy.streamingText = "";
    } else if ("StreamDelta" in event) {
      envoy.streamingText += event.StreamDelta;
    } else if ("StreamEnd" in event) {
      const finalText = event.StreamEnd || envoy.streamingText;
      envoy.text = envoy.text ? `${envoy.text}\n\n${finalText}` : finalText;
      envoy.streamingText = "";
    } else if ("StreamReasoningStart" in event) {
      envoy.streamingReasoning = "";
    } else if ("StreamReasoningDelta" in event) {
      envoy.streamingReasoning += event.StreamReasoningDelta;
    } else if ("StreamReasoningEnd" in event) {
      const finalReasoning = event.StreamReasoningEnd || envoy.streamingReasoning;
      if (finalReasoning.trim()) {
        envoy.reasoning = [...envoy.reasoning, finalReasoning];
      }
      envoy.streamingReasoning = "";
    } else if ("ToolCall" in event) {
      const call = event.ToolCall;
      envoy.tools.push({
        id: call.id,
        name: call.name,
        arguments: call.arguments,
        status: "running",
      });
    } else if ("ToolResult" in event) {
      const r = event.ToolResult;
      const tool = envoy.tools.find((t) => t.id === r.id);
      if (tool) {
        tool.status = "completed";
        tool.output = r.output;
        tool.durationMs = r.duration_ms;
      }
    } else if ("Activity" in event) {
      envoy.activity = event.Activity;
    }
  }

  private handleNotice(notice: AgentNotice) {
    const severity =
      notice.severity === "error" ? "error" : notice.severity === "warning" ? "warning" : "info";
    this.pushToast(severity, notice.title, notice.body);
  }

  private commitStreamingMessage(fullText: string) {
    const text = fullText.trim().length > 0 ? fullText : this.streamingAssistantText;
    if (text.trim().length > 0) {
      this.pushFeed({
        kind: "message",
        key: this.feedKey(),
        message: {
          role: "Assistant",
          content: text,
          timestamp: Math.floor(Date.now() / 1000),
          hidden: false,
        },
      });
    }
    this.streamingAssistantText = "";
    this.streamingReasoningText = "";
  }

  // -------------------------------------------------------------------------
  // Outgoing requests
  // -------------------------------------------------------------------------

  private send(req: AgentRequest) {
    if (this.sessionWs && this.sessionWs.readyState === WebSocket.OPEN) {
      this.sessionWs.send(requestFrame(req));
    } else {
      this.pushToast("warning", "Not attached", "Connect to a session first.");
    }
  }

  public sendChat(text: string, images: ImagePart[] = []) {
    const trimmed = text.trim();
    if (!trimmed && images.length === 0) return;
    if (trimmed.startsWith("/")) {
      this.pushFeed({
        kind: "message",
        key: this.feedKey(),
        message: { role: "User", content: trimmed, sent_at_ms: Date.now(), hidden: false },
      });
      this.send({ SlashCommand: trimmed });
      return;
    }
    this.pushFeed({
      kind: "message",
      key: this.feedKey(),
      message: {
        role: "User",
        content: trimmed,
        images: images.length > 0 ? images : undefined,
        sent_at_ms: Date.now(),
        hidden: false,
      },
    });
    // `images` is required on the wire (no serde default on the Rust field).
    this.send({ Chat: { text: trimmed, images, sent_at_ms: Date.now() } });
  }

  public interrupt() {
    this.send({ Interrupt: null });
  }

  /** Switch the active model (and persist it as the default). */
  public setDefaultModel(id: string) {
    this.send({ SetDefaultModel: { id } });
  }

  /** Delete a session (active or archived) by id or short-id prefix. */
  public deleteSession(id: string) {
    this.send({ DeleteSession: { id } });
  }

  /** Set a session's display title; `null` clears back to the AI/first-prompt fallback. */
  public renameSession(id: string, title: string | null) {
    this.send({ RenameSession: { id, title } });
  }

  public resolvePermission(decision: PermissionDecision) {
    if (!this.pendingPermission) return;
    this.send({
      PermissionReply: {
        request_id: this.pendingPermission.request.id,
        decision,
        parent_call_id: this.pendingPermission.origin.parentCallId,
      },
    });
    this.pendingPermission = null;
  }

  public answerQuestion(answers: string[][]) {
    if (!this.pendingQuestion) return;
    this.send({
      UserQuestionReply: {
        request_id: this.pendingQuestion.request.id,
        answers,
        parent_call_id: this.pendingQuestion.origin.parentCallId,
      },
    });
    this.pendingQuestion = null;
  }

  public replyInput(text: string) {
    if (!this.pendingInput) return;
    this.send({
      InputReply: {
        request_id: this.pendingInput.request.id,
        text,
        parent_call_id: this.pendingInput.origin.parentCallId,
      },
    });
    this.pendingInput = null;
  }

  /** Consume the restored composer draft (one-shot). */
  public takeRestoredDraft(): { text: string; images: ImagePart[] } | null {
    const draft = this.restoredDraft;
    this.restoredDraft = null;
    return draft;
  }

  // -------------------------------------------------------------------------
  // Misc
  // -------------------------------------------------------------------------

  public pushToast(severity: Toast["severity"], title: string, body?: string) {
    const toast: Toast = { id: this.nextToastId++, severity, title, body };
    this.toasts.push(toast);
    const keep = 4;
    if (this.toasts.length > keep) this.toasts.splice(0, this.toasts.length - keep);
    window.setTimeout(() => this.dismissToast(toast.id), 8000);
  }

  public dismissToast(id: number) {
    this.toasts = this.toasts.filter((t) => t.id !== id);
  }

  private feedKey(): string {
    return `f${this.nextFeedKey++}`;
  }

  private pushFeed(item: FeedItem) {
    this.feed.push(item);
  }

  private messageItem(message: Message): FeedItem {
    return { kind: "message", key: this.feedKey(), message };
  }

  private commandItem(record: CommandRecord): FeedItem {
    return { kind: "command", key: this.feedKey(), record };
  }

  private clearSessionState() {
    this.cancelSessionReconnect();
    this.activeSessionId = null;
    this.sessionAttached = false;
    this.feed = [];
    this.streamingAssistantText = "";
    this.streamingReasoningText = "";
    this.liveTools = {};
    this.todos = { items: [], next_id: 1, updated_at_round: 0 };
    this.contextTokens = null;
    this.roundCounter = 0;
    this.autopilot = false;
    this.activity = null;
    this.currentTurn = null;
    this.lastRound = null;
    this.providerInfo = null;
    this.providerPicker = null;
    this.providerKeys = [];
    this.pendingPermission = null;
    this.pendingQuestion = null;
    this.pendingInput = null;
    this.sessionError = null;
    this.restoredDraft = null;
  }

  private detachSocketHandlers(ws: WebSocket) {
    ws.onopen = null;
    ws.onmessage = null;
    ws.onclose = null;
    ws.onerror = null;
  }
}

export const daemon = new DaemonStore();
