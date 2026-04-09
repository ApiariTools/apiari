import { useState, useRef, useEffect } from 'react';
import type { BeeConfigView, TaskView } from '../types';

// ── Types ──────────────────────────────────────────────────────────────

interface FeedItem {
  id: string;
  type: 'decision' | 'info' | 'chat';
  priority: 'red' | 'yellow' | 'muted';
  bee: string;
  workspace: string;
  title: string;
  body?: string;
  actions?: Action[];
  timestamp: Date;
}

interface Action {
  label: string;
  style: 'primary' | 'default' | 'danger';
  onClick: () => void;
}

interface HiveEntry {
  workspace: string;
  bee: string;
  workerCount: number;
  status: string; // "2 workers" or "idle" or "last checked 2m ago"
  isActive: boolean;
}

interface ChatMessage {
  id: string;
  bee: string;
  workspace: string;
  role: 'user' | 'assistant';
  text: string;
  timestamp: Date;
}

interface SignalItem {
  id: number;
  workspace: string;
  source: string;
  title: string;
  severity: string;
  url?: string | null;
  created_at: string;
}

interface BriefingProps {
  workspaces: string[];
  beesByWorkspace: Record<string, BeeConfigView[]>;
  tasks: TaskView[];
  signals: SignalItem[];
  chatMessages: ChatMessage[];
  connected: boolean;
  onSendMessage: (bee: string, workspace: string, text: string) => void;
  onDrillIntoTask: (taskId: string) => void;
}

// ── Helpers ─────────────────────────────────────────────────────────────

function buildHive(
  workspaces: string[],
  beesByWorkspace: Record<string, BeeConfigView[]>,
  tasks: TaskView[],
): HiveEntry[] {
  const entries: HiveEntry[] = [];
  for (const ws of workspaces) {
    const bees = beesByWorkspace[ws] ?? [];
    for (const bee of bees) {
      const workerCount = tasks.filter(
        (t) => t.stage !== 'Merged' && t.stage !== 'Dismissed'
      ).length;
      entries.push({
        workspace: ws,
        bee: bee.name,
        workerCount: bee.name.toLowerCase().includes('code') ? workerCount : 0,
        status: workerCount > 0 ? `${workerCount} active` : 'idle',
        isActive: workerCount > 0,
      });
    }
  }
  return entries;
}

function buildFeed(tasks: TaskView[]): FeedItem[] {
  const items: FeedItem[] = [];

  for (const task of tasks) {
    const isWaiting = task.stage === 'HumanReview' || task.stage === 'Human Review';
    const isInProgress = task.stage === 'InProgress' || task.stage === 'In Progress';
    const isDone = task.stage === 'Merged' || task.stage === 'Dismissed';

    if (isWaiting) {
      items.push({
        id: task.id,
        type: 'decision',
        priority: 'red',
        bee: 'Architect',
        workspace: '',
        title: task.title || 'PR ready for review',
        body: task.pr_url ? `PR: ${task.pr_url}` : undefined,
        actions: [
          { label: 'Review', style: 'primary', onClick: () => {} },
          { label: 'Snooze', style: 'default', onClick: () => {} },
          { label: 'Dismiss', style: 'danger', onClick: () => {} },
        ],
        timestamp: new Date(task.updated_at),
      });
    } else if (isInProgress) {
      items.push({
        id: task.id,
        type: 'info',
        priority: 'muted',
        bee: 'Architect',
        workspace: '',
        title: task.title || 'Working...',
        body: task.worker_id ? `Worker: ${task.worker_id}` : undefined,
        timestamp: new Date(task.updated_at),
      });
    } else if (isDone) {
      items.push({
        id: task.id,
        type: 'info',
        priority: 'muted',
        bee: 'Architect',
        workspace: '',
        title: `✓ ${task.title || 'Completed'}`,
        timestamp: new Date(task.updated_at),
      });
    }
  }

  // Sort: decisions first, then by time
  items.sort((a, b) => {
    if (a.type === 'decision' && b.type !== 'decision') return -1;
    if (a.type !== 'decision' && b.type === 'decision') return 1;
    return b.timestamp.getTime() - a.timestamp.getTime();
  });

  return items;
}

function timeAgo(date: Date): string {
  const secs = Math.floor((Date.now() - date.getTime()) / 1000);
  if (secs < 60) return 'just now';
  if (secs < 3600) return `${Math.floor(secs / 60)}m ago`;
  if (secs < 86400) return `${Math.floor(secs / 3600)}h ago`;
  return `${Math.floor(secs / 86400)}d ago`;
}

// ── Component ──────────────────────────────────────────────────────────

export default function Briefing({
  workspaces,
  beesByWorkspace,
  tasks,
  signals,
  chatMessages,
  connected,
  onSendMessage,
  onDrillIntoTask,
}: BriefingProps) {
  const [input, setInput] = useState('');
  const [targetBee, setTargetBee] = useState('');
  const [targetWorkspace, setTargetWorkspace] = useState(workspaces[0] ?? '');
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const feedEndRef = useRef<HTMLDivElement>(null);

  // Auto-scroll to bottom when messages change
  useEffect(() => {
    feedEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [chatMessages.length]);

  const hive = buildHive(workspaces, beesByWorkspace, tasks);
  const feed = buildFeed(tasks);
  const decisions = feed.filter((f) => f.type === 'decision');
  const quiet = feed.filter((f) => f.type !== 'decision');

  // All bees across all workspaces for the input selector
  const allBees: { workspace: string; name: string }[] = [];
  for (const ws of workspaces) {
    for (const bee of beesByWorkspace[ws] ?? []) {
      allBees.push({ workspace: ws, name: bee.name });
    }
  }

  // Default target bee
  useEffect(() => {
    if (!targetBee && allBees.length > 0) {
      setTargetBee(allBees[0].name);
      setTargetWorkspace(allBees[0].workspace);
    }
  }, [allBees.length]);

  function handleSend() {
    const text = input.trim();
    if (!text || !targetBee) return;
    onSendMessage(targetBee, targetWorkspace, text);
    setInput('');
  }

  function handleBeeSelect(value: string) {
    const [ws, bee] = value.split('/');
    setTargetWorkspace(ws);
    setTargetBee(bee);
  }

  return (
    <div style={{ display: 'flex', height: '100%', width: '100%', flex: 1 }}>
      {/* ── The Hive (left sidebar) ── */}
      <div style={{
        width: 220,
        background: '#fff',
        borderRight: '1px solid #e2e8f0',
        display: 'flex',
        flexDirection: 'column',
        flexShrink: 0,
        overflow: 'auto',
      }}>
        <div style={{
          padding: '14px 16px 8px',
          fontSize: 11,
          fontWeight: 700,
          textTransform: 'uppercase',
          letterSpacing: '0.05em',
          color: '#94a3b8',
        }}>
          The Hive
        </div>

        {workspaces.map((ws) => {
          const bees = hive.filter((h) => h.workspace === ws);
          if (bees.length === 0) return null;
          return (
            <div key={ws} style={{ padding: '4px 0' }}>
              <div style={{
                padding: '4px 16px',
                fontSize: 12,
                fontWeight: 700,
                color: '#334155',
              }}>
                {ws}
              </div>
              {bees.map((entry) => (
                <div
                  key={`${entry.workspace}/${entry.bee}`}
                  style={{
                    padding: '6px 16px 6px 24px',
                    display: 'flex',
                    alignItems: 'center',
                    gap: 8,
                    cursor: 'pointer',
                    borderRadius: 4,
                  }}
                  onClick={() => {
                    setTargetWorkspace(entry.workspace);
                    setTargetBee(entry.bee);
                    inputRef.current?.focus();
                  }}
                >
                  <span style={{
                    width: 6,
                    height: 6,
                    borderRadius: '50%',
                    background: entry.isActive ? '#22c55e' : '#d1d5db',
                    flexShrink: 0,
                  }} />
                  <span style={{
                    fontSize: 13,
                    color: '#334155',
                    flex: 1,
                    overflow: 'hidden',
                    textOverflow: 'ellipsis',
                    whiteSpace: 'nowrap',
                  }}>
                    {entry.bee}
                  </span>
                  <span style={{
                    fontSize: 11,
                    color: '#94a3b8',
                    flexShrink: 0,
                  }}>
                    {entry.status}
                  </span>
                </div>
              ))}
            </div>
          );
        })}

        {/* Connection status */}
        <div style={{ marginTop: 'auto', padding: '12px 16px', borderTop: '1px solid #f1f5f9' }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 6, fontSize: 11, color: '#94a3b8' }}>
            <span style={{
              width: 6,
              height: 6,
              borderRadius: '50%',
              background: connected ? '#22c55e' : '#ef4444',
            }} />
            {connected ? 'connected' : 'disconnected'}
          </div>
        </div>
      </div>

      {/* ── Main feed ── */}
      <div style={{
        flex: 1,
        display: 'flex',
        flexDirection: 'column',
        overflow: 'hidden',
        minWidth: 0,
      }}>
        {/* Feed area */}
        <div style={{ flex: 1, overflow: 'auto', padding: '20px 24px', display: 'flex', flexDirection: 'column', alignItems: 'center' }}>
         <div style={{ width: '100%', maxWidth: 720 }}>

          {/* Decision count */}
          {decisions.length > 0 && (
            <div style={{
              fontSize: 13,
              fontWeight: 600,
              color: '#dc2626',
              marginBottom: 12,
            }}>
              {decisions.length} decision{decisions.length !== 1 ? 's' : ''} needed
            </div>
          )}

          {/* Decision cards */}
          {decisions.map((item) => (
            <div
              key={item.id}
              style={{
                padding: '14px 18px',
                borderRadius: 10,
                border: '1.5px solid',
                borderColor: item.priority === 'red' ? '#fca5a5' : '#fde68a',
                background: item.priority === 'red' ? '#fef2f2' : '#fffbeb',
                marginBottom: 10,
                cursor: 'pointer',
              }}
              onClick={() => onDrillIntoTask(item.id)}
            >
              <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 6 }}>
                <span style={{
                  fontSize: 11,
                  fontWeight: 600,
                  color: item.priority === 'red' ? '#dc2626' : '#d97706',
                  textTransform: 'uppercase',
                }}>
                  {item.bee}
                </span>
                <span style={{ fontSize: 11, color: '#94a3b8' }}>{timeAgo(item.timestamp)}</span>
              </div>
              <div style={{ fontSize: 14, fontWeight: 500, color: '#0f172a', marginBottom: 4 }}>
                {item.title}
              </div>
              {item.body && (
                <div style={{ fontSize: 12, color: '#64748b', marginBottom: 10 }}>
                  {item.body}
                </div>
              )}
              {item.actions && (
                <div style={{ display: 'flex', gap: 8 }}>
                  {item.actions.map((action) => (
                    <button
                      key={action.label}
                      onClick={(e) => { e.stopPropagation(); action.onClick(); }}
                      style={{
                        padding: '5px 14px',
                        borderRadius: 6,
                        border: '1px solid',
                        borderColor: action.style === 'primary' ? '#3b82f6' : action.style === 'danger' ? '#fca5a5' : '#e2e8f0',
                        background: action.style === 'primary' ? '#3b82f6' : '#fff',
                        color: action.style === 'primary' ? '#fff' : action.style === 'danger' ? '#dc2626' : '#334155',
                        cursor: 'pointer',
                        fontSize: 12,
                        fontWeight: 500,
                      }}
                    >
                      {action.label}
                    </button>
                  ))}
                </div>
              )}
            </div>
          ))}

          {/* Quiet section */}
          {quiet.length > 0 && (
            <>
              <div style={{
                fontSize: 11,
                fontWeight: 600,
                color: '#94a3b8',
                textTransform: 'uppercase',
                letterSpacing: '0.05em',
                margin: '20px 0 8px',
                display: 'flex',
                alignItems: 'center',
                gap: 8,
              }}>
                <span style={{ flex: 1, height: 1, background: '#e2e8f0' }} />
                <span>quiet</span>
                <span style={{ flex: 1, height: 1, background: '#e2e8f0' }} />
              </div>

              {quiet.map((item) => (
                <div
                  key={item.id}
                  style={{
                    padding: '8px 0',
                    display: 'flex',
                    alignItems: 'center',
                    gap: 10,
                    cursor: 'pointer',
                  }}
                  onClick={() => onDrillIntoTask(item.id)}
                >
                  <span style={{ fontSize: 12, color: '#94a3b8', width: 50, flexShrink: 0, textAlign: 'right' }}>
                    {timeAgo(item.timestamp)}
                  </span>
                  <span style={{ fontSize: 13, color: '#64748b' }}>
                    {item.title}
                  </span>
                  {item.body && (
                    <span style={{ fontSize: 11, color: '#94a3b8' }}>
                      {item.body}
                    </span>
                  )}
                </div>
              ))}
            </>
          )}

          {/* Signals */}
          {signals.length > 0 && (
            <>
              <div style={{
                fontSize: 11, fontWeight: 600, color: '#94a3b8',
                textTransform: 'uppercase', letterSpacing: '0.05em',
                margin: '20px 0 8px',
                display: 'flex', alignItems: 'center', gap: 8,
              }}>
                <span style={{ flex: 1, height: 1, background: '#e2e8f0' }} />
                <span>signals ({signals.length})</span>
                <span style={{ flex: 1, height: 1, background: '#e2e8f0' }} />
              </div>
              {signals.slice(0, 20).map((sig) => {
                const isCritical = sig.severity === 'Critical' || sig.severity === 'Error';
                const icon = sig.source.includes('sentry') ? '⚡'
                  : sig.source.includes('github') ? '🔀'
                  : sig.source.includes('swarm') ? '🐝'
                  : '📡';
                return (
                  <div key={sig.id} style={{
                    padding: '8px 12px',
                    marginBottom: 4,
                    borderRadius: 8,
                    border: `1px solid ${isCritical ? '#fca5a5' : '#e2e8f0'}`,
                    background: isCritical ? '#fef2f2' : '#fff',
                    display: 'flex',
                    alignItems: 'center',
                    gap: 8,
                    cursor: sig.url ? 'pointer' : 'default',
                  }}
                  onClick={() => sig.url && window.open(sig.url, '_blank')}
                  >
                    <span style={{ fontSize: 14, flexShrink: 0 }}>{icon}</span>
                    <div style={{ flex: 1, minWidth: 0 }}>
                      <div style={{
                        fontSize: 13, color: '#334155',
                        overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap',
                      }}>
                        {sig.title}
                      </div>
                      <div style={{ fontSize: 11, color: '#94a3b8' }}>
                        {sig.source} · {sig.workspace} · {timeAgo(new Date(sig.created_at))}
                      </div>
                    </div>
                    {isCritical && (
                      <span style={{
                        fontSize: 10, fontWeight: 700, color: '#dc2626',
                        background: '#fee2e2', padding: '2px 6px', borderRadius: 4,
                        flexShrink: 0,
                      }}>
                        {sig.severity.toUpperCase()}
                      </span>
                    )}
                  </div>
                );
              })}
            </>
          )}

          {/* Chat messages */}
          {chatMessages.length > 0 && (
            <>
              <div style={{
                fontSize: 11, fontWeight: 600, color: '#94a3b8',
                textTransform: 'uppercase', letterSpacing: '0.05em',
                margin: '20px 0 8px',
                display: 'flex', alignItems: 'center', gap: 8,
              }}>
                <span style={{ flex: 1, height: 1, background: '#e2e8f0' }} />
                <span>chat</span>
                <span style={{ flex: 1, height: 1, background: '#e2e8f0' }} />
              </div>
              {chatMessages.map((msg) => (
                <div key={msg.id} style={{
                  padding: '10px 14px',
                  marginBottom: 6,
                  borderRadius: 10,
                  background: msg.role === 'user' ? '#f8fafc' : '#fff',
                  border: `1px solid ${msg.role === 'user' ? '#e2e8f0' : '#fde68a'}`,
                }}>
                  <div style={{ display: 'flex', alignItems: 'center', gap: 6, marginBottom: 4 }}>
                    <span style={{
                      fontSize: 11, fontWeight: 600,
                      color: msg.role === 'user' ? '#64748b' : '#d97706',
                    }}>
                      {msg.role === 'user' ? 'You' : `@${msg.bee}`}
                    </span>
                    <span style={{ fontSize: 11, color: '#94a3b8' }}>
                      {msg.workspace} · {timeAgo(msg.timestamp)}
                    </span>
                  </div>
                  <div style={{
                    fontSize: 14, color: '#1e293b', lineHeight: 1.5,
                    whiteSpace: 'pre-wrap',
                  }}>
                    {msg.text}
                  </div>
                </div>
              ))}
            </>
          )}
          <div ref={feedEndRef} />
          </div>

          {/* Empty state — centered in full available space */}
          {feed.length === 0 && chatMessages.length === 0 && (
            <div style={{
              flex: 1,
              display: 'flex',
              alignItems: 'center',
              justifyContent: 'center',
              textAlign: 'center',
              color: '#94a3b8',
              padding: '60px 20px',
            }}>
              <div>
                <div style={{ fontSize: 36, marginBottom: 12 }}>🐝</div>
                <div style={{ fontSize: 15, fontWeight: 500, color: '#64748b', marginBottom: 4 }}>
                  All clear
                </div>
                <div style={{ fontSize: 13 }}>
                  No decisions needed. Your Bees are humming along.
                </div>
              </div>
            </div>
          )}
        </div>

        {/* ── Input bar ── */}
        <div style={{
          borderTop: '1px solid #e2e8f0',
          padding: '12px 20px',
          background: '#fff',
          display: 'flex',
          flexDirection: 'column',
          gap: 8,
        }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
            <select
              value={`${targetWorkspace}/${targetBee}`}
              onChange={(e) => handleBeeSelect(e.target.value)}
              style={{
                fontSize: 12,
                padding: '6px 10px',
                border: '1px solid #e2e8f0',
                borderRadius: 8,
                background: '#f8fafc',
                color: '#334155',
                cursor: 'pointer',
              }}
            >
              {allBees.map((b) => (
                <option key={`${b.workspace}/${b.name}`} value={`${b.workspace}/${b.name}`}>
                  @{b.name} ({b.workspace})
                </option>
              ))}
            </select>
            <span style={{ fontSize: 11, color: '#94a3b8' }}>
              Enter to send, Shift+Enter for new line
            </span>
            <div style={{ flex: 1 }} />
            <button
              onClick={handleSend}
              disabled={!input.trim()}
              style={{
                padding: '6px 18px',
                borderRadius: 8,
                border: 'none',
                background: input.trim() ? '#f59e0b' : '#e2e8f0',
                color: input.trim() ? '#fff' : '#94a3b8',
                cursor: input.trim() ? 'pointer' : 'default',
                fontSize: 13,
                fontWeight: 600,
              }}
            >
              Send
            </button>
          </div>
          <textarea
            ref={inputRef}
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && !e.shiftKey) {
                e.preventDefault();
                handleSend();
              }
            }}
            placeholder={targetBee ? `Message @${targetBee}...` : 'Select a Bee...'}
            rows={Math.min(6, Math.max(2, input.split('\n').length))}
            style={{
              width: '100%',
              padding: '10px 14px',
              border: '1px solid #e2e8f0',
              borderRadius: 8,
              fontSize: 14,
              outline: 'none',
              resize: 'none',
              fontFamily: 'inherit',
              lineHeight: 1.5,
              boxSizing: 'border-box',
            }}
          />
        </div>
      </div>

    </div>
  );
}
