import { WorkersPanel } from "../components/WorkersPanel";
import { EmptyState } from "../primitives/EmptyState";
import { InspectorPane } from "../primitives/InspectorPane";
import { PageHeader } from "../primitives/PageHeader";
import { ModeScaffold } from "../primitives/ModeScaffold";
import type { Worker, WorkerDetail as WorkerDetailData } from "../types";
import { Suspense, lazy } from "react";
import styles from "./WorkersMode.module.css";

const WorkerDetail = lazy(() =>
  import("../components/WorkerDetail").then((module) => ({ default: module.WorkerDetail })),
);

function PanelFallback() {
  return <div style={{ color: "var(--text-faint)" }}>Loading worker…</div>;
}

interface Props {
  workspace: string;
  remote?: string;
  workers: Worker[];
  workerId: string | null;
  selectedWorker: Worker | null;
  workerDetail: WorkerDetailData | null;
  isMobile: boolean;
  onSelectWorker: (id: string) => void;
  onBackFromWorker: () => void;
  onPromoteWorker: (id: string) => Promise<{ ok: boolean; worker_id?: string; pr_url?: string; detail: string }>;
  onRedispatchWorker: (id: string) => Promise<{ ok: boolean; worker_id?: string; pr_url?: string; detail: string }>;
}

export function WorkersMode({
  workspace,
  remote,
  workers,
  workerId,
  selectedWorker,
  workerDetail,
  isMobile,
  onSelectWorker,
  onBackFromWorker,
  onPromoteWorker,
  onRedispatchWorker,
}: Props) {
  const activeCount = workers.filter((worker) => worker.status === "running" || worker.status === "active").length;
  const reviewCount = workers.filter((worker) => worker.status === "waiting").length;
  const openPrCount = workers.filter((worker) => worker.pr_url).length;
  const codexCount = workers.filter((worker) => worker.agent.toLowerCase().includes("codex")).length;

  let content;
  if (workerId && selectedWorker) {
    content = (
      <div className={isMobile ? styles.mobileFrame : styles.desktopFrame}>
        {isMobile ? (
          <Suspense fallback={<PanelFallback />}>
            <WorkerDetail
              worker={selectedWorker}
              detail={workerDetail}
              workspace={workspace}
              remote={remote}
              onBack={onBackFromWorker}
              showBack={isMobile}
              onPromoteWorker={onPromoteWorker}
              onRedispatchWorker={onRedispatchWorker}
            />
          </Suspense>
        ) : (
          <>
            <div className={styles.leftRail}>
              <WorkersPanel workers={workers} onSelectWorker={onSelectWorker} />
            </div>
            <div className={styles.rightPane}>
              <Suspense fallback={<PanelFallback />}>
                <WorkerDetail
                  worker={selectedWorker}
                  detail={workerDetail}
                  workspace={workspace}
                  remote={remote}
                  onBack={onBackFromWorker}
                  showBack={isMobile}
                  onPromoteWorker={onPromoteWorker}
                  onRedispatchWorker={onRedispatchWorker}
                />
              </Suspense>
            </div>
          </>
        )}
      </div>
    );
  } else if (isMobile) {
    content = <div className={styles.mobileFrame}><WorkersPanel workers={workers} onSelectWorker={onSelectWorker} /></div>;
  } else {
    content = (
      <div className={styles.desktopFrame}>
        <div className={styles.leftRail}>
          <WorkersPanel workers={workers} onSelectWorker={onSelectWorker} />
        </div>
        <div className={styles.rightPane}>
          <InspectorPane
            placeholder={(
              <EmptyState
                title="Select a worker"
                body="Inspect live execution state, task context, diff output, and worker chat without leaving the Workers tool."
              />
            )}
          />
        </div>
      </div>
    );
  }

  return (
    <ModeScaffold
      hideHeaderOnMobile
      header={(
        <PageHeader
          eyebrow="Execution"
          title="Workers"
          summary={`Track autonomous work across the workspace. ${activeCount} active${reviewCount ? ` · ${reviewCount} in review` : ""}.`}
          meta={(
            <div className={styles.metaRow}>
              <div className={styles.metaCard}>
                <span className={styles.metaLabel}>Running</span>
                <span className={styles.metaValue}>{activeCount}</span>
              </div>
              <div className={styles.metaCard}>
                <span className={styles.metaLabel}>Review</span>
                <span className={styles.metaValue}>{reviewCount}</span>
              </div>
              <div className={styles.metaCard}>
                <span className={styles.metaLabel}>Open PRs</span>
                <span className={styles.metaValue}>{openPrCount}</span>
              </div>
              <div className={styles.metaCard}>
                <span className={styles.metaLabel}>Codex</span>
                <span className={styles.metaValue}>{codexCount}</span>
              </div>
            </div>
          )}
        />
      )}
    >
      <div className={styles.page}>{content}</div>
    </ModeScaffold>
  );
}
