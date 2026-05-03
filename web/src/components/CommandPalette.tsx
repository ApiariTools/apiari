import { useMemo } from "react";
import { Command } from "cmdk";
import type { Workspace, Bot, Worker, CrossWorkspaceBot } from "../types";
import styles from "./CommandPalette.module.css";

interface Props {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  workspaces: Workspace[];
  bots: Bot[];
  workers: Worker[];
  currentWorkspace: string;
  currentRemote?: string;
  currentBot: string;
  onSelectWorkspace: (name: string, remote?: string) => void;
  onSelectBot: (name: string) => void;
  onSelectWorker: (id: string) => void;
  otherWorkspaceBots: CrossWorkspaceBot[];
  onSelectWorkspaceBot: (workspace: string, botName: string, remote?: string) => void;
  onStartResearch?: () => void;
  onOpenWorkspaceLayout?: () => void;
  unread?: Record<string, number>;
  otherWorkspaceUnreads?: Record<string, Record<string, number>>;
}

export function CommandPalette({
  open,
  onOpenChange,
  workspaces,
  bots,
  workers,
  currentWorkspace,
  currentRemote,
  currentBot,
  onSelectWorkspace,
  onSelectBot,
  onSelectWorker,
  otherWorkspaceBots,
  onSelectWorkspaceBot,
  onStartResearch,
  onOpenWorkspaceLayout,
  unread = {},
  otherWorkspaceUnreads = {},
}: Props) {
  // Sort current workspace bots: unreads first (highest count first), then original order
  const sortedBots = useMemo(() => {
    return [...bots].sort((a, b) => {
      const ua = unread[a.name] || 0;
      const ub = unread[b.name] || 0;
      if (ua > 0 && ub === 0) return -1;
      if (ua === 0 && ub > 0) return 1;
      if (ua !== ub) return ub - ua;
      return 0;
    });
  }, [bots, unread]);

  // Sort other workspace bots: unreads first
  const sortedOtherBots = useMemo(() => {
    return [...otherWorkspaceBots].sort((a, b) => {
      const keyA = `${a.remote || "local"}/${a.workspace}`;
      const keyB = `${b.remote || "local"}/${b.workspace}`;
      const ua = otherWorkspaceUnreads[keyA]?.[a.bot.name] || 0;
      const ub = otherWorkspaceUnreads[keyB]?.[b.bot.name] || 0;
      if (ua > 0 && ub === 0) return -1;
      if (ua === 0 && ub > 0) return 1;
      if (ua !== ub) return ub - ua;
      return 0;
    });
  }, [otherWorkspaceBots, otherWorkspaceUnreads]);
  return (
    <Command.Dialog
      open={open}
      onOpenChange={onOpenChange}
      label="Command palette"
      overlayClassName={styles.overlay}
      contentClassName={styles.dialog}
    >
      <Command.Input placeholder="Type a command..." aria-label="Search commands" />
      <Command.List>
        <Command.Empty>No results found.</Command.Empty>
        <Command.Group heading="Workspaces">
          {workspaces.map((ws) => (
            <Command.Item
              key={`${ws.remote || "local"}/${ws.name}`}
              value={`workspace ${ws.name} ${ws.remote || ""}`}
              onSelect={() => {
                onSelectWorkspace(ws.name, ws.remote);
                onOpenChange(false);
              }}
            >
              {ws.name}
              {ws.remote && <span className={styles.remoteBadge}>{ws.remote}</span>}
              {ws.name === currentWorkspace && ws.remote === currentRemote && (
                <span className={styles.current}>current</span>
              )}
            </Command.Item>
          ))}
        </Command.Group>
        <Command.Group heading="Bots">
          {sortedBots.map((b) => (
            <Command.Item
              key={b.name}
              value={`bot ${b.name}`}
              onSelect={() => {
                onSelectBot(b.name);
                onOpenChange(false);
              }}
            >
              {b.name}
              {(unread[b.name] || 0) > 0 && (
                <span className={styles.unreadBadge}>{unread[b.name]}</span>
              )}
              {b.name === currentBot && (
                <span className={styles.current}>current</span>
              )}
            </Command.Item>
          ))}
        </Command.Group>
        {sortedOtherBots.length > 0 && (
          <Command.Group heading="Other Workspace Bots">
            {sortedOtherBots.map((entry) => {
              const key = `${entry.remote || "local"}/${entry.workspace}`;
              const count = otherWorkspaceUnreads[key]?.[entry.bot.name] || 0;
              return (
                <Command.Item
                  key={`${entry.remote || "local"}/${entry.workspace}/${entry.bot.name}`}
                  value={`bot ${entry.workspace} ${entry.bot.name} ${entry.remote || ""}`}
                  onSelect={() => {
                    onSelectWorkspaceBot(entry.workspace, entry.bot.name, entry.remote);
                    onOpenChange(false);
                  }}
                >
                  {entry.workspace} / {entry.bot.name}
                  {count > 0 && (
                    <span className={styles.unreadBadge}>{count}</span>
                  )}
                  {entry.remote && <span className={styles.remoteBadge}>{entry.remote}</span>}
                </Command.Item>
              );
            })}
          </Command.Group>
        )}
        {(onStartResearch || onOpenWorkspaceLayout) && (
          <Command.Group heading="Actions">
            {onStartResearch && (
              <Command.Item
                value="start research"
                onSelect={() => {
                  onStartResearch();
                  onOpenChange(false);
                }}
              >
                Start Research...
              </Command.Item>
            )}
            {onOpenWorkspaceLayout && (
              <Command.Item
                value="workspace layout"
                onSelect={() => {
                  onOpenWorkspaceLayout();
                  onOpenChange(false);
                }}
              >
                Customize Workspace Layout...
              </Command.Item>
            )}
          </Command.Group>
        )}
        <Command.Group heading="Workers">
          {workers.map((w) => (
            <Command.Item
              key={w.id}
              value={`worker ${w.id} ${w.branch || ""} ${w.pr_title || ""}`}
              onSelect={() => {
                onSelectWorker(w.id);
                onOpenChange(false);
              }}
            >
              <span className={styles.workerInfo}>
                <span className={styles.workerName}>{w.id}</span>
                <span className={styles.workerDesc}>
                  {w.pr_title || (w.branch ? w.branch.replace(/^swarm\//, "") : "")}
                </span>
              </span>
              <span className={styles.workerMeta}>
                <span className={styles.workerStatus}>{w.status}</span>
              </span>
            </Command.Item>
          ))}
        </Command.Group>
      </Command.List>
    </Command.Dialog>
  );
}
