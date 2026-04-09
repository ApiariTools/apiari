import { useEffect, useState } from 'react';
import Briefing from './components/Briefing';
import WorkflowGraph from './components/WorkflowGraph';
import TaskPanel from './components/TaskPanel';
import SignalPanel from './components/SignalPanel';
import GraphEditor from './components/GraphEditor';
import BeeEditor from './components/BeeEditor';
import { fetchGraph, fetchTasks, fetchBees, fetchWorkspaces, fetchSignals, fetchConversations, clearTasks, sendChat, connectWs } from './api';
import type { BeeConfigView, GraphView, TaskView } from './types';

type View = 'briefing' | 'workflow' | 'bees';

export default function App() {
  const [graph, setGraph] = useState<GraphView | null>(null);
  const [tasks, setTasks] = useState<TaskView[]>([]);
  const [selectedTaskId, setSelectedTaskId] = useState<string | null>(null);
  const [selectedNodeId, setSelectedNodeId] = useState<string | null>(null);
  const [connected, setConnected] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showEditor, setShowEditor] = useState(false);
  const [view, setView] = useState<View>('briefing');
  const [workspace, setWorkspace] = useState('');
  const [workspaces, setWorkspaces] = useState<string[]>([]);
  const [beesByWorkspace, setBeesByWorkspace] = useState<Record<string, BeeConfigView[]>>({});
  const [signals, setSignals] = useState<Array<{
    id: number;
    workspace: string;
    source: string;
    title: string;
    severity: string;
    url?: string | null;
    created_at: string;
  }>>([]);
  const [chatMessages, setChatMessages] = useState<Array<{
    id: string;
    bee: string;
    workspace: string;
    role: 'user' | 'assistant';
    text: string;
    timestamp: Date;
  }>>([]);

  // Flat list of bees for the current workspace (for config editor)
  const currentBees = beesByWorkspace[workspace] ?? [];

  // Load all workspaces and their bees on mount
  useEffect(() => {
    Promise.all([fetchGraph(), fetchTasks(), fetchWorkspaces()])
      .then(async ([g, t, ws]) => {
        setGraph(g);
        setTasks(t);
        setWorkspaces(ws);
        setError(null);

        // Load bees for all workspaces
        const allBees: Record<string, BeeConfigView[]> = {};
        for (const w of ws) {
          try {
            const b = await fetchBees(w);
            allBees[w] = b.bees;
            if (!workspace) setWorkspace(b.workspace);
          } catch {
            allBees[w] = [];
          }
        }
        setBeesByWorkspace(allBees);
        if (!workspace && ws.length > 0) setWorkspace(ws[0]);

        // Load conversation history for all workspaces
        const allMessages: typeof chatMessages = [];
        for (const w of ws) {
          try {
            const convs = await fetchConversations(w);
            for (const c of convs) {
              if (c.role === 'user' || c.role === 'assistant') {
                allMessages.push({
                  id: `hist-${c.created_at}-${allMessages.length}`,
                  bee: c.bee,
                  workspace: c.workspace,
                  role: c.role as 'user' | 'assistant',
                  text: c.content,
                  timestamp: new Date(c.created_at),
                });
              }
            }
          } catch { /* ignore */ }
        }
        allMessages.sort((a, b) => a.timestamp.getTime() - b.timestamp.getTime());
        setChatMessages(allMessages);

        // Load signals for all workspaces
        const allSignals: typeof signals = [];
        for (const w of ws) {
          try {
            const sigs = await fetchSignals(w);
            allSignals.push(...sigs);
          } catch { /* ignore */ }
        }
        allSignals.sort((a, b) => b.created_at.localeCompare(a.created_at));
        setSignals(allSignals.slice(0, 100));
      })
      .catch(() => {
        setError('Failed to connect to daemon API.');
      });
  }, []);

  // WebSocket for live updates
  useEffect(() => {
    const ws = connectWs((msg) => {
      setConnected(true);
      switch (msg.type) {
        case 'snapshot':
          setGraph(msg.graph);
          setTasks(msg.tasks);
          break;
        case 'task_updated':
          setTasks((prev) => {
            const idx = prev.findIndex((t) => t.id === msg.task.id);
            if (idx >= 0) {
              const next = [...prev];
              next[idx] = msg.task;
              return next;
            }
            return [...prev, msg.task];
          });
          break;
        case 'signal_processed':
          fetchTasks().then(setTasks).catch(console.error);
          break;
        case 'graph_updated':
          setGraph(msg.graph);
          break;
        case 'signal':
          setSignals((prev) => {
            // Deduplicate by id
            if (prev.some((s) => s.id === msg.id)) return prev;
            return [msg, ...prev].slice(0, 100);
          });
          break;
      }
    });
    return () => ws.close();
  }, []);

  function switchWorkspace(ws: string) {
    setWorkspace(ws);
    fetchGraph(ws).then((g) => setGraph(g));
  }

  async function handleSendMessage(bee: string, ws: string, text: string) {
    const id = `chat-${Date.now()}`;
    // Show user message immediately
    setChatMessages((prev) => [...prev, {
      id, bee, workspace: ws, role: 'user', text, timestamp: new Date(),
    }]);
    // Send to daemon and get response
    try {
      const resp = await sendChat(ws, text, bee);
      setChatMessages((prev) => [...prev, {
        id: `${id}-resp`, bee, workspace: ws, role: 'assistant',
        text: resp.text || '(no response)', timestamp: new Date(),
      }]);
    } catch {
      setChatMessages((prev) => [...prev, {
        id: `${id}-err`, bee, workspace: ws, role: 'assistant',
        text: 'Failed to reach Bee', timestamp: new Date(),
      }]);
    }
  }

  function handleDrillIntoTask(taskId: string) {
    setSelectedTaskId(taskId);
    setView('workflow');
  }

  // ── Error screen ──
  if (error) {
    return (
      <div style={{
        height: '100vh', display: 'flex', alignItems: 'center', justifyContent: 'center',
        background: '#f8fafc', color: '#1e293b', fontFamily: 'system-ui, sans-serif',
      }}>
        <div style={{ textAlign: 'center', maxWidth: 420, padding: 24 }}>
          <div style={{ fontSize: 48, marginBottom: 16 }}>🐝</div>
          <h2 style={{ marginBottom: 8, fontSize: 20 }}>Not Connected</h2>
          <p style={{ color: '#64748b', lineHeight: 1.6, fontSize: 14, marginBottom: 20 }}>{error}</p>
          <button onClick={() => window.location.reload()} style={{
            padding: '8px 24px', borderRadius: 8, border: '1px solid #e2e8f0',
            background: '#fff', cursor: 'pointer', fontSize: 14, fontWeight: 500,
          }}>Retry</button>
        </div>
      </div>
    );
  }

  // ── Loading ──
  if (!graph) {
    return (
      <div style={{
        height: '100vh', display: 'flex', alignItems: 'center', justifyContent: 'center',
        background: '#f8fafc', color: '#64748b', fontFamily: 'system-ui', fontSize: 14,
      }}>Loading...</div>
    );
  }

  return (
    <div style={{
      height: '100vh', display: 'flex', flexDirection: 'column',
      background: '#f8fafc', color: '#1e293b',
      fontFamily: 'system-ui, -apple-system, sans-serif',
    }}>
      {/* ── Top nav ── */}
      <div className="nav-bar" style={{
        height: 44, borderBottom: '1px solid #e2e8f0', background: '#fff',
        display: 'flex', alignItems: 'center', padding: '0 16px', gap: 2, flexShrink: 0,
      }}>
        <span style={{ fontSize: 18, marginRight: 8 }}>🐝</span>
        <span style={{ fontSize: 15, fontWeight: 700, color: '#0f172a', marginRight: 20 }}>apiari</span>

        <NavTab active={view === 'briefing'} onClick={() => setView('briefing')}>
          Briefing
        </NavTab>
        <NavTab active={view === 'workflow'} onClick={() => setView('workflow')} className="nav-tab-workflow">
          Workflow
        </NavTab>
        <NavTab active={view === 'bees'} onClick={() => setView('bees')} className="nav-tab-bees">
          Bees
        </NavTab>

        <div style={{ flex: 1 }} />

        {workspaces.length > 1 && view !== 'briefing' && (
          <select value={workspace} onChange={(e) => switchWorkspace(e.target.value)} style={{
            fontSize: 12, padding: '4px 8px', border: '1px solid #e2e8f0', borderRadius: 6,
            background: '#f8fafc', color: '#0f172a', marginRight: 8, cursor: 'pointer',
          }}>
            {workspaces.map((ws) => <option key={ws} value={ws}>{ws}</option>)}
          </select>
        )}

        <span style={{
          width: 8, height: 8, borderRadius: '50%',
          background: connected ? '#22c55e' : '#ef4444',
          boxShadow: connected ? '0 0 6px rgba(34, 197, 94, 0.4)' : 'none',
        }} />
      </div>

      {/* ── Content ── */}
      <div style={{ flex: 1, display: 'flex', overflow: 'hidden' }}>
        {view === 'briefing' && (
          <Briefing
            workspaces={workspaces}
            beesByWorkspace={beesByWorkspace}
            tasks={tasks}
            signals={signals}
            chatMessages={chatMessages}
            connected={connected}
            onSendMessage={handleSendMessage}
            onDrillIntoTask={handleDrillIntoTask}
          />
        )}

        {view === 'workflow' && (
          <>
            <div style={{
              width: 280, background: '#fff', borderRight: '1px solid #e2e8f0',
              display: 'flex', flexDirection: 'column', overflow: 'hidden', flexShrink: 0,
            }}>
              <div style={{ flex: 1, overflow: 'auto' }}>
                <div style={{
                  fontSize: 11, color: '#94a3b8', padding: '14px 18px 6px',
                  textTransform: 'uppercase', letterSpacing: 1, fontWeight: 600,
                  display: 'flex', alignItems: 'center', justifyContent: 'space-between',
                }}>
                  <span>Tasks ({tasks.length})</span>
                  {tasks.length > 0 && (
                    <button onClick={async () => { await clearTasks(); setTasks([]); }} style={{
                      fontSize: 10, color: '#94a3b8', background: 'none', border: '1px solid #e2e8f0',
                      borderRadius: 4, padding: '1px 8px', cursor: 'pointer',
                      textTransform: 'none', letterSpacing: 0, fontWeight: 500,
                    }}>Clear</button>
                  )}
                </div>
                <TaskPanel tasks={tasks} selectedTaskId={selectedTaskId} onSelectTask={setSelectedTaskId} />
              </div>
              <div style={{ borderTop: '1px solid #e2e8f0' }}><SignalPanel /></div>
            </div>

            <div style={{ flex: 1, overflow: 'auto', position: 'relative' }}>
              <button onClick={() => setShowEditor(!showEditor)} style={{
                position: 'sticky', top: 14, float: 'right', marginRight: 18, zIndex: 10,
                padding: '6px 14px', borderRadius: 6, border: '1px solid #e2e8f0',
                background: showEditor ? '#eff6ff' : '#fff',
                color: showEditor ? '#2563eb' : '#64748b',
                cursor: 'pointer', fontSize: 12, fontWeight: 600,
              }}>{showEditor ? 'Close Editor' : 'Edit Graph'}</button>
              <WorkflowGraph graph={graph} tasks={tasks} selectedNodeId={selectedNodeId}
                onSelectNode={showEditor ? setSelectedNodeId : undefined} />
            </div>

            {showEditor && (
              <div style={{
                width: 300, background: '#fff', borderLeft: '1px solid #e2e8f0',
                overflow: 'hidden', flexShrink: 0,
              }}>
                <GraphEditor graph={graph} selectedNodeId={selectedNodeId}
                  onSelectNode={setSelectedNodeId} onGraphChange={setGraph} />
              </div>
            )}
          </>
        )}

        {view === 'bees' && (
          <div style={{ flex: 1, display: 'flex', justifyContent: 'center' }}>
            <div style={{
              width: '100%', maxWidth: 640, background: '#fff',
              borderLeft: '1px solid #e2e8f0', borderRight: '1px solid #e2e8f0',
            }}>
              <BeeEditor bees={currentBees} workspace={workspace}
                onBeesChange={(b) => setBeesByWorkspace((prev) => ({ ...prev, [workspace]: b }))} />
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

function NavTab({ active, onClick, children, className }: {
  active: boolean; onClick: () => void; children: React.ReactNode; className?: string;
}) {
  return (
    <button onClick={onClick} className={className} style={{
      padding: '8px 14px', borderRadius: 6, border: 'none',
      background: active ? '#f1f5f9' : 'transparent',
      color: active ? '#0f172a' : '#64748b',
      cursor: 'pointer', fontSize: 13, fontWeight: active ? 600 : 400,
    }}>{children}</button>
  );
}
