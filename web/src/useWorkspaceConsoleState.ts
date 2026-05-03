import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import * as api from "./api";
import {
  getDefaultWorkspaceSelection,
  getOrderedWorkspaceModes,
  isWorkspaceMode,
  resolveWorkspaceConsoleProfile,
  type WorkspaceMode,
} from "./consoleConfig";
import type {
  Bot,
  CrossWorkspaceBot,
  Followup,
  Message,
  Repo,
  ResearchTask,
  Worker,
  WorkerDetail as WorkerDetailData,
  Workspace,
} from "./types";
import { initWakeLock } from "./wakeLock";

export interface Route {
  workspace: string;
  mode: WorkspaceMode;
  bot: string;
  workerId: string | null;
}

export function parseHash(): Route {
  const raw = window.location.hash.replace(/^#\/?/, "");
  const parts = raw.split("/").filter(Boolean);
  const workspace = parts[0] || "";
  const mode = parts[1];

  if (isWorkspaceMode(mode)) {
    return {
      workspace,
      mode,
      bot: mode === "chat" ? parts[2] || "" : "",
      workerId: mode === "workers" && parts[2] === "worker" ? parts[3] || null : null,
    };
  }

  return {
    workspace,
    mode: parts[2] === "worker" ? "workers" : parts[1] ? "chat" : "overview",
    bot: parts[1] || "",
    workerId: parts[2] === "worker" ? parts[3] || null : null,
  };
}

function buildHash(route: Route): string {
  if (!route.workspace) return "";
  switch (route.mode) {
    case "chat":
      return route.bot ? `#/${route.workspace}/chat/${route.bot}` : `#/${route.workspace}/chat`;
    case "workers":
      return route.workerId
        ? `#/${route.workspace}/workers/worker/${route.workerId}`
        : `#/${route.workspace}/workers`;
    case "repos":
      return `#/${route.workspace}/repos`;
    case "docs":
      return `#/${route.workspace}/docs`;
    default:
      return `#/${route.workspace}`;
  }
}

function pushHash(route: Route) {
  const nextHash = buildHash(route);
  if (window.location.hash !== nextHash) {
    history.pushState(null, "", nextHash || "/");
  }
}

function mergeMessages(prev: Message[], incoming: Message): Message[] {
  const withoutMatchingTemps = incoming.id >= 0
    ? prev.filter((msg) => !(msg.id < 0
      && msg.workspace === incoming.workspace
      && msg.bot === incoming.bot
      && msg.role === incoming.role
      && msg.content === incoming.content))
    : prev;

  const existingIndex = withoutMatchingTemps.findIndex((msg) => msg.id === incoming.id);
  if (existingIndex >= 0) {
    const next = withoutMatchingTemps.slice();
    next[existingIndex] = incoming;
    return next;
  }

  return [...withoutMatchingTemps, incoming].sort((a, b) => a.id - b.id);
}

export function useWorkspaceConsoleState() {
  const initial = parseHash();

  const [workspaces, setWorkspaces] = useState<Workspace[]>([]);
  const [workspace, setWorkspace] = useState(initial.workspace);
  const [remote, setRemote] = useState<string | undefined>();
  const [mode, setMode] = useState<WorkspaceMode>(initial.mode);
  const [bot, setBot] = useState(initial.bot);
  const [workerId, setWorkerId] = useState<string | null>(initial.workerId);

  const [bots, setBots] = useState<Bot[]>([]);
  const [workers, setWorkers] = useState<Worker[]>([]);
  const [repos, setRepos] = useState<Repo[]>([]);
  const [messages, setMessages] = useState<Message[]>([]);
  const [messagesLoading, setMessagesLoading] = useState(true);
  const [loading, setLoading] = useState(false);
  const [streamingContent, setStreamingContent] = useState("");
  const [loadingStatus, setLoadingStatus] = useState<string | undefined>();
  const [workerDetail, setWorkerDetail] = useState<WorkerDetailData | null>(null);
  const [unread, setUnread] = useState<Record<string, number>>({});
  const [researchTasks, setResearchTasks] = useState<ResearchTask[]>([]);
  const [followups, setFollowups] = useState<Followup[]>([]);
  const [usage, setUsage] = useState<api.UsageData>({ installed: false, providers: [], updated_at: null });

  const [menuOpen, setMenuOpen] = useState(false);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [simulatorOpen, setSimulatorOpen] = useState(false);
  const [isMobile, setIsMobile] = useState(window.innerWidth <= 768);
  const [otherWorkspaceBots, setOtherWorkspaceBots] = useState<CrossWorkspaceBot[]>([]);
  const [otherWorkspaceUnreads, setOtherWorkspaceUnreads] = useState<Record<string, Record<string, number>>>({});

  const lastMsgId = useRef(0);
  const nextTempId = useRef(-1);
  const loadingRef = useRef(false);
  const tabHiddenRef = useRef(document.hidden);
  const remoteRef = useRef(remote);
  const messagesRef = useRef<Message[]>([]);
  const streamingContentRef = useRef("");

  useEffect(() => { remoteRef.current = remote; }, [remote]);
  useEffect(() => { messagesRef.current = messages; }, [messages]);
  useEffect(() => { streamingContentRef.current = streamingContent; }, [streamingContent]);
  useEffect(() => { loadingRef.current = loading; }, [loading]);

  const consoleProfile = useMemo(
    () => resolveWorkspaceConsoleProfile(workspace, remote),
    [workspace, remote],
  );

  const appendLocalMessage = useCallback((role: string, content: string) => {
    const tempId = nextTempId.current--;
    const message: Message = {
      id: tempId,
      workspace,
      bot,
      role,
      content,
      attachments: null,
      created_at: new Date().toISOString(),
    };
    setMessages((prev) => mergeMessages(prev, message));
    return message;
  }, [workspace, bot]);

  const finalizeStreamingAssistant = useCallback(() => {
    const content = streamingContentRef.current.trim();
    if (!content) return;

    const lastMessage = messagesRef.current[messagesRef.current.length - 1];
    if (
      lastMessage
      && lastMessage.workspace === workspace
      && lastMessage.bot === bot
      && lastMessage.role === "assistant"
      && lastMessage.content === content
    ) {
      return;
    }

    appendLocalMessage("assistant", content);
  }, [appendLocalMessage, workspace, bot]);

  useEffect(() => {
    const handleResize = () => setIsMobile(window.innerWidth <= 768);
    window.addEventListener("resize", handleResize);
    return () => window.removeEventListener("resize", handleResize);
  }, []);

  useEffect(() => {
    const handleVisibilityChange = () => {
      tabHiddenRef.current = document.hidden;
    };
    document.addEventListener("visibilitychange", handleVisibilityChange);
    return () => document.removeEventListener("visibilitychange", handleVisibilityChange);
  }, []);

  useEffect(() => initWakeLock(), []);

  useEffect(() => {
    api.getWorkspaces().then((ws) => {
      setWorkspaces(ws);
      if (!workspace && ws.length > 0) {
        setWorkspace(ws[0].name);
      }
    });

    if (window.innerWidth <= 768 && !initial.bot) {
      const defaults = getDefaultWorkspaceSelection(resolveWorkspaceConsoleProfile(), true);
      setMode(defaults.mode);
      setBot(defaults.bot);
    }
  }, [initial.bot, workspace]);

  useEffect(() => {
    const wsConn = api.connectWebSocket((event) => {
      const eventRemote = (event.remote as string) || undefined;
      const isCurrentWorkspace = event.workspace === workspace && eventRemote === remote;

      if (event.type === "bot_status") {
        if (isCurrentWorkspace && event.bot === bot) {
          if (event.status === "idle") {
            finalizeStreamingAssistant();
            setLoading(false);
            setLoadingStatus(undefined);
            setStreamingContent("");
          } else {
            setLoading(true);
            setLoadingStatus(event.tool_name ? `Using ${event.tool_name}...` : "Thinking...");
            setStreamingContent(typeof event.streaming_content === "string" ? event.streaming_content : "");
          }
        }
      }

      if (event.type === "research_update" && isCurrentWorkspace) {
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

      if (
        (event.type === "followup_created" || event.type === "followup_fired" || event.type === "followup_cancelled")
        && isCurrentWorkspace
      ) {
        api.getFollowups(workspace, remote).then(setFollowups);
      }

      if (event.type === "message") {
        if (workspace) {
          api.getUnread(workspace, remote).then(setUnread);
        }

        if (isCurrentWorkspace && event.bot === bot) {
          const eventMessage = event as unknown as Message;
          if (typeof eventMessage.id === "number") {
            if (
              eventMessage.role === "assistant"
              && eventMessage.content === streamingContentRef.current.trim()
            ) {
              setStreamingContent("");
            }

            lastMsgId.current = Math.max(lastMsgId.current, eventMessage.id);
            setMessages((prev) => mergeMessages(prev, {
              id: eventMessage.id,
              workspace: eventMessage.workspace,
              bot: eventMessage.bot,
              role: eventMessage.role,
              content: eventMessage.content,
              attachments: null,
              created_at: eventMessage.created_at,
            }));
          }

          api.getConversations(workspace, bot, 30, remote).then((msgs) => {
            const latestId = msgs.length > 0 ? msgs[msgs.length - 1].id : 0;
            if (latestId >= lastMsgId.current) {
              lastMsgId.current = latestId;
              setMessages(msgs);
            }
          }).catch(() => {});
        }
      }
    });

    return () => wsConn.close();
  }, [workspace, remote, bot, finalizeStreamingAssistant]);

  useEffect(() => {
    if (!workspace) return;
    api.getBots(workspace, remote).then(setBots);
    api.getWorkers(workspace, remote).then(setWorkers);
    api.getRepos(workspace, remote).then(setRepos);
    api.getUnread(workspace, remote).then(setUnread);
    api.getResearchTasks(workspace, remote).then(setResearchTasks);
    api.getFollowups(workspace, remote).then(setFollowups);
  }, [workspace, remote]);

  useEffect(() => {
    if (!workspace || !bot || mode !== "chat") return;

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

    api.getBotStatus(workspace, bot, remote).then((status) => {
      if (cancelled) return;
      if (status.status !== "idle") {
        setLoading(true);
        setLoadingStatus(status.tool_name ? `Using ${status.tool_name}...` : "Thinking...");
        setStreamingContent(status.streaming_content || "");
      }
    });

    const seenTimer = setTimeout(() => {
      api.markSeen(workspace, bot, remote);
    }, 500);

    return () => {
      cancelled = true;
      clearTimeout(seenTimer);
    };
  }, [workspace, bot, remote, mode]);

  useEffect(() => {
    if (!workspace || !bot || mode !== "chat") return;

    const getInterval = () => {
      if (tabHiddenRef.current) return 30000;
      if (loadingRef.current) return 2000;
      return 10000;
    };

    let timer: ReturnType<typeof setTimeout>;
    let cancelled = false;

    function poll() {
      const currentRemote = remoteRef.current;
      const conversationsPromise = api.getConversations(workspace, bot, 30, currentRemote).then((msgs) => {
        if (cancelled) return;
        const latestId = msgs.length > 0 ? msgs[msgs.length - 1].id : 0;
        if (latestId > lastMsgId.current) {
          lastMsgId.current = latestId;
          setMessages(msgs);
        }
      });

      const statusPromise = api.getBotStatus(workspace, bot, currentRemote).then((status) => {
        if (cancelled) return;
        if (status.status === "idle") {
          finalizeStreamingAssistant();
          setLoading(false);
          setLoadingStatus(undefined);
          setStreamingContent("");
        } else {
          setLoading(true);
          setLoadingStatus(status.tool_name ? `Using ${status.tool_name}...` : "Thinking...");
          setStreamingContent(status.streaming_content || "");
        }
      });

      Promise.all([conversationsPromise, statusPromise]).then(() => {
        if (!cancelled) {
          timer = setTimeout(poll, getInterval());
        }
      });
    }

    timer = setTimeout(poll, getInterval());
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [workspace, bot, mode, finalizeStreamingAssistant]);

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

  useEffect(() => {
    api.getUsage().then(setUsage).catch(() => {});
    const interval = setInterval(() => {
      api.getUsage().then(setUsage).catch(() => {});
    }, 120000);
    return () => clearInterval(interval);
  }, []);

  useEffect(() => {
    pushHash({ workspace, mode, bot, workerId });
  }, [workspace, mode, bot, workerId]);

  useEffect(() => {
    const onPopState = () => {
      const route = parseHash();
      setWorkspace(route.workspace);
      setMode(route.mode);
      setBot(route.bot);
      setWorkerId(route.workerId);
    };
    window.addEventListener("popstate", onPopState);
    return () => window.removeEventListener("popstate", onPopState);
  }, []);

  useEffect(() => {
    if (!paletteOpen || workspaces.length === 0) return;

    let cancelled = false;
    setOtherWorkspaceBots([]);
    setOtherWorkspaceUnreads({});
    const others = workspaces.filter((ws) => ws.name !== workspace || ws.remote !== remote);

    Promise.allSettled(
      others.map((ws) =>
        api.getBots(ws.name, ws.remote).then((workspaceBots) =>
          workspaceBots.map((workspaceBot) => ({
            workspace: ws.name,
            bot: workspaceBot,
            remote: ws.remote ?? undefined,
          })),
        ),
      ),
    ).then((results) => {
      if (cancelled) return;
      const fulfilled = results
        .filter((result): result is PromiseFulfilledResult<Array<{ workspace: string; bot: Bot; remote: string | undefined }>> => result.status === "fulfilled")
        .flatMap((result) => result.value);
      setOtherWorkspaceBots(fulfilled);
    });

    Promise.allSettled(
      others.map((ws) =>
        api.getUnread(ws.name, ws.remote).then((counts) => ({
          key: `${ws.remote || "local"}/${ws.name}`,
          counts,
        })),
      ),
    ).then((results) => {
      if (cancelled) return;
      const map: Record<string, Record<string, number>> = {};
      for (const result of results) {
        if (result.status === "fulfilled") {
          map[result.value.key] = result.value.counts;
        }
      }
      setOtherWorkspaceUnreads(map);
    });

    return () => {
      cancelled = true;
    };
  }, [paletteOpen, workspaces, workspace, remote]);

  useEffect(() => {
    const handleGlobalKeyDown = (event: KeyboardEvent) => {
      if (event.repeat) return;
      const key = event.key.toLowerCase();
      if ((event.metaKey || event.ctrlKey) && key === "k") {
        event.preventDefault();
        setPaletteOpen((value) => !value);
      }
      if ((event.metaKey || event.ctrlKey) && key === "j") {
        event.preventDefault();
        document.querySelector<HTMLTextAreaElement>('textarea[enterkeyhint="send"]')?.focus();
      }
    };

    window.addEventListener("keydown", handleGlobalKeyDown);
    return () => window.removeEventListener("keydown", handleGlobalKeyDown);
  }, []);

  const handleSelectWorkspace = useCallback((name: string, wsRemote?: string) => {
    const nextProfile = resolveWorkspaceConsoleProfile(name, wsRemote);
    const defaults = getDefaultWorkspaceSelection(nextProfile, isMobile);
    setWorkspace(name);
    setRemote(wsRemote);
    setMode(defaults.mode);
    setBot(defaults.bot);
    setWorkerId(null);
    setLoading(false);
    setLoadingStatus(undefined);
  }, [isMobile]);

  const handleSelectWorkspaceBot = useCallback((name: string, botName: string, wsRemote?: string) => {
    setWorkspace(name);
    setRemote(wsRemote);
    setMode("chat");
    setBot(botName);
    setWorkerId(null);
    setMenuOpen(false);
    setLoading(false);
    setLoadingStatus(undefined);
  }, []);

  const handleSelectBot = useCallback((name: string) => {
    setMode("chat");
    setBot(name);
    setWorkerId(null);
    setMenuOpen(false);
    setLoading(false);
    setLoadingStatus(undefined);
  }, []);

  const handleSelectWorker = useCallback((id: string) => {
    setMode("workers");
    setWorkerId(id);
    setMenuOpen(false);
    if (workspace) {
      api.getWorkerDetail(workspace, id, remote).then(setWorkerDetail).catch(() => setWorkerDetail(null));
    }
  }, [workspace, remote]);

  const handleSelectMode = useCallback((nextMode: WorkspaceMode) => {
    setMode(nextMode);
    setMenuOpen(false);
    if (nextMode !== "workers") {
      setWorkerId(null);
    }
    if (nextMode === "chat" && !bot) {
      setBot(consoleProfile.defaultMobileBot);
    }
  }, [bot, consoleProfile.defaultMobileBot]);

  useEffect(() => {
    if (!workspace || !workerId) return;
    const interval = setInterval(() => {
      api.getWorkerDetail(workspace, workerId, remote).then(setWorkerDetail).catch(() => {});
    }, 3000);
    return () => clearInterval(interval);
  }, [workspace, workerId, remote]);

  const handleBackFromWorker = useCallback(() => {
    setWorkerId(null);
    setMode("workers");
  }, []);

  const handleStartResearch = useCallback((topic?: string) => {
    const nextTopic = topic?.trim() || prompt("Research topic:")?.trim();
    if (!nextTopic) return;

    api.startResearch(workspace, nextTopic, remote).then(() => {
      api.getResearchTasks(workspace, remote).then(setResearchTasks);
      setMessages((prev) => [
        ...prev,
        {
          id: Date.now(),
          workspace,
          bot,
          role: "system",
          content: `Research started: ${nextTopic}`,
          attachments: null,
          created_at: new Date().toISOString(),
        },
      ]);
    }).catch(() => {});
  }, [workspace, remote, bot]);

  const handleSend = useCallback(async (
    text: string,
    attachments?: Array<{ name: string; type: string; dataUrl: string }>,
  ) => {
    if (text.startsWith("/research ")) {
      handleStartResearch(text.slice("/research ".length));
      return;
    }

    const apiAttachments = attachments?.map((attachment) => ({
      name: attachment.name,
      type: attachment.type,
      dataUrl: attachment.dataUrl,
    }));

    appendLocalMessage("user", text);
    setMessagesLoading(false);
    setLoading(true);
    setLoadingStatus("Thinking...");
    await api.sendMessage(workspace, bot, text, apiAttachments, remote);
  }, [appendLocalMessage, workspace, bot, remote, handleStartResearch]);

  const reposWithFreshWorkers = useMemo(() => {
    const workerMap = new Map(workers.map((worker) => [worker.id, worker]));
    return repos.map((repo) => ({
      ...repo,
      workers: repo.workers.map((repoWorker) => workerMap.get(repoWorker.id) || repoWorker),
    }));
  }, [repos, workers]);

  const selectedBot = bots.find((entry) => entry.name === bot);
  const selectedWorker = workerId
    ? workers.find((entry) => entry.id === workerId) || null
    : null;
  const pendingFollowupCount = followups.filter((followup) => followup.status === "pending").length;
  const workspaceVoice = workspaces.find((ws) => ws.name === workspace && ws.remote === remote);
  const visibleModes = getOrderedWorkspaceModes(consoleProfile.navModeOrder);

  return {
    workspaces,
    workspace,
    remote,
    mode,
    bot,
    workerId,
    bots,
    workers,
    repos,
    reposWithFreshWorkers,
    messages,
    messagesLoading,
    loading,
    streamingContent,
    loadingStatus,
    workerDetail,
    unread,
    paletteOpen,
    menuOpen,
    simulatorOpen,
    otherWorkspaceBots,
    otherWorkspaceUnreads,
    researchTasks,
    followups,
    isMobile,
    usage,
    consoleProfile,
    visibleModes,
    selectedBot,
    selectedWorker,
    pendingFollowupCount,
    ttsVoice: workspaceVoice?.tts_voice,
    ttsSpeed: workspaceVoice?.tts_speed,
    setPaletteOpen,
    setMenuOpen,
    setSimulatorOpen,
    handleSelectWorkspace,
    handleSelectWorkspaceBot,
    handleSelectBot,
    handleSelectWorker,
    handleSelectMode,
    handleBackFromWorker,
    handleSend,
    handleStartResearch,
    refreshFollowups: () => api.getFollowups(workspace, remote).then(setFollowups),
    cancelActiveBot: loading ? () => api.cancelBot(workspace, bot, remote) : undefined,
  };
}
