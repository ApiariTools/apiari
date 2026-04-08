import { useEffect, useState } from 'react';
import WorkflowGraph from './components/WorkflowGraph';
import TaskPanel from './components/TaskPanel';
import SignalPanel from './components/SignalPanel';
import GraphEditor from './components/GraphEditor';
import BeeEditor from './components/BeeEditor';
import { fetchGraph, fetchTasks, fetchBees, clearTasks, connectWs } from './api';
import type { BeeConfigView, GraphView, TaskView } from './types';

type View = 'workflow' | 'bees';

export default function App() {
  const [graph, setGraph] = useState<GraphView | null>(null);
  const [tasks, setTasks] = useState<TaskView[]>([]);
  const [selectedTaskId, setSelectedTaskId] = useState<string | null>(null);
  const [selectedNodeId, setSelectedNodeId] = useState<string | null>(null);
  const [connected, setConnected] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [showEditor, setShowEditor] = useState(false);
  const [view, setView] = useState<View>('workflow');
  const [bees, setBees] = useState<BeeConfigView[]>([]);
  const [workspace, setWorkspace] = useState('');

  // Initial fetch
  useEffect(() => {
    Promise.all([fetchGraph(), fetchTasks(), fetchBees()])
      .then(([g, t, b]) => {
        setGraph(g);
        setTasks(t);
        setBees(b.bees);
        setWorkspace(b.workspace);
        setError(null);
      })
      .catch(() => {
        setError(`Failed to connect to daemon API. Is "cargo run -p apiari -- web" running?`);
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
      }
    });

    return () => ws.close();
  }, []);

  if (error) {
    return (
      <div style={{
        height: '100vh',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        background: '#f8fafc',
        color: '#1e293b',
        fontFamily: 'system-ui, sans-serif',
      }}>
        <div style={{ textAlign: 'center', maxWidth: 420, padding: 24 }}>
          <div style={{ fontSize: 48, marginBottom: 16 }}>🐝</div>
          <h2 style={{ color: '#1e293b', marginBottom: 8, fontSize: 20 }}>Not Connected</h2>
          <p style={{ color: '#64748b', lineHeight: 1.6, fontSize: 14, marginBottom: 20 }}>
            {error}
          </p>
          <code style={{
            display: 'block',
            background: '#1e293b',
            color: '#e2e8f0',
            padding: '12px 16px',
            borderRadius: 8,
            fontSize: 13,
            marginBottom: 20,
            textAlign: 'left',
          }}>
            cargo run -p apiari -- web
          </code>
          <button
            onClick={() => window.location.reload()}
            style={{
              padding: '8px 24px',
              borderRadius: 8,
              border: '1px solid #e2e8f0',
              background: '#ffffff',
              color: '#1e293b',
              cursor: 'pointer',
              fontSize: 14,
              fontWeight: 500,
            }}
          >
            Retry Connection
          </button>
        </div>
      </div>
    );
  }

  if (!graph) {
    return (
      <div style={{
        height: '100vh',
        display: 'flex',
        alignItems: 'center',
        justifyContent: 'center',
        background: '#f8fafc',
        color: '#64748b',
        fontFamily: 'system-ui, sans-serif',
        fontSize: 14,
      }}>
        Loading...
      </div>
    );
  }

  return (
    <div style={{
      height: '100vh',
      display: 'flex',
      flexDirection: 'column',
      background: '#f8fafc',
      color: '#1e293b',
      fontFamily: 'system-ui, -apple-system, sans-serif',
    }}>
      {/* Top nav bar */}
      <div style={{
        height: 44,
        borderBottom: '1px solid #e2e8f0',
        background: '#fff',
        display: 'flex',
        alignItems: 'center',
        padding: '0 16px',
        gap: 2,
        flexShrink: 0,
      }}>
        <span style={{ fontSize: 18, marginRight: 8 }}>🐝</span>
        <span style={{ fontSize: 15, fontWeight: 700, color: '#0f172a', marginRight: 24 }}>
          apiari
        </span>
        <NavTab active={view === 'workflow'} onClick={() => setView('workflow')}>
          Workflow
        </NavTab>
        <NavTab active={view === 'bees'} onClick={() => setView('bees')}>
          Bees ({bees.length})
        </NavTab>
        <div style={{ flex: 1 }} />
        <span style={{
          width: 8,
          height: 8,
          borderRadius: '50%',
          background: connected ? '#22c55e' : '#ef4444',
          boxShadow: connected ? '0 0 6px rgba(34, 197, 94, 0.4)' : 'none',
        }} />
        <span style={{ fontSize: 12, color: '#94a3b8', marginLeft: 6 }}>
          {workspace || 'connecting...'}
        </span>
      </div>

      {/* Main content */}
      <div style={{ flex: 1, display: 'flex', overflow: 'hidden' }}>
        {view === 'workflow' ? (
          <>
            {/* Left sidebar — tasks + signals */}
            <div style={{
              width: 280,
              background: '#ffffff',
              borderRight: '1px solid #e2e8f0',
              display: 'flex',
              flexDirection: 'column',
              overflow: 'hidden',
              flexShrink: 0,
            }}>
              <div style={{ flex: 1, overflow: 'auto' }}>
                <div style={{
                  fontSize: 11,
                  color: '#94a3b8',
                  padding: '14px 18px 6px',
                  textTransform: 'uppercase',
                  letterSpacing: 1,
                  fontWeight: 600,
                  display: 'flex',
                  alignItems: 'center',
                  justifyContent: 'space-between',
                }}>
                  <span>Tasks ({tasks.length})</span>
                  {tasks.length > 0 && (
                    <button
                      onClick={async () => {
                        await clearTasks();
                        setTasks([]);
                      }}
                      style={{
                        fontSize: 10,
                        color: '#94a3b8',
                        background: 'none',
                        border: '1px solid #e2e8f0',
                        borderRadius: 4,
                        padding: '1px 8px',
                        cursor: 'pointer',
                        textTransform: 'none',
                        letterSpacing: 0,
                        fontWeight: 500,
                      }}
                    >
                      Clear
                    </button>
                  )}
                </div>
                <TaskPanel
                  tasks={tasks}
                  selectedTaskId={selectedTaskId}
                  onSelectTask={setSelectedTaskId}
                />
              </div>

              <div style={{ borderTop: '1px solid #e2e8f0' }}>
                <SignalPanel />
              </div>
            </div>

            {/* Center — pipeline graph */}
            <div style={{ flex: 1, overflow: 'auto', position: 'relative' }}>
              <button
                onClick={() => setShowEditor(!showEditor)}
                style={{
                  position: 'sticky',
                  top: 14,
                  float: 'right',
                  marginRight: 18,
                  zIndex: 10,
                  padding: '6px 14px',
                  borderRadius: 6,
                  border: '1px solid #e2e8f0',
                  background: showEditor ? '#eff6ff' : '#ffffff',
                  color: showEditor ? '#2563eb' : '#64748b',
                  cursor: 'pointer',
                  fontSize: 12,
                  fontWeight: 600,
                }}
              >
                {showEditor ? 'Close Editor' : 'Edit Graph'}
              </button>

              <WorkflowGraph
                graph={graph}
                tasks={tasks}
                selectedNodeId={selectedNodeId}
                onSelectNode={showEditor ? setSelectedNodeId : undefined}
              />
            </div>

            {/* Right sidebar — graph editor */}
            {showEditor && (
              <div style={{
                width: 300,
                background: '#ffffff',
                borderLeft: '1px solid #e2e8f0',
                overflow: 'hidden',
                flexShrink: 0,
              }}>
                <GraphEditor
                  graph={graph}
                  selectedNodeId={selectedNodeId}
                  onSelectNode={setSelectedNodeId}
                  onGraphChange={setGraph}
                />
              </div>
            )}
          </>
        ) : (
          /* Bees config view */
          <div style={{
            flex: 1,
            display: 'flex',
            justifyContent: 'center',
          }}>
            <div style={{
              width: '100%',
              maxWidth: 640,
              background: '#fff',
              borderLeft: '1px solid #e2e8f0',
              borderRight: '1px solid #e2e8f0',
            }}>
              <BeeEditor
                bees={bees}
                workspace={workspace}
                onBeesChange={setBees}
              />
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

function NavTab({ active, onClick, children }: {
  active: boolean;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      onClick={onClick}
      style={{
        padding: '8px 14px',
        borderRadius: 6,
        border: 'none',
        background: active ? '#f1f5f9' : 'transparent',
        color: active ? '#0f172a' : '#64748b',
        cursor: 'pointer',
        fontSize: 13,
        fontWeight: active ? 600 : 400,
      }}
    >
      {children}
    </button>
  );
}
