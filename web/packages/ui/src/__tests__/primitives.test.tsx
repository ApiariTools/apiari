import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import {
  StatusBadge,
  ObjectRow,
  PageHeader,
  EmptyState,
  ToolPanel,
  ModeScaffold,
  InspectorPane,
  DocumentSurface,
} from "../index";

// ── StatusBadge ──────────────────────────────────────────────────────────────

describe("StatusBadge", () => {
  it("renders children", () => {
    render(<StatusBadge>running</StatusBadge>);
    expect(screen.getByText("running")).toBeInTheDocument();
  });

  it.each(["accent", "success", "danger", "neutral"] as const)("renders %s tone", (tone) => {
    const { container } = render(<StatusBadge tone={tone}>label</StatusBadge>);
    expect(container.firstChild).toBeInTheDocument();
  });
});

// ── ObjectRow ────────────────────────────────────────────────────────────────

describe("ObjectRow", () => {
  it("renders title", () => {
    render(<ObjectRow title="My Row" />);
    expect(screen.getByText("My Row")).toBeInTheDocument();
  });

  it("renders meta when provided", () => {
    render(<ObjectRow title="Row" meta="some meta" />);
    expect(screen.getByText("some meta")).toBeInTheDocument();
  });

  it("renders right slot when provided", () => {
    render(<ObjectRow title="Row" right={<span>right</span>} />);
    expect(screen.getByText("right")).toBeInTheDocument();
  });

  it("renders as div when no onClick", () => {
    const { container } = render(<ObjectRow title="Row" />);
    expect(container.querySelector("button")).toBeNull();
    expect(container.querySelector("div")).toBeInTheDocument();
  });

  it("renders as button when onClick provided", () => {
    const onClick = vi.fn();
    render(<ObjectRow title="Clickable" onClick={onClick} />);
    const btn = screen.getByRole("button");
    fireEvent.click(btn);
    expect(onClick).toHaveBeenCalledOnce();
  });
});

// ── PageHeader ───────────────────────────────────────────────────────────────

describe("PageHeader", () => {
  it("renders title", () => {
    render(<PageHeader title="My Page" />);
    expect(screen.getByText("My Page")).toBeInTheDocument();
  });

  it("renders eyebrow when provided", () => {
    render(<PageHeader title="Title" eyebrow="Section" />);
    expect(screen.getByText("Section")).toBeInTheDocument();
  });

  it("renders summary when provided", () => {
    render(<PageHeader title="Title" summary="A description." />);
    expect(screen.getByText("A description.")).toBeInTheDocument();
  });

  it("renders action buttons", () => {
    const onClick = vi.fn();
    render(<PageHeader title="Title" actions={[{ label: "Save", onClick }]} />);
    fireEvent.click(screen.getByText("Save"));
    expect(onClick).toHaveBeenCalledOnce();
  });

  it("renders no actions when array is empty", () => {
    render(<PageHeader title="Title" actions={[]} />);
    expect(screen.queryByRole("button")).toBeNull();
  });
});

// ── EmptyState ───────────────────────────────────────────────────────────────

describe("EmptyState", () => {
  it("renders title", () => {
    render(<EmptyState title="Nothing here" />);
    expect(screen.getByText("Nothing here")).toBeInTheDocument();
  });

  it("renders body when provided", () => {
    render(<EmptyState title="Title" body="Helper text" />);
    expect(screen.getByText("Helper text")).toBeInTheDocument();
  });

  it("does not render body element when omitted", () => {
    const { container } = render(<EmptyState title="Title" />);
    expect(container.querySelectorAll("div").length).toBe(2); // outer + title only
  });
});

// ── ToolPanel ────────────────────────────────────────────────────────────────

describe("ToolPanel", () => {
  it("renders title and children", () => {
    render(<ToolPanel title="Settings">content</ToolPanel>);
    expect(screen.getByText("Settings")).toBeInTheDocument();
    expect(screen.getByText("content")).toBeInTheDocument();
  });

  it("renders subtitle when provided", () => {
    render(
      <ToolPanel title="Settings" subtitle="Manage preferences">
        content
      </ToolPanel>,
    );
    expect(screen.getByText("Manage preferences")).toBeInTheDocument();
  });

  it("renders backdrop when mobileOpen", () => {
    const onClose = vi.fn();
    const { container } = render(
      <ToolPanel title="Panel" mobileOpen onClose={onClose}>
        content
      </ToolPanel>,
    );
    const backdrop = container.querySelector("[class*='backdrop']");
    expect(backdrop).toBeInTheDocument();
    fireEvent.click(backdrop!);
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("does not render backdrop when closed", () => {
    const { container } = render(<ToolPanel title="Panel">content</ToolPanel>);
    expect(container.querySelector("[class*='backdrop']")).toBeNull();
  });
});

// ── ModeScaffold ─────────────────────────────────────────────────────────────

describe("ModeScaffold", () => {
  it("renders children", () => {
    render(<ModeScaffold>body content</ModeScaffold>);
    expect(screen.getByText("body content")).toBeInTheDocument();
  });

  it("renders header when provided", () => {
    render(<ModeScaffold header={<span>Header</span>}>body</ModeScaffold>);
    expect(screen.getByText("Header")).toBeInTheDocument();
  });

  it("renders no header element when omitted", () => {
    const { container } = render(<ModeScaffold>body</ModeScaffold>);
    expect(container.querySelector("[class*='header']")).toBeNull();
  });
});

// ── InspectorPane ─────────────────────────────────────────────────────────────

describe("InspectorPane", () => {
  it("renders children when provided", () => {
    render(<InspectorPane>details here</InspectorPane>);
    expect(screen.getByText("details here")).toBeInTheDocument();
  });

  it("renders placeholder when no children", () => {
    render(<InspectorPane placeholder={<span>Select an item</span>} />);
    expect(screen.getByText("Select an item")).toBeInTheDocument();
  });
});

// ── DocumentSurface ───────────────────────────────────────────────────────────

describe("DocumentSurface", () => {
  it("renders sidebar and editor slots", () => {
    render(
      <DocumentSurface
        sidebar={<span>sidebar content</span>}
        editor={<span>editor content</span>}
      />,
    );
    expect(screen.getByText("sidebar content")).toBeInTheDocument();
    expect(screen.getByText("editor content")).toBeInTheDocument();
  });
});
