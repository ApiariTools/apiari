import { isWorkspaceMode } from "./consoleConfig";
import type { WorkspaceMode } from "./consoleConfig";

export interface Route {
  workspace: string;
  mode: WorkspaceMode;
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
      bot:
        mode === "chat" || mode === "diagnostics" ? decodeURIComponent(parts[2] || "") || "" : "",
      workerId: mode === "workers" && parts[2] === "worker" ? parts[3] || null : null,
      docName: mode === "docs" ? decodeURIComponent(parts[2] || "") || null : null,
    };
  }

  return {
    workspace,
    mode: parts[2] === "worker" ? "workers" : parts[1] ? "chat" : "overview",
    bot: decodeURIComponent(parts[1] || "") || "",
    workerId: parts[2] === "worker" ? parts[3] || null : null,
    docName: null,
  };
}
