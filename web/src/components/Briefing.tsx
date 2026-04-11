import { useState, useRef, useEffect } from 'react';
import Markdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import type { BeeConfigView, TaskView } from '../types';
import { dismissBriefingItem, snoozeBriefingItem, fetchCanvas, sendWorkerMessage } from '../api';
import './Briefing.css';

// ── Types ──────────────────────────────────────────────────────────────

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

interface BriefingItemData {
  id: string;
  priority: string;
  icon: string;
  title: string;
  body: string | null;
  workspace: string;
  source: string;
  url: string | null;
  actions: Array<{ label: string; style: string }>;
  timestamp: string;
}

interface HiveEntry {
  workspace: string;
  bee: string;
  status: string;
  isActive: boolean;
}

interface CanvasData {
  bee: string;
  content: string;
}

interface WorkerData {
  id: string;
  workspace: string;
  branch: string;
  agent: string;
  status: string;
  pr_url: string | null;
}

interface BriefingProps {
  workspaces: string[];
  beesByWorkspace: Record<string, BeeConfigView[]>;
  tasks: TaskView[];
  signals: SignalItem[];
  briefingItems: BriefingItemData[];
  workers: WorkerData[];
  chatMessages: ChatMessage[];
  connected: boolean;
  onSendMessage: (bee: string, workspace: string, text: string) => void;
  onDrillIntoTask: (taskId: string) => void;
  onRefreshBriefing: () => void;
  onWorkspaceChange?: (workspace: string) => void;
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
      const active = tasks.filter(t => t.stage !== 'Merged' && t.stage !== 'Dismissed').length;
      entries.push({
        workspace: ws,
        bee: bee.name,
        status: active > 0 && bee.name.toLowerCase().includes('code') ? `${active} active` : 'idle',
        isActive: active > 0 && bee.name.toLowerCase().includes('code'),
      });
    }
  }
  return entries;
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
  signals: _signals,
  briefingItems,
  chatMessages,
  connected,
  onSendMessage,
  onDrillIntoTask,
  onRefreshBriefing,
  onWorkspaceChange,
  workers,
}: BriefingProps) {
  const [input, setInput] = useState('');
  const [targetBee, setTargetBee] = useState('');
  const [targetWorkspace, setTargetWorkspace] = useState(workspaces[0] ?? '');
  const [hiveOpen, setHiveOpen] = useState(false);
  const [chatOpen, setChatOpen] = useState(false);
  const [canvases, setCanvases] = useState<CanvasData[]>([]);
  const [expandedCard, setExpandedCard] = useState<string | null>(null);
  const [workerMsg, setWorkerMsg] = useState('');
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const chatEndRef = useRef<HTMLDivElement>(null);

  const hive = buildHive(workspaces, beesByWorkspace, tasks);

  // All bees for the bee selector
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

  // Auto-scroll chat
  useEffect(() => {
    if (chatOpen) {
      chatEndRef.current?.scrollIntoView({ behavior: 'smooth' });
    }
  }, [chatMessages.length, chatOpen]);

  // Load canvases for the active workspace
  useEffect(() => {
    const bees = beesByWorkspace[targetWorkspace] ?? [];
    Promise.all(
      bees.map(b => fetchCanvas(targetWorkspace, b.name).catch(() => ({ bee: b.name, content: '' })))
    ).then(results => {
      setCanvases(results.filter(c => c.content));
    });
  }, [targetWorkspace, beesByWorkspace]);

  function handleBeeSelect(value: string) {
    const [ws, bee] = value.split('/');
    setTargetWorkspace(ws);
    setTargetBee(bee);
    onWorkspaceChange?.(ws);
  }

  function handleSend() {
    const text = input.trim();
    if (!text || !targetBee) return;
    onSendMessage(targetBee, targetWorkspace, text);
    setInput('');
    setChatOpen(true);
  }

  // Filter briefing items by workspace
  const filtered = briefingItems.filter(i => i.workspace === targetWorkspace);
  const actionItems = filtered.filter(i => i.priority === 'action');
  const noticeItems = filtered.filter(i => i.priority === 'notice');
  const quietItems = filtered.filter(i => i.priority === 'quiet');

  // Filter chat to active bee
  const filteredChat = chatMessages.filter(
    m => m.bee === targetBee && m.workspace === targetWorkspace
  );

  return (
    <div className="briefing-root">
      {/* ── Mobile header ── */}
      <div className="briefing-mobile-header">
        <button onClick={() => setHiveOpen(!hiveOpen)} style={{
          padding: '10px 14px', borderRadius: 8, border: '1px solid #e2e8f0',
          background: hiveOpen ? '#f1f5f9' : '#fff', cursor: 'pointer',
          fontSize: 14, fontWeight: 500, minHeight: 44,
        }}>🐝 {hiveOpen ? '▲' : '▼'}</button>
        <select
          value={`${targetWorkspace}/${targetBee}`}
          onChange={(e) => handleBeeSelect(e.target.value)}
          style={{ fontSize: 12, padding: '4px 8px', border: '1px solid #e2e8f0', borderRadius: 6, background: '#f8fafc', flex: 1 }}
        >
          {allBees.map(b => (
            <option key={`${b.workspace}/${b.name}`} value={`${b.workspace}/${b.name}`}>
              @{b.name} ({b.workspace})
            </option>
          ))}
        </select>
        <span style={{ width: 6, height: 6, borderRadius: '50%', flexShrink: 0, background: connected ? '#22c55e' : '#ef4444' }} />
      </div>

      {/* ── Hive sidebar ── */}
      <div className={`briefing-hive ${hiveOpen ? 'hive-open' : ''}`}>
        <div style={{ padding: '14px 16px 8px', fontSize: 11, fontWeight: 700, textTransform: 'uppercase', letterSpacing: '0.05em', color: '#94a3b8' }}>
          The Hive
        </div>
        {workspaces.map(ws => {
          const bees = hive.filter(h => h.workspace === ws);
          if (bees.length === 0) return null;
          const isSelected = ws === targetWorkspace;
          return (
            <div key={ws} style={{ padding: '4px 0' }}>
              <div onClick={() => { setTargetWorkspace(ws); onWorkspaceChange?.(ws); }} style={{
                padding: '6px 16px', fontSize: 12, fontWeight: 700,
                color: isSelected ? '#0f172a' : '#64748b',
                background: isSelected ? '#f1f5f9' : 'transparent',
                borderRadius: 4, cursor: 'pointer',
                display: 'flex', alignItems: 'center', gap: 6,
              }}>
                <span style={{ width: 4, height: 16, borderRadius: 2, flexShrink: 0, background: isSelected ? '#f59e0b' : 'transparent' }} />
                {ws}
              </div>
              {bees.map(entry => (
                <div key={`${entry.workspace}/${entry.bee}`}
                  onClick={() => { setTargetWorkspace(entry.workspace); setTargetBee(entry.bee); setHiveOpen(false); }}
                  style={{
                    padding: '6px 16px 6px 32px', display: 'flex', alignItems: 'center', gap: 8, cursor: 'pointer',
                    background: entry.bee === targetBee && entry.workspace === targetWorkspace ? '#fffbeb' : 'transparent',
                    borderRadius: 4,
                  }}>
                  <span style={{ width: 6, height: 6, borderRadius: '50%', background: entry.isActive ? '#22c55e' : '#d1d5db', flexShrink: 0 }} />
                  <span style={{ fontSize: 13, color: '#334155', flex: 1 }}>{entry.bee}</span>
                  <span style={{ fontSize: 11, color: '#94a3b8' }}>{entry.status}</span>
                </div>
              ))}
            </div>
          );
        })}
        <div style={{ marginTop: 'auto', padding: '12px 16px', borderTop: '1px solid #f1f5f9' }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 6, fontSize: 11, color: '#94a3b8' }}>
            <span style={{ width: 6, height: 6, borderRadius: '50%', background: connected ? '#22c55e' : '#ef4444' }} />
            {connected ? 'connected' : 'disconnected'}
          </div>
        </div>
      </div>

      {/* Backdrop to close hive on mobile */}
      {hiveOpen && (
        <div onClick={() => setHiveOpen(false)} style={{
          position: 'fixed', top: 0, left: 0, right: 0, bottom: 0,
          background: 'rgba(0,0,0,0.15)', zIndex: 25,
        }} />
      )}

      {/* ── Main area: briefing + canvases + chat drawer ── */}
      <div className="briefing-feed">
        {/* Scrollable content area */}
        <div style={{ flex: 1, overflow: 'auto', padding: '20px 24px' }}>
          <div style={{ maxWidth: 720, margin: '0 auto' }}>

            {/* ── Briefing items ── */}
            {actionItems.length > 0 && (
              <div style={{ fontSize: 13, fontWeight: 600, color: '#dc2626', marginBottom: 12 }}>
                {actionItems.length} item{actionItems.length !== 1 ? 's' : ''} need{actionItems.length === 1 ? 's' : ''} attention
              </div>
            )}
            {actionItems.map(item => {
              const isWorker = item.source.startsWith('swarm:');
              const workerId = isWorker ? item.source.split(':')[1] : '';
              const isExpanded = expandedCard === item.id;
              return (
              <div key={item.id} style={{
                padding: '14px 18px', borderRadius: 10, marginBottom: 10,
                border: '1.5px solid #fca5a5', background: '#fef2f2',
              }}>
                <div onClick={() => isWorker ? setExpandedCard(isExpanded ? null : item.id) : (item.url && window.open(item.url, '_blank'))}
                  style={{ cursor: 'pointer' }}>
                  <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginBottom: 6 }}>
                    <span style={{ fontSize: 16 }}>{item.icon}</span>
                    <span style={{ fontSize: 11, fontWeight: 600, color: '#94a3b8' }}>{item.workspace}</span>
                    <span style={{ fontSize: 11, color: '#94a3b8' }}>{timeAgo(new Date(item.timestamp))}</span>
                    {isWorker && <span style={{ fontSize: 11, color: '#94a3b8', marginLeft: 'auto' }}>{isExpanded ? '▲' : '▼'}</span>}
                  </div>
                  <div style={{ fontSize: 14, fontWeight: 500, color: '#0f172a', marginBottom: 4 }}>{item.title}</div>
                  {item.body && <div style={{ fontSize: 12, color: '#64748b', marginBottom: isExpanded ? 4 : 10 }}>{item.body}</div>}
                </div>

                {/* Expanded worker detail */}
                {isWorker && isExpanded && (
                  <div style={{ marginTop: 8, padding: '10px 0', borderTop: '1px solid #fecaca' }}>
                    {item.url && (
                      <a href={item.url} target="_blank" rel="noopener noreferrer" style={{
                        fontSize: 12, color: '#2563eb', display: 'block', marginBottom: 8,
                      }}>Open PR →</a>
                    )}
                    <div style={{ display: 'flex', gap: 8 }}>
                      <input
                        value={workerMsg}
                        onChange={(e) => setWorkerMsg(e.target.value)}
                        onKeyDown={(e) => {
                          if (e.key === 'Enter' && workerMsg.trim()) {
                            sendWorkerMessage(item.workspace, workerId, workerMsg.trim());
                            setWorkerMsg('');
                          }
                        }}
                        placeholder={`Message ${workerId}...`}
                        style={{
                          flex: 1, padding: '8px 12px', border: '1px solid #e2e8f0', borderRadius: 8,
                          fontSize: 14, outline: 'none',
                        }}
                      />
                      <button onClick={() => {
                        if (workerMsg.trim()) {
                          sendWorkerMessage(item.workspace, workerId, workerMsg.trim());
                          setWorkerMsg('');
                        }
                      }} style={{
                        padding: '8px 14px', borderRadius: 8, border: 'none',
                        background: '#f59e0b', color: '#fff', cursor: 'pointer',
                        fontSize: 13, fontWeight: 600,
                      }}>Send</button>
                    </div>
                  </div>
                )}

                {/* Action buttons */}
                {!isExpanded && item.actions.length > 0 && (
                  <div style={{ display: 'flex', gap: 8 }}>
                    {item.actions.map(action => {
                      const signalId = parseInt(item.id.split('-')[1] ?? '0', 10);
                      return (
                        <button key={action.label} onClick={(e) => {
                          e.stopPropagation();
                          if (action.label === 'Dismiss' || action.label === 'Acknowledge') {
                            dismissBriefingItem(signalId, item.workspace).then(onRefreshBriefing);
                          } else if (action.label === 'Snooze') {
                            snoozeBriefingItem(signalId, item.workspace).then(onRefreshBriefing);
                          } else if (item.url) {
                            window.open(item.url, '_blank');
                          }
                        }} style={{
                          padding: '6px 14px', borderRadius: 6, border: '1px solid',
                          borderColor: action.style === 'primary' ? '#3b82f6' : action.style === 'danger' ? '#fca5a5' : '#e2e8f0',
                          background: action.style === 'primary' ? '#3b82f6' : '#fff',
                          color: action.style === 'primary' ? '#fff' : action.style === 'danger' ? '#dc2626' : '#334155',
                          cursor: 'pointer', fontSize: 13, fontWeight: 500, minHeight: 36,
                        }}>{action.label}</button>
                      );
                    })}
                  </div>
                )}
              </div>
              );
            })}

            {/* Notices */}
            {noticeItems.length > 0 && (
              <>
                <SectionDivider label="notices" />
                {noticeItems.map(item => (
                  <div key={item.id} style={{
                    padding: '8px 12px', marginBottom: 4, borderRadius: 8,
                    border: '1px solid #fde68a', background: '#fffbeb',
                    display: 'flex', alignItems: 'center', gap: 8,
                    cursor: item.url ? 'pointer' : 'default',
                  }} onClick={() => item.url && window.open(item.url, '_blank')}>
                    <span style={{ fontSize: 14 }}>{item.icon}</span>
                    <div style={{ flex: 1, minWidth: 0 }}>
                      <div style={{ fontSize: 13, color: '#334155', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{item.title}</div>
                      <div style={{ fontSize: 11, color: '#94a3b8' }}>{item.workspace} · {timeAgo(new Date(item.timestamp))}</div>
                    </div>
                  </div>
                ))}
              </>
            )}

            {/* ── Bee Canvases ── */}
            {canvases.length > 0 && (
              <>
                <SectionDivider label="canvases" />
                {canvases.map(c => (
                  <div key={c.bee} style={{
                    padding: '14px 18px', borderRadius: 10, marginBottom: 10,
                    border: '1px solid #e2e8f0', background: '#fff',
                  }}>
                    <div style={{ fontSize: 12, fontWeight: 600, color: '#d97706', marginBottom: 8 }}>
                      🎨 @{c.bee}
                    </div>
                    <div className="canvas-markdown" style={{ fontSize: 14, lineHeight: 1.7, color: '#1e293b' }}>
                      <Markdown remarkPlugins={[remarkGfm]}>{c.content}</Markdown>
                    </div>
                  </div>
                ))}
              </>
            )}

            {/* ── Workers ── */}
            {(() => {
              const wsWorkers = workers.filter(w => w.workspace === targetWorkspace);
              if (wsWorkers.length === 0) return null;
              return (
                <>
                  <SectionDivider label={`workers (${wsWorkers.length})`} />
                  {wsWorkers.map(w => {
                    const isExpanded = expandedCard === `worker-${w.id}`;
                    const statusIcon = w.status === 'waiting' ? '⏸' : w.status === 'running' ? '▶' : '○';
                    const statusColor = w.status === 'waiting' ? '#d97706' : w.status === 'running' ? '#16a34a' : '#94a3b8';
                    return (
                      <div key={w.id} style={{
                        padding: '10px 14px', marginBottom: 4, borderRadius: 8,
                        border: '1px solid #e2e8f0', background: '#fff',
                      }}>
                        <div onClick={() => setExpandedCard(isExpanded ? null : `worker-${w.id}`)}
                          style={{ cursor: 'pointer', display: 'flex', alignItems: 'center', gap: 8 }}>
                          <span style={{ fontSize: 14 }}>{statusIcon}</span>
                          <div style={{ flex: 1, minWidth: 0 }}>
                            <div style={{ fontSize: 13, fontWeight: 500, color: '#334155' }}>{w.id}</div>
                            <div style={{ fontSize: 11, color: '#94a3b8', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                              {w.branch.replace('swarm/', '')}
                            </div>
                          </div>
                          <span style={{ fontSize: 11, color: statusColor, fontWeight: 600 }}>{w.status}</span>
                          <span style={{ fontSize: 11, color: '#94a3b8' }}>{isExpanded ? '▲' : '▼'}</span>
                        </div>
                        {isExpanded && (
                          <div style={{ marginTop: 8, paddingTop: 8, borderTop: '1px solid #f1f5f9' }}>
                            <div style={{ fontSize: 12, color: '#64748b', marginBottom: 6 }}>
                              Agent: {w.agent} · Branch: {w.branch}
                            </div>
                            {w.pr_url && (
                              <a href={w.pr_url} target="_blank" rel="noopener noreferrer" style={{
                                fontSize: 12, color: '#2563eb', display: 'block', marginBottom: 8,
                              }}>Open PR →</a>
                            )}
                            <div style={{ display: 'flex', gap: 8 }}>
                              <input
                                value={workerMsg}
                                onChange={(e) => setWorkerMsg(e.target.value)}
                                onKeyDown={(e) => {
                                  if (e.key === 'Enter' && workerMsg.trim()) {
                                    sendWorkerMessage(w.workspace, w.id, workerMsg.trim());
                                    setWorkerMsg('');
                                  }
                                }}
                                placeholder={`Message ${w.id}...`}
                                style={{
                                  flex: 1, padding: '8px 12px', border: '1px solid #e2e8f0', borderRadius: 8,
                                  fontSize: 14, outline: 'none',
                                }}
                              />
                              <button onClick={() => {
                                if (workerMsg.trim()) {
                                  sendWorkerMessage(w.workspace, w.id, workerMsg.trim());
                                  setWorkerMsg('');
                                }
                              }} style={{
                                padding: '8px 14px', borderRadius: 8, border: 'none',
                                background: '#f59e0b', color: '#fff', cursor: 'pointer',
                                fontSize: 13, fontWeight: 600,
                              }}>Send</button>
                            </div>
                          </div>
                        )}
                      </div>
                    );
                  })}
                </>
              );
            })()}

            {/* Quiet */}
            {quietItems.length > 0 && (
              <>
                <SectionDivider label="quiet" />
                {quietItems.map(item => (
                  <div key={item.id} style={{
                    padding: '6px 0', display: 'flex', alignItems: 'center', gap: 8,
                    cursor: item.url ? 'pointer' : 'default',
                  }} onClick={() => item.url && window.open(item.url, '_blank')}>
                    <span style={{ fontSize: 12 }}>{item.icon}</span>
                    <span style={{ fontSize: 13, color: '#64748b', flex: 1, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{item.title}</span>
                    <span style={{ fontSize: 11, color: '#94a3b8', flexShrink: 0 }}>{item.workspace}</span>
                  </div>
                ))}
              </>
            )}

            {/* Empty state */}
            {filtered.length === 0 && canvases.length === 0 && (
              <div style={{ textAlign: 'center', color: '#94a3b8', padding: '60px 20px' }}>
                <div style={{ fontSize: 36, marginBottom: 12 }}>🐝</div>
                <div style={{ fontSize: 15, fontWeight: 500, color: '#64748b', marginBottom: 4 }}>All clear</div>
                <div style={{ fontSize: 13 }}>No decisions needed. Your Bees are humming along.</div>
              </div>
            )}
          </div>
        </div>

        {/* ── Chat drawer ── */}
        <div style={{
          borderTop: '1px solid #e2e8f0', background: '#fff',
          display: 'flex', flexDirection: 'column',
          maxHeight: chatOpen ? '50vh' : 'auto',
          transition: 'max-height 0.2s ease',
        }}>
          {/* Chat header — always visible, click to toggle */}
          <div onClick={() => setChatOpen(!chatOpen)} style={{
            padding: '10px 20px', display: 'flex', alignItems: 'center', gap: 8,
            cursor: 'pointer', flexShrink: 0,
          }}>
            <span style={{ fontSize: 12, color: '#d97706', fontWeight: 600 }}>@{targetBee}</span>
            <span style={{ fontSize: 11, color: '#94a3b8' }}>{targetWorkspace}</span>
            {filteredChat.length > 0 && (
              <span style={{ fontSize: 11, color: '#94a3b8' }}>· {filteredChat.length} messages</span>
            )}
            <div style={{ flex: 1 }} />
            <span style={{ fontSize: 12, color: '#94a3b8' }}>{chatOpen ? '▼ Hide' : '▲ Chat'}</span>
          </div>

          {/* Chat messages — visible when open */}
          {chatOpen && (
            <div style={{ flex: 1, overflow: 'auto', padding: '0 20px 8px', maxHeight: '30vh' }}>
              {filteredChat.length === 0 && (
                <div style={{ textAlign: 'center', color: '#94a3b8', padding: '20px', fontSize: 13 }}>
                  Send a message to @{targetBee} to get started.
                </div>
              )}
              {filteredChat.map(msg => (
                <div key={msg.id} style={{
                  padding: '8px 12px', marginBottom: 4, borderRadius: 8,
                  background: msg.role === 'user' ? '#f8fafc' : '#fff',
                  border: `1px solid ${msg.role === 'user' ? '#e2e8f0' : '#fde68a'}`,
                }}>
                  <div style={{ display: 'flex', alignItems: 'center', gap: 6, marginBottom: 3 }}>
                    <span style={{ fontSize: 11, fontWeight: 600, color: msg.role === 'user' ? '#64748b' : '#d97706' }}>
                      {msg.role === 'user' ? 'You' : `@${msg.bee}`}
                    </span>
                    <span style={{ fontSize: 10, color: '#94a3b8' }}>{timeAgo(msg.timestamp)}</span>
                  </div>
                  <div className="canvas-markdown" style={{ fontSize: 13, color: '#1e293b', lineHeight: 1.5 }}>
                    <Markdown remarkPlugins={[remarkGfm]}>{msg.text}</Markdown>
                  </div>
                </div>
              ))}
              <div ref={chatEndRef} />
            </div>
          )}

          {/* Input bar — always visible */}
          <div style={{ padding: '8px 20px 12px', display: 'flex', gap: 8, alignItems: 'flex-end' }}>
            <textarea
              ref={inputRef}
              value={input}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); handleSend(); }
              }}
              placeholder={`Message @${targetBee}...`}
              rows={Math.min(4, Math.max(1, input.split('\n').length))}
              style={{
                flex: 1, padding: '8px 12px', border: '1px solid #e2e8f0', borderRadius: 8,
                fontSize: 16, outline: 'none', resize: 'none', fontFamily: 'inherit',
                lineHeight: 1.4, boxSizing: 'border-box',
              }}
            />
            <button onClick={handleSend} disabled={!input.trim()} style={{
              padding: '8px 16px', borderRadius: 8, border: 'none',
              background: input.trim() ? '#f59e0b' : '#e2e8f0',
              color: input.trim() ? '#fff' : '#94a3b8',
              cursor: input.trim() ? 'pointer' : 'default',
              fontSize: 14, fontWeight: 600, minHeight: 36,
            }}>Send</button>
          </div>
        </div>
      </div>
    </div>
  );
}

// ── Shared components ──────────────────────────────────────────────────

function SectionDivider({ label }: { label: string }) {
  return (
    <div style={{
      fontSize: 11, fontWeight: 600, color: '#94a3b8',
      textTransform: 'uppercase', letterSpacing: '0.05em',
      margin: '20px 0 8px', display: 'flex', alignItems: 'center', gap: 8,
    }}>
      <span style={{ flex: 1, height: 1, background: '#e2e8f0' }} />
      <span>{label}</span>
      <span style={{ flex: 1, height: 1, background: '#e2e8f0' }} />
    </div>
  );
}
