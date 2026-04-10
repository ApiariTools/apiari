import { useState } from 'react';
import { injectSignal } from '../api';

const SIMULATION_STEPS = [
  {
    label: '1. Worker Spawned',
    source: 'swarm_worker_spawned',
    title: 'Worker spawned for task',
    metadata: { worker_id: 'worker-test-1' },
    delay: 0,
  },
  {
    label: '2. Branch Ready',
    source: 'swarm_branch_ready',
    title: 'Branch pushed: feat/test',
    metadata: { branch_name: 'feat/test', worker_id: 'worker-test-1' },
    delay: 2000,
  },
  {
    label: '3. AI Review: Approved',
    source: 'swarm_review_verdict',
    title: 'AI review: APPROVED',
    metadata: { verdict: 'APPROVED', branch_name: 'feat/test', reviewer_worker_id: 'reviewer-1' },
    delay: 2500,
  },
  {
    label: '4. PR Opened',
    source: 'swarm_pr_opened',
    title: 'PR #42 opened',
    metadata: { pr_url: 'https://github.com/test/repo/pull/42', pr_number: 42, repo: 'test/repo', worker_id: 'worker-test-1' },
    delay: 2000,
  },
  {
    label: '5. PR Merged',
    source: 'github_merged_pr',
    title: 'PR #42 merged',
    metadata: { repo: 'test/repo', number: 42, pr_number: 42 },
    delay: 2500,
  },
];

const INDIVIDUAL_SIGNALS = [
  {
    label: 'Worker Spawned',
    source: 'swarm_worker_spawned',
    title: 'Worker spawned',
    metadata: { worker_id: 'worker-test-1' },
  },
  {
    label: 'Branch Ready',
    source: 'swarm_branch_ready',
    title: 'Branch ready',
    metadata: { branch_name: 'feat/test', worker_id: 'worker-test-1' },
  },
  {
    label: 'Review: Approved',
    source: 'swarm_review_verdict',
    title: 'APPROVED',
    metadata: { verdict: 'APPROVED', branch_name: 'feat/test', reviewer_worker_id: 'reviewer-1' },
  },
  {
    label: 'Review: Changes Requested',
    source: 'swarm_review_verdict',
    title: 'CHANGES_REQUESTED',
    metadata: { verdict: 'CHANGES_REQUESTED', comments: 'Fix tests', branch_name: 'feat/test', reviewer_worker_id: 'reviewer-1' },
  },
  {
    label: 'PR Opened',
    source: 'swarm_pr_opened',
    title: 'PR opened',
    metadata: { pr_url: 'https://github.com/test/repo/pull/42', pr_number: 42, repo: 'test/repo' },
  },
  {
    label: 'PR Merged',
    source: 'github_merged_pr',
    title: 'PR merged',
    metadata: { repo: 'test/repo', pr_number: 42 },
  },
  {
    label: 'PR Closed',
    source: 'github_pr_closed',
    title: 'PR closed',
    metadata: { repo: 'test/repo', pr_number: 42 },
  },
];

export default function SignalPanel() {
  const [simulating, setSimulating] = useState(false);
  const [simStep, setSimStep] = useState(-1);
  const [showManual, setShowManual] = useState(false);

  const runSimulation = async () => {
    setSimulating(true);
    for (let i = 0; i < SIMULATION_STEPS.length; i++) {
      const step = SIMULATION_STEPS[i];
      if (step.delay > 0) {
        await new Promise((r) => setTimeout(r, step.delay));
      }
      setSimStep(i);
      await injectSignal(step.source, step.title, step.metadata);
    }
    await new Promise((r) => setTimeout(r, 1000));
    setSimulating(false);
    setSimStep(-1);
  };

  return (
    <div style={{ padding: 12 }}>
      {/* Simulation button */}
      <button
        onClick={runSimulation}
        disabled={simulating}
        style={{
          width: '100%',
          padding: '10px 16px',
          borderRadius: 8,
          border: 'none',
          background: simulating ? '#e2e8f0' : '#2563eb',
          color: simulating ? '#64748b' : '#ffffff',
          cursor: simulating ? 'default' : 'pointer',
          fontSize: 14,
          fontWeight: 600,
          marginBottom: 8,
          transition: 'all 0.2s ease',
        }}
      >
        {simulating
          ? `Running... (${SIMULATION_STEPS[simStep]?.label ?? ''})`
          : '▶  Run Full Simulation'}
      </button>

      {simulating && (
        <div style={{ marginBottom: 12 }}>
          {SIMULATION_STEPS.map((step, i) => (
            <div
              key={i}
              style={{
                fontSize: 12,
                padding: '3px 8px',
                color: i < simStep ? '#16a34a' : i === simStep ? '#2563eb' : '#94a3b8',
                fontWeight: i === simStep ? 600 : 400,
              }}
            >
              {i < simStep ? '✓' : i === simStep ? '→' : '·'} {step.label}
            </div>
          ))}
        </div>
      )}

      {/* Manual signals toggle */}
      <button
        onClick={() => setShowManual(!showManual)}
        style={{
          width: '100%',
          padding: '6px 12px',
          borderRadius: 6,
          border: '1px solid #e2e8f0',
          background: '#f8fafc',
          color: '#64748b',
          cursor: 'pointer',
          fontSize: 12,
          textAlign: 'left',
        }}
      >
        {showManual ? '▾' : '▸'} Manual Signals
      </button>

      {showManual && (
        <div style={{ display: 'flex', flexDirection: 'column', gap: 3, marginTop: 6 }}>
          {INDIVIDUAL_SIGNALS.map((sig) => (
            <button
              key={sig.label}
              onClick={() => injectSignal(sig.source, sig.title, sig.metadata)}
              style={{
                padding: '5px 10px',
                borderRadius: 5,
                border: '1px solid #e2e8f0',
                background: '#ffffff',
                color: '#475569',
                cursor: 'pointer',
                fontSize: 12,
                textAlign: 'left',
              }}
            >
              {sig.label}
              <span style={{ float: 'right', color: '#94a3b8', fontSize: 10 }}>
                {sig.source.replace('swarm_', '').replace('github_', '')}
              </span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}
