import { useEffect, useRef, useState } from "react";
import { X, Minus, ChevronLeft, MessageCircle } from "lucide-react";
import type { Bot } from "@apiari/types";
import { ChatPanel } from "../ChatPanel";
import type { ChatTheme } from "./chatTheme";
import { buildThemeVars } from "./chatTheme";
import { useChatState } from "./useChatState";
import styles from "./ChatLauncher.module.css";

export interface ChatLauncherProps {
  workspace: string;
  /** Which side of the screen the launcher anchors to. Default: "right" */
  position?: "right" | "left";
  /** Max expanded windows shown on desktop before additional ones go minimized. Default: 3 */
  maxWindows?: number;
  /** Theme overrides — any omitted key falls back to the default dark theme */
  theme?: ChatTheme;
  /** Replace the default launcher button */
  renderLauncherButton?: (props: {
    onClick: () => void;
    unreadCount: number;
    openCount: number;
    isOpen: boolean;
  }) => React.ReactNode;
  /** Replace a bot list item in the popover */
  renderBotItem?: (props: {
    bot: Bot;
    unreadCount: number;
    onClick: () => void;
  }) => React.ReactNode;
  /** Replace the header bar of a desktop chat window */
  renderWindowHeader?: (props: {
    bot: Bot;
    onMinimize: () => void;
    onClose: () => void;
  }) => React.ReactNode;
}

function useIsMobile() {
  const [mobile, setMobile] = useState(() =>
    typeof window !== "undefined" ? window.matchMedia("(max-width: 768px)").matches : false,
  );
  useEffect(() => {
    const mq = window.matchMedia("(max-width: 768px)");
    const handler = (e: MediaQueryListEvent) => setMobile(e.matches);
    mq.addEventListener("change", handler);
    return () => mq.removeEventListener("change", handler);
  }, []);
  return mobile;
}

export function ChatLauncher({
  workspace,
  position = "right",
  maxWindows = 3,
  theme,
  renderLauncherButton,
  renderBotItem,
  renderWindowHeader,
}: ChatLauncherProps) {
  const isMobile = useIsMobile();
  const rootRef = useRef<HTMLDivElement>(null);
  const {
    bots,
    activeConversationCount,
    openWindows,
    botStates,
    unread,
    totalUnread,
    showBotList,
    setShowBotList,
    openBot,
    closeBot,
    minimizeBot,
    restoreBot,
    send,
    cancel,
  } = useChatState(workspace);

  // Close popover on outside click
  useEffect(() => {
    if (!showBotList) return;
    const handler = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) {
        setShowBotList(false);
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [showBotList, setShowBotList]);

  const isRight = position === "right";
  const themeVars = buildThemeVars(theme);

  const expandedWindows = openWindows.filter((w) => !w.minimized).slice(0, maxWindows);
  const minimizedWindows = [
    ...openWindows.filter((w) => w.minimized),
    ...openWindows.filter((w) => !w.minimized).slice(maxWindows),
  ];

  function getBotObj(botName: string): Bot {
    return bots.find((b) => b.name === botName) ?? { name: botName, watch: [] };
  }

  function handleLauncherClick() {
    if (isMobile) {
      // On mobile, if there's already an open window, restore it; otherwise show bot list
      const open = openWindows[0];
      if (open) {
        restoreBot(open.bot);
      } else {
        setShowBotList((v) => !v);
      }
    } else {
      setShowBotList((v) => !v);
    }
  }

  // ── Launcher button ─────────────────────────────────────────────────
  const openCount = activeConversationCount;
  const hasUnread = totalUnread > 0;
  const hasOpen = openCount > 0;
  const launcherEl = renderLauncherButton ? (
    renderLauncherButton({
      onClick: handleLauncherClick,
      unreadCount: totalUnread,
      openCount,
      isOpen: showBotList,
    })
  ) : (
    <div
      className={`${styles.launcherWrap} ${isRight ? styles.launcherRight : styles.launcherLeft}`}
    >
      {hasUnread && <span className={styles.launcherBadge}>{totalUnread > 99 ? "99+" : totalUnread}</span>}
      <button
        className={[
          styles.launcher,
          hasUnread ? styles.launcherHasUnread : "",
          hasOpen ? styles.launcherHasOpen : "",
        ]
          .filter(Boolean)
          .join(" ")}
        onClick={handleLauncherClick}
        aria-label={
          [hasOpen ? `${openCount} open` : "", hasUnread ? `${totalUnread} unread` : ""]
            .filter(Boolean)
            .join(", ") || "Open chat"
        }
      >
        {showBotList ? (
          <X size={22} />
        ) : hasOpen ? (
          <span className={styles.launcherCount}>{openCount > 99 ? "99+" : openCount}</span>
        ) : (
          <MessageCircle size={22} />
        )}
      </button>
    </div>
  );

  // ── Bot list popover ────────────────────────────────────────────────
  const popoverEl = showBotList && (
    <div className={`${styles.popover} ${isRight ? styles.popoverRight : styles.popoverLeft}`}>
      <div className={styles.popoverHeader}>Chats</div>
      {bots.map((bot) =>
        renderBotItem ? (
          renderBotItem({
            bot,
            unreadCount: unread[bot.name] ?? 0,
            onClick: () => openBot(bot.name),
          })
        ) : (
          <button key={bot.name} className={styles.botItem} onClick={() => openBot(bot.name)}>
            <span
              className={styles.botDot}
              style={{ background: (bot as { color?: string }).color ?? "var(--cl-accent)" }}
            />
            {bot.name}
            {(unread[bot.name] ?? 0) > 0 && (
              <span className={styles.botItemBadge}>{unread[bot.name]}</span>
            )}
          </button>
        ),
      )}
    </div>
  );

  // ── Mobile overlays ─────────────────────────────────────────────────
  const activeWindow = isMobile ? openWindows.find((w) => !w.minimized) : undefined;
  const activeBotState = activeWindow ? botStates[activeWindow.bot] : undefined;
  const activeBot = activeWindow ? getBotObj(activeWindow.bot) : undefined;

  const mobileContent = isMobile && (
    <>
      {activeWindow && activeBotState && activeBot ? (
        <div className={styles.mobileOverlay}>
          <div className={styles.mobileHeader}>
            <button className={styles.mobileBack} onClick={() => closeBot(activeWindow.bot)}>
              <ChevronLeft size={20} />
            </button>
            <span
              className={styles.mobileDot}
              style={{
                background: (activeBot as { color?: string }).color ?? "var(--cl-accent)",
              }}
            />
            <span className={styles.mobileTitle}>{activeBot.name}</span>
          </div>
          <div className={styles.mobileBody}>
            <ChatPanel
              bot={activeWindow.bot}
              bots={bots}
              messages={activeBotState.messages}
              messagesLoading={false}
              loading={activeBotState.loading}
              loadingStatus={activeBotState.loadingStatus}
              streamingContent={activeBotState.streamingContent}
              onSend={(text, attachments) => send(activeWindow.bot, text, attachments)}
              onCancel={() => cancel(activeWindow.bot)}
              workspace={workspace}
            />
          </div>
        </div>
      ) : showBotList ? (
        <div className={styles.mobileBotList}>
          <div className={styles.mobileBotListHeader}>
            <span className={styles.mobileBotListTitle}>Chats</span>
            <button className={styles.mobileBotListClose} onClick={() => setShowBotList(false)}>
              <X size={20} />
            </button>
          </div>
          <div className={styles.mobileBotListItems}>
            {bots.map((b) =>
              renderBotItem ? (
                renderBotItem({
                  bot: b,
                  unreadCount: unread[b.name] ?? 0,
                  onClick: () => openBot(b.name),
                })
              ) : (
                <button key={b.name} className={styles.botItem} onClick={() => openBot(b.name)}>
                  <span
                    className={styles.botDot}
                    style={{ background: (b as { color?: string }).color ?? "var(--cl-accent)" }}
                  />
                  {b.name}
                  {(unread[b.name] ?? 0) > 0 && (
                    <span className={styles.botItemBadge}>{unread[b.name]}</span>
                  )}
                </button>
              ),
            )}
          </div>
        </div>
      ) : null}
    </>
  );

  // ── Root (shared by desktop + mobile) ───────────────────────────────
  return (
    <div ref={rootRef} style={themeVars} className={styles.root}>
      {/* Expanded windows */}
      {expandedWindows.length > 0 && (
        <div
          className={`${styles.windowsRow} ${isRight ? styles.windowsRowRight : styles.windowsRowLeft}`}
        >
          {expandedWindows.map(({ bot: botName }) => {
            const botState = botStates[botName];
            const bot = getBotObj(botName);
            if (!botState) return null;
            return (
              <div key={botName} className={styles.window}>
                {renderWindowHeader ? (
                  renderWindowHeader({
                    bot,
                    onMinimize: () => minimizeBot(botName),
                    onClose: () => closeBot(botName),
                  })
                ) : (
                  <div className={styles.windowHeader}>
                    <span
                      className={styles.windowHeaderDot}
                      style={{
                        background: (bot as { color?: string }).color ?? "var(--cl-accent)",
                      }}
                    />
                    <span className={styles.windowHeaderTitle}>{bot.name}</span>
                    <button
                      className={styles.windowHeaderBtn}
                      onClick={() => minimizeBot(botName)}
                      title="Minimize"
                    >
                      <Minus size={14} />
                    </button>
                    <button
                      className={styles.windowHeaderBtn}
                      onClick={() => closeBot(botName)}
                      title="Close"
                    >
                      <X size={14} />
                    </button>
                  </div>
                )}
                <div className={styles.windowBody}>
                  <ChatPanel
                    bot={botName}
                    bots={bots}
                    messages={botState.messages}
                    messagesLoading={false}
                    loading={botState.loading}
                    loadingStatus={botState.loadingStatus}
                    streamingContent={botState.streamingContent}
                    onSend={(text, attachments) => send(botName, text, attachments)}
                    onCancel={() => cancel(botName)}
                    workspace={workspace}
                  />
                </div>
              </div>
            );
          })}
        </div>
      )}

      {/* Minimized pills */}
      {minimizedWindows.length > 0 && (
        <div className={`${styles.pillRow} ${isRight ? styles.pillRowRight : styles.pillRowLeft}`}>
          {minimizedWindows.map(({ bot: botName }) => {
            const bot = getBotObj(botName);
            const badge = unread[botName] ?? 0;
            return (
              <button key={botName} className={styles.pill} onClick={() => restoreBot(botName)}>
                <span
                  className={styles.pillDot}
                  style={{ background: (bot as { color?: string }).color ?? "var(--cl-accent)" }}
                />
                {bot.name}
                {badge > 0 && <span className={styles.pillBadge}>{badge}</span>}
                <span
                  className={styles.pillClose}
                  role="button"
                  onClick={(e) => {
                    e.stopPropagation();
                    closeBot(botName);
                  }}
                >
                  <X size={12} />
                </span>
              </button>
            );
          })}
        </div>
      )}

      {/* Mobile overlays */}
      {mobileContent}

      {/* Desktop: bot list popover */}
      {!isMobile && popoverEl}

      {/* Launcher FAB */}
      {launcherEl}
    </div>
  );
}
