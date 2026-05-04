import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import * as api from "./api";
import {
  getDefaultWorkspaceSelection,
  getOrderedWorkspaceModes,
  isWorkspaceMode,
  resolveWorkspaceConsoleProfile,
  type WorkspaceConsoleProfile,
  type WorkspaceMode,
} from "./consoleConfig";
import { useChatModeState } from "./hooks/useChatModeState";
import { useWorkerInspectorState } from "./hooks/useWorkerInspectorState";
import { useWorkspaceResourcesState } from "./hooks/useWorkspaceResourcesState";
import type { Workspace } from "./types";
import { initWakeLock } from "./wakeLock";

export interface Route {
  workspace: string;
  mode: WorkspaceMode;
  bot: string;
  workerId: string | null;
  docName: string | null;
}

interface ModeStateSnapshot {
  bot: string;
  workerId: string | null;
  docName: string | null;
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
      bot: mode === "chat" || mode === "diagnostics" ? parts[2] || "" : "",
      workerId: mode === "workers" && parts[2] === "worker" ? parts[3] || null : null,
      docName: mode === "docs" ? decodeURIComponent(parts[2] || "") || null : null,
    };
  }

  return {
    workspace,
    mode: parts[2] === "worker" ? "workers" : parts[1] ? "chat" : "overview",
    bot: parts[1] || "",
    workerId: parts[2] === "worker" ? parts[3] || null : null,
    docName: null,
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
      return route.docName
        ? `#/${route.workspace}/docs/${encodeURIComponent(route.docName)}`
        : `#/${route.workspace}/docs`;
    case "signals":
      return `#/${route.workspace}/signals`;
    case "diagnostics":
      return route.bot
        ? `#/${route.workspace}/diagnostics/${route.bot}`
        : `#/${route.workspace}/diagnostics`;
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

export function useWorkspaceConsoleState() {
  const initial = parseHash();
  const hasInitialHash = window.location.hash.replace(/^#\/?/, "").length > 0;

  const [workspaces, setWorkspaces] = useState<Workspace[]>([]);
  const [workspace, setWorkspace] = useState(initial.workspace);
  const [remote, setRemote] = useState<string | undefined>();
  const [mode, setMode] = useState<WorkspaceMode>(initial.mode);
  const [bot, setBot] = useState(initial.bot);
  const [workerId, setWorkerId] = useState<string | null>(initial.workerId);
  const [docName, setDocName] = useState<string | null>(initial.docName);

  const [menuOpen, setMenuOpen] = useState(false);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [simulatorOpen, setSimulatorOpen] = useState(false);
  const [layoutDialogOpen, setLayoutDialogOpen] = useState(false);
  const [isMobile, setIsMobile] = useState(window.innerWidth <= 768);
  const [isTablet, setIsTablet] = useState(window.innerWidth <= 1366);
  const [consoleProfileVersion, setConsoleProfileVersion] = useState(0);
  const modeStateByWorkspaceRef = useRef<Record<string, Partial<Record<WorkspaceMode, ModeStateSnapshot>>>>({});
  const lastModeTapRef = useRef<{ mode: WorkspaceMode; at: number } | null>(null);

  const workspaceStateKey = `${remote || "local"}/${workspace || ""}`;

  const consoleProfile = useMemo(
    () => resolveWorkspaceConsoleProfile(workspace, remote),
    [workspace, remote, consoleProfileVersion],
  );

  useEffect(() => {
    const handleResize = () => {
      setIsMobile(window.innerWidth <= 768);
      setIsTablet(window.innerWidth <= 1366);
    };
    window.addEventListener("resize", handleResize);
    return () => window.removeEventListener("resize", handleResize);
  }, []);

  useEffect(() => initWakeLock(), []);

  useEffect(() => {
    api.getWorkspaces().then((ws) => {
      setWorkspaces(ws);
      if (!workspace && ws.length > 0) {
        setWorkspace(ws[0].name);
      }
    });

    if (window.innerWidth <= 768 && !hasInitialHash) {
      const defaults = getDefaultWorkspaceSelection(resolveWorkspaceConsoleProfile(), true);
      setMode(defaults.mode);
      setBot(defaults.bot);
    }
  }, [hasInitialHash, initial.bot, initial.mode, workspace]);

  const {
    bots,
    workers,
    repos,
    reposWithFreshWorkers,
    unread,
    setUnread,
    researchTasks,
    setResearchTasks,
    followups,
    setFollowups,
    usage,
    otherWorkspaceBots,
    otherWorkspaceUnreads,
  } = useWorkspaceResourcesState({ workspace, remote, workspaces, paletteOpen });

  const {
    messages,
    setMessages,
    messagesLoading,
    loading,
    setLoading,
    streamingContent,
    loadingStatus,
    setLoadingStatus,
    hasOlderHistory,
    loadingOlderHistory,
    loadOlderHistory,
    appendLocalMessage,
    appendSystemMessage,
    beginUserSend,
  } = useChatModeState({
    workspace,
    remote,
    bot,
    mode,
    onUnreadRefresh: () => api.getUnread(workspace, remote).then(setUnread),
    onResearchRefresh: () => api.getResearchTasks(workspace, remote).then(setResearchTasks),
    onFollowupsRefresh: () => api.getFollowups(workspace, remote).then(setFollowups),
  });

  const {
    workerDetail,
    setWorkerDetail,
    selectedWorker,
  } = useWorkerInspectorState({ workspace, remote, workerId, workers });

  useEffect(() => {
    pushHash({ workspace, mode, bot, workerId, docName });
  }, [workspace, mode, bot, workerId, docName]);

  useEffect(() => {
    if (!workspace) return;
    const workspaceState = modeStateByWorkspaceRef.current[workspaceStateKey] || {};
    workspaceState[mode] = { bot, workerId, docName };
    modeStateByWorkspaceRef.current[workspaceStateKey] = workspaceState;
  }, [workspace, workspaceStateKey, mode, bot, workerId, docName]);

  useEffect(() => {
    const onPopState = () => {
      const route = parseHash();
      setWorkspace(route.workspace);
      setMode(route.mode);
      setBot(route.bot);
      setWorkerId(route.workerId);
      setDocName(route.docName);
    };
    window.addEventListener("popstate", onPopState);
    return () => window.removeEventListener("popstate", onPopState);
  }, []);

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
    setDocName(null);
    setMenuOpen(false);
    setLoading(false);
    setLoadingStatus(undefined);
  }, [isMobile]);

  const handleSelectWorkspaceBot = useCallback((name: string, botName: string, wsRemote?: string) => {
    setWorkspace(name);
    setRemote(wsRemote);
    setMode("chat");
    setBot(botName);
    setWorkerId(null);
    setDocName(null);
    setMenuOpen(false);
    setLoading(false);
    setLoadingStatus(undefined);
  }, []);

  const handleSelectBot = useCallback((name: string) => {
    setMode("chat");
    setBot(name);
    setWorkerId(null);
    setDocName(null);
    setMenuOpen(false);
    setLoading(false);
    setLoadingStatus(undefined);
  }, []);

  const handleSelectWorker = useCallback((id: string) => {
    setMode("workers");
    setWorkerId(id);
    setDocName(null);
    setMenuOpen(false);
    if (workspace) {
      api.getWorkerDetail(workspace, id, remote).then(setWorkerDetail).catch(() => setWorkerDetail(null));
    }
  }, [workspace, remote]);

  const handleSelectMode = useCallback((nextMode: WorkspaceMode) => {
    const now = Date.now();
    const previousTap = lastModeTapRef.current;
    const repeatedActiveTap =
      previousTap?.mode === nextMode
      && previousTap.at + 500 >= now
      && mode === nextMode;

    lastModeTapRef.current = { mode: nextMode, at: now };

    const workspaceState = modeStateByWorkspaceRef.current[workspaceStateKey] || {};
    const remembered = workspaceState[nextMode];

    setMode(nextMode);
    setMenuOpen(false);

    if (repeatedActiveTap) {
      if (nextMode === "workers") {
        setWorkerId(null);
      } else if (nextMode === "chat") {
        setWorkerId(null);
        setBot("");
      } else if (nextMode === "docs") {
        setDocName(null);
      } else {
        setWorkerId(null);
      }
      return;
    }

    if (nextMode === "workers") {
      setWorkerId(remembered?.workerId ?? null);
      return;
    }

    setWorkerId(null);

    if (nextMode === "chat") {
      setBot(remembered?.bot || bot || "");
      return;
    }

    if (nextMode === "docs") {
      if (isMobile) {
        setDocName(null);
        return;
      }
      setDocName(remembered?.docName ?? null);
      return;
    }

    if (nextMode === "diagnostics") {
      setBot(remembered?.bot || bot || consoleProfile.overviewPrimaryBot || "");
    }
  }, [bot, consoleProfile.overviewPrimaryBot, isMobile, mode, workspaceStateKey]);

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
    const attachmentsJson = apiAttachments && apiAttachments.length > 0
      ? JSON.stringify(apiAttachments)
      : null;

    appendLocalMessage("user", text, attachmentsJson);
    beginUserSend();
    try {
      await api.sendMessage(workspace, bot, text, apiAttachments, remote);
    } catch (error) {
      appendSystemMessage(
        error instanceof Error ? `Send failed: ${error.message}` : "Send failed.",
      );
      setLoading(false);
      setLoadingStatus(undefined);
    }
  }, [
    appendLocalMessage,
    workspace,
    bot,
    remote,
    handleStartResearch,
    beginUserSend,
    appendSystemMessage,
    setLoading,
    setLoadingStatus,
  ]);

  const selectedBot = bots.find((entry) => entry.name === bot);
  const pendingFollowupCount = followups.filter((followup) => followup.status === "pending").length;
  const workspaceVoice = workspaces.find((ws) => ws.name === workspace && ws.remote === remote);
  const visibleModes = getOrderedWorkspaceModes(
    (isMobile ? consoleProfile.navModeOrder.filter((mode) => mode !== "repos") : consoleProfile.navModeOrder)
      .filter((mode) => mode !== "signals" && mode !== "diagnostics"),
  );
  const applyConsoleProfile = useCallback((nextProfile: WorkspaceConsoleProfile) => {
    setConsoleProfileVersion((value) => value + 1);
    const nextMode = nextProfile.navModeOrder.includes(mode) ? mode : nextProfile.defaultDesktopMode;
    setMode(nextMode);
  }, [mode]);

  return {
    workspaces,
    workspace,
    remote,
    mode,
    bot,
    workerId,
    docName,
    bots,
    workers,
    repos,
    reposWithFreshWorkers,
    messages,
    messagesLoading,
    loading,
    streamingContent,
    loadingStatus,
    hasOlderHistory,
    loadingOlderHistory,
    loadOlderHistory,
    workerDetail,
    unread,
    paletteOpen,
    menuOpen,
    simulatorOpen,
    layoutDialogOpen,
    otherWorkspaceBots,
    otherWorkspaceUnreads,
    researchTasks,
    followups,
    isMobile,
    isTablet,
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
    setLayoutDialogOpen,
    setDocName,
    handleSelectWorkspace,
    handleSelectWorkspaceBot,
    handleSelectBot,
    handleSelectWorker,
    handleSelectMode,
    handleBackFromWorker,
    handleSend,
    handleStartResearch,
    applyConsoleProfile,
    refreshFollowups: () => api.getFollowups(workspace, remote).then(setFollowups),
    cancelActiveBot: loading ? () => api.cancelBot(workspace, bot, remote) : undefined,
  };
}
