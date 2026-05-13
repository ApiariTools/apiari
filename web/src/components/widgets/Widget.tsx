import React, { useState } from "react";
import ReactMarkdown from "react-markdown";
import { ExternalLink, Info } from "lucide-react";
import type {
  DashboardWidget,
  WidgetStatus,
  WidgetSeverity,
  StatWidget,
  StatRowWidget,
  ListWidget,
  StatusGridWidget,
  ActivityFeedWidget,
  ProgressWidget,
  SparklineWidget,
  BarChartWidget,
  AlertBannerWidget,
  LinkCollectionWidget,
  MarkdownBlockWidget,
  DonutWidget,
} from "@apiari/types";

interface WidgetBase {
  slot: string;
  title: string;
  updated_at?: string;
  href?: string;
  source?: string;
  editable?: boolean;
}
import styles from "./Widget.module.css";

// ── Helpers ───────────────────────────────────────────────────────────────

function formatRelative(iso: string): string {
  try {
    const normalized = /[-Z+]\d*$/.test(iso.trim()) ? iso : iso.trim() + "Z";
    const d = new Date(normalized);
    if (isNaN(d.getTime())) return "";
    const diffMs = Date.now() - d.getTime();
    const diffMins = Math.floor(diffMs / 60000);
    if (diffMins < 1) return "just now";
    if (diffMins < 60) return `${diffMins}m ago`;
    const diffHours = Math.floor(diffMins / 60);
    if (diffHours < 24) return `${diffHours}h ago`;
    return `${Math.floor(diffHours / 24)}d ago`;
  } catch {
    return "";
  }
}

function statusColor(status?: WidgetStatus): string {
  switch (status) {
    case "ok":
      return "var(--status-running)";
    case "running":
      return "var(--status-running)";
    case "warning":
      return "var(--status-waiting)";
    case "pending":
      return "var(--status-waiting)";
    case "error":
      return "var(--status-failed)";
    default:
      return "var(--text-faint)";
  }
}

function severityColor(s: WidgetSeverity): string {
  switch (s) {
    case "error":
      return "var(--status-failed)";
    case "warning":
      return "var(--status-waiting)";
    default:
      return "#4a9eff";
  }
}

function trendColor(dir?: string): string {
  if (dir === "up") return "var(--status-running)";
  if (dir === "down") return "var(--status-failed)";
  return "var(--text-faint)";
}

// ── Card shell ────────────────────────────────────────────────────────────

function Card({
  title,
  description,
  updated_at,
  href,
  children,
  alert,
  slot,
  source,
  editable,
}: {
  title: string;
  description?: string;
  updated_at?: string;
  href?: string;
  children: React.ReactNode;
  alert?: string;
  slot?: string;
  source?: string;
  editable?: boolean;
}) {
  const [infoOpen, setInfoOpen] = useState(false);

  return (
    <div
      className={styles.card}
      style={alert ? { borderLeftColor: alert, borderLeftWidth: 3 } : undefined}
    >
      <div className={styles.cardHeader}>
        {href ? (
          <a href={href} target="_blank" rel="noopener noreferrer" className={styles.cardTitleLink}>
            {title} <ExternalLink size={11} />
          </a>
        ) : (
          <span className={styles.cardTitle}>{title}</span>
        )}
        <div className={styles.cardHeaderRight}>
          {updated_at && <span className={styles.cardAge}>{formatRelative(updated_at)}</span>}
          <div className={styles.infoWrap}>
            <button
              className={styles.infoBtn}
              onClick={() => setInfoOpen((o) => !o)}
              title="Widget info"
              type="button"
            >
              <Info size={12} />
            </button>
            {infoOpen && (
              <div className={styles.infoPopover}>
                {slot && (
                  <div className={styles.infoRow}>
                    <span className={styles.infoLabel}>Slot</span>
                    <span className={styles.infoValue}>{slot}</span>
                  </div>
                )}
                {source && (
                  <div className={styles.infoRow}>
                    <span className={styles.infoLabel}>Source</span>
                    <span className={styles.infoValue}>{source}</span>
                  </div>
                )}
                {updated_at && (
                  <div className={styles.infoRow}>
                    <span className={styles.infoLabel}>Updated</span>
                    <span className={styles.infoValue}>{formatRelative(updated_at)}</span>
                  </div>
                )}
                {editable && (
                  <div className={styles.infoEditable}>Configurable in workspace settings</div>
                )}
                {!source && !editable && (
                  <div className={styles.infoRow}>
                    <span className={styles.infoLabel}>Source</span>
                    <span className={styles.infoValue}>System</span>
                  </div>
                )}
              </div>
            )}
          </div>
        </div>
      </div>
      {description && <p className={styles.cardDescription}>{description}</p>}
      {children}
    </div>
  );
}

// ── Stat ─────────────────────────────────────────────────────────────────

function meta(w: WidgetBase) {
  return {
    slot: w.slot,
    description: w.description,
    source: w.source,
    editable: w.editable,
    updated_at: w.updated_at,
    href: w.href,
  };
}

function StatWidgetRenderer({ w }: { w: StatWidget }) {
  const color = statusColor(w.status);
  return (
    <Card title={w.title} {...meta(w)}>
      <div className={styles.statValue} style={{ color }}>
        {w.value}
        {w.unit && <span className={styles.statUnit}>{w.unit}</span>}
      </div>
      {w.trend && (
        <div className={styles.statTrend} style={{ color: trendColor(w.trend_direction) }}>
          {w.trend}
        </div>
      )}
    </Card>
  );
}

// ── Stat row ─────────────────────────────────────────────────────────────

function StatRowWidgetRenderer({ w }: { w: StatRowWidget }) {
  return (
    <Card title={w.title} {...meta(w)}>
      <div className={styles.statRow}>
        {w.stats.map((s, i) => (
          <div key={i} className={styles.statRowCell}>
            <span className={styles.statRowValue} style={{ color: statusColor(s.status) }}>
              {s.value}
            </span>
            <span className={styles.statRowLabel}>{s.label}</span>
          </div>
        ))}
      </div>
    </Card>
  );
}

// ── List ─────────────────────────────────────────────────────────────────

function ListWidgetRenderer({ w }: { w: ListWidget }) {
  return (
    <Card title={w.title} {...meta(w)}>
      {w.items.length === 0 ? (
        <p className={styles.emptyMsg}>{w.empty_message ?? "Nothing here"}</p>
      ) : (
        <div className={styles.list}>
          {w.items.map((item) => {
            const row = (
              <div className={styles.listRow} key={item.id}>
                <span className={styles.listDot} style={{ background: statusColor(item.status) }} />
                <div className={styles.listLabel}>
                  <span className={styles.listName}>{item.label}</span>
                  {item.meta && <span className={styles.listMeta}>{item.meta}</span>}
                </div>
                {item.right && <span className={styles.listRight}>{item.right}</span>}
                {item.href && <ExternalLink size={11} className={styles.listExternal} />}
              </div>
            );
            return item.href ? (
              <a
                key={item.id}
                href={item.href}
                target="_blank"
                rel="noopener noreferrer"
                className={styles.listLink}
              >
                {row}
              </a>
            ) : (
              row
            );
          })}
        </div>
      )}
    </Card>
  );
}

// ── Status grid ───────────────────────────────────────────────────────────

function StatusGridWidgetRenderer({ w }: { w: StatusGridWidget }) {
  return (
    <Card title={w.title} {...meta(w)}>
      <div className={styles.grid}>
        {w.items.map((item, i) => {
          const color = statusColor(item.status);
          const pill = (
            <div key={i} className={styles.gridPill}>
              <span className={styles.gridDot} style={{ background: color }} />
              <span className={styles.gridLabel}>{item.label}</span>
            </div>
          );
          return item.href ? (
            <a
              key={i}
              href={item.href}
              target="_blank"
              rel="noopener noreferrer"
              className={styles.gridLink}
            >
              {pill}
            </a>
          ) : (
            pill
          );
        })}
      </div>
    </Card>
  );
}

// ── Activity feed ─────────────────────────────────────────────────────────

function ActivityFeedWidgetRenderer({ w }: { w: ActivityFeedWidget }) {
  return (
    <Card title={w.title} {...meta(w)}>
      <div className={styles.feed}>
        {w.items.map((item) => (
          <div key={item.id} className={styles.feedRow}>
            <span className={styles.feedDot} style={{ background: statusColor(item.kind) }} />
            <div className={styles.feedBody}>
              {item.actor && (
                <span
                  className={styles.feedActor}
                  style={{ color: item.actor_color ?? "var(--text-faint)" }}
                >
                  {item.actor}
                </span>
              )}
              {item.href ? (
                <a
                  href={item.href}
                  target="_blank"
                  rel="noopener noreferrer"
                  className={styles.feedEvent}
                >
                  {item.event}
                </a>
              ) : (
                <span className={styles.feedEvent}>{item.event}</span>
              )}
            </div>
            <span className={styles.feedTime}>{formatRelative(item.timestamp)}</span>
          </div>
        ))}
      </div>
    </Card>
  );
}

// ── Progress ──────────────────────────────────────────────────────────────

function ProgressWidgetRenderer({ w }: { w: ProgressWidget }) {
  const color = statusColor(w.status);
  const pct = Math.min(100, Math.max(0, w.percent));
  return (
    <Card title={w.title} {...meta(w)}>
      <div className={styles.progressLabel}>
        <span>{w.label}</span>
        <span style={{ color }}>{pct}%</span>
      </div>
      <div className={styles.progressTrack}>
        <div className={styles.progressFill} style={{ width: `${pct}%`, background: color }} />
      </div>
      {w.sublabel && <div className={styles.progressSublabel}>{w.sublabel}</div>}
    </Card>
  );
}

// ── Sparkline ─────────────────────────────────────────────────────────────

function Sparkline({ points, color }: { points: number[]; color: string }) {
  if (points.length < 2) return null;
  const w = 200,
    h = 40;
  const min = Math.min(...points);
  const max = Math.max(...points);
  const range = max - min || 1;
  const xs = points.map((_, i) => (i / (points.length - 1)) * w);
  const ys = points.map((v) => h - ((v - min) / range) * (h - 4) - 2);
  const d = xs.map((x, i) => `${i === 0 ? "M" : "L"}${x.toFixed(1)},${ys[i].toFixed(1)}`).join(" ");
  const areaD = `${d} L${w},${h} L0,${h} Z`;
  return (
    <svg viewBox={`0 0 ${w} ${h}`} className={styles.sparklineSvg} preserveAspectRatio="none">
      <defs>
        <linearGradient id="sg" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0%" stopColor={color} stopOpacity="0.15" />
          <stop offset="100%" stopColor={color} stopOpacity="0" />
        </linearGradient>
      </defs>
      <path d={areaD} fill="url(#sg)" />
      <path
        d={d}
        fill="none"
        stroke={color}
        strokeWidth="1.5"
        strokeLinejoin="round"
        strokeLinecap="round"
      />
    </svg>
  );
}

function SparklineWidgetRenderer({ w }: { w: SparklineWidget }) {
  const color = statusColor(w.status);
  return (
    <Card title={w.title} {...meta(w)}>
      <div className={styles.sparklineHeader}>
        <span className={styles.sparklineValue} style={{ color }}>
          {w.value}
          {w.unit && <span className={styles.statUnit}>{w.unit}</span>}
        </span>
        {w.trend && (
          <span className={styles.statTrend} style={{ color: trendColor(w.trend_direction) }}>
            {w.trend}
          </span>
        )}
      </div>
      <Sparkline points={w.points} color={color} />
      {w.x_labels && w.x_labels.length >= 2 && (
        <div className={styles.sparklineLabels}>
          <span>{w.x_labels[0]}</span>
          <span>{w.x_labels[w.x_labels.length - 1]}</span>
        </div>
      )}
    </Card>
  );
}

// ── Bar chart ─────────────────────────────────────────────────────────────

function BarChartWidgetRenderer({ w }: { w: BarChartWidget }) {
  const max = Math.max(...w.bars.map((b) => b.value), 1);
  return (
    <Card title={w.title} {...meta(w)}>
      <div className={styles.bars}>
        {w.bars.map((bar, i) => {
          const pct = (bar.value / max) * 100;
          const color = statusColor(bar.status);
          return (
            <div key={i} className={styles.barRow}>
              <span className={styles.barLabel}>{bar.label}</span>
              <div className={styles.barTrack}>
                <div className={styles.barFill} style={{ width: `${pct}%`, background: color }} />
              </div>
              <span className={styles.barValue} style={{ color }}>
                {bar.value}
              </span>
            </div>
          );
        })}
      </div>
    </Card>
  );
}

// ── Alert banner ──────────────────────────────────────────────────────────

function AlertBannerWidgetRenderer({ w }: { w: AlertBannerWidget }) {
  const color = severityColor(w.severity);
  return (
    <Card title={w.title} {...meta(w)} alert={color}>
      <p className={styles.alertBody}>{w.body}</p>
      {w.action_href && w.action_label && (
        <a
          href={w.action_href}
          target="_blank"
          rel="noopener noreferrer"
          className={styles.alertAction}
          style={{ color }}
        >
          {w.action_label} <ExternalLink size={11} />
        </a>
      )}
    </Card>
  );
}

// ── Link collection ───────────────────────────────────────────────────────

function LinkCollectionWidgetRenderer({ w }: { w: LinkCollectionWidget }) {
  return (
    <Card title={w.title} {...meta(w)}>
      <div className={styles.linkGroups}>
        {w.groups.map((g, i) => (
          <div key={i} className={styles.linkGroup}>
            {g.label && <span className={styles.linkGroupLabel}>{g.label}</span>}
            {g.links.map((link, j) => (
              <a
                key={j}
                href={link.href}
                target="_blank"
                rel="noopener noreferrer"
                className={styles.linkRow}
              >
                <span className={styles.linkName}>{link.label}</span>
                {link.meta && <span className={styles.linkMeta}>{link.meta}</span>}
                <ExternalLink size={11} className={styles.linkExternal} />
              </a>
            ))}
          </div>
        ))}
      </div>
    </Card>
  );
}

// ── Markdown block ────────────────────────────────────────────────────────

function MarkdownBlockWidgetRenderer({ w }: { w: MarkdownBlockWidget }) {
  return (
    <Card title={w.title} {...meta(w)}>
      <div className={styles.markdown}>
        <ReactMarkdown>{w.content}</ReactMarkdown>
      </div>
    </Card>
  );
}

// ── Donut ─────────────────────────────────────────────────────────────────

function DonutWidgetRenderer({ w }: { w: DonutWidget }) {
  const total = w.segments.reduce((s, seg) => s + seg.value, 0) || 1;
  const r = 44,
    cx = 56,
    cy = 56,
    circumference = 2 * Math.PI * r;
  const paths = w.segments.reduce<{ elems: React.ReactNode[]; offset: number }>(
    ({ elems, offset }, seg, i) => {
      const dash = (seg.value / total) * circumference;
      const gap = circumference - dash;
      return {
        elems: [
          ...elems,
          <circle
            key={i}
            cx={cx}
            cy={cy}
            r={r}
            fill="none"
            stroke={seg.color}
            strokeWidth="10"
            strokeDasharray={`${dash} ${gap}`}
            strokeDashoffset={-offset}
            style={{ transform: "rotate(-90deg)", transformOrigin: `${cx}px ${cy}px` }}
          />,
        ],
        offset: offset + dash,
      };
    },
    { elems: [], offset: 0 },
  ).elems;

  return (
    <Card title={w.title} {...meta(w)}>
      <div className={styles.donut}>
        <svg viewBox="0 0 112 112" className={styles.donutSvg}>
          <circle cx={cx} cy={cy} r={r} fill="none" stroke="var(--bg-elevated)" strokeWidth="10" />
          {paths}
          {w.total_label && (
            <text x={cx} y={cy + 5} textAnchor="middle" className={styles.donutLabel}>
              {w.total_label}
            </text>
          )}
        </svg>
        <div className={styles.donutLegend}>
          {w.segments.map((seg, i) => (
            <div key={i} className={styles.donutLegendRow}>
              <span className={styles.donutLegendDot} style={{ background: seg.color }} />
              <span className={styles.donutLegendName}>{seg.label}</span>
              <span className={styles.donutLegendValue}>{seg.value}</span>
            </div>
          ))}
        </div>
      </div>
    </Card>
  );
}

// ── Main dispatcher ───────────────────────────────────────────────────────

export default function Widget({ widget }: { widget: DashboardWidget }) {
  switch (widget.type) {
    case "stat":
      return <StatWidgetRenderer w={widget} />;
    case "stat_row":
      return <StatRowWidgetRenderer w={widget} />;
    case "list":
      return <ListWidgetRenderer w={widget} />;
    case "status_grid":
      return <StatusGridWidgetRenderer w={widget} />;
    case "activity_feed":
      return <ActivityFeedWidgetRenderer w={widget} />;
    case "progress":
      return <ProgressWidgetRenderer w={widget} />;
    case "sparkline":
      return <SparklineWidgetRenderer w={widget} />;
    case "bar_chart":
      return <BarChartWidgetRenderer w={widget} />;
    case "alert_banner":
      return <AlertBannerWidgetRenderer w={widget} />;
    case "link_collection":
      return <LinkCollectionWidgetRenderer w={widget} />;
    case "markdown_block":
      return <MarkdownBlockWidgetRenderer w={widget} />;
    case "donut":
      return <DonutWidgetRenderer w={widget} />;
    default:
      return null;
  }
}
