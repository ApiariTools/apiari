/**
 * Voice mode utilities — pure functions, easily testable.
 */

/** Strip markdown formatting for TTS readability */
export function stripMarkdown(text: string): string {
  return text
    .replace(/```[\s\S]*?```/g, "") // code blocks
    .replace(/`[^`]+`/g, "") // inline code
    .replace(/[*_~]+/g, "") // bold/italic/strikethrough
    .replace(/^#+\s*/gm, "") // headings
    .replace(/^\s*[-*]\s+/gm, "") // list markers
    .replace(/\[([^\]]+)\]\([^)]+\)/g, "$1") // links → text
    .replace(/\n{2,}/g, "\n")
    .trim();
}

/** Split text into sentences for chunked TTS */
export function splitSentences(text: string): string[] {
  const plain = stripMarkdown(text);
  const sentences = plain.split(/(?<=[.!?])\s+/).filter((s) => s.trim().length > 0);
  if (sentences.length === 0 && plain.length > 0) return [plain];
  return sentences;
}

/** Clean whisper transcription artifacts */
export function cleanTranscription(text: string): string {
  return text
    .replace(/\[.*?\]/g, "") // [BLANK_AUDIO], [MUSIC], etc.
    .replace(/\(.*?\)/g, "") // (inaudible), etc.
    .replace(/♪[^♪]*♪/g, "") // music notes
    .replace(/\b(\w+)( \1){2,}\b/gi, "$1") // repeated words
    .replace(/^\s*[.!?,;:]+/, "") // leading punctuation
    .replace(/[\r\n]+/g, " ") // newlines → space
    .replace(/\s{2,}/g, " ") // collapse spaces
    .trim();
}

/** Match a yes/no/clear confirmation response */
export type ConfirmResult = "yes" | "no" | "clear" | "continue";

export function matchConfirmation(text: string): ConfirmResult {
  const lower = text.trim().toLowerCase();
  if (/^(yes|yeah|yep|yup|sure|send|go|do it|okay|ok|affirmative)\b/.test(lower)) return "yes";
  if (/^(no|nope|nah|cancel|never mind|nevermind|stop|don't|wait)\b/.test(lower)) return "no";
  if (/^(clear|reset|start over|erase|delete)\b/.test(lower)) return "clear";
  return "continue";
}

/** Convert Float32Array PCM samples to a WAV Blob */
export function float32ToWav(samples: Float32Array, sampleRate: number): Blob {
  const numChannels = 1;
  const bitsPerSample = 16;
  const byteRate = sampleRate * numChannels * (bitsPerSample / 8);
  const blockAlign = numChannels * (bitsPerSample / 8);
  const dataSize = samples.length * (bitsPerSample / 8);
  const buffer = new ArrayBuffer(44 + dataSize);
  const view = new DataView(buffer);

  const w = (offset: number, str: string) => {
    for (let i = 0; i < str.length; i++) view.setUint8(offset + i, str.charCodeAt(i));
  };
  w(0, "RIFF");
  view.setUint32(4, 36 + dataSize, true);
  w(8, "WAVE");
  w(12, "fmt ");
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true);
  view.setUint16(22, numChannels, true);
  view.setUint32(24, sampleRate, true);
  view.setUint32(28, byteRate, true);
  view.setUint16(32, blockAlign, true);
  view.setUint16(34, bitsPerSample, true);
  w(36, "data");
  view.setUint32(40, dataSize, true);

  let offset = 44;
  for (let i = 0; i < samples.length; i++) {
    const s = Math.max(-1, Math.min(1, samples[i]));
    view.setInt16(offset, s < 0 ? s * 0x8000 : s * 0x7fff, true);
    offset += 2;
  }

  return new Blob([buffer], { type: "audio/wav" });
}

/** Transcribe audio via the whisper API */
export async function transcribe(audio: Float32Array): Promise<string | null> {
  const blob = float32ToWav(audio, 16000);
  const form = new FormData();
  form.append("audio", blob, "audio.wav");
  const res = await fetch("/api/transcribe", { method: "POST", body: form });
  if (!res.ok) return null;
  const data = await res.json();
  if (data.error || !data.text) return null;
  return cleanTranscription(data.text);
}
