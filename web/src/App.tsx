import { useEffect, useState } from 'react';
import Dashboard from './components/Dashboard';
import WorkflowGraph from './components/WorkflowGraph';
import GraphEditor from './components/GraphEditor';
import BeeEditor from './components/BeeEditor';
import { fetchGraph, fetchTasks, fetchBees, fetchWorkspaces, fetchBriefing, fetchWorkers, fetchConversations, clearTasks, sendChat, runWorkflow, connectWs } from './api';
import type { BeeConfigView, GraphView, TaskView } from './types';

type View = 'home' | 'dashboard' | 'workflow' | 'bees';

export default function App() {
  const [graph, setGraph] = useState<GraphView | null>(null);
  const [tasks, setTasks] = useState<TaskView[]>([]);
  const [selectedTaskId, setSelectedTaskId] = useState<string | null>(null);
  const [selectedNodeId, setSelectedNodeId] = useState<string | null>(null);
  const [connected, setConnected] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showEditor, setShowEditor] = useState(false);
  const [view, setView] = useState<View>('home');
  const [workspace, setWorkspace] = useState('');
  const [workspaces, setWorkspaces] = useState<string[]>([]);
  const [beesByWorkspace, setBeesByWorkspace] = useState<Record<string, BeeConfigView[]>>({});

  const [briefingItems, setBriefingItems] = useState<Array<{
    id: string; priority: string; icon: string; title: string; body: string | null;
    workspace: string; source: string; url: string | null;
    actions: Array<{ label: string; style: string }>; timestamp: string;
  }>>([]);
  const [workers, setWorkers] = useState<Array<{
    id: string; workspace: string; branch: string; agent: string; status: string; pr_url: string | null;
  }>>([]);
  const [chatMessages, setChatMessages] = useState<Array<{
    id: string; bee: string; workspace: string; role: 'user' | 'assistant';
    text: string; timestamp: Date;
  }>>([]);

  const currentBees = beesByWorkspace[workspace] ?? [];

  // ── Load everything on mount ──
  useEffect(() => {
    Promise.all([fetchWorkspaces()])
      .then(async ([ws]) => {
        setWorkspaces(ws);
        setError(null);

        const allBees: Record<string, BeeConfigView[]> = {};
        for (const w of ws) {
          try { const b = await fetchBees(w); allBees[w] = b.bees; } catch { allBees[w] = []; }
        }
        setBeesByWorkspace(allBees);

        fetchBriefing().then(setBriefingItems).catch(() => {});
        fetchWorkers().then(setWorkers).catch(() => {});

        // Load conversations
        const allMessages: typeof chatMessages = [];
        for (const w of ws) {
          try {
            const convs = await fetchConversations(w);
            for (const c of convs) {
              if (c.role === 'user' || c.role === 'assistant') {
                allMessages.push({
                  id: `hist-${c.created_at}-${allMessages.length}`,
                  bee: c.bee, workspace: c.workspace,
                  role: c.role as 'user' | 'assistant',
                  text: c.content, timestamp: new Date(c.created_at),
                });
              }
            }
          } catch {}
        }
        allMessages.sort((a, b) => a.timestamp.getTime() - b.timestamp.getTime());
        setChatMessages(allMessages);
      })
      .catch(() => setError('Failed to connect to daemon API.'));
  }, []);

  // ── WebSocket ──
  useEffect(() => {
    const ws = connectWs((msg) => {
      setConnected(true);
      switch (msg.type) {
        case 'snapshot': setGraph(msg.graph); setTasks(msg.tasks); break;
        case 'task_updated':
          setTasks(prev => {
            const idx = prev.findIndex(t => t.id === msg.task.id);
            if (idx >= 0) { const next = [...prev]; next[idx] = msg.task; return next; }
            return [...prev, msg.task];
          });
          break;
        case 'signal_processed': fetchTasks(workspace).then(setTasks).catch(() => {}); break;
        case 'graph_updated': setGraph(msg.graph); break;
      }
    });
    return () => ws.close();
  }, []);

  function enterWorkspace(ws: string) {
    setWorkspace(ws);
    setView('dashboard');
    fetchGraph(ws).then(setGraph);
    fetchTasks(ws).then(setTasks).catch(() => setTasks([]));
    fetchBriefing().then(setBriefingItems).catch(() => {});
    fetchWorkers().then(setWorkers).catch(() => {});
  }

  function refreshBriefing() {
    fetchBriefing().then(setBriefingItems).catch(() => {});
    fetchWorkers().then(setWorkers).catch(() => {});
  }

  async function handleSendMessage(bee: string, ws: string, text: string) {
    const id = `chat-${Date.now()}`;
    const respId = `${id}-resp`;
    setChatMessages(prev => [...prev, { id, bee, workspace: ws, role: 'user', text, timestamp: new Date() }]);
    setChatMessages(prev => [...prev, { id: respId, bee, workspace: ws, role: 'assistant', text: '', timestamp: new Date() }]);
    try {
      const response = await sendChat(ws, text, bee, (partialText) => {
        setChatMessages(prev => prev.map(msg => msg.id === respId ? { ...msg, text: partialText } : msg));
      });
      const researchMatch = response.match(/\[RESEARCH:\s*(.+?)\]/);
      if (researchMatch) {
        const topic = researchMatch[1].trim();
        const wfId = `${id}-workflow`;
        setChatMessages(prev => [...prev, { id: wfId, bee, workspace: ws, role: 'assistant', text: '🔬 Starting research workflow...\n', timestamp: new Date() }]);
        await runWorkflow(ws, topic, bee, 'researcher',
          (_s, label) => { setChatMessages(prev => prev.map(m => m.id === wfId ? { ...m, text: m.text + `\n## ${label}\n` } : m)); },
          (pt) => { setChatMessages(prev => prev.map(m => m.id === wfId ? { ...m, text: pt } : m)); },
          () => { setChatMessages(prev => prev.map(m => m.id === wfId ? { ...m, text: m.text + '\n---\n' } : m)); },
        );
      }
    } catch {
      setChatMessages(prev => prev.map(msg => msg.id === respId ? { ...msg, text: 'Failed to reach Bee' } : msg));
    }
  }

  // ── Error ──
  if (error) {
    return (
      <div style={{ height: '100vh', display: 'flex', alignItems: 'center', justifyContent: 'center', background: 'var(--bg, #f8fafc)', fontFamily: 'system-ui' }}>
        <div style={{ textAlign: 'center', padding: 24 }}>
          <div style={{ fontSize: 48, marginBottom: 16 }}>🐝</div>
          <h2 style={{ fontSize: 20, marginBottom: 8 }}>Not Connected</h2>
          <p style={{ color: 'var(--text-muted, #64748b)', fontSize: 14, marginBottom: 20 }}>{error}</p>
          <button onClick={() => window.location.reload()} style={{ padding: '8px 24px', borderRadius: 8, border: '1px solid var(--border, #e2e8f0)', background: 'var(--bg-card, #fff)', cursor: 'pointer' }}>Retry</button>
        </div>
      </div>
    );
  }

  return (
    <div className="app-shell" style={{ color: 'var(--text, #1e293b)', fontFamily: 'system-ui, -apple-system, sans-serif' }}>

      {/* ── Home: Workspace picker ── */}
      {view === 'home' && (
        <div style={{ flex: 1, display: 'flex', flexDirection: 'column', alignItems: 'center', justifyContent: 'center', gap: 24, padding: 24 }}>
          <div style={{ textAlign: 'center' }}>
            <div style={{ fontSize: 48, marginBottom: 8 }}>🐝</div>
            <div style={{ fontSize: 20, fontWeight: 700, color: 'var(--text-bright, #0f172a)' }}>apiari</div>
          </div>
          <div style={{ display: 'flex', gap: 12, flexWrap: 'wrap', justifyContent: 'center' }}>
            {workspaces.map(ws => {
              const wsBees = beesByWorkspace[ws] ?? [];
              const wsWorkers = workers.filter(w => w.workspace === ws);
              const wsActions = briefingItems.filter(i => i.workspace === ws && i.priority === 'action');
              return (
                <button key={ws} onClick={() => enterWorkspace(ws)} style={{
                  padding: '20px 28px', borderRadius: 12, border: '1.5px solid #e2e8f0',
                  background: 'var(--bg-card, #fff)', cursor: 'pointer', minWidth: 160,
                  display: 'flex', flexDirection: 'column', alignItems: 'flex-start', gap: 8,
                  transition: 'all 0.15s',
                }}>
                  <span style={{ fontSize: 16, fontWeight: 700, color: 'var(--text-bright, #0f172a)' }}>{ws}</span>
                  <div style={{ display: 'flex', gap: 12, fontSize: 12, color: 'var(--text-muted, #64748b)' }}>
                    <span>🐝 {wsBees.length}</span>
                    <span>🔧 {wsWorkers.length}</span>
                    {wsActions.length > 0 && <span style={{ color: '#dc2626', fontWeight: 600 }}>⚠️ {wsActions.length}</span>}
                  </div>
                </button>
              );
            })}
          </div>
          <div style={{ display: 'flex', alignItems: 'center', gap: 6, fontSize: 11, color: '#94a3b8' }}>
            <span style={{ width: 6, height: 6, borderRadius: '50%', background: connected ? '#22c55e' : '#ef4444' }} />
            {connected ? 'connected' : 'connecting...'}
          </div>
        </div>
      )}

      {/* ── Workspace views ── */}
      {view !== 'home' && (
        <>
          {/* Nav bar */}
          <div className="nav-bar" style={{
            height: 44, borderBottom: '1px solid var(--border, #e2e8f0)', background: 'var(--bg-card, #fff)',
            display: 'flex', alignItems: 'center', padding: '0 12px', gap: 2, flexShrink: 0,
          }}>
            <button onClick={() => setView('home')} style={{
              background: 'none', border: 'none', cursor: 'pointer',
              fontSize: 16, padding: '4px 8px', color: 'var(--text-muted, #64748b)',
            }}>←</button>
            <span style={{ fontSize: 15, fontWeight: 700, color: 'var(--text-bright, #0f172a)', marginRight: 16 }}>{workspace}</span>

            <NavTab active={view === 'dashboard'} onClick={() => setView('dashboard')}>Dashboard</NavTab>
            <NavTab active={view === 'workflow'} onClick={() => setView('workflow')} className="nav-tab-workflow">Workflow</NavTab>
            <NavTab active={view === 'bees'} onClick={() => setView('bees')} className="nav-tab-bees">Bees</NavTab>

            <div style={{ flex: 1 }} />
            <span style={{ width: 8, height: 8, borderRadius: '50%', background: connected ? '#22c55e' : '#ef4444' }} />
          </div>

          {/* Content */}
          <div style={{ flex: 1, display: 'flex', overflow: 'hidden', minHeight: 0 }}>
            {view === 'dashboard' && (
              <Dashboard
                workspace={workspace}
                bees={currentBees}
                briefingItems={briefingItems}
                workers={workers}
                tasks={tasks}
                chatMessages={chatMessages}
                connected={connected}
                onSendMessage={handleSendMessage}
                onDrillIntoTask={(taskId) => { setSelectedTaskId(taskId); setView('workflow'); }}
                onRefreshBriefing={refreshBriefing}
              />
            )}

            {view === 'workflow' && graph && (
              <>
                <div style={{ flex: 1, overflow: 'auto', position: 'relative' }}>
                  <button onClick={() => setShowEditor(!showEditor)} style={{
                    position: 'sticky', top: 14, float: 'right', marginRight: 18, zIndex: 10,
                    padding: '6px 14px', borderRadius: 6, border: '1px solid var(--border, #e2e8f0)',
                    background: showEditor ? '#eff6ff' : '#fff',
                    color: showEditor ? '#2563eb' : '#64748b',
                    cursor: 'pointer', fontSize: 12, fontWeight: 600,
                  }}>{showEditor ? 'Close Editor' : 'Edit Graph'}</button>
                  <WorkflowGraph graph={graph} tasks={tasks} selectedNodeId={selectedNodeId}
                    onSelectNode={showEditor ? setSelectedNodeId : undefined} />
                </div>
                {showEditor && (
                  <div style={{ width: 300, background: 'var(--bg-card, #fff)', borderLeft: '1px solid var(--border, #e2e8f0)', overflow: 'hidden', flexShrink: 0 }}>
                    <GraphEditor graph={graph} selectedNodeId={selectedNodeId}
                      onSelectNode={setSelectedNodeId} onGraphChange={setGraph} />
                  </div>
                )}
              </>
            )}

            {view === 'bees' && (
              <div style={{ flex: 1, display: 'flex', justifyContent: 'center' }}>
                <div style={{ width: '100%', maxWidth: 640, background: 'var(--bg-card, #fff)', borderLeft: '1px solid var(--border, #e2e8f0)', borderRight: '1px solid var(--border, #e2e8f0)' }}>
                  <BeeEditor bees={currentBees} workspace={workspace}
                    onBeesChange={(b) => setBeesByWorkspace(prev => ({ ...prev, [workspace]: b }))} />
                </div>
              </div>
            )}
          </div>
        </>
      )}
    </div>
  );
}

function NavTab({ active, onClick, children, className }: {
  active: boolean; onClick: () => void; children: React.ReactNode; className?: string;
}) {
  return (
    <button onClick={onClick} className={className} style={{
      padding: '8px 12px', borderRadius: 6, border: 'none',
      background: active ? '#f1f5f9' : 'transparent',
      color: active ? '#0f172a' : '#64748b',
      cursor: 'pointer', fontSize: 13, fontWeight: active ? 600 : 400,
    }}>{children}</button>
  );
}
