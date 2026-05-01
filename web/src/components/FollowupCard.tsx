import { useState, useEffect, useCallback } from "react";
import { Timer, Check, X } from "lucide-react";
import type { Followup } from "../types";
import { cancelFollowup } from "../api";
import styles from "./FollowupCard.module.css";

function formatCountdown(ms: number): string {
  if (ms <= 0) return "0:00";
  const totalSecs = Math.ceil(ms / 1000);
  const h = Math.floor(totalSecs / 3600);
  const m = Math.floor((totalSecs % 3600) / 60);
  const s = totalSecs % 60;
  if (h > 0) return `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
  return `${m}:${String(s).padStart(2, "0")}`;
}

interface Props {
  followup: Followup;
  workspace: string;
  onCancelled?: (id: string) => void;
  inline?: boolean;
}

export function FollowupCard({ followup, workspace, onCancelled, inline }: Props) {
  const [remaining, setRemaining] = useState(() =>
    new Date(followup.fires_at).getTime() - Date.now()
  );
  const [status, setStatus] = useState(followup.status);

  useEffect(() => {
    setStatus(followup.status);
  }, [followup.status]);

  useEffect(() => {
    if (status !== "pending") return;
    const id = setInterval(() => {
      const ms = new Date(followup.fires_at).getTime() - Date.now();
      setRemaining(ms);
      if (ms <= 0) {
        setStatus("fired");
        clearInterval(id);
      }
    }, 1000);
    return () => clearInterval(id);
  }, [followup.fires_at, status]);

  const handleCancel = useCallback(async () => {
    const res = await cancelFollowup(workspace, followup.id);
    if (res.ok) {
      setStatus("cancelled");
      onCancelled?.(followup.id);
    }
  }, [workspace, followup.id, onCancelled]);

  const cardClass = [
    styles.card,
    status === "fired" ? styles.fired : "",
    status === "cancelled" ? styles.cancelled : "",
    inline ? styles.inline : "",
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <div className={cardClass}>
      <div className={styles.header}>
        {status === "pending" && (
          <>
            <Timer size={14} />
            <span className={styles.timer}>Follow-up in {formatCountdown(remaining)}</span>
          </>
        )}
        {status === "fired" && (
          <>
            <Check size={14} />
            <span className={styles.firedLabel}>Follow-up triggered</span>
          </>
        )}
        {status === "cancelled" && (
          <>
            <X size={14} />
            <span className={styles.cancelledLabel}>Follow-up cancelled</span>
          </>
        )}
      </div>
      <div className={styles.action}>&ldquo;{followup.action}&rdquo;</div>
      {status === "pending" && (
        <div className={styles.footer}>
          <button className={styles.cancelBtn} onClick={handleCancel}>
            Cancel
          </button>
        </div>
      )}
    </div>
  );
}

/** Sticky indicator shown at bottom of chat when follow-up is scrolled out of view */
export function FollowupIndicator({ followup }: { followup: Followup }) {
  const [remaining, setRemaining] = useState(() =>
    new Date(followup.fires_at).getTime() - Date.now()
  );

  useEffect(() => {
    if (followup.status !== "pending") return;
    const id = setInterval(() => {
      setRemaining(new Date(followup.fires_at).getTime() - Date.now());
    }, 1000);
    return () => clearInterval(id);
  }, [followup.fires_at, followup.status]);

  if (followup.status !== "pending" || remaining <= 0) return null;

  return (
    <div className={styles.sticky}>
      <Timer size={12} />
      <span className={styles.stickyTimer}>Follow-up in {formatCountdown(remaining)}</span>
    </div>
  );
}
