import { useCallback, useEffect, useState } from "react";
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

  const refreshWorkerDetail = useCallback((targetWorkerId?: string) => {
    const nextWorkerId = targetWorkerId ?? workerId;
    if (!workspace || !nextWorkerId) {
      setWorkerDetail(null);
      return Promise.resolve(null);
    }
    return api
      .getWorkerDetail(workspace, nextWorkerId, remote)
      .then((detail) => {
        setWorkerDetail(detail);
        return detail;
      })
      .catch(() => {
        setWorkerDetail(null);
        return null;
      });
  }, [workspace, workerId, remote]);

  useEffect(() => {
    if (!workspace || !workerId) return;
    const interval = setInterval(() => {
      refreshWorkerDetail();
    }, 3000);
    return () => clearInterval(interval);
  }, [refreshWorkerDetail, workspace, workerId]);

  const selectedWorker = workerId
    ? workers.find((entry) => entry.id === workerId) || null
    : null;

  return {
    workerDetail,
    setWorkerDetail,
    selectedWorker,
    refreshWorkerDetail,
  };
}
