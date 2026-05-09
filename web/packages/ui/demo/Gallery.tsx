import { useState } from "react";
import { FileText, Inbox, Plus, Trash2, Settings, Search } from "lucide-react";
import {
  Button,
  Input,
  Textarea,
  Select,
  Spinner,
  Dots,
  Skeleton,
  StatusBadge,
  TabBar,
  ObjectRow,
  PageHeader,
  EmptyState,
  ToolPanel,
  ModeScaffold,
  InspectorPane,
  DocumentSurface,
} from "../src/index";

export function Gallery() {
  const [toolOpen, setToolOpen] = useState(false);
  const [pillTab, setPillTab] = useState("timeline");
  const [underlineTab, setUnderlineTab] = useState("output");

  return (
    <div className="gallery">
      <div>
        <h1>
          @apiari/<span>ui</span>
        </h1>
        <p style={{ marginTop: 8 }}>Component gallery — all primitives in one place.</p>
      </div>

      {/* Button */}
      <section className="section">
        <h2>Button — variants</h2>
        <div className="row">
          <Button variant="primary">Primary</Button>
          <Button variant="secondary">Secondary</Button>
          <Button variant="ghost">Ghost</Button>
          <Button variant="danger">Danger</Button>
          <Button variant="icon" aria-label="Settings">
            <Settings size={15} />
          </Button>
        </div>

        <h2>Button — sizes</h2>
        <div className="row" style={{ alignItems: "center" }}>
          <Button variant="primary" size="sm">
            Small
          </Button>
          <Button variant="primary" size="md">
            Medium
          </Button>
          <Button variant="primary" size="lg">
            Large
          </Button>
        </div>

        <h2>Button — states</h2>
        <div className="row">
          <Button variant="primary" loading>
            Loading
          </Button>
          <Button variant="secondary" disabled>
            Disabled
          </Button>
          <Button variant="primary" size="sm">
            <Plus size={13} />
            With icon
          </Button>
          <Button variant="danger" size="sm">
            <Trash2 size={13} />
            Delete
          </Button>
        </div>
      </section>

      {/* Input / Textarea / Select */}
      <section className="section">
        <h2>Input</h2>
        <div className="fill" style={{ display: "flex", flexDirection: "column", gap: 8 }}>
          <Input placeholder="Default (md)" />
          <Input size="sm" placeholder="Small" />
          <Input size="lg" placeholder="Large" />
          <Input prefix={<Search size={13} />} placeholder="With prefix icon" />
          <Input disabled placeholder="Disabled" />
        </div>

        <h2>Textarea</h2>
        <div className="fill">
          <Textarea placeholder="Write something…" rows={3} />
        </div>

        <h2>Select</h2>
        <div className="fill" style={{ display: "flex", flexDirection: "column", gap: 8 }}>
          <Select size="sm">
            <option>Small option A</option>
            <option>Small option B</option>
          </Select>
          <Select>
            <option>Default option A</option>
            <option>Default option B</option>
          </Select>
          <Select size="lg">
            <option>Large option A</option>
            <option>Large option B</option>
          </Select>
          <Select disabled>
            <option>Disabled</option>
          </Select>
        </div>
      </section>

      {/* Spinner / Dots / Skeleton */}
      <section className="section">
        <h2>Spinner</h2>
        <div className="row" style={{ alignItems: "center" }}>
          <Spinner size="sm" />
          <Spinner size="md" />
          <Spinner size="lg" />
          <Spinner size="md" style={{ color: "var(--accent)" }} />
          <Spinner size="md" style={{ color: "var(--red)" }} />
        </div>

        <h2>Dots</h2>
        <div className="row" style={{ alignItems: "center" }}>
          <Dots />
          <Dots style={{ color: "var(--accent)" }} />
        </div>

        <h2>Skeleton</h2>
        <div className="fill" style={{ display: "flex", flexDirection: "column", gap: 8 }}>
          <Skeleton width="55%" height={22} />
          <Skeleton width="30%" height={14} />
          <Skeleton width="70%" height={14} />
          <Skeleton width="45%" height={14} />
        </div>
      </section>

      {/* TabBar */}
      <section className="section">
        <h2>TabBar — pill</h2>
        <TabBar
          variant="pill"
          value={pillTab}
          onChange={setPillTab}
          tabs={[
            { value: "timeline", label: "Timeline" },
            { value: "reviews", label: "Reviews", badge: 4 },
            { value: "brief", label: "Brief" },
          ]}
        />
        <div style={{ padding: "12px 0", color: "var(--text-faint)", fontSize: 13 }}>
          Active: {pillTab}
        </div>

        <h2>TabBar — underline</h2>
        <TabBar
          variant="underline"
          value={underlineTab}
          onChange={setUnderlineTab}
          tabs={[
            { value: "output", label: "Output" },
            { value: "task", label: "Task" },
            { value: "diff", label: "Diff" },
            { value: "chat", label: "Chat", badge: 2 },
          ]}
        />
        <div style={{ padding: "12px 0", color: "var(--text-faint)", fontSize: 13 }}>
          Active: {underlineTab}
        </div>
      </section>

      {/* StatusBadge */}
      <section className="section">
        <h2>StatusBadge</h2>
        <div className="row">
          <StatusBadge tone="accent">accent</StatusBadge>
          <StatusBadge tone="success">success</StatusBadge>
          <StatusBadge tone="danger">danger</StatusBadge>
          <StatusBadge tone="neutral">neutral</StatusBadge>
          <StatusBadge>default</StatusBadge>
        </div>
      </section>

      {/* ObjectRow */}
      <section className="section">
        <h2>ObjectRow</h2>
        <div className="fill">
          <ObjectRow title="Simple row" />
          <ObjectRow title="With meta" meta="Last updated 2m ago" />
          <ObjectRow
            title="With right slot"
            meta="some metadata"
            right={<StatusBadge tone="success">running</StatusBadge>}
          />
          <ObjectRow
            title="Clickable row"
            meta="Tap me"
            right={<StatusBadge tone="accent">active</StatusBadge>}
            onClick={() => alert("clicked")}
          />
        </div>
      </section>

      {/* PageHeader */}
      <section className="section">
        <h2>PageHeader</h2>
        <div className="fill">
          <PageHeader
            eyebrow="Workspace"
            title="My Project"
            summary="A description of what this workspace is for."
            actions={[
              { label: "Secondary", onClick: () => {}, kind: "secondary" },
              { label: "Primary", onClick: () => {}, kind: "primary" },
            ]}
          />
        </div>
      </section>

      {/* EmptyState */}
      <section className="section">
        <h2>EmptyState</h2>
        <div className="row">
          <div
            style={{
              flex: 1,
              minWidth: 240,
              border: "1px solid var(--border)",
              borderRadius: 8,
              padding: 24,
            }}
          >
            <EmptyState title="Nothing here yet" />
          </div>
          <div
            style={{
              flex: 1,
              minWidth: 240,
              border: "1px solid var(--border)",
              borderRadius: 8,
              padding: 24,
            }}
          >
            <EmptyState title="No messages" body="Start a conversation to see messages here." />
          </div>
        </div>
      </section>

      {/* ToolPanel */}
      <section className="section">
        <h2>ToolPanel</h2>
        <div className="fill">
          <ToolPanel title="Settings" subtitle="Configure your workspace">
            <div style={{ padding: 16, color: "var(--text-faint)" }}>Panel content goes here.</div>
          </ToolPanel>
        </div>
        <div className="row">
          <button
            onClick={() => setToolOpen(true)}
            style={{
              background: "var(--accent)",
              color: "#111",
              border: "none",
              borderRadius: 6,
              padding: "6px 14px",
              cursor: "pointer",
              fontWeight: 600,
              fontSize: 13,
            }}
          >
            Open mobile ToolPanel
          </button>
        </div>
        <ToolPanel
          title="Mobile Panel"
          subtitle="Slides in from the right"
          mobileOpen={toolOpen}
          onClose={() => setToolOpen(false)}
        >
          <div style={{ padding: 16, color: "var(--text-faint)" }}>Mobile panel content.</div>
        </ToolPanel>
      </section>

      {/* ModeScaffold */}
      <section className="section">
        <h2>ModeScaffold</h2>
        <div
          className="fill"
          style={{
            height: 200,
            border: "1px solid var(--border)",
            borderRadius: 8,
            overflow: "hidden",
          }}
        >
          <ModeScaffold
            header={
              <div style={{ padding: "10px 16px", fontWeight: 600, color: "var(--text-strong)" }}>
                Scaffold Header
              </div>
            }
          >
            <div style={{ padding: 16, color: "var(--text-faint)" }}>Body content area</div>
          </ModeScaffold>
        </div>
      </section>

      {/* InspectorPane */}
      <section className="section">
        <h2>InspectorPane</h2>
        <div className="row" style={{ alignItems: "stretch", height: 120 }}>
          <div
            style={{
              flex: 1,
              border: "1px solid var(--border)",
              borderRadius: 8,
              overflow: "hidden",
            }}
          >
            <InspectorPane>
              <div style={{ padding: 16, color: "var(--text-faint)" }}>Inspector content</div>
            </InspectorPane>
          </div>
          <div
            style={{
              flex: 1,
              border: "1px solid var(--border)",
              borderRadius: 8,
              overflow: "hidden",
            }}
          >
            <InspectorPane placeholder={<span>Select an item to inspect</span>} />
          </div>
        </div>
      </section>

      {/* DocumentSurface */}
      <section className="section">
        <h2>DocumentSurface</h2>
        <div
          className="fill"
          style={{
            height: 200,
            border: "1px solid var(--border)",
            borderRadius: 8,
            overflow: "hidden",
          }}
        >
          <DocumentSurface
            sidebar={
              <div style={{ padding: 16, color: "var(--text-faint)" }}>
                <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 12 }}>
                  <FileText size={14} />
                  <span>Sidebar</span>
                </div>
                <div style={{ fontSize: 12 }}>File tree or nav</div>
              </div>
            }
            editor={
              <div style={{ padding: 16, color: "var(--text-faint)" }}>
                <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 12 }}>
                  <Inbox size={14} />
                  <span>Editor area</span>
                </div>
                <div style={{ fontSize: 12 }}>Main content</div>
              </div>
            }
          />
        </div>
      </section>
    </div>
  );
}
