import { Suspense, lazy } from "react";
import { TopBar } from "./components/TopBar";
import { CommandPalette } from "./components/CommandPalette";
import { WorkspaceNav } from "./components/WorkspaceNav";
import { ChatPanel } from "./components/ChatPanel";
import { ReposPanel } from "./components/ReposPanel";
import { WorkersPanel } from "./components/WorkersPanel";
import { OverviewPanel } from "./components/OverviewPanel";
import { useWorkspaceConsoleState } from "./useWorkspaceConsoleState";

const WorkerDetail = lazy(() =>
  import("./components/WorkerDetail").then((module) => ({ default: module.WorkerDetail })),
);
const DocsPanel = lazy(() =>
  import("./components/DocsPanel").then((module) => ({ default: module.DocsPanel })),
);
const SimulatorPanel = lazy(() =>
  import("./components/SimulatorPanel").then((module) => ({ default: module.SimulatorPanel })),
);

function PanelFallback() {
  return (
    <div
      style={{
        flex: 1,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        color: "var(--text-faint)",
      }}
    >
      Loading panel...
    </div>
  );
}

export default function App() {
  const state = useWorkspaceConsoleState();

  let mainContent;
  if (state.mode === "docs") {
    mainContent = (
      <Suspense fallback={<PanelFallback />}>
        <DocsPanel workspace={state.workspace} remote={state.remote} />
      </Suspense>
    );
  } else if (state.mode === "workers") {
    mainContent = state.workerId && state.selectedWorker ? (
      <Suspense fallback={<PanelFallback />}>
        <WorkerDetail
          worker={state.selectedWorker}
          detail={state.workerDetail}
          workspace={state.workspace}
          remote={state.remote}
          onBack={state.handleBackFromWorker}
        />
      </Suspense>
    ) : (
      <div style={{ flex: 1, display: "flex", overflow: "hidden" }}>
        <WorkersPanel workers={state.workers} onSelectWorker={state.handleSelectWorker} />
        <div
          style={{
            flex: 1,
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            color: "var(--text-faint)",
          }}
        >
          Select a worker to inspect its live state.
        </div>
      </div>
    );
  } else if (state.mode === "repos") {
    mainContent = (
      <ReposPanel
        repos={state.reposWithFreshWorkers}
        researchTasks={state.researchTasks}
        onSelectWorker={state.handleSelectWorker}
      />
    );
  } else if (state.mode === "chat") {
    mainContent = state.bot ? (
      <ChatPanel
        bot={state.bot}
        botDescription={state.selectedBot?.description}
        botProvider={state.selectedBot?.provider}
        botModel={state.selectedBot?.model}
        messages={state.messages}
        messagesLoading={state.messagesLoading}
        loading={state.loading}
        loadingStatus={state.loadingStatus}
        streamingContent={state.streamingContent}
        onSend={state.handleSend}
        workerCount={state.workers.length}
        onWorkersToggle={() => state.handleSelectMode("workers")}
        onCancel={state.cancelActiveBot}
        ttsVoice={state.ttsVoice}
        ttsSpeed={state.ttsSpeed}
        followups={state.followups.filter((followup) => followup.bot === state.bot)}
        workspace={state.workspace}
        onFollowupCancelled={state.refreshFollowups}
      />
    ) : (
      <div
        style={{
          flex: 1,
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          flexDirection: "column",
          gap: 8,
        }}
      >
        <div style={{ fontSize: 14, color: "var(--text-faint)" }}>
          Select a bot to start chatting
        </div>
      </div>
    );
  } else {
    mainContent = (
      <OverviewPanel
        workspace={state.workspace}
        bots={state.bots}
        workers={state.workers}
        repos={state.reposWithFreshWorkers}
        followups={state.followups}
        researchTasks={state.researchTasks}
        unread={state.unread}
        onSelectBot={state.handleSelectBot}
        onSelectWorker={state.handleSelectWorker}
        onOpenMode={state.handleSelectMode}
      />
    );
  }

  return (
    <>
      <TopBar
        workspaces={state.workspaces}
        active={state.workspace}
        activeRemote={state.remote}
        onSelect={state.handleSelectWorkspace}
        onMenuToggle={() => state.setMenuOpen((value) => !value)}
        onOpenPalette={() => state.setPaletteOpen(true)}
        onToggleSimulator={() => state.setSimulatorOpen((value) => !value)}
        usage={state.usage}
      />
      <div style={{ flex: 1, display: "flex", overflow: "hidden", position: "relative" }}>
        {state.menuOpen && (
          <div className="drawer-backdrop" onClick={() => state.setMenuOpen(false)} />
        )}
        <WorkspaceNav
          activeMode={state.mode}
          onSelectMode={state.handleSelectMode}
          bots={state.bots}
          activeBot={state.mode === "chat" ? state.bot : null}
          onSelectBot={state.handleSelectBot}
          workerCount={state.workers.length}
          repoCount={state.repos.length}
          pendingFollowupCount={state.pendingFollowupCount}
          mobileOpen={state.menuOpen}
          unread={state.unread}
        />
        <div style={{ flex: 1, display: "flex", overflow: "hidden" }}>
          {mainContent}
          {state.mode === "chat" && (
            <ReposPanel
              repos={state.reposWithFreshWorkers}
              researchTasks={state.researchTasks}
              onSelectWorker={state.handleSelectWorker}
            />
          )}
        </div>
      </div>
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
      />
    </>
  );
}
