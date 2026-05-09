import { ReposPanel } from "../components/ReposPanel";
import { PageHeader, ModeScaffold } from "@apiari/ui";
import type { Repo, ResearchTask } from "@apiari/types";

interface Props {
  repos: Repo[];
  researchTasks: ResearchTask[];
  onSelectWorker: (id: string) => void;
}

export function ReposMode({ repos, researchTasks, onSelectWorker }: Props) {
  const dirtyRepos = repos.filter((repo) => !repo.is_clean).length;
  const runningResearch = researchTasks.filter((task) => task.status === "running").length;

  return (
    <ModeScaffold
      hideHeaderOnMobile
      header={(
        <PageHeader
          eyebrow="Workspace state"
          title="Repos"
          summary={`${repos.length} repos tracked · ${dirtyRepos} modified · ${runningResearch} research tasks running.`}
        />
      )}
    >
      <ReposPanel repos={repos} researchTasks={researchTasks} onSelectWorker={onSelectWorker} />
    </ModeScaffold>
  );
}
