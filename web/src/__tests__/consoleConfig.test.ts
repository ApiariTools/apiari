import { beforeEach, describe, expect, it } from "vitest";
import {
  DEFAULT_WORKSPACE_CONSOLE_PROFILE,
  getDefaultWorkspaceSelection,
  getOrderedWorkspaceModes,
  resolveWorkspaceConsoleProfile,
} from "../consoleConfig";

describe("consoleConfig", () => {
  beforeEach(() => {
    window.localStorage.clear();
  });

  it("returns the default mode order", () => {
    expect(getOrderedWorkspaceModes().map((mode) => mode.id)).toEqual(
      DEFAULT_WORKSPACE_CONSOLE_PROFILE.navModeOrder,
    );
  });

  it("returns desktop and mobile defaults from the profile", () => {
    expect(getDefaultWorkspaceSelection(DEFAULT_WORKSPACE_CONSOLE_PROFILE, false)).toEqual({
      mode: "overview",
      bot: "",
    });
    expect(getDefaultWorkspaceSelection(DEFAULT_WORKSPACE_CONSOLE_PROFILE, true)).toEqual({
      mode: "chat",
      bot: "Main",
    });
  });

  it("applies stored workspace-specific overrides", () => {
    window.localStorage.setItem("apiari.consoleProfileOverrides", JSON.stringify({
      "local/apiari": {
        defaultDesktopMode: "repos",
        navModeOrder: ["repos", "workers", "chat", "overview", "docs"],
        showChatRepoRail: false,
      },
    }));

    expect(resolveWorkspaceConsoleProfile("apiari")).toMatchObject({
      defaultDesktopMode: "repos",
      navModeOrder: ["repos", "workers", "chat", "overview", "docs"],
      showChatRepoRail: false,
    });
  });
});
