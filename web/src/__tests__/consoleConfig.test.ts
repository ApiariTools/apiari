import { beforeEach, describe, expect, it } from "vitest";
import {
  clearWorkspaceConsoleProfileOverride,
  DEFAULT_WORKSPACE_CONSOLE_PROFILE,
  getDefaultWorkspaceSelection,
  getOrderedWorkspaceModes,
  resolveWorkspaceConsoleProfile,
  saveWorkspaceConsoleProfileOverride,
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
    window.localStorage.setItem(
      "apiari.consoleProfileOverrides",
      JSON.stringify({
        "local/apiari": {
          defaultDesktopMode: "repos",
          navModeOrder: ["repos", "workers", "chat", "overview", "docs"],
          showChatRepoRail: false,
        },
      }),
    );

    expect(resolveWorkspaceConsoleProfile("apiari")).toMatchObject({
      defaultDesktopMode: "repos",
      navModeOrder: ["repos", "workers", "chat", "overview", "docs"],
      showChatRepoRail: false,
    });
  });

  it("saves and clears workspace overrides through helpers", () => {
    saveWorkspaceConsoleProfileOverride("apiari", undefined, {
      defaultDesktopMode: "workers",
      overviewPrimaryBot: "Customer",
    });

    expect(resolveWorkspaceConsoleProfile("apiari")).toMatchObject({
      defaultDesktopMode: "workers",
      overviewPrimaryBot: "Customer",
    });

    clearWorkspaceConsoleProfileOverride("apiari");

    expect(resolveWorkspaceConsoleProfile("apiari")).toMatchObject(
      DEFAULT_WORKSPACE_CONSOLE_PROFILE,
    );
  });
});
