import { useEffect, useMemo, useState } from "react";
import * as api from "../api";
import type { Bot, CrossWorkspaceBot, Followup, Repo, ResearchTask, Worker, Workspace } from "../types";

interface Props {
  workspace: string;
  remote?: string;
  workspaces: Workspace[];
  paletteOpen: boolean;
}

export function useWorkspaceResourcesState({ workspace, remote, workspaces, paletteOpen }: Props) {
  const [bots, setBots] = useState<Bot[]>([]);
  const [workers, setWorkers] = useState<Worker[]>([]);
  const [repos, setRepos] = useState<Repo[]>([]);
  const [unread, setUnread] = useState<Record<string, number>>({});
  const [researchTasks, setResearchTasks] = useState<ResearchTask[]>([]);
  const [followups, setFollowups] = useState<Followup[]>([]);
  const [usage, setUsage] = useState<api.UsageData>({ installed: false, providers: [], updated_at: null });
  const [otherWorkspaceBots, setOtherWorkspaceBots] = useState<CrossWorkspaceBot[]>([]);
  const [otherWorkspaceUnreads, setOtherWorkspaceUnreads] = useState<Record<string, Record<string, number>>>({});

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

  const reposWithFreshWorkers = useMemo(() => {
    const workerMap = new Map(workers.map((worker) => [worker.id, worker]));
    return repos.map((repo) => ({
      ...repo,
      workers: repo.workers.map((repoWorker) => workerMap.get(repoWorker.id) || repoWorker),
    }));
  }, [repos, workers]);

  return {
    bots,
    setBots,
    workers,
    setWorkers,
    repos,
    setRepos,
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
  };
}
