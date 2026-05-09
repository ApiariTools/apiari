import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("@apiari/api");

import { DocsPanel } from "../components/DocsPanel";
import * as api from "@apiari/api";

beforeEach(() => {
  vi.clearAllMocks();
  Object.defineProperty(window, "innerWidth", { value: 1024, writable: true });
  window.dispatchEvent(new Event("resize"));
});

describe("DocsPanel", () => {
  it("renders doc list", async () => {
    render(<DocsPanel workspace="test" />);
    await waitFor(() => {
      expect(screen.getByText("Architecture")).toBeInTheDocument();
      expect(screen.getByText("Setup Guide")).toBeInTheDocument();
    });
  });

  it("auto-selects the first doc and loads its content", async () => {
    render(<DocsPanel workspace="test" />);
    await waitFor(() => {
      expect(api.getDoc).toHaveBeenCalledWith("test", "architecture.md", undefined);
    });
    await waitFor(() => {
      expect(screen.getByText("Save")).toBeInTheDocument();
    });
  });

  it("selecting a doc loads content", async () => {
    const user = userEvent.setup();
    render(<DocsPanel workspace="test" />);
    await waitFor(() => expect(screen.getByText("Architecture")).toBeInTheDocument());
    await user.click(screen.getByText("Architecture"));
    await waitFor(() => {
      expect(api.getDoc).toHaveBeenCalledWith("test", "architecture.md", undefined);
    });
  });

  it("save calls API", async () => {
    const user = userEvent.setup();
    render(<DocsPanel workspace="test" />);
    await waitFor(() => expect(screen.getByText("Architecture")).toBeInTheDocument());
    await user.click(screen.getByText("Architecture"));
    await waitFor(() => expect(screen.getByText("Save")).toBeInTheDocument());

    // Type into the textarea to make it edited
    const textarea = screen.getByRole("textbox");
    await user.clear(textarea);
    await user.type(textarea, "new content");

    await user.click(screen.getByText("Save"));
    await waitFor(() => {
      expect(api.saveDoc).toHaveBeenCalledWith("test", "architecture.md", "new content", undefined);
    });
  });

  it("delete calls API with confirmation", async () => {
    const user = userEvent.setup();
    const confirmSpy = vi.spyOn(window, "confirm").mockReturnValue(true);
    render(<DocsPanel workspace="test" />);
    await waitFor(() => expect(screen.getByText("Architecture")).toBeInTheDocument());
    await user.click(screen.getByText("Architecture"));
    await waitFor(() => expect(screen.getByTitle("Delete")).toBeInTheDocument());
    await user.click(screen.getByTitle("Delete"));
    expect(confirmSpy).toHaveBeenCalled();
    await waitFor(() => {
      expect(api.deleteDoc).toHaveBeenCalledWith("test", "architecture.md", undefined);
    });
    confirmSpy.mockRestore();
  });

  it("new doc flow", async () => {
    const user = userEvent.setup();
    const promptSpy = vi.spyOn(window, "prompt").mockReturnValue("new-doc");
    render(<DocsPanel workspace="test" />);
    await waitFor(() => expect(screen.getByText("New")).toBeInTheDocument());
    await user.click(screen.getByText("New"));
    expect(promptSpy).toHaveBeenCalled();
    await waitFor(() => {
      expect(api.saveDoc).toHaveBeenCalledWith("test", "new-doc.md", "", undefined);
    });
    promptSpy.mockRestore();
  });

  it("uses a focused list-to-doc flow on mobile", async () => {
    Object.defineProperty(window, "innerWidth", { value: 600, writable: true });
    window.dispatchEvent(new Event("resize"));

    const user = userEvent.setup();
    render(<DocsPanel workspace="test" />);

    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Back to document list" })).toBeInTheDocument(),
    );
    await user.click(screen.getByRole("button", { name: "Back to document list" }));
    await waitFor(() => expect(screen.getByText("Setup Guide")).toBeInTheDocument());
    await user.click(screen.getByText("Setup Guide"));

    await waitFor(() => {
      expect(api.getDoc).toHaveBeenCalledWith("test", "setup.md", undefined);
    });
    await waitFor(() => {
      expect(screen.getByRole("button", { name: "Back to document list" })).toBeInTheDocument();
    });
    expect(screen.queryByText("Architecture")).not.toBeInTheDocument();
  });

  it("hides the editor when the mobile doc list is reopened", async () => {
    Object.defineProperty(window, "innerWidth", { value: 600, writable: true });
    window.dispatchEvent(new Event("resize"));

    const user = userEvent.setup();
    render(<DocsPanel workspace="test" />);

    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Back to document list" })).toBeInTheDocument(),
    );
    expect(screen.getByRole("textbox")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Back to document list" }));

    await waitFor(() => {
      expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
    });
    expect(screen.getByText("Architecture")).toBeInTheDocument();
    expect(screen.getByText("Setup Guide")).toBeInTheDocument();
  });

  it("restores an initial selected doc when remounted", async () => {
    render(<DocsPanel workspace="test" initialSelectedDocName="setup.md" />);

    await waitFor(() => {
      expect(api.getDoc).toHaveBeenCalledWith("test", "setup.md", undefined);
    });
  });
});
