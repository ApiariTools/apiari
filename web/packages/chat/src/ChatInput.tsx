import { useRef, useEffect, useState } from "react";
import { Mic, Square, Paperclip, ArrowUp, Loader2 } from "lucide-react";
import type { MicVAD } from "@ricky0123/vad-web";
import { cleanTranscription, matchConfirmation, float32ToWav } from "./voice";
import styles from "./ChatInput.module.css";

export interface Attachment {
  name: string;
  type: string;
  dataUrl: string;
}

export type VoiceState = "listening" | "processing" | "speaking";

interface Props {
  placeholder: string;
  disabled?: boolean;
  onSend: (text: string, attachments?: Attachment[]) => void;
  showAttachments?: boolean;
  /** Show the mic button and enable voice recording. Default: true */
  showMic?: boolean;
  voiceMode?: boolean;
  voiceState?: VoiceState;
  triggerRecord?: number;
  playTts?: (url: string, onEnd?: () => void) => Promise<void>;
  queueCount?: number;
}

const VOICE_CONFIRM_DELAY_MS = 5000;

export function ChatInput({
  placeholder,
  disabled,
  onSend,
  showAttachments = true,
  showMic = true,
  voiceMode,
  voiceState,
  triggerRecord,
  playTts,
  queueCount = 0,
}: Props) {
  // ── Text input state ──
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [hasText, setHasText] = useState(false);

  // ── Mic/voice state ──
  const [micState, setMicState] = useState<"idle" | "loading" | "listening" | "transcribing">(
    "idle",
  );
  const [transcribeError, setTranscribeError] = useState<string | null>(null);
  const [partialText, setPartialText] = useState("");
  const [confirming, setConfirming] = useState(false);
  const [silenceCountdown, setSilenceCountdown] = useState(0);
  const countdownIntervalRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const partialTextRef = useRef("");
  const confirmingRef = useRef(false);
  const isListeningRef = useRef(false);
  const pendingTranscriptions = useRef(0);

  // ── VAD refs ──
  const vadRef = useRef<MicVAD | null>(null);
  const streamRef = useRef<MediaStream | null>(null);
  const analyserRef = useRef<AnalyserNode | null>(null);
  const vizCtxRef = useRef<AudioContext | null>(null);

  // ── Timers ──
  const longPressTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const didLongPress = useRef(false);
  const voiceSendTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // ── Waveform ──
  const canvasRef = useRef<HTMLCanvasElement | null>(null);

  const smoothedBars = useRef<number[]>([]);

  // ── Effects ──

  // Waveform: simple interval-based drawing. Runs whenever canvas is visible.
  // No start/stop/restart lifecycle — just draws if analyser exists, no-ops if not.
  useEffect(() => {
    const id = setInterval(() => {
      if (analyserRef.current && canvasRef.current) drawWaveform();
    }, 1000 / 30); // 30fps
    return () => clearInterval(id);
  }, []);

  // Cleanup on unmount
  // Ignore speech when page is not visible (tab switched, app backgrounded)
  const pageVisibleRef = useRef(true);
  useEffect(() => {
    function onVisibility() {
      pageVisibleRef.current = document.visibilityState === "visible";
    }
    document.addEventListener("visibilitychange", onVisibility);
    return () => document.removeEventListener("visibilitychange", onVisibility);
  }, []);

  useEffect(() => {
    return () => {
      vadRef.current?.destroy();
      vadRef.current = null;
      streamRef.current?.getTracks().forEach((t) => t.stop());
      streamRef.current = null;
      vizCtxRef.current?.close();
      vizCtxRef.current = null;
      clearTimer(longPressTimer);
      clearTimer(voiceSendTimerRef);
      if (countdownIntervalRef.current) clearInterval(countdownIntervalRef.current);
    };
  }, []);

  // Parent triggers recording (voice mode loop)
  useEffect(() => {
    if (triggerRecord && triggerRecord > 0 && micState === "idle") {
      startListening();
    }
  }, [triggerRecord]);

  // Track bot processing state — VAD stays running for waveform,
  // but we ignore speech events while bot is thinking
  const botThinkingRef = useRef(false);

  useEffect(() => {
    if (!voiceMode) return;
    botThinkingRef.current = !!disabled;
    if (!disabled && vadRef.current) {
      isListeningRef.current = true;
      setMicState("listening");
    }
  }, [disabled, voiceMode]);

  // ── Timer helpers ──

  function clearTimer(ref: React.MutableRefObject<ReturnType<typeof setTimeout> | null>) {
    if (ref.current) {
      clearTimeout(ref.current);
      ref.current = null;
    }
  }

  // ── Voice mode confirmation ──

  function resetVoiceSendTimer() {
    clearTimer(voiceSendTimerRef);
    // Clear any existing countdown
    if (countdownIntervalRef.current) {
      clearInterval(countdownIntervalRef.current);
      countdownIntervalRef.current = null;
    }
    setSilenceCountdown(0);

    // Start countdown after 2 seconds of silence (visual feedback)
    const countdownStart = VOICE_CONFIRM_DELAY_MS - 3000; // show countdown for last 3 seconds
    voiceSendTimerRef.current = setTimeout(() => {
      let remaining = 3;
      setSilenceCountdown(remaining);
      countdownIntervalRef.current = setInterval(() => {
        remaining--;
        setSilenceCountdown(remaining);
        if (remaining <= 0) {
          if (countdownIntervalRef.current) {
            clearInterval(countdownIntervalRef.current);
            countdownIntervalRef.current = null;
          }
          const text = partialTextRef.current.trim();
          if (text && voiceMode && !confirmingRef.current) {
            confirmingRef.current = true;
            setConfirming(true);
            if (playTts) {
              playTts("/api/tts/speak?" + new URLSearchParams({ text: "Send?" }).toString());
            }
          }
        }
      }, 1000);
    }, countdownStart);
  }

  function cancelCountdown() {
    if (countdownIntervalRef.current) {
      clearInterval(countdownIntervalRef.current);
      countdownIntervalRef.current = null;
    }
    setSilenceCountdown(0);
  }

  async function handleConfirmation(audio: Float32Array) {
    const blob = float32ToWav(audio, 16000);
    try {
      const form = new FormData();
      form.append("audio", blob, "audio.wav");
      const res = await fetch("/api/transcribe", { method: "POST", body: form });
      if (!res.ok) return;
      const data = await res.json();
      if (!data.text) return;

      const answer = cleanTranscription(data.text);
      if (!answer) return;

      const result = matchConfirmation(answer);
      switch (result) {
        case "yes":
          if (partialTextRef.current.trim()) onSend(partialTextRef.current.trim());
          clearPartial();
          break;
        case "no":
        case "clear":
          clearPartial();
          break;
        case "continue":
          partialTextRef.current = partialTextRef.current
            ? partialTextRef.current + " " + answer
            : answer;
          setPartialText(partialTextRef.current);
          resetVoiceSendTimer();
          break;
      }
    } catch {
      return;
    }
    confirmingRef.current = false;
    setConfirming(false);
  }

  function clearPartial() {
    partialTextRef.current = "";
    setPartialText("");
    cancelCountdown();
  }

  // ── Transcription ──

  async function transcribeChunk(audio: Float32Array) {
    const blob = float32ToWav(audio, 16000);
    pendingTranscriptions.current++;
    try {
      const form = new FormData();
      form.append("audio", blob, "audio.wav");
      const res = await fetch("/api/transcribe", { method: "POST", body: form });
      if (!res.ok) return;
      const data = await res.json();
      if (data.text) {
        const cleaned = cleanTranscription(data.text);
        if (cleaned) {
          // Check if user said "send" at the end — auto-send without confirmation
          const sendMatch = cleaned.match(/^(.+?)\s+send[.!]?$/i);
          if (voiceMode && sendMatch) {
            const content = partialTextRef.current
              ? partialTextRef.current + " " + sendMatch[1]
              : sendMatch[1];
            onSend(content.trim());
            clearPartial();
            cancelCountdown();
            clearTimer(voiceSendTimerRef);
          } else {
            partialTextRef.current = partialTextRef.current
              ? partialTextRef.current + " " + cleaned
              : cleaned;
            setPartialText(partialTextRef.current);
            if (voiceMode) {
              cancelCountdown();
              resetVoiceSendTimer();
            }
          }
        }
      }
    } catch {
      return;
    } finally {
      pendingTranscriptions.current--;
      if (!isListeningRef.current && pendingTranscriptions.current === 0) {
        finalize();
      }
    }
  }

  function finalize() {
    clearTimer(voiceSendTimerRef);
    confirmingRef.current = false;
    setConfirming(false);
    const text = partialTextRef.current.trim();
    if (text) {
      if (voiceMode) {
        onSend(text);
      } else {
        const el = textareaRef.current;
        if (el) {
          el.value = el.value ? el.value + " " + text : text;
          autoGrow();
        }
      }
    }
    clearPartial();
    setMicState("idle");
  }

  // ── VAD lifecycle ──

  const micInitRef = useRef(false);

  async function startListening() {
    if (micState !== "idle" || micInitRef.current) return;
    micInitRef.current = true;
    setTranscribeError(null);
    clearPartial();

    try {
      // Resume existing VAD (fast path)
      if (vadRef.current) {
        await vadRef.current.start();
        isListeningRef.current = true;
        setMicState("listening");
        micInitRef.current = false;
        return;
      }

      // Show loading state immediately (first-time init is slow)
      setMicState("loading");

      // First time: acquire mic + load VAD model (slow)
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      streamRef.current = stream;

      const { MicVAD } = await import("@ricky0123/vad-web");
      const vad = await MicVAD.new({
        model: "legacy",
        modelURL: "/silero_vad_legacy.onnx",
        workletURL: "/vad.worklet.bundle.min.js",
        onnxWASMBasePath: "/",
        positiveSpeechThreshold: 0.8,
        negativeSpeechThreshold: 0.5,
        minSpeechFrames: 5,
        redemptionMs: 600,
        getStream: async () => stream,
        onSpeechEnd: (audio) => {
          // Ignore speech while bot is thinking, page hidden, or TTS playing
          if (botThinkingRef.current || !pageVisibleRef.current) return;
          if (confirmingRef.current) {
            handleConfirmation(audio);
          } else {
            transcribeChunk(audio);
          }
        },
        onSpeechStart: () => {},
      } as Parameters<typeof MicVAD.new>[0]);

      vadRef.current = vad;

      // Set up waveform analyser on its OWN AudioContext (independent from VAD —
      // VAD pausing won't kill the analyser)
      const vizCtx = new AudioContext();
      vizCtxRef.current = vizCtx;
      const source = vizCtx.createMediaStreamSource(stream);
      const analyser = vizCtx.createAnalyser();
      analyser.fftSize = 256;
      source.connect(analyser);
      analyserRef.current = analyser;

      await vad.start();
      isListeningRef.current = true;
      setMicState("listening");
      micInitRef.current = false;
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      setTranscribeError(
        msg.includes("Permission") || msg.includes("NotAllowed")
          ? "Microphone access denied"
          : `Voice init failed: ${msg}`,
      );
      setMicState("idle");
      micInitRef.current = false;
    }
  }

  async function stopListening() {
    isListeningRef.current = false;
    if (vadRef.current) await vadRef.current.pause();

    if (!voiceMode) {
      analyserRef.current = null;
      vizCtxRef.current?.close();
      vizCtxRef.current = null;
      streamRef.current?.getTracks().forEach((t) => t.stop());
      streamRef.current = null;
      if (vadRef.current) {
        await vadRef.current.destroy();
        vadRef.current = null;
      }
    }

    if (pendingTranscriptions.current > 0) {
      setMicState("transcribing");
    } else {
      finalize();
    }
  }

  // ── Waveform ──

  function drawWaveform() {
    const canvas = canvasRef.current;
    const analyser = analyserRef.current;
    if (!canvas || !analyser) return;

    const rect = canvas.getBoundingClientRect();
    const dpr = window.devicePixelRatio || 1;
    canvas.width = rect.width * dpr;
    canvas.height = rect.height * dpr;

    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.scale(dpr, dpr);

    const w = rect.width;
    const h = rect.height;
    const freqData = new Uint8Array(analyser.frequencyBinCount);
    analyser.getByteFrequencyData(freqData);

    const halfCount = 12;
    const usableBins = Math.floor(freqData.length * 0.4);
    if (smoothedBars.current.length !== halfCount) {
      smoothedBars.current = new Array(halfCount).fill(0);
    }

    ctx.clearRect(0, 0, w, h);
    const totalBars = halfCount * 2;
    const gap = 3;
    const barWidth = (w - gap * (totalBars - 1)) / totalBars;
    const centerY = h / 2;

    for (let i = 0; i < halfCount; i++) {
      const startBin = Math.floor(Math.pow(i / halfCount, 1.5) * usableBins);
      const endBin = Math.floor(Math.pow((i + 1) / halfCount, 1.5) * usableBins);
      const binCount = Math.max(1, endBin - startBin);
      let sum = 0;
      for (let j = startBin; j < startBin + binCount; j++) sum += freqData[j];
      const raw = sum / binCount / 255;
      const gated = Math.max(0, raw - 0.2);
      const scaled = Math.pow(gated / 0.8, 0.9) * 1.4;
      const target = Math.min(scaled, 1.0);
      const prev = smoothedBars.current[i];
      smoothedBars.current[i] =
        target > prev ? prev + (target - prev) * 0.7 : prev + (target - prev) * 0.25;
    }

    for (let i = 0; i < halfCount; i++) {
      const barH = Math.max(4, 4 + smoothedBars.current[i] * (h - 8));
      ctx.fillStyle = "#e85555";

      const xRight = (halfCount + i) * (barWidth + gap);
      ctx.beginPath();
      ctx.roundRect(xRight, centerY - barH / 2, barWidth, barH, 2);
      ctx.fill();

      const xLeft = (halfCount - 1 - i) * (barWidth + gap);
      ctx.beginPath();
      ctx.roundRect(xLeft, centerY - barH / 2, barWidth, barH, 2);
      ctx.fill();
    }
  }

  // ── Text input helpers ──

  function send() {
    const el = textareaRef.current;
    if (!el) return;
    const text = el.value.trim();
    if (!text && attachments.length === 0) return;
    el.value = "";
    el.style.height = "auto";
    setHasText(false);
    onSend(text, attachments.length > 0 ? attachments : undefined);
    setAttachments([]);
  }

  function autoGrow() {
    const el = textareaRef.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = Math.min(el.scrollHeight, 160) + "px";
    setHasText(el.value.trim().length > 0);
  }

  function handleKeyDown(e: React.KeyboardEvent) {
    if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      send();
    }
  }

  function handleFiles(files: FileList | null) {
    if (!files) return;
    Array.from(files).forEach((file) => {
      const reader = new FileReader();
      reader.onload = () => {
        setAttachments((prev) => [
          ...prev,
          { name: file.name, type: file.type, dataUrl: reader.result as string },
        ]);
      };
      reader.readAsDataURL(file);
    });
  }

  function removeAttachment(index: number) {
    setAttachments((prev) => prev.filter((_, i) => i !== index));
  }

  // ── Render ──

  return (
    <div className={styles.inputArea}>
      {(voiceMode ||
        micState === "loading" ||
        micState === "listening" ||
        micState === "transcribing") && (
        <>
          <div
            className={`${styles.voiceBar} ${voiceState === "processing" ? styles.voiceBarProcessing : ""} ${voiceState === "speaking" ? styles.voiceBarSpeaking : ""}`}
          >
            <canvas ref={canvasRef} className={styles.waveform} />
            {voiceMode && voiceState === "processing" && (
              <div className={styles.voiceBarLabel}>Thinking...</div>
            )}
            {voiceMode && voiceState === "speaking" && (
              <div className={styles.voiceBarLabel}>Speaking</div>
            )}
          </div>
          {partialText && (
            <div className={styles.partialText}>
              {partialText}
              {silenceCountdown > 0 && !confirming && (
                <span className={styles.countdown}>{silenceCountdown}</span>
              )}
              {confirming && (
                <span className={styles.confirmPrompt}>
                  Say "yes" to send, "no" to keep editing, or "clear" to start over
                </span>
              )}
            </div>
          )}
        </>
      )}
      {showAttachments && attachments.length > 0 && (
        <div className={styles.attachmentPreview}>
          {attachments.map((a, i) => (
            <div key={i} className={styles.attachmentChip}>
              {a.type.startsWith("image/") ? (
                <img src={a.dataUrl} alt={a.name} className={styles.attachmentThumb} />
              ) : (
                <span className={styles.attachmentName}>{a.name}</span>
              )}
              <button
                type="button"
                className={styles.attachmentRemove}
                aria-label={`Remove ${a.name}`}
                onClick={() => removeAttachment(i)}
              >
                &times;
              </button>
            </div>
          ))}
        </div>
      )}
      <div className={styles.inputRow}>
        {showAttachments && (
          <>
            <input
              ref={fileInputRef}
              type="file"
              multiple
              accept="image/*,.pdf,.txt,.md,.json,.csv,.ts,.tsx,.js,.jsx,.py,.rs,.go,.rb,.swift"
              style={{ display: "none" }}
              onChange={(e) => handleFiles(e.target.files)}
            />
            <button
              type="button"
              className={styles.attachBtn}
              aria-label="Attach file"
              onClick={() => fileInputRef.current?.click()}
            >
              <Paperclip size={16} />
            </button>
          </>
        )}
        <textarea
          ref={textareaRef}
          className={styles.inputField}
          placeholder={placeholder}
          rows={1}
          enterKeyHint="enter"
          onInput={autoGrow}
          onKeyDown={handleKeyDown}
        />
        {!showMic ? (
          (hasText || attachments.length > 0) && (
            <button
              type="button"
              className={`${styles.actionBtn} ${styles.actionBtnSend}`}
              aria-label="Send message"
              onMouseDown={(e) => e.preventDefault()}
              onMouseUp={send}
              onTouchEnd={(e) => {
                e.preventDefault();
                send();
              }}
            >
              <ArrowUp size={18} />
            </button>
          )
        ) : micState === "loading" ? (
          <button
            type="button"
            className={`${styles.actionBtn} ${styles.micLoading}`}
            aria-label="Initializing microphone"
            disabled
          >
            <Loader2 size={16} className={styles.micSpinner} />
          </button>
        ) : micState === "listening" ? (
          <button
            type="button"
            className={`${styles.actionBtn} ${styles.micRecording}`}
            aria-label="Stop listening"
            onClick={stopListening}
            onTouchEnd={(e) => {
              e.preventDefault();
              stopListening();
            }}
          >
            <Square size={16} />
          </button>
        ) : micState === "transcribing" ? (
          <button type="button" className={styles.actionBtn} aria-label="Transcribing" disabled>
            ...
          </button>
        ) : (
          <button
            type="button"
            className={`${styles.actionBtn} ${hasText || attachments.length > 0 ? styles.actionBtnSend : ""}`}
            aria-label={hasText || attachments.length > 0 ? "Send message" : "Record voice"}
            onMouseDown={(e) => {
              e.preventDefault();
              didLongPress.current = false;
              longPressTimer.current = setTimeout(() => {
                didLongPress.current = true;
                startListening();
              }, 500);
            }}
            onMouseUp={() => {
              clearTimer(longPressTimer);
              if (didLongPress.current) {
                stopListening();
                return;
              }
              if (hasText || attachments.length > 0) {
                send();
              } else {
                startListening();
              }
            }}
            onMouseLeave={() => clearTimer(longPressTimer)}
            onTouchStart={() => {
              didLongPress.current = false;
              longPressTimer.current = setTimeout(() => {
                didLongPress.current = true;
                startListening();
              }, 500);
            }}
            onTouchEnd={(e) => {
              e.preventDefault();
              clearTimer(longPressTimer);
              if (didLongPress.current) {
                stopListening();
                return;
              }
              if (hasText || attachments.length > 0) {
                send();
              } else {
                startListening();
              }
            }}
          >
            {hasText || attachments.length > 0 ? <ArrowUp size={18} /> : <Mic size={16} />}
          </button>
        )}
      </div>
      {queueCount > 0 && (
        <div className={styles.queueIndicator}>
          {queueCount} message{queueCount !== 1 ? "s" : ""} queued
        </div>
      )}
      {micState === "transcribing" && !partialText && (
        <div className={styles.transcribeStatus}>Transcribing...</div>
      )}
      {transcribeError && <div className={styles.transcribeError}>{transcribeError}</div>}
    </div>
  );
}
