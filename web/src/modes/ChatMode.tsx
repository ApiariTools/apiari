import { ChatPanel } from "@apiari/chat";
import { PageHeader, ModeScaffold, StatusBadge } from "@apiari/ui";
import type { Bot, Followup, Message, DashboardWidget } from "@apiari/types";
import type { Attachment } from "@apiari/chat";
import Widget from "../components/widgets/Widget";
import styles from "./ChatMode.module.css";

interface Props {
  workspace: string;
  bot: string;
  botDescription?: string;
  botProvider?: string;
  botModel?: string;
  messages: Message[];
  messagesLoading: boolean;
  loading: boolean;
  loadingStatus?: string;
  streamingContent?: string;
  hasOlderHistory: boolean;
  loadingOlderHistory: boolean;
  onLoadOlderHistory: () => Promise<void>;
  workerCount: number;
  onWorkersToggle: () => void;
  onCancel?: () => void;
  onSend: (text: string, attachments?: Attachment[]) => void;
  ttsVoice?: string;
  ttsSpeed?: number;
  followups: Followup[];
  onFollowupCancelled: () => void;
  bots: Bot[];
  unread: Record<string, number>;
  onSelectBot: (name: string) => void;
}

export function ChatMode(props: Props) {
  const pendingFollowups = props.followups.filter(
    (followup) => followup.status === "pending",
  ).length;
  const providerLabel = [props.botProvider, props.botModel].filter(Boolean).join(" / ");
  return (
    <ModeScaffold
      hideHeaderOnMobile
      header={
        <PageHeader
          eyebrow="Conversation"
          title={props.bot}
          summary={
            props.botDescription ||
            "Continue the active bot conversation and keep follow-through local to this tool."
          }
          meta={
            <div className={styles.modeMeta}>
              {providerLabel ? <span className={styles.providerChip}>{providerLabel}</span> : null}
              {pendingFollowups > 0 ? (
                <StatusBadge tone="accent">{pendingFollowups} pending followups</StatusBadge>
              ) : null}
            </div>
          }
          actions={
            props.onWorkersToggle
              ? [
                  {
                    label: props.workerCount ? `Workers (${props.workerCount})` : "Workers",
                    onClick: props.onWorkersToggle,
                    kind: "secondary",
                  },
                ]
              : []
          }
        />
      }
    >
      <div className={styles.page}>
        <ChatPanel
          bot={props.bot}
          botDescription={props.botDescription}
          botProvider={props.botProvider}
          botModel={props.botModel}
          messages={props.messages}
          messagesLoading={props.messagesLoading}
          loading={props.loading}
          loadingStatus={props.loadingStatus}
          streamingContent={props.streamingContent}
          hasOlderHistory={props.hasOlderHistory}
          loadingOlderHistory={props.loadingOlderHistory}
          onLoadOlderHistory={props.onLoadOlderHistory}
          onSend={props.onSend}
          workerCount={props.workerCount}
          onWorkersToggle={props.onWorkersToggle}
          onCancel={props.onCancel}
          ttsVoice={props.ttsVoice}
          ttsSpeed={props.ttsSpeed}
          followups={props.followups}
          workspace={props.workspace}
          onFollowupCancelled={props.onFollowupCancelled}
          bots={props.bots}
          unread={props.unread}
          onSelectBot={props.onSelectBot}
          compactHeader
          renderWidgets={(widgets) =>
            (widgets as DashboardWidget[]).map((w, i) => <Widget key={w.slot ?? i} widget={w} />)
          }
        />
      </div>
    </ModeScaffold>
  );
}
