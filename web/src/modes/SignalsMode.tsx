import { useEffect, useMemo, useState } from "react";
import { ExternalLink, Radar } from "lucide-react";
import * as api from "../api";
import type { Signal } from "../types";
import { ModeScaffold } from "../primitives/ModeScaffold";
import { PageHeader } from "../primitives/PageHeader";
import { StatusBadge } from "../primitives/StatusBadge";
import { EmptyState } from "../primitives/EmptyState";
import styles from "./SignalsMode.module.css";

interface Props {
  workspace: string;
  remote?: string;
}

function toneForSeverity(severity: string): "accent" | "success" | "neutral" | "danger" {
  const normalized = severity.toLowerCase();
  if (normalized === "critical" || normalized === "error") return "danger";
  if (normalized === "warning") return "accent";
  if (normalized === "info") return "neutral";
  return "neutral";
}

function formatSignalTime(value: string) {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString([], {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
}

function secondaryTimestamp(signal: Signal) {
  return signal.resolved_at ?? signal.updated_at ?? signal.created_at;
}

export function SignalsMode({ workspace, remote }: Props) {
  const [signals, setSignals] = useState<Signal[]>([]);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);

  useEffect(() => {
    let cancelled = false;

    async function load() {
      setLoading(true);
      try {
        const nextSignals = remote ? [] : await api.getSignals(workspace, { history: true, limit: 100 });
        if (!cancelled) {
          setSignals(nextSignals);
        }
      } finally {
        if (!cancelled) {
          setLoading(false);
        }
      }
    }

    void load();
    return () => {
      cancelled = true;
    };
  }, [workspace, remote]);

  async function refreshSignals() {
    if (remote) return;
    setRefreshing(true);
    try {
      setSignals(await api.getSignals(workspace, { history: true, limit: 100 }));
    } finally {
      setRefreshing(false);
    }
  }

  const counts = useMemo(() => {
    const critical = signals.filter((signal) => ["critical", "error"].includes(signal.severity.toLowerCase())).length;
    const warnings = signals.filter((signal) => signal.severity.toLowerCase() === "warning").length;
    return { critical, warnings };
  }, [signals]);

  return (
    <ModeScaffold
      scrollBody
      header={(
        <PageHeader
          eyebrow="Debug surface"
          title="Signals"
          summary="Inspect the watcher-fed signal queue directly. Useful for validating whether bots are actually getting signal input."
          meta={(
            <div className={styles.meta}>
              <StatusBadge tone={counts.critical > 0 ? "danger" : "neutral"}>{counts.critical} critical/error</StatusBadge>
              <StatusBadge tone={counts.warnings > 0 ? "accent" : "neutral"}>{counts.warnings} warnings</StatusBadge>
              <StatusBadge tone="neutral">{signals.length} open signals</StatusBadge>
            </div>
          )}
          actions={remote ? [] : [
            {
              label: refreshing ? "Refreshing..." : "Refresh",
              onClick: () => {
                void refreshSignals();
              },
              kind: "secondary",
            },
          ]}
        />
      )}
    >
      <div className={styles.page}>
        {remote ? (
          <EmptyState
            title="Signals debug is local-only for now"
            body="The web debug page uses the local /api/signals endpoint. Remote workspace signal browsing is not wired yet."
          />
        ) : loading ? (
          <EmptyState
            title="Loading signals..."
            body="Checking the signal store for recent watcher activity."
          />
        ) : signals.length === 0 ? (
          <EmptyState
            title="No recent signals"
            body="No recent signal history was found. This usually means the watcher pipeline is quiet, misconfigured, or not writing into the signal store."
          />
        ) : (
          <div className={styles.list}>
            {signals.map((signal) => (
              <article key={signal.id} className={styles.card}>
                <div className={styles.cardTop}>
                  <div className={styles.titleWrap}>
                    <div className={styles.sourceRow}>
                      <Radar size={14} className={styles.sourceIcon} />
                      <span className={styles.source}>{signal.source}</span>
                    </div>
                    <h3 className={styles.title}>{signal.title}</h3>
                  </div>
                  <div className={styles.rightMeta}>
                    <StatusBadge tone={toneForSeverity(signal.severity)}>{signal.severity}</StatusBadge>
                    <span className={styles.time}>{formatSignalTime(secondaryTimestamp(signal))}</span>
                  </div>
                </div>
                <div className={styles.footer}>
                  <span className={styles.status}>Status: {signal.status}</span>
                  {signal.url ? (
                    <a href={signal.url} target="_blank" rel="noreferrer" className={styles.link}>
                      Open source
                      <ExternalLink size={13} />
                    </a>
                  ) : null}
                </div>
              </article>
            ))}
          </div>
        )}
      </div>
    </ModeScaffold>
  );
}
