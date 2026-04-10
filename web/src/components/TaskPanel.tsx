import type { TaskView } from '../types';

interface Props {
  tasks: TaskView[];
  selectedTaskId: string | null;
  onSelectTask: (id: string) => void;
}

const STAGE_COLORS: Record<string, string> = {
  'Triage': '#16a34a',
  'In Progress': '#2563eb',
  'In AI Review': '#d97706',
  'Human Review': '#7c3aed',
  'Merged': '#059669',
  'Dismissed': '#dc2626',
};

export default function TaskPanel({ tasks, selectedTaskId, onSelectTask }: Props) {
  if (tasks.length === 0) {
    return (
      <div style={{ padding: 16, color: '#94a3b8', fontStyle: 'italic', fontSize: 13 }}>
        No tasks yet. Click "Run Full Simulation" below to watch a task walk through the graph.
      </div>
    );
  }

  return (
    <div style={{ padding: '4px 0' }}>
      {tasks.map((task) => (
        <div
          key={task.id}
          onClick={() => onSelectTask(task.id)}
          style={{
            padding: '10px 14px',
            margin: '3px 8px',
            borderRadius: 8,
            cursor: 'pointer',
            background: task.id === selectedTaskId ? '#f1f5f9' : '#ffffff',
            border: task.id === selectedTaskId ? '1px solid #cbd5e1' : '1px solid #f1f5f9',
            transition: 'all 0.15s ease',
          }}
        >
          <div style={{
            fontSize: 13,
            fontWeight: 600,
            color: '#1e293b',
            marginBottom: 6,
            lineHeight: 1.3,
          }}>
            {task.title}
          </div>
          <div style={{ display: 'flex', alignItems: 'center', gap: 8, fontSize: 11 }}>
            <span
              style={{
                padding: '2px 8px',
                borderRadius: 4,
                background: STAGE_COLORS[task.stage] ?? '#64748b',
                color: '#fff',
                fontWeight: 600,
                fontSize: 10,
                letterSpacing: 0.3,
              }}
            >
              {task.stage}
            </span>
            {task.cursor && (
              <span style={{
                color: '#64748b',
                background: '#f1f5f9',
                padding: '2px 6px',
                borderRadius: 3,
                fontSize: 10,
                fontFamily: 'monospace',
              }}>
                {task.cursor.current_node}
              </span>
            )}
          </div>
          {task.cursor && task.cursor.history.length > 0 && (
            <div style={{
              marginTop: 6,
              fontSize: 10,
              color: '#94a3b8',
              display: 'flex',
              gap: 4,
              flexWrap: 'wrap',
            }}>
              {task.cursor.history.map((step, i) => (
                <span key={i}>
                  {i > 0 && '→ '}
                  {step.to_node}
                </span>
              ))}
            </div>
          )}
        </div>
      ))}
    </div>
  );
}
