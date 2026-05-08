import { vi } from "vitest";

export const getWorkspaces = vi.fn().mockResolvedValue([
  { name: "apiari" },
  { name: "mgm" },
]);

export const getBots = vi.fn().mockResolvedValue([
  {
    name: "Main",
    color: "#f5c542",
    role: "Coordinator",
    description: "General workspace coordinator for planning, execution, and follow-through.",
    provider: "claude",
    model: "sonnet",
    watch: [],
  },
  {
    name: "Customer",
    color: "#e85555",
    role: "Customer bot",
    description: "Tracks product issues and customer-facing followups across the workspace.",
    provider: "codex",
    model: "gpt-5.5",
    watch: ["sentry"],
  },
]);

export const getWorkers = vi.fn().mockResolvedValue([]);
export const getWorkerEnvironment = vi.fn().mockResolvedValue({
  repo: "apiari",
  ready: true,
  git_worktree_metadata_writable: true,
  frontend_toolchain_required: true,
  frontend_toolchain_ready: true,
  worktree_links_ready: true,
  setup_commands_ready: true,
  blockers: [],
  suggested_fixes: [],
});

export const getTasks = vi.fn().mockResolvedValue([
  {
    id: "task-1",
    title: "Tighten mobile card spacing",
    stage: "Human Review",
    lifecycle_state: "Human Review",
    source: "manual",
    worker_id: "worker-1",
    pr_url: "https://github.com/example/apiari/pull/12",
    pr_number: 12,
    repo: "apiari",
    created_at: "2026-01-01T00:00:00Z",
    updated_at: "2026-01-01T02:00:00Z",
    resolved_at: null,
    latest_attempt: {
      worker_id: "worker-1",
      role: "implementation",
      state: "succeeded",
      detail: "Opened PR for review.",
      created_at: "2026-01-01T00:10:00Z",
      updated_at: "2026-01-01T02:00:00Z",
      completed_at: "2026-01-01T02:00:00Z",
    },
    cursor: null,
  },
]);

export const getRepos = vi.fn().mockResolvedValue([]);

export const getConversations = vi.fn().mockResolvedValue([
  { id: 1, workspace: "apiari", bot: "Main", role: "user", content: "hello", attachments: null, created_at: new Date().toISOString() },
  { id: 2, workspace: "apiari", bot: "Main", role: "assistant", content: "Hi! How can I help?", attachments: null, created_at: new Date().toISOString() },
]);

export const getBotStatus = vi.fn().mockResolvedValue({
  status: "idle",
  streaming_content: "",
  tool_name: null,
});

export const getUnread = vi.fn().mockResolvedValue({ Customer: 2 });
export const markSeen = vi.fn().mockResolvedValue(undefined);
export const sendMessage = vi.fn().mockResolvedValue({ ok: true });
export const cancelBot = vi.fn().mockResolvedValue({ ok: true });
export const getWorkerDetail = vi.fn().mockResolvedValue(null);
export const sendWorkerMessage = vi.fn().mockResolvedValue({ ok: true });
export const getUsage = vi.fn().mockResolvedValue({ installed: false, providers: [], updated_at: null });
export const getDocs = vi.fn().mockResolvedValue([
  { name: "architecture.md", title: "Architecture", updated_at: "2026-01-01T00:00:00Z" },
  { name: "setup.md", title: "Setup Guide", updated_at: "2026-01-01T00:00:00Z" },
]);
export const getDoc = vi.fn().mockResolvedValue({
  name: "architecture.md",
  title: "Architecture",
  content: "# Architecture\n\nDetails here",
  updated_at: "2026-01-01T00:00:00Z",
});
export const saveDoc = vi.fn().mockResolvedValue({ ok: true });
export const deleteDoc = vi.fn().mockResolvedValue({ ok: true });
export const getFollowups = vi.fn().mockResolvedValue([]);
export const cancelFollowup = vi.fn().mockResolvedValue({ ok: true });
export const getResearchTasks = vi.fn().mockResolvedValue([]);
export const startResearch = vi.fn().mockResolvedValue({ id: "research-1", topic: "test", status: "running" });
export const getSignals = vi.fn().mockResolvedValue([]);
export const getProviderCapabilities = vi.fn().mockResolvedValue([
  {
    name: "claude",
    installed: true,
    binary_path: "/usr/local/bin/claude",
    sandbox_flag_supported: null,
    approval_flag_supported: null,
    notes: [],
  },
  {
    name: "codex",
    installed: true,
    binary_path: "/usr/local/bin/codex",
    sandbox_flag_supported: true,
    approval_flag_supported: false,
    notes: ["Current codex exec CLI does not support --approval-policy."],
  },
]);
export const getBotDebugData = vi.fn().mockResolvedValue({
  workspace: "apiari",
  bot: "Main",
  provider: "claude",
  effective_config: {
    api_name: "Main",
    resolved_bee_name: "Bee",
    workspace_authority: "autonomous",
    configured_execution_policy: "autonomous",
    effective_execution_policy: "autonomous",
    provider: "claude",
    model: "sonnet",
    role: "Coordinator",
    color: "#f5c542",
    max_turns: 20,
    max_session_turns: 50,
    heartbeat: null,
    signal_sources: ["swarm", "github"],
  },
  status: { status: "idle", streaming_content: "", tool_name: null },
  recent_failures: [],
  recent_decisions: [],
  recent_messages: [],
});
export const connectWebSocket = vi.fn().mockReturnValue({ close: vi.fn() });

// v2 Worker API mocks
export const listWorkersV2 = vi.fn().mockResolvedValue([]);
export const getWorkerV2 = vi.fn().mockResolvedValue(null);
export const sendWorkerMessageV2 = vi.fn().mockResolvedValue(undefined);
export const cancelWorkerV2 = vi.fn().mockResolvedValue(undefined);
export const requeueWorkerV2 = vi.fn().mockResolvedValue(undefined);
export const requestWorkerReview = vi.fn().mockResolvedValue(undefined);
export const listWorkerReviews = vi.fn().mockResolvedValue([]);
export const promoteWorker = vi.fn().mockResolvedValue({ ok: true, detail: '' });
export const redispatchWorker = vi.fn().mockResolvedValue({ ok: true, detail: '' });
export const closeWorker = vi.fn().mockResolvedValue({ ok: true, detail: '' });
export const getWorkerDiff = vi.fn().mockResolvedValue(null);

// Auto Bot API mocks
export const listAutoBots = vi.fn().mockResolvedValue([]);
export const getAutoBot = vi.fn().mockResolvedValue(null);
export const createAutoBot = vi.fn().mockResolvedValue(null);
export const updateAutoBot = vi.fn().mockResolvedValue(null);
export const deleteAutoBot = vi.fn().mockResolvedValue(undefined);
export const triggerAutoBot = vi.fn().mockResolvedValue(undefined);
export const getAutoBotRuns = vi.fn().mockResolvedValue([]);

// v2 Worker create mock
export const createWorkerV2 = vi.fn().mockResolvedValue({
  id: 'worker-new',
  workspace: 'apiari',
  state: 'briefed',
  label: 'Briefed',
  agent_kind: 'codex',
  model: null,
  brief: null,
  repo: 'apiari',
  branch: null,
  goal: 'Test goal',
  tests_passing: false,
  branch_ready: false,
  pr_url: null,
  pr_approved: false,
  is_stalled: false,
  revision_count: 0,
  review_mode: 'local_first',
  blocked_reason: null,
  last_output_at: null,
  state_entered_at: '2026-05-04T10:00:00Z',
  created_at: '2026-05-04T10:00:00Z',
  updated_at: '2026-05-04T10:00:00Z',
});

// Context Bot API mock
export const chatWithContextBot = vi.fn().mockResolvedValue({
  response: 'Here is your brief.',
  session_id: 'session-1',
});
