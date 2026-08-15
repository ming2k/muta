/**
 * Wire contracts and DTO types for Neenee Web Client.
 * Directly maps to crates/neenee-contracts.
 */

export type Role = "user" | "assistant" | "system";

export interface ImagePart {
  media_type: string;
  data: string; // Base64 encoded
}

export interface ToolCall {
  id: string;
  name: string;
  arguments: string;
}

export interface ToolOutput {
  type: string;
  content?: string;
  exit_code?: number;
  [key: string]: unknown;
}

export interface Message {
  role: Role;
  content: string;
  images?: ImagePart[];
  timestamp?: number;
  tool_calls?: ToolCall[];
}

export interface TodoItem {
  id: string;
  title: string;
  completed: boolean;
  priority?: "low" | "medium" | "high";
}

export interface ContextTokenSnapshot {
  tokens: number;
  source: "api" | "projection";
}

export interface ProviderModelInfo {
  model: string;
  protocol: string;
  effort?: string;
  thinking?: boolean;
  last_used_ms?: number;
  favorite?: boolean;
}

export interface ProviderPickerRow {
  id: string;
  name: string;
  model: string;
  models: string[];
  model_info?: ProviderModelInfo[];
  builtin: boolean;
  protocol: string;
  base_url: string;
  key_ready: boolean;
}

export interface ProviderPickerSnapshot {
  default_id: string;
  rows: ProviderPickerRow[];
}

export type SessionStatus = "idle" | "running" | "blocked" | "completed" | "error";

export interface MonitoredSession {
  id: string;
  title: string;
  status: SessionStatus;
  provider: string;
  model: string;
  project_root: string;
  created_at: number;
  updated_at: number;
  active_tool?: string;
  context_tokens?: number;
}

export type MonitorEvent =
  | { type: "Snapshot"; sessions: MonitoredSession[] }
  | { type: "SessionAdded"; session: MonitoredSession }
  | { type: "SessionUpdated"; session: MonitoredSession }
  | { type: "SessionRemoved"; session_id: string }
  | { type: "DaemonDraining" };

export type PermissionDecision = "allow_once" | "allow_always" | "deny";

export type AgentRequest =
  | { type: "Chat"; text: string; images?: ImagePart[]; sent_at_ms?: number }
  | { type: "SlashCommand"; command: string }
  | { type: "Interrupt" }
  | { type: "PermissionReply"; request_id: string; decision: PermissionDecision; parent_call_id?: string }
  | { type: "UserQuestionReply"; request_id: string; answers: string[][]; parent_call_id?: string }
  | { type: "ShellCommand"; command: string };

export type RoundEvent =
  | { type: "AssistantDelta"; delta: string; start: boolean }
  | { type: "AssistantEnd"; full_text: string }
  | { type: "ReasoningDelta"; delta: string; start: boolean }
  | { type: "ReasoningEnd"; full_text: string }
  | { type: "ToolCall"; id: string; name: string; arguments: string }
  | { type: "ToolResult"; id: string; name: string; output: string; structured: ToolOutput; duration_ms: number }
  | { type: "ToolStream"; id: string; stream: unknown }
  | { type: "TodosUpdated"; todos: TodoItem[] }
  | { type: "ContextTokens"; snapshot: ContextTokenSnapshot };

export type AgentResponse =
  | { type: "Round"; session_id: string; event: RoundEvent }
  | { type: "ProviderKeys"; keys: [string, boolean][] }
  | { type: "ProviderPicker"; snapshot: ProviderPickerSnapshot }
  | { type: "ConversationCleared" }
  | { type: "ConversationReplaced"; session_id: string; messages: Message[] }
  | { type: "Error"; message: string }
  | { type: "Exit" };

export interface LiveToolExecution {
  id: string;
  name: string;
  arguments: string;
  status: "running" | "completed" | "failed";
  output?: string;
  durationMs?: number;
}
