import { useEffect, useState } from "react";
import * as api from "@apiari/api";
import type { BotDebugData, BotTurnDecision, ProviderCapability } from "@apiari/types";
import { EmptyState, ModeScaffold, PageHeader, StatusBadge } from "@apiari/ui";
import styles from "./DiagnosticsMode.module.css";

interface Props {
  workspace: string;
  bot: string;
  remote?: string;
}

function formatTime(value: string) {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString([], {
    month: "short",
    day: "numeric",
    hour: "numeric",
    minute: "2-digit",
  });
}

function capabilityTone(capability: ProviderCapability) {
  if (!capability.installed) return "danger" as const;
  if (capability.approval_flag_supported === false) return "accent" as const;
  return "neutral" as const;
}

function decisionTone(decision: BotTurnDecision) {
  if (decision.decision_type === "dispatch_matched") return "success" as const;
  if (decision.decision_type === "dispatch_failed") return "danger" as const;
  if (decision.decision_type.includes("repo_ambiguous")) return "accent" as const;
  return "neutral" as const;
}

export function DiagnosticsMode({ workspace, bot, remote }: Props) {
  const [debugData, setDebugData] = useState<BotDebugData | null>(null);
  const [capabilities, setCapabilities] = useState<ProviderCapability[]>([]);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);

  useEffect(() => {
    let cancelled = false;

    async function load() {
      setLoading(true);
      try {
        const [nextDebugData, nextCapabilities] = await Promise.all([
          api.getBotDebugData(workspace, bot, 20, remote),
          remote ? Promise.resolve([]) : api.getProviderCapabilities(),
        ]);
        if (!cancelled) {
          setDebugData(nextDebugData);
          setCapabilities(nextCapabilities);
        }
      } finally {
        if (!cancelled) {
          setLoading(false);
        }
      }
    }

    if (bot) {
      void load();
    }

    return () => {
      cancelled = true;
    };
  }, [workspace, bot, remote]);

  async function refresh() {
    setRefreshing(true);
    try {
      const [nextDebugData, nextCapabilities] = await Promise.all([
        api.getBotDebugData(workspace, bot, 20, remote),
        remote ? Promise.resolve([]) : api.getProviderCapabilities(),
      ]);
      setDebugData(nextDebugData);
      setCapabilities(nextCapabilities);
    } finally {
      setRefreshing(false);
    }
  }

  const selectedCapability = capabilities.find((entry) => entry.name === debugData?.provider);

  return (
    <ModeScaffold
      scrollBody
      header={(
        <PageHeader
          eyebrow="Debug surface"
          title={bot ? `${bot} diagnostics` : "Bot diagnostics"}
          summary="Inspect provider capability mismatches, recent coordinator failures, and the exact recent bot transcript."
          meta={debugData?.status ? (
            <div className={styles.meta}>
              <StatusBadge tone={debugData.status.status === "idle" ? "neutral" : "accent"}>
                status: {debugData.status.status}
              </StatusBadge>
              {debugData.provider ? (
                <StatusBadge tone="neutral">
                  provider: {debugData.provider}
                </StatusBadge>
              ) : null}
            </div>
          ) : undefined}
          actions={bot ? [
            {
              label: refreshing ? "Refreshing..." : "Refresh",
              onClick: () => {
                void refresh();
              },
              kind: "secondary",
            },
          ] : []}
        />
      )}
    >
      <div className={styles.page}>
        {!bot ? (
          <EmptyState
            title="No bot selected"
            body="Open diagnostics after choosing a bot. The page is scoped to one bot at a time."
          />
        ) : loading ? (
          <EmptyState
            title="Loading diagnostics..."
            body="Fetching provider capabilities, recent failures, and recent messages."
          />
        ) : !debugData ? (
          <EmptyState
            title="No diagnostics available"
            body="The daemon did not return any debug data for this bot."
          />
        ) : (
          <>
            <section className={styles.section}>
              <div className={styles.sectionHeader}>
                <h2>Effective bot config</h2>
              </div>
              {!debugData.effective_config ? (
                <EmptyState
                  title="No effective config available"
                  body="The daemon could not resolve this bot to a loaded bee config."
                />
              ) : (
                <article className={styles.card}>
                  <div className={styles.cardTop}>
                    <strong>{debugData.effective_config.api_name}</strong>
                    <StatusBadge tone="neutral">
                      effective policy: {debugData.effective_config.effective_execution_policy}
                    </StatusBadge>
                  </div>
                  <div className={styles.cardBody}>
                    <div className={styles.inlineMeta}>
                      <span>resolved bee: {debugData.effective_config.resolved_bee_name}</span>
                      <span>workspace authority: {debugData.effective_config.workspace_authority}</span>
                    </div>
                    <div className={styles.inlineMeta}>
                      <span>provider: {debugData.effective_config.provider}</span>
                      <span>model: {debugData.effective_config.model}</span>
                    </div>
                    <div className={styles.inlineMeta}>
                      <span>configured policy: {debugData.effective_config.configured_execution_policy}</span>
                      <span>max turns: {debugData.effective_config.max_turns}</span>
                      <span>max session turns: {debugData.effective_config.max_session_turns}</span>
                    </div>
                    {debugData.effective_config.heartbeat ? (
                      <div className={styles.inlineMeta}>
                        <span>heartbeat: {debugData.effective_config.heartbeat}</span>
                      </div>
                    ) : null}
                    {debugData.effective_config.role || debugData.effective_config.color ? (
                      <div className={styles.inlineMeta}>
                        {debugData.effective_config.role ? <span>role: {debugData.effective_config.role}</span> : null}
                        {debugData.effective_config.color ? <span>color: {debugData.effective_config.color}</span> : null}
                      </div>
                    ) : null}
                    <div className={styles.inlineMeta}>
                      <span>
                        signal hooks: {debugData.effective_config.signal_sources.length > 0
                          ? debugData.effective_config.signal_sources.join(", ")
                          : "none"}
                      </span>
                    </div>
                  </div>
                </article>
              )}
            </section>

            <section className={styles.section}>
              <div className={styles.sectionHeader}>
                <h2>Provider capabilities</h2>
              </div>
              {remote ? (
                <EmptyState
                  title="Provider probes are local-only"
                  body="Remote diagnostics are not wired yet. This section only inspects local provider CLIs."
                />
              ) : (
                <div className={styles.cardList}>
                  {capabilities.map((capability) => (
                    <article key={capability.name} className={styles.card}>
                      <div className={styles.cardTop}>
                        <strong>{capability.name}</strong>
                        <StatusBadge tone={capabilityTone(capability)}>
                          {capability.installed ? "installed" : "missing"}
                        </StatusBadge>
                      </div>
                      <div className={styles.cardBody}>
                        {capability.binary_path ? (
                          <div className={styles.path}>{capability.binary_path}</div>
                        ) : null}
                        {capability.sandbox_flag_supported != null ? (
                          <div className={styles.inlineMeta}>
                            <span>sandbox: {capability.sandbox_flag_supported ? "yes" : "no"}</span>
                            <span>approval policy: {capability.approval_flag_supported ? "yes" : "no"}</span>
                          </div>
                        ) : null}
                        {capability.notes.length > 0 ? (
                          <ul className={styles.notes}>
                            {capability.notes.map((note) => (
                              <li key={note}>{note}</li>
                            ))}
                          </ul>
                        ) : null}
                      </div>
                    </article>
                  ))}
                </div>
              )}
            </section>

            <section className={styles.section}>
              <div className={styles.sectionHeader}>
                <h2>Recent routing decisions</h2>
              </div>
              {debugData.recent_decisions.length === 0 ? (
                <EmptyState
                  title="No recent routing decisions"
                  body="This bot has not recorded any recent dispatch matched/skipped decisions yet."
                />
              ) : (
                <div className={styles.cardList}>
                  {debugData.recent_decisions.map((decision) => (
                    <article key={decision.id} className={styles.card}>
                      <div className={styles.cardTop}>
                        <strong>{decision.decision_type}</strong>
                        <StatusBadge tone={decisionTone(decision)}>
                          {decision.decision_type}
                        </StatusBadge>
                      </div>
                      <div className={styles.cardBody}>
                        <div className={styles.inlineMeta}>
                          {decision.provider ? <span>provider: {decision.provider}</span> : null}
                          <span>{formatTime(decision.created_at)}</span>
                        </div>
                        <pre className={styles.messageBlock}>{decision.detail}</pre>
                      </div>
                    </article>
                  ))}
                </div>
              )}
            </section>

            <section className={styles.section}>
              <div className={styles.sectionHeader}>
                <h2>Recent turn failures</h2>
              </div>
              {debugData.recent_failures.length === 0 ? (
                <EmptyState
                  title="No recent failures logged"
                  body="This bot has not recorded any recent prepare-dispatch, runtime, or empty-response failures."
                />
              ) : (
                <div className={styles.cardList}>
                  {debugData.recent_failures.map((failure) => (
                    <article key={failure.id} className={styles.card}>
                      <div className={styles.cardTop}>
                        <strong>{failure.source}</strong>
                        <span className={styles.time}>{formatTime(failure.created_at)}</span>
                      </div>
                      <div className={styles.cardBody}>
                        {failure.provider ? (
                          <div className={styles.inlineMeta}>
                            <span>provider: {failure.provider}</span>
                          </div>
                        ) : null}
                        <pre className={styles.errorBlock}>{failure.error_text}</pre>
                      </div>
                    </article>
                  ))}
                </div>
              )}
            </section>

            <section className={styles.section}>
              <div className={styles.sectionHeader}>
                <h2>Recent bot transcript</h2>
                {selectedCapability?.approval_flag_supported === false ? (
                  <StatusBadge tone="accent">capability mismatch detected</StatusBadge>
                ) : null}
              </div>
              <div className={styles.cardList}>
                {debugData.recent_messages.map((message) => (
                  <article key={message.id} className={styles.card}>
                    <div className={styles.cardTop}>
                      <strong>{message.role}</strong>
                      <span className={styles.time}>{formatTime(message.created_at)}</span>
                    </div>
                    <div className={styles.cardBody}>
                      <pre className={styles.messageBlock}>{message.content}</pre>
                    </div>
                  </article>
                ))}
              </div>
            </section>
          </>
        )}
      </div>
    </ModeScaffold>
  );
}
