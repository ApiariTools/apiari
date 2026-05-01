import { useEffect, useRef, useState, useCallback } from "react";
import { X, Smartphone } from "lucide-react";
import styles from "./SimulatorPanel.module.css";

interface SimulatorStatus {
  booted: boolean;
  device: string | null;
  udid: string | null;
}

interface Props {
  open: boolean;
  onClose: () => void;
}

const SIM_ASPECT = 393 / 852;

export function SimulatorPanel({ open, onClose }: Props) {
  const [status, setStatus] = useState<SimulatorStatus | null>(null);
  const [connected, setConnected] = useState(false);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const wsRef = useRef<WebSocket | null>(null);
  const frameRef = useRef<HTMLDivElement>(null);
  const dragRef = useRef<{ startX: number; startY: number } | null>(null);
  const [ripples, setRipples] = useState<{ id: number; x: number; y: number }[]>([]);
  const rippleId = useRef(0);

  // Check simulator status
  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    const check = () => {
      fetch("/api/simulator/status")
        .then((r) => r.json())
        .then((s: SimulatorStatus) => {
          if (!cancelled) setStatus(s);
        })
        .catch(() => {});
    };
    check();
    const interval = setInterval(check, 5000);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, [open]);

  // Connect WebSocket when open and booted
  useEffect(() => {
    if (!open || !status?.booted) {
      if (wsRef.current) {
        wsRef.current.close();
        wsRef.current = null;
        setConnected(false);
      }
      return;
    }

    const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
    const ws = new WebSocket(`${protocol}//${window.location.host}/api/simulator/stream`);
    ws.binaryType = "arraybuffer";
    wsRef.current = ws;

    ws.onopen = () => setConnected(true);
    ws.onclose = () => setConnected(false);

    ws.onmessage = (event) => {
      if (event.data instanceof ArrayBuffer) {
        const blob = new Blob([event.data], { type: "image/jpeg" });
        const url = URL.createObjectURL(blob);
        const img = new Image();
        img.onload = () => {
          const canvas = canvasRef.current;
          if (canvas) {
            // Only resize canvas when dimensions actually change to avoid costly reallocation
            if (canvas.width !== img.width) canvas.width = img.width;
            if (canvas.height !== img.height) canvas.height = img.height;
            const ctx = canvas.getContext("2d");
            ctx?.drawImage(img, 0, 0);
          }
          URL.revokeObjectURL(url);
        };
        img.src = url;
      }
    };

    return () => {
      ws.close();
      wsRef.current = null;
      setConnected(false);
    };
  }, [open, status?.booted]);

  const sendInput = useCallback((data: Record<string, unknown>) => {
    if (wsRef.current?.readyState === WebSocket.OPEN) {
      wsRef.current.send(JSON.stringify(data));
    }
  }, []);

  const getNormCoords = useCallback((e: React.MouseEvent | React.Touch) => {
    const canvas = canvasRef.current;
    if (!canvas) return null;
    const rect = canvas.getBoundingClientRect();
    return {
      x: Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width)),
      y: Math.max(0, Math.min(1, (e.clientY - rect.top) / rect.height)),
    };
  }, []);

  const addRipple = useCallback((clientX: number, clientY: number) => {
    const frame = frameRef.current;
    if (!frame) return;
    const rect = frame.getBoundingClientRect();
    const id = ++rippleId.current;
    setRipples((prev) => [
      ...prev,
      { id, x: clientX - rect.left, y: clientY - rect.top },
    ]);
    setTimeout(() => {
      setRipples((prev) => prev.filter((r) => r.id !== id));
    }, 400);
  }, []);

  // Mouse events
  const handleMouseDown = useCallback((e: React.MouseEvent) => {
    const coords = getNormCoords(e);
    if (coords) {
      dragRef.current = { startX: coords.x, startY: coords.y };
    }
  }, [getNormCoords]);

  const handleMouseUp = useCallback((e: React.MouseEvent) => {
    const coords = getNormCoords(e);
    if (!coords || !dragRef.current) return;

    const dx = Math.abs(coords.x - dragRef.current.startX);
    const dy = Math.abs(coords.y - dragRef.current.startY);

    if (dx < 0.02 && dy < 0.02) {
      // Tap
      sendInput({ type: "tap", x: coords.x, y: coords.y });
      addRipple(e.clientX, e.clientY);
    } else {
      // Swipe
      sendInput({
        type: "swipe",
        fromX: dragRef.current.startX,
        fromY: dragRef.current.startY,
        toX: coords.x,
        toY: coords.y,
      });
    }
    dragRef.current = null;
  }, [getNormCoords, sendInput, addRipple]);

  // Touch events
  const handleTouchStart = useCallback((e: React.TouchEvent) => {
    if (e.touches.length !== 1) return;
    const coords = getNormCoords(e.touches[0]);
    if (coords) {
      dragRef.current = { startX: coords.x, startY: coords.y };
    }
  }, [getNormCoords]);

  const handleTouchEnd = useCallback((e: React.TouchEvent) => {
    if (e.changedTouches.length !== 1 || !dragRef.current) return;
    const touch = e.changedTouches[0];
    const coords = getNormCoords(touch);
    if (!coords) return;

    const dx = Math.abs(coords.x - dragRef.current.startX);
    const dy = Math.abs(coords.y - dragRef.current.startY);

    if (dx < 0.02 && dy < 0.02) {
      sendInput({ type: "tap", x: coords.x, y: coords.y });
      addRipple(touch.clientX, touch.clientY);
    } else {
      sendInput({
        type: "swipe",
        fromX: dragRef.current.startX,
        fromY: dragRef.current.startY,
        toX: coords.x,
        toY: coords.y,
      });
    }
    dragRef.current = null;
  }, [getNormCoords, sendInput, addRipple]);

  // Keyboard events
  const handleKeyDown = useCallback((e: React.KeyboardEvent) => {
    // Don't capture if modifier keys are held (except shift for typing)
    if (e.metaKey || e.ctrlKey || e.altKey) return;

    e.preventDefault();
    e.stopPropagation();

    const specialKeys: Record<string, string> = {
      Enter: "return",
      Backspace: "delete",
      Delete: "forwardDelete",
      Escape: "escape",
      Tab: "tab",
      ArrowUp: "upArrow",
      ArrowDown: "downArrow",
      ArrowLeft: "leftArrow",
      ArrowRight: "rightArrow",
    };

    if (specialKeys[e.key]) {
      sendInput({ type: "key", key: specialKeys[e.key] });
    } else if (e.key.length === 1) {
      sendInput({ type: "type", text: e.key });
    }
  }, [sendInput]);

  // Compute canvas display size
  const [canvasStyle, setCanvasStyle] = useState<React.CSSProperties>({});
  useEffect(() => {
    if (!open) return;
    const update = () => {
      const frame = frameRef.current;
      if (!frame) return;
      const parent = frame.parentElement;
      if (!parent) return;
      const availW = parent.clientWidth - 48; // 16px padding + 12px phone padding each side
      const availH = parent.clientHeight - 48;
      const w = Math.min(availW, availH * SIM_ASPECT);
      const h = w / SIM_ASPECT;
      setCanvasStyle({ width: w, height: h });
    };
    update();
    window.addEventListener("resize", update);
    return () => window.removeEventListener("resize", update);
  }, [open]);

  return (
    <>
      {open && <div className={styles.backdrop} onClick={onClose} />}
      <div className={`${styles.panel} ${open ? styles.panelOpen : ""}`}>
        <div className={styles.header}>
          <div className={styles.title}>
            <span
              className={styles.statusDot}
              style={{
                background: connected
                  ? "var(--green)"
                  : status?.booted
                    ? "var(--accent)"
                    : "var(--text-faint)",
              }}
            />
            {status?.device || "Simulator"}
          </div>
          <button className={styles.closeBtn} onClick={onClose} aria-label="Close simulator">
            <X size={16} />
          </button>
        </div>
        <div className={styles.body}>
          {!status?.booted ? (
            <div className={styles.noSim}>
              <Smartphone size={32} />
              <div>No simulator running</div>
              <div style={{ fontSize: 12 }}>
                Start a simulator with Xcode or{" "}
                <code>xcrun simctl boot</code>
              </div>
            </div>
          ) : (
            <div className={styles.phoneFrame} ref={frameRef}>
              <canvas
                ref={canvasRef}
                className={styles.canvas}
                style={canvasStyle}
                tabIndex={0}
                onMouseDown={handleMouseDown}
                onMouseUp={handleMouseUp}
                onTouchStart={handleTouchStart}
                onTouchEnd={handleTouchEnd}
                onKeyDown={handleKeyDown}
              />
              {ripples.map((r) => (
                <div
                  key={r.id}
                  className={styles.ripple}
                  style={{ left: r.x, top: r.y }}
                />
              ))}
            </div>
          )}
        </div>
      </div>
    </>
  );
}
