import { useEffect, useState, useCallback, useMemo, useRef } from "react";
import { TopBar } from "./components/TopBar";
import { CommandPalette } from "./components/CommandPalette";
import { BotNav } from "./components/BotNav";
import { ChatPanel } from "./components/ChatPanel";
import { ReposPanel } from "./components/ReposPanel";
import { WorkerDetail } from "./components/WorkerDetail";
import { DocsPanel } from "./components/DocsPanel";
import { SimulatorPanel } from "./components/SimulatorPanel";
import type { Workspace, Bot, Worker, Repo, Message, WorkerDetail as WorkerDetailData, CrossWorkspaceBot, ResearchTask, Followup } from "./types";
import * as api from "./api";
import { initWakeLock } from "./wakeLock";

// ── Route parsing ──

interface Route {
  workspace: string;
  bot: string;
  workerId: string | null;
}

function parseHash(): Route {
  const raw = window.location.hash.replace(/^#\/?/, "");
  const parts = raw.split("/").filter(Boolean);
  return {
    workspace: parts[0] || "",
    bot: parts[1] || "",
    workerId: parts[2] === "worker" ? parts[3] || null : null,
  };
}

function buildHash(r: Route): string {
  if (!r.workspace) return "";
  let h = r.bot ? `#/${r.workspace}/${r.bot}` : `#/${r.workspace}`;
  if (r.workerId) h += `/worker/${r.workerId}`;
  return h;
}

function pushHash(r: Route) {
  const h = buildHash(r);
  if (window.location.hash !== h) history.pushState(null, "", h || "/");
}

// ── App ──

export default function App() {
  const initial = parseHash();
  const [workspaces, setWorkspaces] = useState<Workspace[]>([]);
  const [workspace, setWorkspace] = useState(initial.workspace);
  const [remote, setRemote] = useState<string | undefined>();
  const [bot, setBot] = useState(initial.bot);
  const [workerId, setWorkerId] = useState<string | null>(initial.workerId);
  const [bots, setBots] = useState<Bot[]>([]);
  const [workers, setWorkers] = useState<Worker[]>([]);
  const [repos, setRepos] = useState<Repo[]>([]);
  const [messages, setMessages] = useState<Message[]>([]);
  const [messagesLoading, setMessagesLoading] = useState(true);
  const [loading, setLoading] = useState(false);
  const [streamingContent, setStreamingContent] = useState("");
  const [menuOpen, setMenuOpen] = useState(false);
  const [workerDetail, setWorkerDetail] = useState<WorkerDetailData | null>(null);
  const [workersOpen, setWorkersOpen] = useState(false);
  const [loadingStatus, setLoadingStatus] = useState<string | undefined>();
  const [unread, setUnread] = useState<Record<string, number>>({});
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [otherWorkspaceBots, setOtherWorkspaceBots] = useState<CrossWorkspaceBot[]>([]);
  const [docsOpen, setDocsOpen] = useState(false);
  const [simulatorOpen, setSimulatorOpen] = useState(false);
  const [researchTasks, setResearchTasks] = useState<ResearchTask[]>([]);
  const [followups, setFollowups] = useState<Followup[]>([]);
  const [isMobile, setIsMobile] = useState(window.innerWidth <= 768);
  const [usage, setUsage] = useState<api.UsageData>({ installed: false, providers: [], updated_at: null });
  const lastMsgId = useRef<number>(0);
  const loadingRef = useRef(false);
  const tabHiddenRef = useRef(document.hidden);
  const remoteRef = useRef(remote);
  useEffect(() => { remoteRef.current = remote; }, [remote]);

  // Track mobile state
  useEffect(() => {
    const handler = () => setIsMobile(window.innerWidth <= 768);
    window.addEventListener("resize", handler);
    return () => window.removeEventListener("resize", handler);
  }, []);

  // Track tab visibility
  useEffect(() => {
    const handler = () => { tabHiddenRef.current = document.hidden; };
    document.addEventListener("visibilitychange", handler);
    return () => document.removeEventListener("visibilitychange", handler);
  }, []);

  // Keep loadingRef in sync
  useEffect(() => { loadingRef.current = loading; }, [loading]);

  // Keep screen awake on mobile/iPad
  useEffect(() => initWakeLock(), []);

  // Load workspaces on mount
  useEffect(() => {
    api.getWorkspaces().then((ws) => {
      setWorkspaces(ws);
      if (!workspace && ws.length > 0) {
        setWorkspace(ws[0].name);
      }
    });
    if (window.innerWidth <= 768 && !initial.bot) {
      setBot("Main");
    }
  }, []);

  // WebSocket for real-time updates
  useEffect(() => {
    const wsConn = api.connectWebSocket((event) => {
      // Match remote events: remote field must match current remote
      const eventRemote = (event.remote as string) || undefined;
      const isCurrentWs = event.workspace === workspace && eventRemote === remote;

      if (event.type === "bot_status") {
        if (isCurrentWs && event.bot === bot) {
          if (event.status === "idle") {
            setLoading(false);
            setLoadingStatus(undefined);
            setStreamingContent("");
          } else {
            setLoading(true);
            setLoadingStatus(
              event.tool_name ? `Using ${event.tool_name}...` : "Thinking...",
            );
          }
        }
      }
      if (event.type === "research_update") {
        if (isCurrentWs) {
          api.getResearchTasks(workspace, remote).then(setResearchTasks);
          if (event.status === "complete") {
            setMessages((prev) => [
              ...prev,
              {
                id: Date.now(),
                workspace,
                bot,
                role: "system",
                content: `Research complete: ${event.topic} → docs/${event.output_file}`,
                attachments: null,
                created_at: new Date().toISOString(),
              },
            ]);
          }
        }
      }
      if (event.type === "followup_created" || event.type === "followup_fired" || event.type === "followup_cancelled") {
        if (isCurrentWs) {
          api.getFollowups(workspace, remote).then(setFollowups);
        }
      }
      if (event.type === "message") {
        // Refresh unread counts
        if (workspace) api.getUnread(workspace, remote).then(setUnread);
        // Trigger an immediate fetch instead of appending directly —
        // this avoids duplicates from WS + poll both adding the same message
        if (isCurrentWs && event.bot === bot) {
          api.getConversations(workspace, bot, 30, remote).then((msgs) => {
            const latestId = msgs.length > 0 ? msgs[msgs.length - 1].id : 0;
            if (latestId > lastMsgId.current) {
              lastMsgId.current = latestId;
              setMessages(msgs);
            }
          });
        }
      }
    });
    return () => wsConn.close();
  }, [workspace, bot, remote]);

  // Load bots + workers when workspace changes
  useEffect(() => {
    if (!workspace) return;
    api.getBots(workspace, remote).then(setBots);
    api.getWorkers(workspace, remote).then(setWorkers);
    api.getRepos(workspace, remote).then(setRepos);
    api.getUnread(workspace, remote).then(setUnread);
    api.getResearchTasks(workspace, remote).then(setResearchTasks);
    api.getFollowups(workspace, remote).then(setFollowups);
  }, [workspace, remote]);

  // Load conversations + initial status when workspace or bot changes
  useEffect(() => {
    if (!workspace || !bot) return;
    let cancelled = false;
    setMessages([]);
    setMessagesLoading(true);
    setLoading(false);
    setLoadingStatus(undefined);
    setStreamingContent("");
    lastMsgId.current = 0;
    api.getConversations(workspace, bot, 30, remote).then((msgs) => {
      if (cancelled) return;
      setMessages(msgs);
      setMessagesLoading(false);
      if (msgs.length > 0) lastMsgId.current = msgs[msgs.length - 1].id;
    });
    api.getBotStatus(workspace, bot, remote).then((s) => {
      if (cancelled) return;
      if (s.status !== "idle") {
        setLoading(true);
        setLoadingStatus(s.tool_name ? `Using ${s.tool_name}...` : "Thinking...");
        setStreamingContent(s.streaming_content || "");
      }
    });
    // Mark current bot as seen after a brief delay (so badges show first on load)
    const seenTimer = setTimeout(() => {
      api.markSeen(workspace, bot, remote);
    }, 500);
    return () => {
      cancelled = true;
      clearTimeout(seenTimer);
    };
  }, [workspace, bot, remote]);

  // Adaptive polling: 2s when active, 10s when idle, 30s when tab hidden
  useEffect(() => {
    if (!workspace || !bot) return;

    const getInterval = () => {
      if (tabHiddenRef.current) return 30000;
      if (loadingRef.current) return 2000;
      return 10000;
    };

    let timer: ReturnType<typeof setTimeout>;
    let cancelled = false;
    function poll() {
      const r = remoteRef.current;
      const convP = api.getConversations(workspace, bot, 30, r).then((msgs) => {
        if (cancelled) return;
        const latestId = msgs.length > 0 ? msgs[msgs.length - 1].id : 0;
        if (latestId > lastMsgId.current) {
          lastMsgId.current = latestId;
          setMessages(msgs);
        }
      });
      const statusP = api.getBotStatus(workspace, bot, r).then((s) => {
        if (cancelled) return;
        if (s.status === "idle") {
          setLoading(false);
          setLoadingStatus(undefined);
          setStreamingContent("");
        } else {
          setLoading(true);
          setLoadingStatus(s.tool_name ? `Using ${s.tool_name}...` : "Thinking...");
          setStreamingContent(s.streaming_content || "");
        }
      });
      Promise.all([convP, statusP]).then(() => {
        if (!cancelled) timer = setTimeout(poll, getInterval());
      });
    }
    timer = setTimeout(poll, getInterval());
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [workspace, bot]);

  // Poll workers every 5s, repos every 30s, research every 10s
  useEffect(() => {
    if (!workspace) return;
    const workerInterval = setInterval(() => {
      api.getWorkers(workspace, remote).then(setWorkers);
    }, 5000);
    const repoInterval = setInterval(() => {
      api.getRepos(workspace, remote).then(setRepos);
    }, 30000);
    const researchInterval = setInterval(() => {
      api.getResearchTasks(workspace, remote).then(setResearchTasks);
    }, 10000);
    const followupInterval = setInterval(() => {
      api.getFollowups(workspace, remote).then(setFollowups);
    }, 10000);
    return () => {
      clearInterval(workerInterval);
      clearInterval(repoInterval);
      clearInterval(researchInterval);
      clearInterval(followupInterval);
    };
  }, [workspace, remote]);

  // Poll usage every 2 minutes
  useEffect(() => {
    api.getUsage().then(setUsage).catch(() => {});
    const interval = setInterval(() => {
      api.getUsage().then(setUsage).catch(() => {});
    }, 120000);
    return () => clearInterval(interval);
  }, []);

  // Sync hash
  useEffect(() => {
    pushHash({ workspace, bot, workerId });
  }, [workspace, bot, workerId]);

  // Browser back/forward
  useEffect(() => {
    const onPop = () => {
      const r = parseHash();
      setWorkspace(r.workspace);
      setBot(r.bot);
      setWorkerId(r.workerId);
    };
    window.addEventListener("popstate", onPop);
    return () => window.removeEventListener("popstate", onPop);
  }, []);

  // Fetch bots + unread counts for other workspaces when palette opens
  const [otherWorkspaceUnreads, setOtherWorkspaceUnreads] = useState<Record<string, Record<string, number>>>({});
  useEffect(() => {
    if (!paletteOpen || workspaces.length === 0) return;
    let cancelled = false;
    setOtherWorkspaceBots([]);
    setOtherWorkspaceUnreads({});
    const others = workspaces.filter((ws) => ws.name !== workspace || ws.remote !== remote);
    Promise.allSettled(
      others.map((ws) =>
        api.getBots(ws.name, ws.remote).then((bots) =>
          bots.map((b) => ({ workspace: ws.name, bot: b, remote: ws.remote }))
        )
      )
    ).then((results) => {
      if (!cancelled) {
        const fulfilled = results
          .filter((r): r is PromiseFulfilledResult<CrossWorkspaceBot[]> => r.status === "fulfilled")
          .flatMap((r) => r.value);
        setOtherWorkspaceBots(fulfilled);
      }
    });
    // Fetch unread counts for other workspaces
    Promise.allSettled(
      others.map((ws) =>
        api.getUnread(ws.name, ws.remote).then((counts) => ({
          key: `${ws.remote || "local"}/${ws.name}`,
          counts,
        }))
      )
    ).then((results) => {
      if (!cancelled) {
        const map: Record<string, Record<string, number>> = {};
        for (const r of results) {
          if (r.status === "fulfilled") {
            map[r.value.key] = r.value.counts;
          }
        }
        setOtherWorkspaceUnreads(map);
      }
    });
    return () => { cancelled = true; };
  }, [paletteOpen, workspaces, workspace, remote]);

  // Cmd+K: command palette, Cmd+J: focus chat
  useEffect(() => {
    function handleGlobalKeyDown(e: KeyboardEvent) {
      if (e.repeat) return;
      const key = e.key.toLowerCase();
      if ((e.metaKey || e.ctrlKey) && key === "k") {
        e.preventDefault();
        setPaletteOpen((v) => !v);
      }
      if ((e.metaKey || e.ctrlKey) && key === "j") {
        e.preventDefault();
        document.querySelector<HTMLTextAreaElement>('textarea[enterkeyhint="send"]')?.focus();
      }
    }
    window.addEventListener("keydown", handleGlobalKeyDown);
    return () => window.removeEventListener("keydown", handleGlobalKeyDown);
  }, []);

  const handleSelectWorkspace = useCallback((ws: string, wsRemote?: string) => {
    setWorkspace(ws);
    setRemote(wsRemote);
    setBot(isMobile ? "Main" : "");
    setWorkerId(null);
    setLoading(false);
    setLoadingStatus(undefined);
  }, [isMobile]);

  const handleSelectWorkspaceBot = useCallback((ws: string, botName: string, wsRemote?: string) => {
    setWorkspace(ws);
    setRemote(wsRemote);
    setBot(botName);
    setWorkerId(null);
    setDocsOpen(false);
    setMenuOpen(false);
    setLoading(false);
    setLoadingStatus(undefined);
  }, []);

  const handleSelectBot = useCallback((name: string) => {
    setBot(name);
    setWorkerId(null);
    setDocsOpen(false);
    setMenuOpen(false);
    setLoading(false);
    setLoadingStatus(undefined);
  }, []);

  const handleSelectDocs = useCallback(() => {
    setDocsOpen(true);
    setWorkerId(null);
    setMenuOpen(false);
  }, []);

  const handleSelectWorker = useCallback((id: string) => {
    setWorkerId(id);
    setDocsOpen(false);
    setMenuOpen(false);
    if (workspace) {
      api.getWorkerDetail(workspace, id, remote).then(setWorkerDetail).catch(() => setWorkerDetail(null));
    }
  }, [workspace, remote]);

  // Poll worker detail while viewing a worker
  useEffect(() => {
    if (!workspace || !workerId) return;
    const interval = setInterval(() => {
      api.getWorkerDetail(workspace, workerId, remote).then(setWorkerDetail).catch(() => {});
    }, 3000);
    return () => clearInterval(interval);
  }, [workspace, workerId, remote]);

  const handleBackFromWorker = useCallback(() => {
    setWorkerId(null);
  }, []);

  const handleSend = useCallback(
    async (text: string, attachments?: import("./components/ChatPanel").Attachment[]) => {
      // Intercept /research command
      if (text.startsWith("/research ")) {
        const topic = text.slice("/research ".length).trim();
        if (topic) {
          try {
            await api.startResearch(workspace, topic, remote);
            // Add a local system-style message to show it was started
            setMessages((prev) => [
              ...prev,
              {
                id: Date.now(),
                workspace,
                bot,
                role: "system",
                content: `Research started: ${topic}`,
                attachments: null,
                created_at: new Date().toISOString(),
              },
            ]);
            // Refresh research tasks
            api.getResearchTasks(workspace, remote).then(setResearchTasks);
          } catch (e) {
            console.error("Failed to start research:", e);
          }
        }
        return;
      }

      const apiAttachments = attachments?.map((a) => ({
        name: a.name,
        type: a.type,
        dataUrl: a.dataUrl,
      }));

      // Fire and forget — daemon handles everything
      setLoading(true);
      setLoadingStatus("Thinking...");
      await api.sendMessage(workspace, bot, text, apiAttachments, remote);
      // Polling will pick up the user message + bot response from DB
    },
    [workspace, bot, remote],
  );

  // Merge fresh worker data (5s poll) into repos (30s poll) so worker status stays current
  const reposWithFreshWorkers = useMemo(() => {
    const workerMap = new Map(workers.map((w) => [w.id, w]));
    return repos.map((repo) => ({
      ...repo,
      workers: repo.workers.map((rw) => workerMap.get(rw.id) || rw),
    }));
  }, [repos, workers]);

  const selectedBot = bots.find((b) => b.name === bot);
  const selectedWorker = workerId
    ? workers.find((w) => w.id === workerId) || null
    : null;

  return (
    <>
      <TopBar
        workspaces={workspaces}
        active={workspace}
        activeRemote={remote}
        onSelect={handleSelectWorkspace}
        onMenuToggle={() => setMenuOpen((v) => !v)}
        onOpenPalette={() => setPaletteOpen(true)}
        onToggleSimulator={() => setSimulatorOpen((v) => !v)}
        usage={usage}
      />
      <div style={{ flex: 1, display: "flex", overflow: "hidden", position: "relative" }}>
        {/* Mobile drawer overlay */}
        {menuOpen && (
          <div
            className="drawer-backdrop"
            onClick={() => setMenuOpen(false)}
          />
        )}
        <BotNav
          bots={bots}
          workers={workers}
          activeBot={docsOpen || workerId ? null : bot}
          activeWorkerId={workerId}
          onSelectBot={handleSelectBot}
          onSelectWorker={handleSelectWorker}
          mobileOpen={menuOpen}
          unread={unread}
          docsOpen={docsOpen}
          onSelectDocs={handleSelectDocs}
        />
        {docsOpen ? (
          <DocsPanel workspace={workspace} remote={remote} />
        ) : workerId && selectedWorker ? (
          <WorkerDetail
            worker={selectedWorker}
            detail={workerDetail}
            workspace={workspace}
            remote={remote}
            onBack={handleBackFromWorker}
          />
        ) : bot ? (
          <ChatPanel
            bot={bot}
            botDescription={selectedBot?.description}
            botProvider={selectedBot?.provider}
            botModel={selectedBot?.model}
            messages={messages}
            messagesLoading={messagesLoading}
            loading={loading}
            loadingStatus={loadingStatus}
            streamingContent={streamingContent}
            onSend={handleSend}
            workerCount={workers.length}
            onWorkersToggle={() => setWorkersOpen((v) => !v)}
            onCancel={loading ? () => api.cancelBot(workspace, bot, remote) : undefined}
            ttsVoice={workspaces.find((w) => w.name === workspace && w.remote === remote)?.tts_voice}
            ttsSpeed={workspaces.find((w) => w.name === workspace && w.remote === remote)?.tts_speed}
            followups={followups.filter((f) => f.bot === bot)}
            workspace={workspace}
            onFollowupCancelled={() => api.getFollowups(workspace, remote).then(setFollowups)}
          />
        ) : (
          <div style={{ flex: 1, display: "flex", alignItems: "center", justifyContent: "center", flexDirection: "column", gap: 8 }}>
            <div style={{ fontSize: 14, color: "var(--text-faint)" }}>Select a bot to start chatting</div>
          </div>
        )}
        <ReposPanel
          repos={reposWithFreshWorkers}
          researchTasks={researchTasks}
          onSelectWorker={(id) => {
            setWorkersOpen(false);
            handleSelectWorker(id);
          }}
          mobileOpen={workersOpen}
          onClose={() => setWorkersOpen(false)}
        />
      </div>
      <SimulatorPanel open={simulatorOpen} onClose={() => setSimulatorOpen(false)} />
      <CommandPalette
        open={paletteOpen}
        onOpenChange={setPaletteOpen}
        workspaces={workspaces}
        bots={bots}
        workers={workers}
        currentWorkspace={workspace}
        currentRemote={remote}
        currentBot={bot}
        onSelectWorkspace={handleSelectWorkspace}
        onSelectBot={handleSelectBot}
        onSelectWorker={handleSelectWorker}
        otherWorkspaceBots={otherWorkspaceBots}
        onSelectWorkspaceBot={handleSelectWorkspaceBot}
        unread={unread}
        otherWorkspaceUnreads={otherWorkspaceUnreads}
        onStartResearch={() => {
          const topic = prompt("Research topic:");
          if (topic?.trim()) {
            api.startResearch(workspace, topic.trim(), remote).then(() => {
              api.getResearchTasks(workspace, remote).then(setResearchTasks);
              setMessages((prev) => [
                ...prev,
                {
                  id: Date.now(),
                  workspace,
                  bot,
                  role: "system",
                  content: `Research started: ${topic.trim()}`,
                  attachments: null,
                  created_at: new Date().toISOString(),
                },
              ]);
            }).catch(() => {});
          }
        }}
      />
    </>
  );
}
