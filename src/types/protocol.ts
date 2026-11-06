/**
 * Orion Wire Protocol & Domain Types
 * Shared between Rust backend (Tauri IPC / Axum WebSockets) and React 19 frontend.
 */

export type AgentKind =
  | "claude"
  | "codex"
  | "antigravity"
  | "opencode"
  | "cursor"
  | "grok"
  | "omp"
  | "omo"
  | "pi"
  | "hermes"
  | "devin"
  | "shell";

export type AgentStatus = "idle" | "running" | "needsinput" | "done" | "error";

export interface DiscoveredAgent {
  kind: AgentKind;
  name: String;
  command: String;
  path: String | null;
  installed: boolean;
  version: String | null;
}

export type TabLayout = "single" | "splithorizontal" | "splitvertical" | "grid";

export interface WorkspaceInfo {
  id: string;
  name: string;
  working_dir: string;
  tabs: TabInfo[];
  active_tab_id: string | null;
}

export interface TabInfo {
  id: string;
  title: string;
  panes: PaneInfo[];
  active_pane_id: string | null;
  layout: TabLayout;
}

export interface PaneInfo {
  id: string;
  tab_id: string;
  title: string;
  agent_kind: AgentKind;
  status: AgentStatus;
  cwd: string;
  pid: number | null;
  git_branch: string | null;
  worktree_path: string | null;
  has_unread: boolean;
  plan_summary: PlanSummary | null;
}

export interface PlanSummary {
  total_steps: number;
  completed_steps: number;
  active_step_label: string | null;
  steps: PlanStep[];
}

export type PlanStepStatus = "pending" | "inprogress" | "completed" | "failed" | "skipped";

export interface PlanStep {
  id: string;
  label: string;
  status: PlanStepStatus;
  duration_seconds: number | null;
}

export type PromptType =
  | { command_approval: { command: string } }
  | { file_edit_approval: { file_path: string; diff: string | null } }
  | "multiple_choice"
  | "text_input"
  | "model_selection";

export interface PromptOption {
  index: number;
  label: string;
  description: string | null;
  is_danger: boolean;
}

export interface ApprovalRequest {
  id: string;
  pane_id: string;
  prompt_type: PromptType;
  question: string;
  description: string | null;
  options: PromptOption[];
  default_option_index: number | null;
  created_at: string;
}

export interface ApprovalResponse {
  request_id: string;
  pane_id: string;
  selected_option_index: number | null;
  text_answer: string | null;
  confirmed: boolean;
}

export type TurnRole = "user" | "assistant" | "system";

export type ToolCallStatus = "running" | "success" | "failed" | "denied";

export interface ToolCallItem {
  id: string;
  tool_name: string;
  parameters: unknown;
  output: string | null;
  status: ToolCallStatus;
  duration_ms: number | null;
}

export interface ConversationTurn {
  id: string;
  pane_id: string;
  role: TurnRole;
  content: string;
  tool_calls: ToolCallItem[];
  timestamp: string;
}

export interface TerminalSize {
  rows: number;
  cols: number;
  pixel_width: number;
  pixel_height: number;
}

export interface RemoteAccessStatus {
  tailscale_active: boolean;
  tailscale_ip: string | null;
  tailnet_url: string | null;
  port: number;
  qr_code_svg: string | null;
  pairing_code: string | null;
}
