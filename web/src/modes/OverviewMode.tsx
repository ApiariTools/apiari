import { OverviewPanel } from "../components/OverviewPanel";
import { PageHeader } from "../primitives/PageHeader";
import { ModeScaffold } from "../primitives/ModeScaffold";
import type { WorkspaceMode } from "../consoleConfig";
import type { Bot, Followup, Repo, ResearchTask, Worker } from "../types";
import { StatusBadge } from "../primitives/StatusBadge";

interface Props {
  workspace: string;
  remote?: string;
  bots: Bot[];
  workers: Worker[];
  repos: Repo[];
  followups: Followup[];
  researchTasks: ResearchTask[];
  unread: Record<string, number>;
  primaryBot: string;
  onSelectBot: (name: string) => void;
  onSelectWorker: (id: string) => void;
  onOpenMode: (mode: WorkspaceMode) => void;
}

export function OverviewMode(props: Props) {
  const pendingFollowups = props.followups.filter((followup) => followup.status === "pending").length;
  const activeWorkers = props.workers.filter((worker) => worker.status === "running" || worker.status === "active").length;
  const modifiedRepos = props.repos.filter((repo) => !repo.is_clean).length;
  return (
    <ModeScaffold
      scrollBody
      hideHeaderOnMobile
      header={(
        <PageHeader
          eyebrow="Workspace control room"
          title={props.workspace}
          summary="Make bots, workers, repos, docs, and follow-ups the primary objects. Chat stays available, but the workspace leads."
          meta={(
            <div style={{ display: "flex", flexWrap: "wrap", gap: 8, marginTop: 14 }}>
              <StatusBadge tone="accent">{pendingFollowups} pending followups</StatusBadge>
              <StatusBadge tone="success">{activeWorkers} workers active</StatusBadge>
              <StatusBadge tone={modifiedRepos > 0 ? "accent" : "neutral"}>{modifiedRepos} repos modified</StatusBadge>
            </div>
          )}
          actions={[
            { label: `Open ${props.primaryBot} chat`, onClick: () => props.onSelectBot(props.primaryBot), kind: "primary" },
            { label: "Review workers", onClick: () => props.onOpenMode("workers"), kind: "secondary" },
          ]}
        />
      )}
    >
      <OverviewPanel {...props} showHero={false} />
    </ModeScaffold>
  );
}
