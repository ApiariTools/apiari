import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, it, expect } from "vitest";
import App from "../App";

describe("App shell", () => {
  it("renders the sidebar with Auto Bots and Workers sections", () => {
    render(<App />);
    expect(screen.getAllByText("Auto Bots").length).toBeGreaterThan(0);
    expect(screen.getAllByText("Workers").length).toBeGreaterThan(0);
  });

  it("renders stub auto bots in the sidebar", () => {
    render(<App />);
    expect(screen.getByText("Triage")).toBeInTheDocument();
    expect(screen.getByText("Standup")).toBeInTheDocument();
  });

  it("renders stub workers in the sidebar", () => {
    render(<App />);
    expect(screen.getByText("fix-auth-rate-limit")).toBeInTheDocument();
    expect(screen.getByText("update-deps")).toBeInTheDocument();
    expect(screen.getByText("add-tests")).toBeInTheDocument();
  });

  it("shows empty state when nothing is selected", () => {
    render(<App />);
    expect(screen.getByText("Select something")).toBeInTheDocument();
    expect(screen.getByText("Choose a worker or auto bot from the sidebar")).toBeInTheDocument();
  });

  it("shows placeholder detail when a worker is selected", async () => {
    const user = userEvent.setup();
    render(<App />);
    await user.click(screen.getByText("fix-auth-rate-limit"));
    expect(screen.getByText("Worker: apiari-1")).toBeInTheDocument();
    expect(screen.queryByText("Select something")).not.toBeInTheDocument();
  });

  it("shows placeholder detail when an auto bot is selected", async () => {
    const user = userEvent.setup();
    render(<App />);
    await user.click(screen.getByText("Triage"));
    expect(screen.getByText("Auto Bot: triage")).toBeInTheDocument();
    expect(screen.queryByText("Select something")).not.toBeInTheDocument();
  });

  it("switches selected item when another is clicked", async () => {
    const user = userEvent.setup();
    render(<App />);
    await user.click(screen.getByText("fix-auth-rate-limit"));
    expect(screen.getByText("Worker: apiari-1")).toBeInTheDocument();
    await user.click(screen.getByText("update-deps"));
    expect(screen.getByText("Worker: apiari-2")).toBeInTheDocument();
    expect(screen.queryByText("Worker: apiari-1")).not.toBeInTheDocument();
  });

  it("renders the mobile bottom tab bar with Auto Bots and Workers tabs", () => {
    render(<App />);
    const nav = screen.getByRole("navigation", { name: "Mobile navigation" });
    expect(nav).toBeInTheDocument();
    expect(nav).toHaveTextContent("Auto Bots");
    expect(nav).toHaveTextContent("Workers");
  });

  it("sidebar navigation has accessible label", () => {
    render(<App />);
    expect(screen.getByRole("navigation", { name: "Sidebar" })).toBeInTheDocument();
  });
});
