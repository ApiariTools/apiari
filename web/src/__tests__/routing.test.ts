import { describe, it, expect, beforeEach } from "vitest";
import { parseHash } from "../routeUtils";

describe("workspace routing", () => {
  beforeEach(() => {
    window.location.hash = "";
  });

  it("decodes encoded bot names from chat routes", () => {
    window.location.hash = "#/apiari/chat/Main%20Bot";
    expect(parseHash()).toMatchObject({
      workspace: "apiari",
      mode: "chat",
      bot: "Main Bot",
    });
  });

  it("decodes encoded bot names from legacy chat routes", () => {
    window.location.hash = "#/apiari/Main%20Bot";
    expect(parseHash()).toMatchObject({
      workspace: "apiari",
      mode: "chat",
      bot: "Main Bot",
    });
  });
});
