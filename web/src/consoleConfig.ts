import type { ComponentType } from "react";
import { FileText, LayoutGrid, MessageSquare, Package, Wrench } from "lucide-react";

export type WorkspaceMode = "overview" | "chat" | "workers" | "repos" | "docs";

export interface WorkspaceModeDefinition {
  id: WorkspaceMode;
  label: string;
  icon: ComponentType<{ size?: number; className?: string }>;
}

export interface WorkspaceConsoleProfile {
  defaultDesktopMode: WorkspaceMode;
  defaultMobileMode: WorkspaceMode;
  defaultMobileBot: string;
  navModeOrder: WorkspaceMode[];
  showChatRepoRail: boolean;
  overviewPrimaryBot: string;
}

interface WorkspaceConsoleProfileRule {
  workspace?: string;
  remote?: string;
  profile: Partial<WorkspaceConsoleProfile>;
}

const CONSOLE_PROFILE_STORAGE_KEY = "apiari.consoleProfileOverrides";

export const WORKSPACE_MODE_DEFINITIONS: Record<WorkspaceMode, WorkspaceModeDefinition> = {
  overview: { id: "overview", label: "Overview", icon: LayoutGrid },
  chat: { id: "chat", label: "Chat", icon: MessageSquare },
  workers: { id: "workers", label: "Workers", icon: Wrench },
  repos: { id: "repos", label: "Repos", icon: Package },
  docs: { id: "docs", label: "Docs", icon: FileText },
};

export const DEFAULT_WORKSPACE_CONSOLE_PROFILE: WorkspaceConsoleProfile = {
  defaultDesktopMode: "overview",
  defaultMobileMode: "chat",
  defaultMobileBot: "Main",
  navModeOrder: ["overview", "chat", "workers", "repos", "docs"],
  showChatRepoRail: true,
  overviewPrimaryBot: "Main",
};

const WORKSPACE_CONSOLE_PROFILE_RULES: WorkspaceConsoleProfileRule[] = [];

export function isWorkspaceMode(value: string | undefined): value is WorkspaceMode {
  return value != null && value in WORKSPACE_MODE_DEFINITIONS;
}

export function getOrderedWorkspaceModes(order: WorkspaceMode[] = DEFAULT_WORKSPACE_CONSOLE_PROFILE.navModeOrder) {
  return order.map((mode) => WORKSPACE_MODE_DEFINITIONS[mode]);
}

function storageKeyForWorkspace(workspace?: string, remote?: string) {
  if (!workspace) return null;
  return `${remote || "local"}/${workspace}`;
}

function readStoredWorkspaceConsoleProfile(workspace?: string, remote?: string): Partial<WorkspaceConsoleProfile> | null {
  const storageKey = storageKeyForWorkspace(workspace, remote);
  if (!storageKey || typeof window === "undefined") return null;

  try {
    const raw = window.localStorage.getItem(CONSOLE_PROFILE_STORAGE_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as Record<string, Partial<WorkspaceConsoleProfile>>;
    return parsed[storageKey] || null;
  } catch {
    return null;
  }
}

function readStoredWorkspaceConsoleProfiles(): Record<string, Partial<WorkspaceConsoleProfile>> {
  if (typeof window === "undefined") return {};
  try {
    const raw = window.localStorage.getItem(CONSOLE_PROFILE_STORAGE_KEY);
    if (!raw) return {};
    return JSON.parse(raw) as Record<string, Partial<WorkspaceConsoleProfile>>;
  } catch {
    return {};
  }
}

function writeStoredWorkspaceConsoleProfiles(
  profiles: Record<string, Partial<WorkspaceConsoleProfile>>,
) {
  if (typeof window === "undefined") return;
  window.localStorage.setItem(CONSOLE_PROFILE_STORAGE_KEY, JSON.stringify(profiles));
}

export function resolveWorkspaceConsoleProfile(workspace?: string, remote?: string): WorkspaceConsoleProfile {
  const profile: WorkspaceConsoleProfile = {
    ...DEFAULT_WORKSPACE_CONSOLE_PROFILE,
  };

  for (const rule of WORKSPACE_CONSOLE_PROFILE_RULES) {
    const workspaceMatches = !rule.workspace || rule.workspace === workspace;
    const remoteMatches = !rule.remote || rule.remote === remote;
    if (workspaceMatches && remoteMatches) {
      Object.assign(profile, rule.profile);
    }
  }

  Object.assign(profile, readStoredWorkspaceConsoleProfile(workspace, remote));

  return profile;
}

export function getDefaultWorkspaceSelection(profile: WorkspaceConsoleProfile, isMobile: boolean) {
  return {
    mode: isMobile ? profile.defaultMobileMode : profile.defaultDesktopMode,
    bot: isMobile && profile.defaultMobileMode === "chat" ? profile.defaultMobileBot : "",
  };
}

export function saveWorkspaceConsoleProfileOverride(
  workspace: string,
  remote: string | undefined,
  profile: Partial<WorkspaceConsoleProfile>,
) {
  const key = storageKeyForWorkspace(workspace, remote);
  if (!key) return;
  const profiles = readStoredWorkspaceConsoleProfiles();
  profiles[key] = profile;
  writeStoredWorkspaceConsoleProfiles(profiles);
}

export function clearWorkspaceConsoleProfileOverride(workspace: string, remote?: string) {
  const key = storageKeyForWorkspace(workspace, remote);
  if (!key) return;
  const profiles = readStoredWorkspaceConsoleProfiles();
  delete profiles[key];
  writeStoredWorkspaceConsoleProfiles(profiles);
}
