/**
 * Voice mode sound cues using Web Audio API.
 * Polished, warm tones using layered harmonics, filters, and ADSR envelopes.
 * No audio files needed.
 */

let audioCtx: AudioContext | null = null;

/** Allow voice mode to inject Howler's unlocked AudioContext (iOS gesture requirement) */
export function setSharedAudioContext(ctx: AudioContext) {
  audioCtx = ctx;
}

function getCtx(): AudioContext | null {
  if (!audioCtx) {
    try {
      audioCtx = new AudioContext();
    } catch {
      return null;
    }
  }
  if (audioCtx.state === "suspended") {
    audioCtx.resume().catch(() => {});
  }
  return audioCtx;
}

/** Cached reverb impulse buffer — reused across all playTone calls. */
let reverbBuffer: AudioBuffer | null = null;

function getReverbBuffer(ctx: AudioContext): AudioBuffer {
  if (reverbBuffer && reverbBuffer.sampleRate === ctx.sampleRate) return reverbBuffer;
  const sampleRate = ctx.sampleRate;
  const length = sampleRate * 0.4;
  const impulse = ctx.createBuffer(2, length, sampleRate);
  for (let ch = 0; ch < 2; ch++) {
    const data = impulse.getChannelData(ch);
    for (let i = 0; i < length; i++) {
      data[i] = (Math.random() * 2 - 1) * Math.pow(1 - i / length, 3);
    }
  }
  reverbBuffer = impulse;
  return impulse;
}

/** Play a layered tone with harmonics, filter, and reverb. */
function playTone(
  ctx: AudioContext,
  freq: number,
  startTime: number,
  duration: number,
  opts: {
    type?: OscillatorType;
    harmonics?: { ratio: number; gain: number; type?: OscillatorType }[];
    attack?: number;
    decay?: number;
    sustain?: number;
    release?: number;
    peakGain?: number;
    filterFreq?: number;
    filterQ?: number;
    reverbMix?: number;
  } = {}
): void {
  const {
    type = "sine",
    harmonics = [],
    attack = 0.01,
    decay = 0.05,
    sustain = 0.3,
    release = 0.1,
    peakGain = 0.12,
    filterFreq = 3000,
    filterQ = 1,
    reverbMix = 0.15,
  } = opts;

  const master = ctx.createGain();
  const filter = ctx.createBiquadFilter();
  filter.type = "lowpass";
  filter.frequency.value = filterFreq;
  filter.Q.value = filterQ;

  // Dry/wet reverb mix
  const dryGain = ctx.createGain();
  const wetGain = ctx.createGain();
  dryGain.gain.value = 1 - reverbMix;
  wetGain.gain.value = reverbMix;

  const reverb = ctx.createConvolver();
  reverb.buffer = getReverbBuffer(ctx);

  filter.connect(dryGain);
  filter.connect(reverb);
  reverb.connect(wetGain);
  dryGain.connect(master);
  wetGain.connect(master);
  master.connect(ctx.destination);

  // ADSR envelope
  const sustainTime = duration - attack - decay - release;
  const sustainLevel = peakGain * sustain;
  master.gain.setValueAtTime(0, startTime);
  master.gain.linearRampToValueAtTime(peakGain, startTime + attack);
  master.gain.linearRampToValueAtTime(sustainLevel, startTime + attack + decay);
  if (sustainTime > 0) {
    master.gain.setValueAtTime(sustainLevel, startTime + attack + decay + sustainTime);
  }
  master.gain.linearRampToValueAtTime(0, startTime + duration);

  // Collect all nodes for cleanup
  const allNodes: AudioNode[] = [master, filter, dryGain, wetGain, reverb];

  // Fundamental
  const osc = ctx.createOscillator();
  osc.type = type;
  osc.frequency.value = freq;
  osc.connect(filter);
  osc.start(startTime);
  osc.stop(startTime + duration + 0.1);
  allNodes.push(osc);

  // Harmonics
  for (const h of harmonics) {
    const hOsc = ctx.createOscillator();
    const hGain = ctx.createGain();
    hOsc.type = h.type || "sine";
    hOsc.frequency.value = freq * h.ratio;
    hGain.gain.value = h.gain;
    hOsc.connect(hGain);
    hGain.connect(filter);
    hOsc.start(startTime);
    hOsc.stop(startTime + duration + 0.1);
    allNodes.push(hOsc, hGain);
  }

  // Disconnect all nodes after sound finishes to prevent leaks
  osc.onended = () => {
    for (const node of allNodes) node.disconnect();
  };
}

/**
 * Satisfying warm "sent" confirmation — like iMessage send.
 * Two layered chime tones with quick attack and smooth decay. ~300ms.
 */
export function playSentCue() {
  const ctx = getCtx();
  if (!ctx) return;

  const t = ctx.currentTime;

  // Primary tone — C6 (1047 Hz) with octave + fifth harmonics
  playTone(ctx, 1047, t, 0.3, {
    type: "sine",
    harmonics: [
      { ratio: 2, gain: 0.3, type: "sine" },     // octave up
      { ratio: 1.5, gain: 0.15, type: "triangle" }, // fifth
    ],
    attack: 0.008,
    decay: 0.08,
    sustain: 0.15,
    release: 0.2,
    peakGain: 0.13,
    filterFreq: 4000,
    filterQ: 0.7,
    reverbMix: 0.2,
  });

  // Second tone — E6 (1319 Hz), slight delay for shimmer
  playTone(ctx, 1319, t + 0.04, 0.25, {
    type: "sine",
    harmonics: [
      { ratio: 2, gain: 0.2, type: "sine" },
    ],
    attack: 0.01,
    decay: 0.06,
    sustain: 0.1,
    release: 0.18,
    peakGain: 0.08,
    filterFreq: 3500,
    filterQ: 0.5,
    reverbMix: 0.25,
  });
}

/**
 * Gentle ambient pulse repeating every 1.5s while waiting.
 * Very subtle filtered tone with slow attack/release, alternating pitches.
 * Returns a stop function.
 */
export function startThinkingCue(): () => void {
  const ctx = getCtx();
  if (!ctx) return () => {};

  let stopped = false;
  let timeoutId: ReturnType<typeof setTimeout>;
  let pulseCount = 0;

  function pulse() {
    if (stopped || !ctx) return;

    const t = ctx.currentTime;
    // Alternate between two gentle pitches (G4 and A4)
    const freq = pulseCount % 2 === 0 ? 392 : 440;

    playTone(ctx, freq, t, 0.8, {
      type: "sine",
      harmonics: [
        { ratio: 2, gain: 0.15, type: "sine" },
      ],
      attack: 0.15,
      decay: 0.2,
      sustain: 0.2,
      release: 0.35,
      peakGain: 0.04,
      filterFreq: 1200,
      filterQ: 0.5,
      reverbMix: 0.3,
    });

    pulseCount++;
    timeoutId = setTimeout(pulse, 1500);
  }

  pulse();
  return () => {
    stopped = true;
    clearTimeout(timeoutId);
  };
}

/**
 * Brief warm notification before bot speaks — gentle descending "ding-dong".
 * Two notes: higher then lower, like a soft marimba tap. ~250ms.
 */
export function playSpeakingCue() {
  const ctx = getCtx();
  if (!ctx) return;

  const t = ctx.currentTime;

  // First note — E5 (659 Hz)
  playTone(ctx, 659, t, 0.18, {
    type: "triangle",
    harmonics: [
      { ratio: 2, gain: 0.25, type: "sine" },
      { ratio: 3, gain: 0.08, type: "sine" },
    ],
    attack: 0.005,
    decay: 0.05,
    sustain: 0.2,
    release: 0.1,
    peakGain: 0.1,
    filterFreq: 2500,
    filterQ: 1.2,
    reverbMix: 0.2,
  });

  // Second note — C5 (523 Hz), descending interval
  playTone(ctx, 523, t + 0.12, 0.2, {
    type: "triangle",
    harmonics: [
      { ratio: 2, gain: 0.2, type: "sine" },
      { ratio: 3, gain: 0.06, type: "sine" },
    ],
    attack: 0.005,
    decay: 0.06,
    sustain: 0.15,
    release: 0.12,
    peakGain: 0.09,
    filterFreq: 2200,
    filterQ: 1.0,
    reverbMix: 0.25,
  });
}
