import { describe, it, expect } from "vitest";
import {
  stripMarkdown,
  splitSentences,
  cleanTranscription,
  matchConfirmation,
  float32ToWav,
} from "../voice";

describe("stripMarkdown", () => {
  it("removes code blocks", () => {
    expect(stripMarkdown("before ```code``` after")).toBe("before  after");
  });

  it("removes inline code", () => {
    expect(stripMarkdown("run `npm install` now")).toBe("run  now");
  });

  it("removes bold/italic markers", () => {
    expect(stripMarkdown("**bold** and *italic*")).toBe("bold and italic");
  });

  it("removes heading markers", () => {
    expect(stripMarkdown("## Title\nContent")).toBe("Title\nContent");
  });

  it("converts links to text", () => {
    expect(stripMarkdown("see [docs](https://example.com)")).toBe("see docs");
  });

  it("removes list markers", () => {
    expect(stripMarkdown("- item one\n- item two")).toBe("item one\nitem two");
  });
});

describe("splitSentences", () => {
  it("splits on sentence boundaries", () => {
    expect(splitSentences("Hello. How are you? I'm fine!")).toEqual([
      "Hello.",
      "How are you?",
      "I'm fine!",
    ]);
  });

  it("returns single sentence for no punctuation", () => {
    expect(splitSentences("just some text")).toEqual(["just some text"]);
  });

  it("returns empty array for empty text", () => {
    expect(splitSentences("")).toEqual([]);
  });

  it("strips markdown before splitting", () => {
    expect(splitSentences("**Hello.** How are you?")).toEqual(["Hello.", "How are you?"]);
  });

  it("handles code blocks", () => {
    const result = splitSentences("Try this. ```code block``` Then do that.");
    expect(result[0]).toBe("Try this.");
    expect(result.some((s) => s.includes("code block"))).toBe(false);
  });
});

describe("cleanTranscription", () => {
  it("removes [BLANK_AUDIO]", () => {
    expect(cleanTranscription("[BLANK_AUDIO] hello")).toBe("hello");
  });

  it("removes (inaudible)", () => {
    expect(cleanTranscription("hello (inaudible) world")).toBe("hello world");
  });

  it("removes music notes", () => {
    expect(cleanTranscription("♪ la la ♪ hello")).toBe("hello");
  });

  it("collapses repeated words", () => {
    expect(cleanTranscription("the the the cat")).toBe("the cat");
  });

  it("removes leading punctuation", () => {
    expect(cleanTranscription("... hello")).toBe("hello");
  });

  it("collapses newlines", () => {
    expect(cleanTranscription("hello\n\nworld")).toBe("hello world");
  });

  it("handles empty string", () => {
    expect(cleanTranscription("")).toBe("");
  });

  it("handles pure noise", () => {
    expect(cleanTranscription("[BLANK_AUDIO]")).toBe("");
  });
});

describe("matchConfirmation", () => {
  it("matches yes variants", () => {
    expect(matchConfirmation("yes")).toBe("yes");
    expect(matchConfirmation("Yeah")).toBe("yes");
    expect(matchConfirmation("yep sure")).toBe("yes");
    expect(matchConfirmation("send it")).toBe("yes");
    expect(matchConfirmation("OK")).toBe("yes");
    expect(matchConfirmation("okay")).toBe("yes");
    expect(matchConfirmation("go")).toBe("yes");
    expect(matchConfirmation("do it")).toBe("yes");
  });

  it("matches no variants", () => {
    expect(matchConfirmation("no")).toBe("no");
    expect(matchConfirmation("Nope")).toBe("no");
    expect(matchConfirmation("cancel that")).toBe("no");
    expect(matchConfirmation("never mind")).toBe("no");
    expect(matchConfirmation("stop")).toBe("no");
    expect(matchConfirmation("wait")).toBe("no");
  });

  it("matches clear variants", () => {
    expect(matchConfirmation("clear")).toBe("clear");
    expect(matchConfirmation("reset everything")).toBe("clear");
    expect(matchConfirmation("start over")).toBe("clear");
    expect(matchConfirmation("erase")).toBe("clear");
  });

  it("returns continue for unrecognized", () => {
    expect(matchConfirmation("I also want to say")).toBe("continue");
    expect(matchConfirmation("actually add more")).toBe("continue");
    expect(matchConfirmation("hmm")).toBe("continue");
  });

  it("trims whitespace", () => {
    expect(matchConfirmation("  yes  ")).toBe("yes");
  });

  it("is case insensitive", () => {
    expect(matchConfirmation("YES")).toBe("yes");
    expect(matchConfirmation("NO")).toBe("no");
    expect(matchConfirmation("CLEAR")).toBe("clear");
  });
});

describe("float32ToWav", () => {
  it("produces a valid WAV blob", () => {
    const samples = new Float32Array([0, 0.5, -0.5, 1, -1]);
    const blob = float32ToWav(samples, 16000);
    expect(blob.type).toBe("audio/wav");
    // WAV header (44 bytes) + 5 samples * 2 bytes each = 54
    expect(blob.size).toBe(54);
  });

  it("handles empty samples", () => {
    const blob = float32ToWav(new Float32Array(0), 16000);
    expect(blob.size).toBe(44); // header only
  });
});
