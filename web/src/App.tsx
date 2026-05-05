import { AppShell } from "./shell/AppShell";
import { CommandPalette } from "./components/CommandPalette";
import { ReposPanel } from "./components/ReposPanel";
import { WorkspaceLayoutDialog } from "./components/WorkspaceLayoutDialog";
import { useWorkspaceConsoleState } from "./useWorkspaceConsoleState";
import styles from "./App.module.css";
import { ChatMode } from "./modes/ChatMode";
import { DocsMode } from "./modes/DocsMode";
import { DiagnosticsMode } from "./modes/DiagnosticsMode";
import { OverviewMode } from "./modes/OverviewMode";
import { ReposMode } from "./modes/ReposMode";
import { SignalsMode } from "./modes/SignalsMode";
import { TasksMode } from "./modes/TasksMode";
import { WorkersMode } from "./modes/WorkersMode";
import { Suspense, lazy } from "react";
import { ChatLanding } from "./components/ChatLanding";

const SimulatorPanel = lazy(() =>
  import("./components/SimulatorPanel").then((module) => ({ default: module.SimulatorPanel })),
);

export default function App() {
  const state = useWorkspaceConsoleState();
  const mobileBadgeCounts = {
    overview: state.pendingFollowupCount || null,
    tasks: state.tasks.filter((task) => task.stage !== "Merged" && task.stage !== "Dismissed").length || null,
    workers: state.workers.length || null,
    repos: state.repos.length || null,
  };
  const showChatRepoRail = !state.isTablet && state.consoleProfile.showChatRepoRail;
  const showMobileModeBar =
    state.isMobile
    && !state.workerId
    && !state.menuOpen
    && !state.paletteOpen
    && !state.layoutDialogOpen
    && !state.simulatorOpen;

  let mainContent;
  if (state.mode === "docs") {
    mainContent = (
      <DocsMode
        workspace={state.workspace}
        remote={state.remote}
        docName={state.docName}
        onSelectedDocNameChange={state.setDocName}
        openListByDefaultOnMobile={state.isMobile}
      />
    );
  } else if (state.mode === "signals") {
    mainContent = (
      <SignalsMode
        workspace={state.workspace}
        remote={state.remote}
      />
    );
  } else if (state.mode === "diagnostics") {
    mainContent = (
      <DiagnosticsMode
        workspace={state.workspace}
        remote={state.remote}
        bot={state.bot}
      />
    );
  } else if (state.mode === "workers") {
    mainContent = (
      <WorkersMode
        workspace={state.workspace}
        remote={state.remote}
        workers={state.workers}
        workerEnvironment={state.workerEnvironment}
        workerId={state.workerId}
        selectedWorker={state.selectedWorker}
        workerDetail={state.workerDetail}
        isMobile={state.isMobile}
        onSelectWorker={state.handleSelectWorker}
        onBackFromWorker={state.handleBackFromWorker}
        onPromoteWorker={state.handlePromoteWorker}
        onRedispatchWorker={state.handleRedispatchWorker}
        onCloseWorker={state.handleCloseWorker}
      />
    );
  } else if (state.mode === "tasks") {
    mainContent = (
      <TasksMode
        tasks={state.tasks}
        workers={state.workers}
        onSelectWorker={state.handleSelectWorker}
      />
    );
  } else if (state.mode === "repos") {
    mainContent = (
      <ReposMode
        repos={state.reposWithFreshWorkers}
        researchTasks={state.researchTasks}
        onSelectWorker={state.handleSelectWorker}
      />
    );
  } else if (state.mode === "chat") {
    const chatSurface = state.bot ? (
      <ChatMode
        workspace={state.workspace}
        bot={state.bot}
        botDescription={state.selectedBot?.description}
        botProvider={state.selectedBot?.provider}
        botModel={state.selectedBot?.model}
        messages={state.messages}
        messagesLoading={state.messagesLoading}
        loading={state.loading}
        loadingStatus={state.loadingStatus}
        streamingContent={state.streamingContent}
        hasOlderHistory={state.hasOlderHistory}
        loadingOlderHistory={state.loadingOlderHistory}
        onLoadOlderHistory={state.loadOlderHistory}
        onSend={state.handleSend}
        workerCount={state.workers.length}
        onWorkersToggle={() => state.handleSelectMode("workers")}
        onCancel={state.cancelActiveBot}
        ttsVoice={state.ttsVoice}
        ttsSpeed={state.ttsSpeed}
        followups={state.followups.filter((followup) => followup.bot === state.bot)}
        onFollowupCancelled={state.refreshFollowups}
        bots={state.bots}
        unread={state.unread}
        onSelectBot={state.handleSelectBot}
      />
    ) : (
      <ChatLanding
        workspace={state.workspace}
        remote={state.remote}
        bots={state.bots}
        unread={state.unread}
        onSelectBot={state.handleSelectBot}
      />
    );

    mainContent = showChatRepoRail ? (
      <div className={styles.chatWorkspaceLayout}>
        <div className={styles.chatMain}>{chatSurface}</div>
        <div className={styles.chatRail}>
          <ReposPanel
            repos={state.reposWithFreshWorkers}
            researchTasks={state.researchTasks}
            onSelectWorker={state.handleSelectWorker}
          />
        </div>
      </div>
    ) : chatSurface;
  } else {
    mainContent = (
      <OverviewMode
        workspace={state.workspace}
        remote={state.remote}
        bots={state.bots}
        workers={state.workers}
        repos={state.reposWithFreshWorkers}
        followups={state.followups}
        researchTasks={state.researchTasks}
        unread={state.unread}
        primaryBot={state.consoleProfile.overviewPrimaryBot}
        onSelectBot={state.handleSelectBot}
        onSelectWorker={state.handleSelectWorker}
        onOpenMode={state.handleSelectMode}
      />
    );
  }

  return (
    <>
      <AppShell
        workspaces={state.workspaces}
        activeWorkspace={state.workspace}
        activeRemote={state.remote}
        onSelectWorkspace={state.handleSelectWorkspace}
        onMenuToggle={() => state.setMenuOpen((value) => !value)}
        onOpenPalette={() => state.setPaletteOpen(true)}
        onToggleSimulator={() => state.setSimulatorOpen((value) => !value)}
        usage={state.usage}
        isMobile={state.isMobile}
        isTablet={state.isTablet}
        menuOpen={state.menuOpen}
        onCloseMenu={() => state.setMenuOpen(false)}
        visibleModes={state.visibleModes}
        activeMode={state.mode}
        onSelectMode={state.handleSelectMode}
        taskCount={state.tasks.filter((task) => task.stage !== "Merged" && task.stage !== "Dismissed").length}
        workerCount={state.workers.length}
        repoCount={state.repos.length}
        pendingFollowupCount={state.pendingFollowupCount}
        showMobileModeBar={showMobileModeBar}
        mobileBadgeCounts={mobileBadgeCounts}
      >
          {mainContent}
      </AppShell>
      <Suspense fallback={null}>
        <SimulatorPanel
          open={state.simulatorOpen}
          onClose={() => state.setSimulatorOpen(false)}
        />
      </Suspense>
      <CommandPalette
        open={state.paletteOpen}
        onOpenChange={state.setPaletteOpen}
        workspaces={state.workspaces}
        bots={state.bots}
        workers={state.workers}
        currentWorkspace={state.workspace}
        currentRemote={state.remote}
        currentBot={state.bot}
        onSelectWorkspace={state.handleSelectWorkspace}
        onSelectBot={state.handleSelectBot}
        onSelectWorker={state.handleSelectWorker}
        otherWorkspaceBots={state.otherWorkspaceBots}
        onSelectWorkspaceBot={state.handleSelectWorkspaceBot}
        unread={state.unread}
        otherWorkspaceUnreads={state.otherWorkspaceUnreads}
        onStartResearch={() => state.handleStartResearch()}
        onOpenWorkspaceLayout={() => state.setLayoutDialogOpen(true)}
        onOpenSignals={() => state.handleSelectMode("signals")}
        onOpenDiagnostics={() => state.handleSelectMode("diagnostics")}
      />
      <WorkspaceLayoutDialog
        open={state.layoutDialogOpen}
        workspace={state.workspace}
        remote={state.remote}
        bots={state.bots}
        profile={state.consoleProfile}
        onClose={() => state.setLayoutDialogOpen(false)}
        onProfileSaved={state.applyConsoleProfile}
      />
    </>
  );
}
