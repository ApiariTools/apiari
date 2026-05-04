import { Suspense, lazy, useState, useRef, useEffect } from "react";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import type { Worker, WorkerDetail as WorkerDetailData } from "../types";
import * as api from "../api";
import { ChatInput } from "./ChatInput";
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
}

function branchName(branch: string): string {
  return branch.replace(/^swarm\//, "");
}

type InfoTab = "output" | "task" | "diff" | "chat";

function formatTime(iso: string): string {
  const trimmed = iso.trim();
  if (!trimmed) return "";
  const normalized = trimmed.includes("T")
    ? (trimmed.includes("Z") || trimmed.includes("+") || trimmed.includes("-", 10) ? trimmed : `${trimmed}Z`)
    : trimmed;
  const date = new Date(normalized);
  if (Number.isNaN(date.getTime())) {
    return trimmed;
  }
  return date.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
}

export function WorkerDetail({ worker, detail, workspace, remote, onBack, showBack = true }: Props) {
  const [sending, setSending] = useState(false);
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
    if (infoTab === "diff" && diffContent === undefined) {
      api.getWorkerDiff(workspace, worker.id, remote).then(setDiffContent).catch(() => setDiffContent(null));
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

  function renderChat() {
    return (
      <>
        <div className={styles.messages}>
          {(!detail || detail.conversation.length === 0) && (
            <div className={styles.empty}>No conversation data available</div>
          )}
          {detail?.conversation.map((msg, i) => (
            <div key={i} className={`${styles.msg} ${msg.role === "user" ? styles.userMsg : ""} ${msg.role === "tool" ? styles.toolMsg : ""}`}>
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
          {showBack ? <button className={styles.back} onClick={onBack}>&larr;</button> : null}
          <div className={styles.headerMid}>
            <div className={styles.titleRow}>
              <span className={styles.title}>{worker.id}</span>
              <span className={styles.agentBadge} data-agent={worker.agent.split(/[- ]/)[0].toLowerCase()}>
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
              {worker.status} &middot; {branchName(worker.branch)}
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
            <button className={`${styles.headerBtn} ${styles.headerBtnDanger}`}>
              Close
            </button>
          </div>
        </div>

        {/* PR review summary */}
        {(worker.review_state || worker.ci_status || (worker.open_comments != null && worker.open_comments > 0)) && (
          <div className={styles.reviewSummary}>
            {worker.review_state && (
              <span className={styles.reviewBadge} data-state={worker.review_state.toLowerCase()}>
                {worker.review_state === "APPROVED" ? "Approved" :
                 worker.review_state === "CHANGES_REQUESTED" ? "Changes requested" :
                 "Review pending"}
              </span>
            )}
            {worker.ci_status && (
              <span className={styles.ciBadge} data-status={worker.ci_status.toLowerCase()}>
                {worker.ci_status === "SUCCESS" ? "CI passing" :
                 worker.ci_status === "FAILURE" ? "CI failing" :
                 "CI pending"}
              </span>
            )}
            {worker.open_comments != null && worker.open_comments > 0 && (
              <span className={styles.commentCount}>
                {worker.open_comments} open / {worker.resolved_comments ?? 0} resolved comments
              </span>
            )}
          </div>
        )}

        {/* Tabs */}
        <div className={styles.tabs}>
          <button
            className={`${styles.tab} ${infoTab === "output" ? styles.tabActive : ""}`}
            onClick={() => setInfoTab("output")}
          >
            Output
          </button>
          <button
            className={`${styles.tab} ${infoTab === "task" ? styles.tabActive : ""}`}
            onClick={() => setInfoTab("task")}
          >
            Task
          </button>
          <button
            className={`${styles.tab} ${infoTab === "diff" ? styles.tabActive : ""}`}
            onClick={() => setInfoTab("diff")}
          >
            Diff
          </button>
          <button
            className={`${styles.tab} ${infoTab === "chat" ? styles.tabActive : ""}`}
            onClick={() => setInfoTab("chat")}
          >
            Chat
          </button>
        </div>

        {/* Tab content */}
        <div className={styles.tabContent}>
          {infoTab === "output" && (
            detail?.output ? (
              <div className={styles.prose}>
                <Markdown remarkPlugins={[remarkGfm]}>{detail.output}</Markdown>
              </div>
            ) : (
              <div className={styles.empty}>No output yet</div>
            )
          )}
          {infoTab === "task" && (
            detail?.prompt ? (
              <div className={styles.prose}>
                <Markdown remarkPlugins={[remarkGfm]}>{detail.prompt}</Markdown>
              </div>
            ) : (
              <div className={styles.empty}>No task prompt</div>
            )
          )}
          {infoTab === "diff" && (
            diffContent === undefined ? (
              <div className={styles.empty}>Loading diff...</div>
            ) : diffContent ? (
              <Suspense fallback={<div className={styles.empty}>Loading diff...</div>}>
                <WorkerDiffPanel diff={diffContent} />
              </Suspense>
            ) : (
              <div className={styles.empty}>No diff available</div>
            )
          )}
          {infoTab === "chat" && (
            <div className={styles.chatInTab}>
              {renderChat()}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
