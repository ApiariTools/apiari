import { useCallback, useEffect, useRef, useState } from "react";
import ReactMarkdown from "react-markdown";
import {
  Activity,
  Clock,
  MessageSquare,
  Send,
  Zap,
  AlertCircle,
  CheckCircle,
  XCircle,
} from "lucide-react";
import type {
  AutoBotDetail as AutoBotDetailData,
  AutoBotRun,
  ContextBotContext,
  DashboardWidget,
} from "@apiari/types";
import {
  getAutoBot,
  triggerAutoBot,
  updateAutoBot,
  listWidgets,
  chatWithAutoBot,
} from "@apiari/api";
import { Button, Spinner } from "@apiari/ui";
import Widget from "../widgets/Widget";
import { formatRelative } from "../../utils/time";
import styles from "./AutoBotDetail.module.css";

// ── Status dot ────────────────────────────────────────────────────────

function StatusDot({ status }: { status: string }) {
  const cls =
    status === "running"
      ? styles.statusDotRunning
      : status === "error"
        ? styles.statusDotError
        : styles.statusDotIdle;

  return <span className={`${styles.statusDot} ${cls}`} aria-hidden="true" />;
}

// ── Trigger line ──────────────────────────────────────────────────────

function TriggerLine({ bot }: { bot: AutoBotDetailData }) {
  if (bot.trigger_type === "cron") {
    return (
      <div className={styles.triggerLine}>
        <Clock size={13} />
        {bot.cron_schedule
          ? `Runs on schedule: ${bot.cron_schedule}`
          : "Scheduled (no cron expression set)"}
      </div>
    );
  }
  return (
    <div className={styles.triggerLine}>
      <Zap size={13} />
      {bot.signal_source
        ? `Watches: ${bot.signal_source} signals`
        : "Watches signals (no source set)"}
    </div>
  );
}

// ── Outcome badge ─────────────────────────────────────────────────────

function OutcomeBadge({ outcome }: { outcome: AutoBotRun["outcome"] }) {
  if (outcome === null) {
    return (
      <span className={`${styles.badge} ${styles.badgeRunning}`} data-testid="badge-running">
        <Spinner size="sm" />
        Running
      </span>
    );
  }

  switch (outcome) {
    case "dispatched_worker":
      return (
        <span
          className={`${styles.badge} ${styles.badgeDispatched}`}
          data-testid="badge-dispatched"
        >
          <CheckCircle size={11} />
          Dispatched worker
        </span>
      );
    case "notified":
      return (
        <span className={`${styles.badge} ${styles.badgeNotified}`} data-testid="badge-notified">
          <Activity size={11} />
          Notified
        </span>
      );
    case "noise":
      return (
        <span className={`${styles.badge} ${styles.badgeNoise}`} data-testid="badge-noise">
          No action
        </span>
      );
    case "error":
      return (
        <span className={`${styles.badge} ${styles.badgeError}`} data-testid="badge-error">
          <AlertCircle size={11} />
          Error
        </span>
      );
    default:
      return null;
  }
}

// ── Run card ──────────────────────────────────────────────────────────

interface RunCardProps {
  run: AutoBotRun;
  onSelectWorker?: (workerId: string) => void;
}

function RunCard({ run, onSelectWorker }: RunCardProps) {
  const timestamp =
    run.outcome === null ? "just now" : formatRelative(run.finished_at ?? run.started_at);

  return (
    <div className={styles.runCard} data-testid="run-card">
      <div className={styles.runCardHeader}>
        <OutcomeBadge outcome={run.outcome} />
        <div className={styles.runMeta}>
          <span className={styles.runTriggeredBy}>{run.triggered_by}</span>
          <span className={styles.runTimestamp}>{timestamp}</span>
        </div>
      </div>

      {run.summary && (
        <div className={styles.runSummary}>
          <ReactMarkdown>{run.summary}</ReactMarkdown>
        </div>
      )}

      {run.outcome === null && !run.summary && <div className={styles.runSummary}>Running...</div>}

      {run.worker_id && onSelectWorker && (
        <button
          className={styles.workerLink}
          onClick={() => onSelectWorker(run.worker_id!)}
          type="button"
        >
          <XCircle size={11} />
          Worker: {run.worker_id}
        </button>
      )}
    </div>
  );
}

// ── Chat bubble ───────────────────────────────────────────────────────

function ChatBubble({ run, onSelectWorker }: RunCardProps) {
  const isPending = run.outcome === null;
  const timestamp = isPending ? "just now" : formatRelative(run.finished_at ?? run.started_at);

  return (
    <div className={styles.chatTurn} data-testid="chat-turn">
      {run.chat_message && (
        <div className={styles.chatUserBubble}>
          <span className={styles.chatUserText}>{run.chat_message}</span>
          <span className={styles.chatTimestamp}>{timestamp}</span>
        </div>
      )}
      <div className={styles.chatBotBubble}>
        {isPending ? (
          <span className={styles.chatPending}>
            <Spinner size="sm" />
            Thinking…
          </span>
        ) : run.summary ? (
          <div className={styles.chatBotText}>
            <ReactMarkdown>{run.summary}</ReactMarkdown>
          </div>
        ) : (
          <span className={styles.chatBotText} style={{ color: "var(--text-faint)" }}>
            No response
          </span>
        )}
        {run.worker_id && onSelectWorker && (
          <button
            className={styles.workerLink}
            onClick={() => onSelectWorker(run.worker_id!)}
            type="button"
          >
            <XCircle size={11} />
            Worker: {run.worker_id}
          </button>
        )}
      </div>
    </div>
  );
}

// ── Toggle switch ─────────────────────────────────────────────────────

interface ToggleProps {
  enabled: boolean;
  onChange: (enabled: boolean) => void;
  disabled?: boolean;
}

function Toggle({ enabled, onChange, disabled }: ToggleProps) {
  return (
    <label className={styles.toggleLabel} data-testid="enable-toggle">
      <input
        type="checkbox"
        className={styles.toggleInput}
        checked={enabled}
        onChange={(e) => onChange(e.target.checked)}
        disabled={disabled}
        aria-label={enabled ? "Disable bot" : "Enable bot"}
      />
      <span className={`${styles.toggleTrack} ${enabled ? styles.toggleTrackEnabled : ""}`}>
        <span className={`${styles.toggleThumb} ${enabled ? styles.toggleThumbEnabled : ""}`} />
      </span>
      <span className={styles.toggleText}>{enabled ? "Enabled" : "Disabled"}</span>
    </label>
  );
}

// ── Main component ────────────────────────────────────────────────────

export interface AutoBotDetailProps {
  workspace: string;
  autoBotId: string;
  onSelectWorker?: (workerId: string) => void;
  onOpenContextBot?: (context: ContextBotContext, title: string) => void;
}

export default function AutoBotDetail({
  workspace,
  autoBotId,
  onSelectWorker,
  onOpenContextBot,
}: AutoBotDetailProps) {
  const [data, setData] = useState<AutoBotDetailData | null>(null);
  const [widgets, setWidgets] = useState<DashboardWidget[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [triggering, setTriggering] = useState(false);
  const [togglingEnabled, setTogglingEnabled] = useState(false);
  const [chatInput, setChatInput] = useState("");
  const [chatPending, setChatPending] = useState(false);
  const pollRef = useRef<number | null>(null);
  const chatEndRef = useRef<HTMLDivElement>(null);

  const load = useCallback(
    async (initial = false) => {
      try {
        const d = await getAutoBot(workspace, autoBotId);
        setData(d);
        if (initial) setLoading(false);
      } catch (e) {
        if (initial) {
          setError(e instanceof Error ? e.message : "Failed to load auto bot");
          setLoading(false);
        }
      }
    },
    [workspace, autoBotId],
  );

  const loadWidgets = useCallback(() => {
    listWidgets(workspace, autoBotId)
      .then(setWidgets)
      .catch(() => {});
  }, [workspace, autoBotId]);

  useEffect(() => {
    setLoading(true);
    setError(null);
    load(true);
    loadWidgets();

    pollRef.current = window.setInterval(() => {
      load(false);
      loadWidgets();
    }, 10000);

    return () => {
      if (pollRef.current !== null) {
        window.clearInterval(pollRef.current);
        pollRef.current = null;
      }
    };
  }, [load, loadWidgets]);

  // Speed up polling while a chat run is in flight
  useEffect(() => {
    if (!chatPending || !data) return;
    const hasActiveChatRun = data.runs.some((r) => r.triggered_by === "chat" && r.outcome === null);
    if (!hasActiveChatRun) {
      setChatPending(false);
      return;
    }
    const fastPoll = window.setInterval(() => load(false), 2000);
    return () => window.clearInterval(fastPoll);
  }, [chatPending, data, load]);

  const handleTrigger = async () => {
    if (triggering || !data) return;
    setTriggering(true);
    try {
      await triggerAutoBot(workspace, autoBotId);
      await load(false);
    } catch (e) {
      console.error("trigger failed", e);
    } finally {
      setTriggering(false);
    }
  };

  const handleToggleEnabled = async (enabled: boolean) => {
    if (togglingEnabled || !data) return;
    setTogglingEnabled(true);
    setData((prev) => (prev ? { ...prev, enabled } : prev));
    try {
      const updated = await updateAutoBot(workspace, autoBotId, { enabled });
      setData((prev) => (prev ? { ...prev, ...updated } : prev));
    } catch (e) {
      setData((prev) => (prev ? { ...prev, enabled: !enabled } : prev));
      console.error("toggle enabled failed", e);
    } finally {
      setTogglingEnabled(false);
    }
  };

  const handleChat = async () => {
    const msg = chatInput.trim();
    if (!msg || chatPending) return;
    setChatInput("");
    setChatPending(true);
    try {
      await chatWithAutoBot(workspace, autoBotId, msg);
      await load(false);
      setTimeout(() => chatEndRef.current?.scrollIntoView({ behavior: "smooth" }), 100);
    } catch (e) {
      console.error("chat failed", e);
      setChatPending(false);
    }
  };

  const handleChatKey = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleChat();
    }
  };

  if (loading) {
    return (
      <div className={styles.container}>
        <div className={styles.stateCenter}>
          <Spinner />
        </div>
      </div>
    );
  }

  if (error || !data) {
    return (
      <div className={styles.container}>
        <div className={styles.stateCenter}>{error ?? "Auto bot not found"}</div>
      </div>
    );
  }

  const allRuns = [...data.runs].sort(
    (a, b) => new Date(a.started_at).getTime() - new Date(b.started_at).getTime(),
  );
  const chatRuns = allRuns.filter((r) => r.triggered_by === "chat");
  const regularRuns = [...data.runs]
    .filter((r) => r.triggered_by !== "chat")
    .sort((a, b) => new Date(b.started_at).getTime() - new Date(a.started_at).getTime());

  return (
    <div className={styles.container}>
      {/* Header */}
      <div className={styles.header}>
        <div className={styles.headerTop}>
          <div className={styles.headerLeft}>
            <StatusDot status={data.status} />
            <h1
              className={`${styles.botName} ${!data.enabled ? styles.botNameDisabled : ""}`}
              data-testid="bot-name"
            >
              {data.name}
              {!data.enabled && <span className={styles.disabledLabel}> (disabled)</span>}
            </h1>
          </div>
          <div className={styles.headerActions}>
            <Toggle
              enabled={data.enabled}
              onChange={handleToggleEnabled}
              disabled={togglingEnabled}
            />
            {onOpenContextBot && (
              <Button
                variant="secondary"
                size="sm"
                type="button"
                onClick={() => {
                  onOpenContextBot(
                    {
                      view: "auto_bot_detail",
                      entity_id: data.id,
                      entity_snapshot: {
                        name: data.name,
                        status: data.status,
                        enabled: data.enabled,
                        trigger_type: data.trigger_type,
                        cron_schedule: data.cron_schedule,
                        signal_source: data.signal_source,
                      },
                    },
                    `Viewing: ${data.name}`,
                  );
                }}
                data-testid="ask-btn"
              >
                <MessageSquare size={13} />
                Ask
              </Button>
            )}
            <Button
              variant="secondary"
              size="sm"
              onClick={handleTrigger}
              disabled={triggering}
              type="button"
              data-testid="trigger-btn"
            >
              <Zap size={13} />
              {triggering ? "Triggering..." : "Trigger Now"}
            </Button>
          </div>
        </div>

        <TriggerLine bot={data} />
      </div>

      <div className={styles.body}>
        {/* Widgets panel */}
        {widgets.length > 0 && (
          <div className={styles.widgetsSection}>
            <div className={styles.widgetsGrid}>
              {widgets.map((w) => (
                <Widget key={w.slot} widget={w} />
              ))}
            </div>
          </div>
        )}

        {/* Chat */}
        <div className={styles.chatSection}>
          <div className={styles.chatHistory} data-testid="chat-history">
            {chatRuns.length === 0 && (
              <div className={styles.chatEmpty}>Ask this bot anything about its last run.</div>
            )}
            {chatRuns.map((run) => (
              <ChatBubble key={run.id} run={run} onSelectWorker={onSelectWorker} />
            ))}
            <div ref={chatEndRef} />
          </div>
          <div className={styles.chatInputRow}>
            <textarea
              className={styles.chatTextarea}
              placeholder="Ask a question…"
              value={chatInput}
              onChange={(e) => setChatInput(e.target.value)}
              onKeyDown={handleChatKey}
              rows={1}
              enterKeyHint="send"
              disabled={chatPending}
            />
            <button
              className={styles.chatSendBtn}
              onClick={handleChat}
              disabled={chatPending || !chatInput.trim()}
              type="button"
              aria-label="Send"
              onMouseDown={(e) => e.preventDefault()}
            >
              {chatPending ? <Spinner size="sm" /> : <Send size={14} />}
            </button>
          </div>
        </div>

        {/* Run history */}
        <div className={styles.historySection}>
          <div className={styles.historySectionLabel}>Run History</div>
          {regularRuns.length > 0 ? (
            <div className={styles.feed} data-testid="run-feed">
              {regularRuns.map((run) => (
                <RunCard key={run.id} run={run} onSelectWorker={onSelectWorker} />
              ))}
            </div>
          ) : (
            <div className={styles.emptyState} data-testid="empty-state">
              No runs yet. This bot hasn't fired.
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
