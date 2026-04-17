import { useState, useRef, useEffect } from 'react';
import Markdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { dismissBriefingItem, snoozeBriefingItem, fetchCanvas, sendWorkerMessage } from '../api';
import type { BeeConfigView, TaskView } from '../types';
import './Dashboard.css';

// ── Types ──────────────────────────────────────────────────────────────

interface ChatMessage {
  id: string;
  bee: string;
  workspace: string;
  role: 'user' | 'assistant';
  text: string;
  timestamp: Date;
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

interface WorkerData {
  id: string;
  workspace: string;
  branch: string;
  agent: string;
  status: string;
  pr_url: string | null;
}

interface CanvasData {
  bee: string;
  content: string;
}

interface DashboardProps {
  workspace: string;
  bees: BeeConfigView[];
  briefingItems: BriefingItemData[];
  workers: WorkerData[];
  tasks: TaskView[];
  chatMessages: ChatMessage[];
  connected: boolean;
  onSendMessage: (bee: string, workspace: string, text: string) => void;
  onDrillIntoTask: (taskId: string) => void;
  onRefreshBriefing: () => void;
}

function timeAgo(date: Date): string {
  const secs = Math.floor((Date.now() - date.getTime()) / 1000);
  if (secs < 60) return 'now';
  if (secs < 3600) return `${Math.floor(secs / 60)}m`;
  if (secs < 86400) return `${Math.floor(secs / 3600)}h`;
  return `${Math.floor(secs / 86400)}d`;
}

// ── Component ──────────────────────────────────────────────────────────

export default function Dashboard({
  workspace,
  bees,
  briefingItems,
  workers,
  tasks,
  chatMessages,
  connected,
  onSendMessage,
  onDrillIntoTask,
  onRefreshBriefing,
}: DashboardProps) {
  const [targetBee, setTargetBee] = useState(bees[0]?.name ?? '');
  const [input, setInput] = useState('');
  const [chatOpen, setChatOpen] = useState(false);
  const [expandedItem, setExpandedItem] = useState<string | null>(null);
  const [workerMsg, setWorkerMsg] = useState('');
  const [canvases, setCanvases] = useState<CanvasData[]>([]);
  const [expandedCanvas, setExpandedCanvas] = useState<string | null>(null);
  const [activeWorker, setActiveWorker] = useState<string | null>(null);
  const [activeWorkerMsg, setActiveWorkerMsg] = useState('');
  const chatEndRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);

  // Load canvases
  useEffect(() => {
    Promise.all(
      bees.map(b => fetchCanvas(workspace, b.name).catch(() => ({ bee: b.name, content: '' })))
    ).then(results => setCanvases(results.filter(c => c.content)));
  }, [workspace, bees]);

  // Auto-scroll chat
  useEffect(() => {
    if (chatOpen) chatEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [chatMessages.length, chatOpen]);

  // Default bee
  useEffect(() => {
    if (!targetBee && bees.length > 0) setTargetBee(bees[0].name);
  }, [bees]);

  function handleSend() {
    const text = input.trim();
    if (!text || !targetBee) return;
    onSendMessage(targetBee, workspace, text);
    setInput('');
    setChatOpen(true);
  }

  const actionItems = briefingItems.filter(i => i.workspace === workspace && i.priority === 'action');
  const wsWorkers = workers.filter(w => w.workspace === workspace);
  const filteredChat = chatMessages.filter(m => m.bee === targetBee && m.workspace === workspace);

  const activeWorkerData = activeWorker ? wsWorkers.find(w => w.id === activeWorker) : null;
  const activeWorkerTask = activeWorkerData ? tasks.find(t => t.worker_id === activeWorkerData.id) : null;

  return (
    <div className="dashboard">

      {/* ── Worker Detail View ── */}
      {activeWorkerData && (
        <div style={{ flex: 1, display: 'flex', flexDirection: 'column', minHeight: 0 }}>
          {/* Header */}
          <div style={{
            padding: '12px 16px', borderBottom: '1px solid #e2e8f0', background: '#fff',
            display: 'flex', alignItems: 'center', gap: 10, flexShrink: 0,
          }}>
            <button onClick={() => setActiveWorker(null)} style={{
              background: 'none', border: 'none', cursor: 'pointer', fontSize: 16, color: '#64748b', padding: '4px 8px',
            }}>←</button>
            <div>
              <div style={{ fontSize: 15, fontWeight: 600, color: '#0f172a' }}>{activeWorkerData.id}</div>
              <div style={{ fontSize: 12, color: '#94a3b8' }}>{activeWorkerData.branch.replace('swarm/', '')}</div>
            </div>
            <div style={{ flex: 1 }} />
            <span style={{
              fontSize: 12, fontWeight: 600, padding: '3px 10px', borderRadius: 6,
              background: activeWorkerData.status === 'waiting' ? '#fef3c7' : activeWorkerData.status === 'running' ? '#dcfce7' : '#f1f5f9',
              color: activeWorkerData.status === 'waiting' ? '#92400e' : activeWorkerData.status === 'running' ? '#166534' : '#64748b',
            }}>{activeWorkerData.status}</span>
          </div>

          {/* Detail info */}
          <div style={{ padding: '12px 16px', borderBottom: '1px solid #f1f5f9', background: '#fafbfc', fontSize: 13, display: 'flex', flexWrap: 'wrap', gap: 16 }}>
            <div><span style={{ color: '#94a3b8' }}>Agent:</span> <span style={{ color: '#334155' }}>{activeWorkerData.agent}</span></div>
            <div><span style={{ color: '#94a3b8' }}>Branch:</span> <span style={{ color: '#334155', fontFamily: 'monospace', fontSize: 12 }}>{activeWorkerData.branch}</span></div>
            {activeWorkerData.pr_url && (
              <a href={activeWorkerData.pr_url} target="_blank" rel="noopener noreferrer" style={{ color: '#2563eb', textDecoration: 'none' }}>Open PR →</a>
            )}
            {activeWorkerTask && (
              <button onClick={() => onDrillIntoTask(activeWorkerTask.id)} style={{
                color: '#7c3aed', background: 'none', border: 'none', cursor: 'pointer', fontSize: 13,
              }}>View in Workflow →</button>
            )}
          </div>

          {/* Task/cursor info */}
          {activeWorkerTask?.cursor && (
            <div style={{ padding: '10px 16px', borderBottom: '1px solid #f1f5f9', fontSize: 12 }}>
              <span style={{ color: '#94a3b8' }}>Current step: </span>
              <span style={{ color: '#334155', fontWeight: 600 }}>{activeWorkerTask.cursor.current_node}</span>
              {activeWorkerTask.cursor.history.length > 0 && (
                <span style={{ color: '#94a3b8', marginLeft: 8 }}>
                  Path: {activeWorkerTask.cursor.history.map(s => s.to_node).join(' → ')}
                </span>
              )}
            </div>
          )}

          {/* Chat area — this is where you talk to the worker */}
          <div style={{ flex: 1, overflow: 'auto', padding: '12px 16px', background: '#fff' }}>
            <div style={{ textAlign: 'center', color: '#94a3b8', fontSize: 13, padding: '20px 0' }}>
              Send a message directly to this worker
            </div>
          </div>

          {/* Message input */}
          <div style={{
            padding: '10px 16px 14px', borderTop: '1px solid #e2e8f0', background: '#fff',
            display: 'flex', gap: 8, alignItems: 'flex-end',
          }}>
            <textarea
              value={activeWorkerMsg}
              onChange={e => setActiveWorkerMsg(e.target.value)}
              onKeyDown={e => {
                if (e.key === 'Enter' && !e.shiftKey && activeWorkerMsg.trim()) {
                  e.preventDefault();
                  sendWorkerMessage(workspace, activeWorkerData.id, activeWorkerMsg.trim());
                  setActiveWorkerMsg('');
                }
              }}
              placeholder={`Message ${activeWorkerData.id}...`}
              rows={2}
              style={{
                flex: 1, padding: '10px 12px', border: '1px solid #e2e8f0', borderRadius: 8,
                fontSize: 16, outline: 'none', resize: 'none', fontFamily: 'inherit', lineHeight: 1.4,
              }}
            />
            <button onClick={() => {
              if (activeWorkerMsg.trim()) {
                sendWorkerMessage(workspace, activeWorkerData.id, activeWorkerMsg.trim());
                setActiveWorkerMsg('');
              }
            }} className={`chat-send ${activeWorkerMsg.trim() ? 'chat-send-active' : ''}`}>Send</button>
          </div>
        </div>
      )}

      {/* ── Bento Grid ── */}
      {!activeWorkerData && (
      <div className="bento-grid">

        {/* Attention card */}
        <div className="bento-card bento-attention">
          <div className="bento-header">
            <span className="bento-title">⚠️ Attention</span>
            <span className="bento-count">{actionItems.length}</span>
          </div>
          <div className="bento-body">
            {actionItems.length === 0 && (
              <div className="bento-empty">All clear</div>
            )}
            {actionItems.map(item => {
              const signalId = parseInt(item.id.split('-')[1] ?? '0', 10);
              const isWorker = item.source.startsWith('swarm:');
              const workerId = isWorker ? item.source.split(':')[1] : '';
              const isEscalation = item.source === 'escalation';
              const isExpanded = expandedItem === item.id;
              const matchedWorker = isWorker ? wsWorkers.find(w => w.id === workerId) : null;
              const matchedTask = matchedWorker ? tasks.find(t => t.worker_id === workerId) : null;

              return (
                <div key={item.id} className="attention-item-wrap">
                  <div className="attention-item" onClick={() => setExpandedItem(isExpanded ? null : item.id)}>
                    <span className="attention-icon">{item.icon}</span>
                    <div className="attention-content">
                      <div className="attention-title">{item.title}</div>
                      {item.body && <div className="attention-body">{item.body}</div>}
                    </div>
                    <span style={{ fontSize: 11, color: '#94a3b8' }}>{isExpanded ? '▲' : '▼'}</span>
                  </div>

                  {isExpanded && (
                    <div className="attention-detail">
                      {/* Worker actions */}
                      {isWorker && matchedWorker && (
                        <>
                          <div className="attention-detail-row">
                            <span className="detail-label">Branch:</span>
                            <span className="detail-value">{matchedWorker.branch.replace('swarm/', '')}</span>
                          </div>
                          {matchedWorker.pr_url && (
                            <a href={matchedWorker.pr_url} target="_blank" rel="noopener noreferrer" className="detail-link">Open PR →</a>
                          )}
                          {matchedTask && (
                            <button className="detail-link" onClick={() => onDrillIntoTask(matchedTask.id)}>View in Workflow →</button>
                          )}
                          <button className="detail-link" onClick={() => {
                            setTargetBee('CodeBee');
                            setChatOpen(true);
                            onSendMessage('CodeBee', workspace, `What's the status of worker ${workerId}? It's on branch ${matchedWorker?.branch ?? 'unknown'}${matchedWorker?.pr_url ? ` with PR: ${matchedWorker.pr_url}` : ' (no PR yet)'}. Is it stuck? What should I do?`);
                          }}>
                            Ask CodeBee about this →
                          </button>
                          <div className="worker-msg-row" style={{ marginTop: 6 }}>
                            <input value={workerMsg} onChange={e => setWorkerMsg(e.target.value)}
                              onKeyDown={e => { if (e.key === 'Enter' && workerMsg.trim()) { sendWorkerMessage(workspace, workerId, workerMsg.trim()); setWorkerMsg(''); } }}
                              placeholder={`Message ${workerId} directly...`} className="worker-msg-input" />
                            <button className="btn-sm btn-primary" onClick={() => { if (workerMsg.trim()) { sendWorkerMessage(workspace, workerId, workerMsg.trim()); setWorkerMsg(''); } }}>Send</button>
                          </div>
                        </>
                      )}

                      {/* Escalation / general actions */}
                      {isEscalation && (
                        <div style={{ marginBottom: 6 }}>
                          <button className="detail-link" onClick={() => {
                            setTargetBee('CustomerBee');
                            setChatOpen(true);
                            onSendMessage('CustomerBee', workspace, `Tell me more about this escalation: "${item.title}"`);
                          }}>
                            Ask CustomerBee about this →
                          </button>
                        </div>
                      )}

                      {/* PR-related */}
                      {!isWorker && item.url && (
                        <a href={item.url} target="_blank" rel="noopener noreferrer" className="detail-link">Open in GitHub →</a>
                      )}

                      {/* Always show dismiss/snooze */}
                      <div className="attention-actions" style={{ marginTop: 6 }}>
                        <button className="btn-sm btn-muted" onClick={() => snoozeBriefingItem(signalId, workspace).then(onRefreshBriefing)}>Snooze 1h</button>
                        <button className="btn-sm btn-danger" onClick={() => dismissBriefingItem(signalId, workspace).then(onRefreshBriefing)}>Dismiss</button>
                      </div>
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        </div>

        {/* Bees card */}
        <div className="bento-card bento-bees">
          <div className="bento-header">
            <span className="bento-title">🐝 Bees</span>
            <span className="bento-count">{bees.length}</span>
          </div>
          <div className="bento-body">
            {bees.map(bee => {
              const isActive = targetBee === bee.name;
              return (
                <div key={bee.name}
                  className={`bee-row ${isActive ? 'bee-active' : ''}`}
                  onClick={() => { setTargetBee(bee.name); setChatOpen(true); }}
                >
                  <span className="bee-dot" />
                  <span className="bee-name">{bee.name}</span>
                  <span className="bee-provider">{bee.provider}</span>
                </div>
              );
            })}
          </div>
        </div>

        {/* Workers card */}
        <div className="bento-card bento-workers">
          <div className="bento-header">
            <span className="bento-title">🔧 Workers</span>
            <span className="bento-count">{wsWorkers.length}</span>
          </div>
          <div className="bento-body">
            {wsWorkers.length === 0 && (
              <div className="bento-empty">No active workers</div>
            )}
            {wsWorkers.map(w => {
              const isExpanded = expandedItem === `worker-${w.id}`;
              const icon = w.status === 'waiting' ? '⏸' : w.status === 'running' ? '▶' : '○';
              return (
                <div key={w.id} className="worker-item">
                  <div className="worker-row" onClick={() => setActiveWorker(w.id)}>
                    <span className="worker-icon">{icon}</span>
                    <span className="worker-id">{w.id}</span>
                    <span className={`worker-status worker-${w.status}`}>{w.status}</span>
                  </div>
                  {isExpanded && (
                    <div className="worker-detail">
                      <div className="worker-branch">{w.branch.replace('swarm/', '')}</div>
                      {w.pr_url && <a href={w.pr_url} target="_blank" rel="noopener noreferrer" className="worker-pr">Open PR →</a>}
                      {(() => {
                        const t = tasks.find(t => t.worker_id === w.id);
                        return t ? <button className="worker-workflow" onClick={() => onDrillIntoTask(t.id)}>View in Workflow →</button> : null;
                      })()}
                      <div className="worker-msg-row">
                        <input value={workerMsg} onChange={e => setWorkerMsg(e.target.value)}
                          onKeyDown={e => { if (e.key === 'Enter' && workerMsg.trim()) { sendWorkerMessage(workspace, w.id, workerMsg.trim()); setWorkerMsg(''); } }}
                          placeholder={`Message ${w.id}...`} className="worker-msg-input" />
                        <button className="btn-sm btn-primary" onClick={() => { if (workerMsg.trim()) { sendWorkerMessage(workspace, w.id, workerMsg.trim()); setWorkerMsg(''); } }}>Send</button>
                      </div>
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        </div>

        {/* Canvas card */}
        <div className="bento-card bento-canvas">
          <div className="bento-header">
            <span className="bento-title">🎨 Canvas</span>
            <span className="bento-count">{canvases.length}</span>
          </div>
          <div className="bento-body">
            {canvases.length === 0 && (
              <div className="bento-empty">No canvases yet</div>
            )}
            {canvases.map(c => {
              const isExpanded = expandedCanvas === c.bee;
              return (
                <div key={c.bee} className="canvas-item">
                  <div className="canvas-row" onClick={() => setExpandedCanvas(isExpanded ? null : c.bee)}>
                    <span className="canvas-bee">@{c.bee}</span>
                    <span className="canvas-preview">
                      {isExpanded ? '▲' : c.content.split('\n').find(l => l.trim())?.slice(0, 40) ?? '...'}
                    </span>
                  </div>
                  {isExpanded && (
                    <div className="canvas-content canvas-markdown">
                      <Markdown remarkPlugins={[remarkGfm]}>{c.content}</Markdown>
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        </div>

      </div>
      )}

      {/* ── Chat drawer ── */}
      <div className={`chat-drawer ${chatOpen ? 'chat-open' : ''}`}>
        <div className="chat-header" onClick={() => setChatOpen(!chatOpen)}>
          <select value={targetBee} onChange={e => setTargetBee(e.target.value)} className="chat-bee-select"
            onClick={e => e.stopPropagation()}>
            {bees.map(b => <option key={b.name} value={b.name}>@{b.name}</option>)}
          </select>
          <span className="chat-toggle">{chatOpen ? '▼' : '▲ Chat'}</span>
        </div>

        {chatOpen && (
          <div className="chat-messages">
            {filteredChat.length === 0 && (
              <div className="chat-empty">Message @{targetBee} to get started</div>
            )}
            {filteredChat.map(msg => (
              <div key={msg.id} className={`chat-msg chat-${msg.role}`}>
                <div className="chat-msg-header">
                  <span className="chat-msg-author">{msg.role === 'user' ? 'You' : `@${msg.bee}`}</span>
                  <span className="chat-msg-time">{timeAgo(msg.timestamp)}</span>
                </div>
                <div className="chat-msg-body canvas-markdown">
                  <Markdown remarkPlugins={[remarkGfm]}>{msg.text}</Markdown>
                </div>
              </div>
            ))}
            <div ref={chatEndRef} />
          </div>
        )}

        <div className="chat-input-row">
          <textarea ref={inputRef} value={input} onChange={e => setInput(e.target.value)}
            onKeyDown={e => { if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); handleSend(); } }}
            placeholder={`Message @${targetBee}...`}
            rows={Math.min(3, Math.max(1, input.split('\n').length))}
            className="chat-textarea" />
          <button onClick={handleSend} disabled={!input.trim()}
            className={`chat-send ${input.trim() ? 'chat-send-active' : ''}`}>Send</button>
        </div>
      </div>
    </div>
  );
}
