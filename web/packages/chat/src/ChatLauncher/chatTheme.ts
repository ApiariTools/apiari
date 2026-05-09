export interface ChatTheme {
  accent?: string;
  bg?: string;
  bgWindow?: string;
  bgInput?: string;
  border?: string;
  text?: string;
  textStrong?: string;
  textFaint?: string;
  radius?: string;
  radiusWindow?: string;
  launcherSize?: string;
  windowWidth?: string;
  windowHeight?: string;
  fontFamily?: string;
  zIndex?: number;
}

const defaults: Required<ChatTheme> = {
  accent: "#f5c542",
  bg: "#191919",
  bgWindow: "#111111",
  bgInput: "#191919",
  border: "#282828",
  text: "#aaaaaa",
  textStrong: "#eeeeee",
  textFaint: "#555555",
  radius: "12px",
  radiusWindow: "16px",
  launcherSize: "56px",
  windowWidth: "360px",
  windowHeight: "480px",
  fontFamily: "system-ui, -apple-system, sans-serif",
  zIndex: 2000000,
};

export function buildThemeVars(theme?: ChatTheme): React.CSSProperties {
  const t = { ...defaults, ...theme };
  return {
    "--cl-accent": t.accent,
    "--cl-bg": t.bg,
    "--cl-bg-window": t.bgWindow,
    "--cl-bg-input": t.bgInput,
    "--cl-border": t.border,
    "--cl-text": t.text,
    "--cl-text-strong": t.textStrong,
    "--cl-text-faint": t.textFaint,
    "--cl-radius": t.radius,
    "--cl-radius-window": t.radiusWindow,
    "--cl-launcher-size": t.launcherSize,
    "--cl-window-width": t.windowWidth,
    "--cl-window-height": t.windowHeight,
    "--cl-font": t.fontFamily,
    "--cl-z": String(t.zIndex),
  } as React.CSSProperties;
}
