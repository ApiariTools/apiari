export interface Workspace {
  name: string;
  remote?: string;
  tts_voice?: string;
  tts_speed?: number;
}

export interface Bot {
  name: string;
  color?: string;
  role?: string;
  description?: string;
  provider?: string;
  model?: string;
  watch: string[];
}

export interface Worker {
  id: string;
  branch: string;
  status: string;
  agent: string;
  pr_url: string | null;
  pr_title: string | null;
  description: string | null;
  elapsed_secs: number | null;
  dispatched_by: string | null;
  review_state?: string;
  ci_status?: string;
  total_comments?: number;
  open_comments?: number;
  resolved_comments?: number;
}

export interface Repo {
  name: string;
  path: string;
  has_swarm: boolean;
  is_clean: boolean;
  branch: string;
  workers: Worker[];
}

export interface Message {
  id: number;
  workspace: string;
  bot: string;
  role: string;
  content: string;
  attachments: string | null;
  created_at: string;
}

export interface WorkerDetail extends Worker {
  prompt: string | null;
  output: string | null;
  conversation: WorkerMessage[];
}

export interface WorkerMessage {
  role: string;
  content: string;
  timestamp?: string;
}

export interface CrossWorkspaceBot {
  workspace: string;
  bot: Bot;
  remote?: string;
}

export interface Doc {
  name: string;
  title: string;
  content?: string;
  updated_at: string;
}

export interface Followup {
  id: string;
  workspace: string;
  bot: string;
  action: string;
  created_at: string;
  fires_at: string;
  status: "pending" | "fired" | "cancelled";
}

export interface ResearchTask {
  id: string;
  workspace: string;
  topic: string;
  status: string;
  error: string | null;
  started_at: string;
  completed_at: string | null;
  output_file: string | null;
}

export interface Signal {
  id: number;
  workspace: string;
  source: string;
  title: string;
  severity: string;
  status: string;
  url: string | null;
  created_at: string;
  updated_at: string;
  resolved_at: string | null;
}

export interface ProviderCapability {
  name: string;
  installed: boolean;
  binary_path: string | null;
  sandbox_flag_supported: boolean | null;
  approval_flag_supported: boolean | null;
  notes: string[];
}

export interface BotTurnFailure {
  id: number;
  bot: string;
  provider: string | null;
  source: string;
  error_text: string;
  created_at: string;
}

export interface BotDebugData {
  workspace: string;
  bot: string;
  provider: string | null;
  status: {
    status: string;
    streaming_content: string;
    tool_name: string | null;
  } | null;
  recent_failures: BotTurnFailure[];
  recent_messages: Message[];
}
