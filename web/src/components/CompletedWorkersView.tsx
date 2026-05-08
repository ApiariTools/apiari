import type { WorkerV2 } from "../types";
import styles from "./CompletedWorkersView.module.css";

interface Props {
  workers: WorkerV2[];
  onSelectWorker: (id: string) => void;
}

function statusLabel(state: WorkerV2["state"]): string {
  return state === "abandoned" ? "Abandoned" : "Completed";
}

function workerTitle(worker: WorkerV2): string {
  return worker.goal ?? worker.branch ?? worker.id;
}

export default function CompletedWorkersView({ workers, onSelectWorker }: Props) {
  return (
    <div className={styles.container}>
      <div className={styles.header}>
        <span className={styles.eyebrow}>Worker history</span>
        <h1 className={styles.title}>Completed workers</h1>
        <p className={styles.summary}>
          Closed worker history across this workspace. Open any row to inspect its detail view.
        </p>
      </div>

      {workers.length === 0 ? (
        <div className={styles.empty}>No completed or abandoned workers yet.</div>
      ) : (
        <div className={styles.list}>
          {workers.map((worker) => (
            <button
              key={worker.id}
              type="button"
              className={styles.row}
              onClick={() => onSelectWorker(worker.id)}
            >
              <div className={styles.rowMain}>
                <div className={styles.rowTitle}>{workerTitle(worker)}</div>
                <div className={styles.rowMeta}>{worker.id}</div>
              </div>
              <div className={styles.rowSide}>
                <span
                  className={`${styles.status} ${
                    worker.state === "abandoned" ? styles.statusAbandoned : styles.statusDone
                  }`}
                >
                  {statusLabel(worker.state)}
                </span>
                {worker.pr_url ? (
                  <span className={styles.metaPill}>PR linked</span>
                ) : worker.branch_ready ? (
                  <span className={styles.metaPill}>Branch ready</span>
                ) : null}
              </div>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
