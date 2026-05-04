import { Suspense, lazy } from "react";
import { PageHeader } from "../primitives/PageHeader";
import { ModeScaffold } from "../primitives/ModeScaffold";

const DocsPanel = lazy(() =>
  import("../components/DocsPanel").then((module) => ({ default: module.DocsPanel })),
);

function PanelFallback() {
  return <div style={{ color: "var(--text-faint)" }}>Loading docs…</div>;
}

interface Props {
  workspace: string;
  remote?: string;
  docName: string | null;
  onSelectedDocNameChange: (name: string | null) => void;
  openListByDefaultOnMobile?: boolean;
}

export function DocsMode({ workspace, remote, docName, onSelectedDocNameChange, openListByDefaultOnMobile = false }: Props) {
  return (
    <ModeScaffold
      hideHeaderOnMobile
      header={(
        <PageHeader
          eyebrow="Workspace reference"
          title="Docs"
          summary="Browse and edit durable workspace documentation without losing your place when you move between tools."
        />
      )}
    >
      <Suspense fallback={<PanelFallback />}>
        <DocsPanel
          workspace={workspace}
          remote={remote}
          initialSelectedDocName={docName}
          onSelectedDocNameChange={onSelectedDocNameChange}
          openListByDefaultOnMobile={openListByDefaultOnMobile}
        />
      </Suspense>
    </ModeScaffold>
  );
}
