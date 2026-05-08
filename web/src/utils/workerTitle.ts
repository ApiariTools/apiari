import type { WorkerV2 } from "../types";

export function getWorkerTitle(worker: Pick<WorkerV2, "display_title" | "goal" | "branch" | "id">): string {
  return worker.display_title ?? worker.goal ?? worker.branch ?? worker.id;
}
