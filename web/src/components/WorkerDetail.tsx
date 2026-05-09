import { Suspense, lazy, useState, useRef, useEffect } from "react";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import type { Worker, WorkerDetail as WorkerDetailData } from "@apiari/types";
import * as api from "@apiari/api";
import { ChatInput } from "@apiari/chat";
import { TabBar, Button, Dots } from "@apiari/ui";
import styles from "./WorkerDetail.module.css";

const WorkerDiffPanel = lazy(() =>
  import("./WorkerDiffPanel").then((module) => ({ default: module.WorkerDiffPanel })),
);

interface Props {
  worker: Worker;
  detail: WorkerDetailData | null;
  workspace: string;
  remote?: string;
  onBack: () => void;
  showBack?: boolean;
  onPromoteWorker: (
    id: string,
  ) => Promise<{ ok: boolean; worker_id?: string; pr_url?: string; detail: string }>;
  onRedispatchWorker: (
    id: string,
  ) => Promise<{ ok: boolean; worker_id?: string; pr_url?: string; detail: string }>;
  onCloseWorker: (
    id: string,
  ) => Promise<{ ok: boolean; worker_id?: string; pr_url?: string; detail: string }>;
}

function branchName(branch: string): string {
  return branch.replace(/^swarm\//, "");
}

type InfoTab = "output" | "task" | "diff" | "chat";

function formatTime(iso: string): string {
  const trimmed = iso.trim();
  if (!trimmed) return "";
  const normalized = trimmed.includes("T")
    ? trimmed.includes("Z") || trimmed.includes("+") || trimmed.includes("-", 10)
      ? trimmed
      : `${trimmed}Z`
    : trimmed;
  const date = new Date(normalized);
  if (Number.isNaN(date.getTime())) {
    return trimmed;
  }
  return date.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}

export function WorkerDetail({
  worker,
  detail,
  workspace,
  remote,
  onBack,
  showBack = true,
  onPromoteWorker,
  onRedispatchWorker,
  onCloseWorker,
}: Props) {
  const [sending, setSending] = useState(false);
  const [acting, setActing] = useState(false);
  const [actionMessage, setActionMessage] = useState<string | null>(null);
  const [confirmClose, setConfirmClose] = useState(false);
  const [infoTab, setInfoTab] = useState<InfoTab>("output");
  const [diffContent, setDiffContent] = useState<string | null | undefined>(undefined);
  const bottomRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [detail?.conversation.length, infoTab]);

  // Reset cached diff when worker changes
  useEffect(() => {
    setDiffContent(undefined);
  }, [workspace, worker.id, remote]);

  useEffect(() => {
    setActionMessage(null);
    setConfirmClose(false);
  }, [worker.id]);

  useEffect(() => {
    if (infoTab === "diff" && diffContent === undefined) {
      api
        .getWorkerDiff(workspace, worker.id, remote)
        .then(setDiffContent)
        .catch(() => setDiffContent(null));
    }
  }, [infoTab, workspace, worker.id, remote, diffContent]);

  async function handleWorkerSend(text: string) {
    if (!text || sending) return;
    setSending(true);
    try {
      await api.sendWorkerMessage(workspace, worker.id, text, remote);
    } finally {
      setSending(false);
    }
  }

  async function handlePromote() {
    if (acting) return;
    setActing(true);
    try {
      const result = await onPromoteWorker(worker.id);
      setActionMessage(result.detail);
    } finally {
      setActing(false);
    }
  }

  async function handleRedispatch() {
    if (acting) return;
    setActing(true);
    try {
      const result = await onRedispatchWorker(worker.id);
      setActionMessage(result.detail);
    } finally {
      setActing(false);
    }
  }

  async function handleCloseConfirmed() {
    if (acting) return;
    setActing(true);
    try {
      const result = await onCloseWorker(worker.id);
      setActionMessage(result.detail);
      setConfirmClose(false);
    } finally {
      setActing(false);
    }
  }

  const taskTitle = detail?.task_title ?? worker.task_title;
  const taskStage = detail?.task_stage ?? worker.task_stage;
  const taskLifecycleState =
    detail?.task_lifecycle_state ?? worker.task_lifecycle_state ?? taskStage;
  const taskRepo = detail?.task_repo ?? worker.task_repo;
  const latestAttempt = detail?.latest_attempt ?? worker.latest_attempt ?? null;
  const executionNote = detail?.execution_note ?? worker.execution_note;
  const readyBranch = detail?.ready_branch ?? worker.ready_branch;
  const hasUncommittedChanges = detail?.has_uncommitted_changes ?? worker.has_uncommitted_changes;
  const taskPacket = detail?.task_packet ?? null;
  const branchLabel = branchName(worker.branch);
  const locationLabel = taskRepo ? `repo ${taskRepo}` : branchLabel;

  function renderChat() {
    return (
      <>
        <div className={styles.messages}>
          {(!detail || detail.conversation.length === 0) && (
            <div className={styles.empty}>No conversation data available</div>
          )}
          {detail?.conversation.map((msg, i) => (
            <div
              key={i}
              className={`${styles.msg} ${msg.role === "user" ? styles.userMsg : ""} ${msg.role === "tool" ? styles.toolMsg : ""}`}
            >
              {msg.role === "tool" ? (
                <div className={styles.toolLabel}>{msg.content}</div>
              ) : (
                <>
                  <div className={styles.msgMeta}>
                    <strong>{msg.role === "user" ? "You" : worker.id}</strong>
                    {msg.timestamp && <> · {formatTime(msg.timestamp)}</>}
                  </div>
                  <div className={styles.msgText}>
                    <Markdown remarkPlugins={[remarkGfm]}>{msg.content}</Markdown>
                  </div>
                </>
              )}
            </div>
          ))}
          {sending && (
            <div className={styles.msg}>
              <div className={styles.msgMeta}>
                <strong>{worker.id}</strong>
              </div>
              <div className={styles.thinking}>
                <Dots />
              </div>
            </div>
          )}
          <div ref={bottomRef} />
        </div>
        <ChatInput
          placeholder="Message worker..."
          disabled={sending}
          onSend={handleWorkerSend}
          showAttachments={false}
        />
      </>
    );
  }

  return (
    <div className={styles.layout}>
      {/* Left: worker info */}
      <div className={styles.info}>
        {/* Header with back, title, status, actions */}
        <div className={styles.infoHeader}>
          {showBack ? (
            <Button variant="ghost" size="sm" onClick={onBack}>
              &larr;
            </Button>
          ) : null}
          <div className={styles.headerMid}>
            <div className={styles.titleRow}>
              <span className={styles.title}>{worker.id}</span>
              <span
                className={styles.agentBadge}
                data-agent={worker.agent.split(/[- ]/)[0].toLowerCase()}
              >
                {worker.agent}
              </span>
            </div>
            <div className={styles.subtitle}>
              <span
                className={`${styles.statusDot} ${worker.status === "running" || worker.status === "active" ? styles.running : ""}`}
                style={{
                  background:
                    worker.status === "running" || worker.status === "active"
                      ? "var(--green)"
                      : worker.status === "waiting"
                        ? "var(--accent)"
                        : "var(--text-faint)",
                }}
              />
              {worker.status} &middot; {locationLabel}
            </div>
          </div>
          <div className={styles.headerActions}>
            {worker.pr_url && (
              <a
                href={worker.pr_url}
                target="_blank"
                rel="noopener noreferrer"
                className={styles.headerBtn}
              >
                PR
              </a>
            )}
            {!worker.pr_url && (hasUncommittedChanges || worker.status === "stalled") && (
              <Button variant="secondary" size="sm" onClick={handlePromote} disabled={acting}>
                Promote to PR
              </Button>
            )}
            {(worker.status === "stalled" || hasUncommittedChanges) && (
              <Button variant="secondary" size="sm" onClick={handleRedispatch} disabled={acting}>
                Redispatch
              </Button>
            )}
            <Button
              variant="danger"
              size="sm"
              onClick={() => setConfirmClose((value) => !value)}
              disabled={acting}
            >
              Close
            </Button>
          </div>
        </div>

        {actionMessage && <div className={styles.actionNotice}>{actionMessage}</div>}
        {confirmClose && (
          <div className={styles.actionNotice}>
            Close this worker and dismiss its task?{" "}
            <Button variant="secondary" size="sm" onClick={handleCloseConfirmed} disabled={acting}>
              Confirm
            </Button>{" "}
            <Button
              variant="secondary"
              size="sm"
              onClick={() => setConfirmClose(false)}
              disabled={acting}
            >
              Cancel
            </Button>
          </div>
        )}

        {/* PR review summary */}
        {(worker.review_state ||
          worker.ci_status ||
          taskStage ||
          taskRepo ||
          executionNote ||
          hasUncommittedChanges ||
          readyBranch ||
          (worker.open_comments != null && worker.open_comments > 0)) && (
          <div className={styles.reviewSummary}>
            {worker.review_state && (
              <span className={styles.reviewBadge} data-state={worker.review_state.toLowerCase()}>
                {worker.review_state === "APPROVED"
                  ? "Approved"
                  : worker.review_state === "CHANGES_REQUESTED"
                    ? "Changes requested"
                    : "Review pending"}
              </span>
            )}
            {worker.ci_status && (
              <span className={styles.ciBadge} data-status={worker.ci_status.toLowerCase()}>
                {worker.ci_status === "SUCCESS"
                  ? "CI passing"
                  : worker.ci_status === "FAILURE"
                    ? "CI failing"
                    : "CI pending"}
              </span>
            )}
            {taskStage && (
              <span className={styles.reviewBadge} data-state="pending">
                {taskLifecycleState}
              </span>
            )}
            {worker.status === "stalled" && (
              <span className={styles.reviewBadge} data-state="changes_requested">
                Stalled
              </span>
            )}
            {hasUncommittedChanges && (
              <span className={styles.reviewBadge} data-state="pending">
                Uncommitted diff
              </span>
            )}
            {!readyBranch && hasUncommittedChanges && (
              <span className={styles.commentCount}>no ready branch</span>
            )}
            {readyBranch && <span className={styles.commentCount}>ready branch {readyBranch}</span>}
            {taskRepo && <span className={styles.commentCount}>repo {taskRepo}</span>}
            {executionNote && <span className={styles.commentCount}>{executionNote}</span>}
            {worker.open_comments != null && worker.open_comments > 0 && (
              <span className={styles.commentCount}>
                {worker.open_comments} open / {worker.resolved_comments ?? 0} resolved comments
              </span>
            )}
          </div>
        )}

        {/* Tabs */}
        <TabBar
          variant="underline"
          value={infoTab}
          onChange={(v) => setInfoTab(v as InfoTab)}
          className={styles.tabs}
          tabs={[
            { value: "output", label: "Output" },
            { value: "task", label: "Task" },
            { value: "diff", label: "Diff" },
            { value: "chat", label: "Chat" },
          ]}
        />

        {/* Tab content */}
        <div className={styles.tabContent}>
          {infoTab === "output" &&
            (detail?.output ? (
              <div className={styles.prose}>
                <Markdown remarkPlugins={[remarkGfm]}>{detail.output}</Markdown>
              </div>
            ) : (
              <div className={styles.empty}>No output yet</div>
            ))}
          {infoTab === "task" && (
            <div className={styles.prose}>
              {taskTitle && (
                <p>
                  <strong>Task:</strong> {taskTitle}
                </p>
              )}
              {taskLifecycleState && (
                <p>
                  <strong>Lifecycle:</strong> {taskLifecycleState}
                </p>
              )}
              {taskStage && taskLifecycleState !== taskStage ? (
                <p>
                  <strong>Internal stage:</strong> {taskStage}
                </p>
              ) : null}
              {taskRepo && (
                <p>
                  <strong>Repo:</strong> {taskRepo}
                </p>
              )}
              {latestAttempt?.role ? (
                <p>
                  <strong>Latest attempt:</strong> {latestAttempt.role} {latestAttempt.state}
                </p>
              ) : null}
              {latestAttempt?.detail ? (
                <p>
                  <strong>Attempt detail:</strong> {latestAttempt.detail}
                </p>
              ) : null}
              {hasUncommittedChanges && (
                <p>
                  <strong>Execution:</strong> Uncommitted diff present
                </p>
              )}
              {readyBranch ? (
                <p>
                  <strong>Ready branch:</strong> {readyBranch}
                </p>
              ) : hasUncommittedChanges ? (
                <p>
                  <strong>Ready branch:</strong> not signalled
                </p>
              ) : null}
              {executionNote && (
                <p>
                  <strong>Note:</strong> {executionNote}
                </p>
              )}
              {taskPacket?.worker_mode && (
                <p>
                  <strong>Worker kind:</strong> {taskPacket.worker_mode}
                </p>
              )}
              {detail?.prompt ? (
                <>
                  {(taskTitle || taskStage || taskRepo) && (
                    <p>
                      <strong>Worker prompt</strong>
                    </p>
                  )}
                  <Markdown remarkPlugins={[remarkGfm]}>{detail.prompt}</Markdown>
                </>
              ) : !(taskTitle || taskStage || taskRepo) ? (
                <div className={styles.empty}>No task context</div>
              ) : null}
              {taskPacket?.task_md ? (
                <>
                  <p>
                    <strong>Inherited task</strong>
                  </p>
                  <Markdown remarkPlugins={[remarkGfm]}>{taskPacket.task_md}</Markdown>
                </>
              ) : null}
              {taskPacket?.context_md ? (
                <>
                  <p>
                    <strong>Inherited context</strong>
                  </p>
                  <Markdown remarkPlugins={[remarkGfm]}>{taskPacket.context_md}</Markdown>
                </>
              ) : null}
              {taskPacket?.shaping_md ? (
                <>
                  <p>
                    <strong>Coordinator shaping</strong>
                  </p>
                  <Markdown remarkPlugins={[remarkGfm]}>{taskPacket.shaping_md}</Markdown>
                </>
              ) : null}
              {taskPacket?.plan_md ? (
                <>
                  <p>
                    <strong>Execution plan</strong>
                  </p>
                  <Markdown remarkPlugins={[remarkGfm]}>{taskPacket.plan_md}</Markdown>
                </>
              ) : null}
              {taskPacket?.progress_md ? (
                <>
                  <p>
                    <strong>Worker notes</strong>
                  </p>
                  <Markdown remarkPlugins={[remarkGfm]}>{taskPacket.progress_md}</Markdown>
                </>
              ) : null}
            </div>
          )}
          {infoTab === "diff" &&
            (diffContent === undefined ? (
              <div className={styles.empty}>Loading diff...</div>
            ) : diffContent ? (
              <Suspense fallback={<div className={styles.empty}>Loading diff...</div>}>
                <WorkerDiffPanel diff={diffContent} />
              </Suspense>
            ) : (
              <div className={styles.empty}>No diff available</div>
            ))}
          {infoTab === "chat" && <div className={styles.chatInTab}>{renderChat()}</div>}
        </div>
      </div>
    </div>
  );
}
