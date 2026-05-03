import { useEffect, useMemo, useState } from "react";
import {
  ArrowDown,
  ArrowUp,
  LayoutGrid,
  Package,
  Wrench,
  MessageSquare,
  FileText,
} from "lucide-react";
import {
  clearWorkspaceConsoleProfileOverride,
  DEFAULT_WORKSPACE_CONSOLE_PROFILE,
  saveWorkspaceConsoleProfileOverride,
  type WorkspaceConsoleProfile,
  type WorkspaceMode,
} from "../consoleConfig";
import type { Bot } from "../types";
import styles from "./WorkspaceLayoutDialog.module.css";

interface Props {
  open: boolean;
  workspace: string;
  remote?: string;
  bots: Bot[];
  profile: WorkspaceConsoleProfile;
  onClose: () => void;
  onProfileSaved: (profile: WorkspaceConsoleProfile) => void;
}

const MODE_LABELS: Record<WorkspaceMode, string> = {
  overview: "Overview",
  chat: "Chat",
  workers: "Workers",
  repos: "Repos",
  docs: "Docs",
};

const MODE_ICONS = {
  overview: LayoutGrid,
  chat: MessageSquare,
  workers: Wrench,
  repos: Package,
  docs: FileText,
};

function moveMode(order: WorkspaceMode[], index: number, direction: -1 | 1) {
  const nextIndex = index + direction;
  if (nextIndex < 0 || nextIndex >= order.length) return order;
  const next = order.slice();
  const [mode] = next.splice(index, 1);
  next.splice(nextIndex, 0, mode);
  return next;
}

export function WorkspaceLayoutDialog({
  open,
  workspace,
  remote,
  bots,
  profile,
  onClose,
  onProfileSaved,
}: Props) {
  const [draft, setDraft] = useState<WorkspaceConsoleProfile>(profile);
  const botOptions = useMemo(
    () => Array.from(new Set(["Main", ...bots.map((bot) => bot.name)])).filter(Boolean),
    [bots],
  );

  useEffect(() => {
    if (open) setDraft(profile);
  }, [open, profile]);

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open, onClose]);

  if (!open) return null;

  const save = () => {
    saveWorkspaceConsoleProfileOverride(workspace, remote, draft);
    onProfileSaved(draft);
    onClose();
  };

  const reset = () => {
    clearWorkspaceConsoleProfileOverride(workspace, remote);
    onProfileSaved(DEFAULT_WORKSPACE_CONSOLE_PROFILE);
    onClose();
  };

  return (
    <div className={styles.overlay} onClick={onClose}>
      <div
        className={styles.dialog}
        onClick={(event) => event.stopPropagation()}
        role="dialog"
        aria-modal="true"
        aria-label="Workspace layout settings"
      >
        <div className={styles.header}>
          <div>
            <div className={styles.eyebrow}>Workspace layout</div>
            <h2 className={styles.title}>{workspace}{remote ? ` (${remote})` : ""}</h2>
            <p className={styles.subtitle}>
              Configure how this workspace opens, how navigation is ordered, and whether chat keeps a repo rail visible.
            </p>
          </div>
          <button className={styles.close} onClick={onClose} aria-label="Close workspace layout settings">
            ×
          </button>
        </div>

        <div className={styles.content}>
          <div className={styles.grid}>
            <label className={styles.field}>
              <span className={styles.label}>Default desktop mode</span>
              <select
                className={styles.select}
                value={draft.defaultDesktopMode}
                onChange={(event) => setDraft((current) => ({
                  ...current,
                  defaultDesktopMode: event.target.value as WorkspaceMode,
                }))}
              >
                {draft.navModeOrder.map((mode) => (
                  <option key={mode} value={mode}>{MODE_LABELS[mode]}</option>
                ))}
              </select>
            </label>

            <label className={styles.field}>
              <span className={styles.label}>Default mobile mode</span>
              <select
                className={styles.select}
                value={draft.defaultMobileMode}
                onChange={(event) => setDraft((current) => ({
                  ...current,
                  defaultMobileMode: event.target.value as WorkspaceMode,
                }))}
              >
                {draft.navModeOrder.map((mode) => (
                  <option key={mode} value={mode}>{MODE_LABELS[mode]}</option>
                ))}
              </select>
            </label>

            <label className={styles.field}>
              <span className={styles.label}>Primary overview bot</span>
              <select
                className={styles.select}
                value={draft.overviewPrimaryBot}
                onChange={(event) => setDraft((current) => ({
                  ...current,
                  overviewPrimaryBot: event.target.value,
                }))}
              >
                {botOptions.map((name) => (
                  <option key={name} value={name}>{name}</option>
                ))}
              </select>
            </label>

            <label className={styles.field}>
              <span className={styles.label}>Default mobile bot</span>
              <select
                className={styles.select}
                value={draft.defaultMobileBot}
                onChange={(event) => setDraft((current) => ({
                  ...current,
                  defaultMobileBot: event.target.value,
                }))}
              >
                {botOptions.map((name) => (
                  <option key={name} value={name}>{name}</option>
                ))}
              </select>
            </label>
          </div>

          <div className={styles.toggle}>
            <div>
              <div className={styles.label}>Show repo rail during chat</div>
              <div className={styles.hint}>Keep repos and research visible beside the active chat conversation.</div>
            </div>
            <input
              type="checkbox"
              checked={draft.showChatRepoRail}
              onChange={(event) => setDraft((current) => ({
                ...current,
                showChatRepoRail: event.target.checked,
              }))}
              aria-label="Show repo rail during chat"
            />
          </div>

          <div className={styles.section}>
            <div className={styles.label}>Navigation order</div>
            <div className={styles.hint}>Reorder primary workspace modes to match how you operate.</div>
            <div className={styles.modeList}>
              {draft.navModeOrder.map((mode, index) => {
                const Icon = MODE_ICONS[mode];
                return (
                  <div key={mode} className={styles.modeRow}>
                    <div className={styles.modeInfo}>
                      <Icon size={16} />
                      <span>{MODE_LABELS[mode]}</span>
                    </div>
                    <div className={styles.modeButtons}>
                      <button
                        className={styles.moveBtn}
                        onClick={() => setDraft((current) => ({
                          ...current,
                          navModeOrder: moveMode(current.navModeOrder, index, -1),
                        }))}
                        disabled={index === 0}
                        aria-label={`Move ${MODE_LABELS[mode]} earlier`}
                      >
                        <ArrowUp size={14} />
                      </button>
                      <button
                        className={styles.moveBtn}
                        onClick={() => setDraft((current) => ({
                          ...current,
                          navModeOrder: moveMode(current.navModeOrder, index, 1),
                        }))}
                        disabled={index === draft.navModeOrder.length - 1}
                        aria-label={`Move ${MODE_LABELS[mode]} later`}
                      >
                        <ArrowDown size={14} />
                      </button>
                    </div>
                  </div>
                );
              })}
            </div>
          </div>

          <div className={styles.footer}>
            <div className={styles.hint}>Saved per workspace and remote target on this machine.</div>
            <div className={styles.footerActions}>
              <button className={styles.secondary} onClick={reset}>Reset defaults</button>
              <button className={styles.secondary} onClick={onClose}>Cancel</button>
              <button className={styles.primary} onClick={save}>Save layout</button>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
