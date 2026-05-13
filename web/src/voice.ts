export function stripMarkdown(text: string): string {
  return text
    .replace(/```[\s\S]*?```/g, "")
    .replace(/`[^`]*`/g, "")
    .replace(/\*\*([^*]+)\*\*/g, "$1")
    .replace(/\*([^*]+)\*/g, "$1")
    .replace(/^#{1,6}\s+/gm, "")
    .replace(/\[([^\]]+)\]\([^)]+\)/g, "$1")
    .replace(/^- /gm, "");
}

export function splitSentences(text: string): string[] {
  const cleaned = stripMarkdown(text);
  if (!cleaned.trim()) return [];
  const sentences = cleaned.match(/[^.?!]+[.?!]?/g) || [];
  return sentences.map((s) => s.trim()).filter((s) => s.length > 0);
}

export function cleanTranscription(text: string): string {
  if (!text) return "";
  let result = text;
  result = result.replace(/\[BLANK_AUDIO\]/g, "");
  result = result.replace(/\(inaudible\)/gi, "");
  result = result.replace(/♪[^♪]*♪/g, "");
  result = result.replace(/♪/g, "");
  result = result.replace(/\n{2,}/g, " ");
  result = result.replace(/[ \t]+/g, " ");
  result = result.replace(/\b(\w+)(\s+\1)+\b/gi, "$1");
  result = result.replace(/^[\s.,!?…·\-.]+/, "");
  return result.trim();
}

export function matchConfirmation(text: string): "yes" | "no" | "clear" | "continue" {
  const n = text.trim().toLowerCase();
  const has = (p: string) => new RegExp(`\\b${p}\\b`).test(n);

  const yesWords = ["yes", "yeah", "yep", "sure", "send it", "ok", "okay", "go", "do it"];
  const noWords = ["no", "nope", "cancel that", "never mind", "stop", "wait"];
  const clearWords = ["clear", "reset everything", "start over", "erase"];

  if (yesWords.some(has)) return "yes";
  if (noWords.some(has)) return "no";
  if (clearWords.some(has)) return "clear";
  return "continue";
}

export function float32ToWav(samples: Float32Array, sampleRate: number): Blob {
  const dataSize = samples.length * 2;
  const buffer = new ArrayBuffer(44 + dataSize);
  const view = new DataView(buffer);

  const write = (offset: number, str: string) => {
    for (let i = 0; i < str.length; i++) view.setUint8(offset + i, str.charCodeAt(i));
  };

  write(0, "RIFF");
  view.setUint32(4, 36 + dataSize, true);
  write(8, "WAVE");
  write(12, "fmt ");
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true);
  view.setUint16(22, 1, true);
  view.setUint32(24, sampleRate, true);
  view.setUint32(28, sampleRate * 2, true);
  view.setUint16(32, 2, true);
  view.setUint16(34, 16, true);
  write(36, "data");
  view.setUint32(40, dataSize, true);

  for (let i = 0; i < samples.length; i++) {
    const clamped = Math.max(-1, Math.min(1, samples[i]));
    view.setInt16(44 + i * 2, clamped * 32767, true);
  }

  return new Blob([buffer], { type: "audio/wav" });
}
