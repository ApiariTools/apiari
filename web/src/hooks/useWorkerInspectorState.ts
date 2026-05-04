import { useEffect, useState } from "react";
import * as api from "../api";
import type { Worker, WorkerDetail as WorkerDetailData } from "../types";

interface Props {
  workspace: string;
  remote?: string;
  workerId: string | null;
  workers: Worker[];
}

export function useWorkerInspectorState({ workspace, remote, workerId, workers }: Props) {
  const [workerDetail, setWorkerDetail] = useState<WorkerDetailData | null>(null);

  useEffect(() => {
    if (!workspace || !workerId) return;
    const interval = setInterval(() => {
      api.getWorkerDetail(workspace, workerId, remote).then(setWorkerDetail).catch(() => {});
    }, 3000);
    return () => clearInterval(interval);
  }, [workspace, workerId, remote]);

  const selectedWorker = workerId
    ? workers.find((entry) => entry.id === workerId) || null
    : null;

  return {
    workerDetail,
    setWorkerDetail,
    selectedWorker,
  };
}
