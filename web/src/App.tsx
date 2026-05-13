import { useEffect, useLayoutEffect, useRef, useState } from "react";
import Layout from "./components/Layout/Layout";
import Sidebar from "./components/Sidebar/Sidebar";
import BottomTabBar from "./components/BottomTabBar/BottomTabBar";
import Dashboard from "./components/Dashboard/Dashboard";
import WorkerDetailV2 from "./components/WorkerDetailV2/WorkerDetailV2";
import AutoBotDetail from "./components/AutoBotDetail/AutoBotDetail";
import CompletedWorkersView from "./components/CompletedWorkersView";
import ContextBotManager from "./components/ContextBot/ContextBotManager";
import CommandPalette from "./components/CommandPalette/CommandPalette";
import QuickDispatch from "./components/QuickDispatch/QuickDispatch";
import { Bot, Wrench, LayoutDashboard } from "lucide-react";
import {
  getWorkspaces,
  listWorkersV2,
  listAutoBots,
  connectWebSocket,
  chatWithContextBot,
  listContextBotSessions,
  deleteContextBotSession,
} from "@apiari/api";
import type { WorkerV2, AutoBot, ContextBotContext, ContextBotSession } from "@apiari/types";
import type { SidebarItem } from "./components/Sidebar/Sidebar";
import { getWorkerTitle } from "./utils/workerTitle";
import "./theme.css";

let _nextSessionId = 0;
function nextSessionId() {
  return String(++_nextSessionId);
}

type EntityType = "auto_bot" | "worker" | "completed_workers";

interface SelectedEntity {
  type: EntityType;
  id: string;
}

// Map between URL segment and EntityType
const URL_SEGMENT_TO_TYPE: Record<string, EntityType> = {
  worker: "worker",
  "auto-bot": "auto_bot",
  "completed-workers": "completed_workers",
};

const TYPE_TO_URL_SEGMENT: Record<EntityType, string> = {
  worker: "worker",
  auto_bot: "auto-bot",
  completed_workers: "completed-workers",
};

function updateHash(ws: string, type?: EntityType, id?: string) {
  if (type === "completed_workers") {
    window.location.hash = `/${ws}/${TYPE_TO_URL_SEGMENT[type]}`;
  } else if (type && id) {
    window.location.hash = `/${ws}/${TYPE_TO_URL_SEGMENT[type]}/${id}`;
  } else {
    window.location.hash = `/${ws}`;
  }
}

function parseHash(hash: string): { ws: string; type?: EntityType; id?: string } | null {
  // hash looks like "#/workspace" or "#/workspace/worker/id" or "#/workspace/auto-bot/id"
  // or "#/workspace/completed-workers" (no id)
  const stripped = hash.replace(/^#\//, "");
  if (!stripped) return null;
  const parts = stripped.split("/");
  const ws = parts[0];
  if (!ws) return null;
  const segment = parts[1];
  const id = parts[2];
  if (segment) {
    const type = URL_SEGMENT_TO_TYPE[segment];
    if (type) {
      if (id) return { ws, type, id };
      return { ws, type };
    }
  }
  return { ws };
}

function workerToSidebarItem(w: WorkerV2): SidebarItem {
  const title = getWorkerTitle(w);
  const shortTitle = title.length > 40 ? title.slice(0, 40).replace(/\s+\S*$/, "") + "…" : title;
  const tags: SidebarItem["tags"] = [];
  if (w.pr_url) tags.push({ label: "PR", color: w.pr_approved ? "green" : "amber" });
  return {
    id: w.id,
    name: shortTitle,
    status: w.state,
    meta: w.id,
    tags,
  };
}

function autoBotToSidebarItem(b: AutoBot): SidebarItem {
  return {
    id: b.id,
    name: b.name,
    status: b.status,
    meta: b.trigger_type === "cron" ? "cron" : (b.signal_source ?? "signal"),
  };
}

export default function App() {
  const [workspaces, setWorkspaces] = useState<string[]>([]);
  const [workspace, setWorkspace] = useState<string>("");
  const [selected, setSelected] = useState<SelectedEntity | null>(null);
  const [mobileTab, setMobileTab] = useState("dashboard");
  const [workers, setWorkers] = useState<WorkerV2[]>([]);
  const [autoBots, setAutoBots] = useState<AutoBot[]>([]);
  const [loading, setLoading] = useState(true);
  const [contextSessions, setContextSessions] = useState<ContextBotSession[]>([]);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [quickDispatchOpen, setQuickDispatchOpen] = useState(false);
  const [daemonRestarted, setDaemonRestarted] = useState(false);
  const workerPollRef = useRef<number | null>(null);
  const autoBotPollRef = useRef<number | null>(null);
  const workspaceRef = useRef<string>("");

  // Keep workspaceRef in sync so the WS handler always sees the latest value
  useLayoutEffect(() => {
    workspaceRef.current = workspace;
  });

  // Fetch workspace list on mount, then restore from hash if valid
  useEffect(() => {
    getWorkspaces()
      .then((ws) => {
        const names = ws.map((w) => w.name);
        setWorkspaces(names);
        if (names.length === 0) return;

        const parsed = parseHash(window.location.hash);
        if (parsed && names.includes(parsed.ws)) {
          setWorkspace(parsed.ws);
          if (parsed.type) {
            setSelected({ type: parsed.type, id: parsed.id ?? "" });
          }
        } else {
          setWorkspace(names[0]);
          updateHash(names[0]);
        }
      })
      .catch(() => {
        // fallback: keep workspace empty
      });
  }, []);

  const navigateTo = (type: EntityType, id: string) => {
    setSelected({ type, id });
    updateHash(workspace, type, id);
  };

  const handleSelect = (type: EntityType, id: string) => {
    navigateTo(type, id);
  };

  // ── Context bot handlers ────────────────────────────────────────────

  const DEFAULT_CONTEXT_BOT_MODEL = "claude-sonnet-4-6";

  // Load persisted sessions whenever workspace changes
  useEffect(() => {
    if (!workspace) return;
    listContextBotSessions(workspace)
      .then((sessions) => {
        if (sessions.length > 0) setContextSessions(sessions);
      })
      .catch(() => {});
  }, [workspace]);

  function openContextBot(context: ContextBotContext, title: string, model?: string) {
    setContextSessions((prev) => {
      // If a session already exists for this entity, just un-minimize and surface it
      const existing = prev.find((s) => s.context.entity_id === context.entity_id);
      if (existing) {
        return prev.map((s) => (s.id === existing.id ? { ...s, minimized: false } : s));
      }
      const id = nextSessionId();
      const session: ContextBotSession = {
        id,
        context,
        title,
        model: model ?? DEFAULT_CONTEXT_BOT_MODEL,
        messages: [],
        minimized: false,
        loading: false,
      };
      // Don't persist until first message — avoids empty sessions on reload
      return [...prev, session];
    });
  }

  async function handleContextBotSend(sessionId: string, message: string) {
    const session = contextSessions.find((s) => s.id === sessionId);
    if (!session) return;

    const isFirstMessage = session.messages.length === 0;
    const newTitle = isFirstMessage
      ? message.length > 52
        ? message.slice(0, 49) + "…"
        : message
      : session.title;

    const userMsg = {
      role: "user" as const,
      content: message,
      timestamp: new Date().toISOString(),
    };
    const messagesWithUser = [...session.messages, userMsg];

    setContextSessions((prev) =>
      prev.map((s) =>
        s.id === sessionId
          ? { ...s, title: newTitle, loading: true, messages: messagesWithUser }
          : s,
      ),
    );

    try {
      // Server returns 202 immediately; actual response arrives via WebSocket.
      // Server also persists user message to DB immediately.
      const ack = await chatWithContextBot(
        workspace,
        message,
        session.context,
        session.server_session_id,
        session.model,
        { history: session.messages, title: newTitle },
      );

      // Store the server_session_id so the WS handler can match by it.
      setContextSessions((prev) =>
        prev.map((s) =>
          s.id === sessionId ? { ...s, server_session_id: ack.session_id, model: ack.model } : s,
        ),
      );
    } catch (err) {
      // Only fires if the 202 request itself fails (e.g. claude CLI not found).
      const detail = err instanceof Error ? err.message : "unknown error";
      const errorMsg = {
        role: "assistant" as const,
        content: `Error: ${detail}`,
        timestamp: new Date().toISOString(),
      };
      setContextSessions((prev) =>
        prev.map((s) =>
          s.id === sessionId
            ? { ...s, loading: false, messages: [...messagesWithUser, errorMsg] }
            : s,
        ),
      );
    }
  }

  // Fetch workers when workspace changes, then poll every 5s
  useEffect(() => {
    if (!workspace) return;
    let cancelled = false;

    async function fetchWorkers(initial = false) {
      try {
        const list = await listWorkersV2(workspace);
        if (!cancelled) {
          setWorkers(list);
          if (initial) setLoading(false);
        }
      } catch {
        if (initial && !cancelled) setLoading(false);
      }
    }

    fetchWorkers(true);

    workerPollRef.current = window.setInterval(() => {
      fetchWorkers(false);
    }, 5000);

    return () => {
      cancelled = true;
      if (workerPollRef.current !== null) {
        window.clearInterval(workerPollRef.current);
        workerPollRef.current = null;
      }
    };
  }, [workspace]);

  // Fetch auto bots when workspace changes, then poll every 15s
  useEffect(() => {
    if (!workspace) return;
    let cancelled = false;

    async function fetchAutoBots() {
      try {
        const list = await listAutoBots(workspace);
        if (!cancelled) setAutoBots(list);
      } catch {
        // ignore errors — sidebar just stays empty
      }
    }

    fetchAutoBots();

    autoBotPollRef.current = window.setInterval(() => {
      fetchAutoBots();
    }, 15000);

    return () => {
      cancelled = true;
      if (autoBotPollRef.current !== null) {
        window.clearInterval(autoBotPollRef.current);
        autoBotPollRef.current = null;
      }
    };
  }, [workspace]);

  // WebSocket — update workers and auto bots on relevant events
  useEffect(() => {
    let knownStartupId: string | null = null;
    const ws = connectWebSocket((event) => {
      if (event.type === "snapshot") {
        const sid = event.startup_id as string | undefined;
        if (sid) {
          if (knownStartupId && knownStartupId !== sid) {
            setDaemonRestarted(true);
          }
          knownStartupId = sid;
        }
      }

      if (event.type === "worker_v2_state") {
        const workerId = event.worker_id as string;
        const state = event.state as WorkerV2["state"];
        const label = event.label as string;
        const props = (event.properties ?? {}) as Partial<WorkerV2>;
        setWorkers((prev) =>
          prev.map((w) => (w.id === workerId ? { ...w, state, label, ...props } : w)),
        );
      }

      if (event.type === "auto_bot_run_started") {
        const autoBotId = event.auto_bot_id as string;
        setAutoBots((prev) =>
          prev.map((b) => (b.id === autoBotId ? { ...b, status: "running" as const } : b)),
        );
      }

      if (event.type === "auto_bot_run_finished") {
        // Refresh the full list so status reverts correctly
        if (workspaceRef.current) {
          listAutoBots(workspaceRef.current)
            .then((list) => setAutoBots(list))
            .catch(() => {});
        }
      }

      if (event.type === "context_bot_activity") {
        const sid = event.session_id as string;
        const activity = event.activity as string;
        setContextSessions((prev) =>
          prev.map((s) => (s.server_session_id === sid ? { ...s, activity } : s)),
        );
      }

      if (event.type === "context_bot_response") {
        const sid = event.session_id as string;
        const response = event.response as string;
        const model = event.model as string;
        const error = event.error as string | undefined;
        const dispatchedWorkerId = event.dispatched_worker_id as string | undefined;

        setContextSessions((prev) =>
          prev.map((s) => {
            if (s.server_session_id !== sid) return s;
            const assistantMsg = {
              role: "assistant" as const,
              content: error ? `Error: ${error}` : response,
              timestamp: new Date().toISOString(),
            };
            return {
              ...s,
              loading: false,
              activity: undefined,
              model,
              messages: [...s.messages, assistantMsg],
            };
          }),
        );

        if (dispatchedWorkerId && workspaceRef.current) {
          listWorkersV2(workspaceRef.current)
            .then((list) => setWorkers(list))
            .catch(() => {});
          setSelected({ type: "worker", id: dispatchedWorkerId });
          updateHash(workspaceRef.current, "worker", dispatchedWorkerId);
        }
      }
    });
    return () => ws.close();
  }, []);

  // Cmd+K / Ctrl+K opens command palette
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "k") {
        e.preventDefault();
        setPaletteOpen((open) => !open);
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, []);

  // 'n' key opens quick dispatch (when not in an input)
  useEffect(() => {
    function handleKey(e: KeyboardEvent) {
      if (
        e.key === "n" &&
        !e.metaKey &&
        !e.ctrlKey &&
        !(e.target instanceof HTMLInputElement) &&
        !(e.target instanceof HTMLTextAreaElement)
      ) {
        setQuickDispatchOpen(true);
      }
    }
    window.addEventListener("keydown", handleKey);
    return () => window.removeEventListener("keydown", handleKey);
  }, []);

  // hashchange — enables browser back/forward navigation
  useEffect(() => {
    function handleHashChange() {
      const parsed = parseHash(window.location.hash);
      if (!parsed) return;
      if (parsed.ws) setWorkspace(parsed.ws);
      if (parsed.type) {
        setSelected({ type: parsed.type, id: parsed.id ?? "" });
      } else {
        setSelected(null);
      }
    }
    window.addEventListener("hashchange", handleHashChange);
    return () => window.removeEventListener("hashchange", handleHashChange);
  }, []);

  const DONE_STATES = ["done", "abandoned"];
  const activeWorkers = workers.filter((w) => !DONE_STATES.includes(w.state));
  const doneWorkers = workers.filter((w) => DONE_STATES.includes(w.state));
  const sidebarWorkers = activeWorkers.map(workerToSidebarItem);
  const sidebarAutoBots = autoBots.map(autoBotToSidebarItem);

  const activityItems = workers
    .filter((w) => w.state === "running")
    .map((w) => ({
      label: `${getWorkerTitle(w).slice(0, 35)} · running`,
      color: "var(--status-running)",
    }));

  const mobileList = (
    <Sidebar
      selectedType={selected?.type ?? null}
      selectedId={selected?.id ?? null}
      onSelect={handleSelect}
      onHome={() => {
        setSelected(null);
        setMobileTab("dashboard");
        updateHash(workspace);
      }}
      autoBots={sidebarAutoBots}
      workers={sidebarWorkers}
      doneWorkerCount={doneWorkers.length}
      onShowDoneWorkers={() => {
        setSelected({ type: "completed_workers", id: "" });
        setMobileTab("dashboard");
        updateHash(workspace, "completed_workers");
      }}
      doneWorkersSelected={selected?.type === "completed_workers"}
      workspaces={workspaces}
      workspace={workspace}
      onWorkspaceChange={(ws) => {
        setWorkspace(ws);
        setSelected(null);
        updateHash(ws);
      }}
      onQuickDispatch={() => setQuickDispatchOpen(true)}
      activityItems={activityItems}
    />
  );

  const mainContent = !workspace ? (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        height: "100%",
        color: "var(--text-faint)",
        fontFamily: "var(--font)",
        fontSize: "14px",
      }}
    >
      Loading workspaces...
    </div>
  ) : selected ? (
    selected.type === "worker" ? (
      <WorkerDetailV2
        workspace={workspace}
        workerId={selected.id}
        onOpenContextBot={openContextBot}
        onNavigateToWorker={(id) => navigateTo("worker", id)}
      />
    ) : selected.type === "completed_workers" ? (
      <CompletedWorkersView
        workers={doneWorkers}
        onSelectWorker={(id) => navigateTo("worker", id)}
      />
    ) : (
      <AutoBotDetail
        workspace={workspace}
        autoBotId={selected.id}
        onSelectWorker={(id) => navigateTo("worker", id)}
        onOpenContextBot={openContextBot}
      />
    )
  ) : (
    <Dashboard
      workspace={workspace}
      workers={workers}
      autoBots={autoBots}
      onSelectWorker={(id) => navigateTo("worker", id)}
      onSelectAutoBot={(id) => navigateTo("auto_bot", id)}
      onOpenContextBot={openContextBot}
    />
  );

  // Mobile: route by tab + selection state
  const mobileMainContent = !workspace
    ? mainContent
    : (() => {
        // A selected entity always takes the full screen on mobile
        if (selected?.type === "worker") {
          return (
            <WorkerDetailV2
              workspace={workspace}
              workerId={selected.id}
              onBack={() => {
                setSelected(null);
                updateHash(workspace);
              }}
              onOpenContextBot={openContextBot}
              onNavigateToWorker={(id) => navigateTo("worker", id)}
            />
          );
        }
        if (selected?.type === "completed_workers") {
          return (
            <CompletedWorkersView
              workers={doneWorkers}
              onSelectWorker={(id) => navigateTo("worker", id)}
            />
          );
        }
        if (selected?.type === "auto_bot") {
          return (
            <AutoBotDetail
              workspace={workspace}
              autoBotId={selected.id}
              onSelectWorker={(id) => navigateTo("worker", id)}
              onOpenContextBot={openContextBot}
            />
          );
        }
        // No selection — show tab content
        if (mobileTab === "workers") return mobileList;
        if (mobileTab === "auto_bots") return mobileList;
        return (
          <Dashboard
            workspace={workspace}
            workers={workers}
            autoBots={autoBots}
            onSelectWorker={(id) => navigateTo("worker", id)}
            onSelectAutoBot={(id) => navigateTo("auto_bot", id)}
            onOpenContextBot={openContextBot}
          />
        );
      })();

  const tabs = [
    { id: "dashboard", label: "Dashboard", icon: <LayoutDashboard size={20} /> },
    { id: "workers", label: "Workers", icon: <Wrench size={20} /> },
    { id: "auto_bots", label: "Auto Bots", icon: <Bot size={20} /> },
  ];

  if (loading) {
    // Render shell immediately; sidebar will populate once data arrives
  }

  return (
    <>
      {daemonRestarted && (
        <div
          style={{
            position: "fixed",
            top: 0,
            left: 0,
            right: 0,
            zIndex: 9999,
            background: "var(--accent)",
            color: "#111",
            padding: "8px 16px",
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            fontSize: 13,
            fontWeight: 600,
          }}
        >
          Daemon restarted — reload to sync
          <button
            onClick={() => window.location.reload()}
            style={{
              background: "#111",
              color: "var(--accent)",
              border: "none",
              borderRadius: 4,
              padding: "4px 12px",
              cursor: "pointer",
              fontWeight: 700,
              fontSize: 13,
            }}
          >
            Reload
          </button>
        </div>
      )}
      <Layout
        sidebar={
          <Sidebar
            selectedType={selected?.type ?? null}
            selectedId={selected?.id ?? null}
            onSelect={handleSelect}
            onHome={() => {
              setSelected(null);
              updateHash(workspace);
            }}
            autoBots={sidebarAutoBots}
            workers={sidebarWorkers}
            doneWorkerCount={doneWorkers.length}
            onShowDoneWorkers={() => {
              setSelected({ type: "completed_workers", id: "" });
              updateHash(workspace, "completed_workers");
            }}
            doneWorkersSelected={selected?.type === "completed_workers"}
            workspaces={workspaces}
            workspace={workspace}
            onWorkspaceChange={(ws) => {
              setWorkspace(ws);
              setSelected(null);
              updateHash(ws);
            }}
            onQuickDispatch={() => setQuickDispatchOpen(true)}
            activityItems={activityItems}
          />
        }
        main={mobileMainContent}
        bottomBar={
          <BottomTabBar
            tabs={tabs}
            activeTab={mobileTab}
            onTabChange={(id) => {
              setMobileTab(id);
              setSelected(null);
              updateHash(workspace);
            }}
          />
        }
      />
      <ContextBotManager
        sessions={contextSessions}
        currentTarget={(() => {
          if (!workspace) return null;
          if (selected?.type === "worker") {
            const w = workers.find((w) => w.id === selected.id);
            return {
              context: {
                view: "worker_detail",
                entity_id: selected.id,
                entity_snapshot: w ? { state: w.state, goal: w.goal } : {},
              },
              title: w ? getWorkerTitle(w) : selected.id,
            };
          }
          return {
            context: {
              view: "workspace_overview",
              entity_id: workspace,
              entity_snapshot: { workspace },
            },
            title: workspace,
          };
        })()}
        onNewSession={openContextBot}
        onSend={handleContextBotSend}
        onChangeModel={(id, model) =>
          setContextSessions((prev) => prev.map((s) => (s.id === id ? { ...s, model } : s)))
        }
        onMinimize={(id) =>
          setContextSessions((prev) =>
            prev.map((s) => (s.id === id ? { ...s, minimized: !s.minimized } : s)),
          )
        }
        onClose={(id) => {
          setContextSessions((prev) => prev.filter((s) => s.id !== id));
          deleteContextBotSession(workspace, id).catch(() => {});
        }}
      />
      {paletteOpen && (
        <CommandPalette
          workers={workers}
          autoBots={autoBots}
          onSelectWorker={(id) => {
            navigateTo("worker", id);
            setPaletteOpen(false);
          }}
          onSelectAutoBot={(id) => {
            navigateTo("auto_bot", id);
            setPaletteOpen(false);
          }}
          onClose={() => setPaletteOpen(false)}
        />
      )}
      {quickDispatchOpen && (
        <QuickDispatch
          workspace={workspace}
          onClose={() => setQuickDispatchOpen(false)}
          onDispatched={(workerId) => {
            setQuickDispatchOpen(false);
            navigateTo("worker", workerId);
            listWorkersV2(workspace)
              .then(setWorkers)
              .catch(() => {});
          }}
        />
      )}
    </>
  );
}
