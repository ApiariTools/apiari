import { useState } from 'react';
import type { BeeConfigView, SignalHookView } from '../types';
import { saveBees } from '../api';

const PROVIDERS = ['claude', 'codex', 'gemini'];
const DEFAULT_MODELS: Record<string, string> = {
  claude: 'sonnet',
  codex: 'o4-mini',
  gemini: 'gemini-2.0-flash',
};

function emptyBee(): BeeConfigView {
  return {
    name: '',
    provider: 'claude',
    model: 'sonnet',
    max_turns: 20,
    max_session_turns: 50,
    signal_hooks: [],
  };
}

function emptyHook(): SignalHookView {
  return { source: '', prompt: '', ttl_secs: 120 };
}

interface BeeEditorProps {
  bees: BeeConfigView[];
  workspace: string;
  onBeesChange: (bees: BeeConfigView[]) => void;
}

export default function BeeEditor({ bees, workspace, onBeesChange }: BeeEditorProps) {
  const [selectedIdx, setSelectedIdx] = useState<number | null>(bees.length > 0 ? 0 : null);
  const [saveStatus, setSaveStatus] = useState<string | null>(null);

  const selected = selectedIdx !== null ? bees[selectedIdx] : null;

  function updateBee(idx: number, patch: Partial<BeeConfigView>) {
    const updated = bees.map((b, i) => (i === idx ? { ...b, ...patch } : b));
    onBeesChange(updated);
  }

  function addBee() {
    const newBee = emptyBee();
    newBee.name = `Bee${bees.length + 1}`;
    const updated = [...bees, newBee];
    onBeesChange(updated);
    setSelectedIdx(updated.length - 1);
  }

  function removeBee(idx: number) {
    const updated = bees.filter((_, i) => i !== idx);
    onBeesChange(updated);
    if (selectedIdx === idx) {
      setSelectedIdx(updated.length > 0 ? Math.min(idx, updated.length - 1) : null);
    } else if (selectedIdx !== null && selectedIdx > idx) {
      setSelectedIdx(selectedIdx - 1);
    }
  }

  function addHook(beeIdx: number) {
    const bee = bees[beeIdx];
    updateBee(beeIdx, { signal_hooks: [...bee.signal_hooks, emptyHook()] });
  }

  function updateHook(beeIdx: number, hookIdx: number, patch: Partial<SignalHookView>) {
    const bee = bees[beeIdx];
    const hooks = bee.signal_hooks.map((h, i) => (i === hookIdx ? { ...h, ...patch } : h));
    updateBee(beeIdx, { signal_hooks: hooks });
  }

  function removeHook(beeIdx: number, hookIdx: number) {
    const bee = bees[beeIdx];
    updateBee(beeIdx, { signal_hooks: bee.signal_hooks.filter((_, i) => i !== hookIdx) });
  }

  async function handleSave() {
    setSaveStatus('Saving...');
    const result = await saveBees(bees, workspace);
    if (result.ok) {
      setSaveStatus('Saved \u2713');
      setTimeout(() => setSaveStatus(null), 2000);
    } else {
      setSaveStatus(`Error: ${result.error}`);
    }
  }

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%' }}>
      {/* Header */}
      <div style={{
        padding: '12px 16px',
        borderBottom: '1px solid #e2e8f0',
        display: 'flex',
        justifyContent: 'space-between',
        alignItems: 'center',
        background: '#f8fafc',
      }}>
        <div>
          <span style={{ fontWeight: 600, fontSize: 14 }}>Bees</span>
          <span style={{ color: '#94a3b8', fontSize: 12, marginLeft: 8 }}>{workspace}</span>
        </div>
        <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
          {saveStatus && (
            <span style={{
              fontSize: 12,
              color: saveStatus.startsWith('Error') ? '#dc2626' : '#16a34a',
            }}>{saveStatus}</span>
          )}
          <button onClick={handleSave} style={btnStyle}>Save</button>
        </div>
      </div>

      {/* Bee list */}
      <div style={{
        padding: '8px 16px',
        borderBottom: '1px solid #e2e8f0',
        display: 'flex',
        gap: 6,
        flexWrap: 'wrap',
        alignItems: 'center',
      }}>
        {bees.map((bee, idx) => (
          <button
            key={idx}
            onClick={() => setSelectedIdx(idx)}
            style={{
              padding: '4px 12px',
              borderRadius: 6,
              border: selectedIdx === idx ? '2px solid #f59e0b' : '1px solid #e2e8f0',
              background: selectedIdx === idx ? '#fef3c7' : '#fff',
              cursor: 'pointer',
              fontSize: 13,
              fontWeight: selectedIdx === idx ? 600 : 400,
            }}
          >
            {bee.name || 'Unnamed'}
            <span style={{ color: '#94a3b8', fontSize: 11, marginLeft: 4 }}>
              {bee.provider}
            </span>
          </button>
        ))}
        <button onClick={addBee} style={{ ...btnStyle, fontSize: 13, padding: '4px 10px' }}>
          + Add Bee
        </button>
      </div>

      {/* Selected bee editor */}
      {selected && selectedIdx !== null ? (
        <div style={{ flex: 1, overflowY: 'auto', padding: 16 }}>
          {/* Identity */}
          <Section title="Identity">
            <Field label="Name">
              <input
                value={selected.name}
                onChange={e => updateBee(selectedIdx, { name: e.target.value })}
                style={inputStyle}
                placeholder="e.g. CodeBee"
              />
            </Field>
            <Field label="Provider">
              <select
                value={selected.provider}
                onChange={e => {
                  const provider = e.target.value;
                  updateBee(selectedIdx, {
                    provider,
                    model: DEFAULT_MODELS[provider] || selected.model,
                  });
                }}
                style={inputStyle}
              >
                {PROVIDERS.map(p => <option key={p} value={p}>{p}</option>)}
              </select>
            </Field>
            <Field label="Model">
              <input
                value={selected.model}
                onChange={e => updateBee(selectedIdx, { model: e.target.value })}
                style={inputStyle}
                placeholder="e.g. sonnet, o4-mini, gemini-2.0-flash"
              />
            </Field>
            <Field label="Telegram Topic ID">
              <input
                value={selected.topic_id ?? ''}
                onChange={e => {
                  const val = e.target.value.trim();
                  updateBee(selectedIdx, {
                    topic_id: val ? parseInt(val, 10) || undefined : undefined,
                  });
                }}
                style={inputStyle}
                placeholder="Optional — unique per bee"
              />
            </Field>
          </Section>

          {/* Session */}
          <Section title="Session">
            <div style={{ display: 'flex', gap: 12 }}>
              <Field label="Max turns per dispatch">
                <input
                  type="number"
                  value={selected.max_turns}
                  onChange={e => updateBee(selectedIdx, { max_turns: parseInt(e.target.value) || 20 })}
                  style={{ ...inputStyle, width: 80 }}
                />
              </Field>
              <Field label="Auto-compact after">
                <input
                  type="number"
                  value={selected.max_session_turns}
                  onChange={e => updateBee(selectedIdx, { max_session_turns: parseInt(e.target.value) || 50 })}
                  style={{ ...inputStyle, width: 80 }}
                />
              </Field>
            </div>
          </Section>

          {/* Prompt */}
          <Section title="Prompt">
            <textarea
              value={selected.prompt ?? ''}
              onChange={e => updateBee(selectedIdx, { prompt: e.target.value || undefined })}
              style={{ ...inputStyle, minHeight: 80, fontFamily: 'monospace', fontSize: 12 }}
              placeholder="Custom prompt preamble (identity, role, specialty)..."
            />
          </Section>

          {/* Signal Hooks */}
          <Section title="Signal Hooks">
            {selected.signal_hooks.map((hook, hookIdx) => (
              <div key={hookIdx} style={{
                border: '1px solid #e2e8f0',
                borderRadius: 6,
                padding: 10,
                marginBottom: 8,
                background: '#fafbfc',
              }}>
                <div style={{ display: 'flex', justifyContent: 'space-between', marginBottom: 6 }}>
                  <span style={{ fontSize: 12, fontWeight: 600, color: '#475569' }}>
                    Hook {hookIdx + 1}
                  </span>
                  <button
                    onClick={() => removeHook(selectedIdx, hookIdx)}
                    style={{ fontSize: 11, color: '#dc2626', background: 'none', border: 'none', cursor: 'pointer' }}
                  >
                    Remove
                  </button>
                </div>
                <Field label="Source">
                  <input
                    value={hook.source}
                    onChange={e => updateHook(selectedIdx, hookIdx, { source: e.target.value })}
                    style={inputStyle}
                    placeholder='e.g. swarm, github, sentry'
                  />
                </Field>
                <Field label="Action">
                  <input
                    value={hook.action ?? ''}
                    onChange={e => updateHook(selectedIdx, hookIdx, { action: e.target.value || undefined })}
                    style={inputStyle}
                    placeholder="What should this bee DO when the hook fires?"
                  />
                </Field>
              </div>
            ))}
            <button onClick={() => addHook(selectedIdx)} style={{ ...btnStyle, fontSize: 12 }}>
              + Add Hook
            </button>
          </Section>

          {/* Danger zone */}
          <div style={{ padding: '16px 0', borderTop: '1px solid #fee2e2', marginTop: 16 }}>
            <button
              onClick={() => removeBee(selectedIdx)}
              style={{
                ...btnStyle,
                background: '#fef2f2',
                color: '#dc2626',
                border: '1px solid #fecaca',
              }}
            >
              Remove this Bee
            </button>
          </div>
        </div>
      ) : (
        <div style={{ flex: 1, display: 'flex', alignItems: 'center', justifyContent: 'center', color: '#94a3b8' }}>
          {bees.length === 0 ? 'No bees configured. Add one to get started.' : 'Select a bee to edit.'}
        </div>
      )}
    </div>
  );
}

// ── Helpers ────────────────────────────────────────────────────────────

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div style={{ marginBottom: 16 }}>
      <div style={{
        fontSize: 11,
        fontWeight: 700,
        textTransform: 'uppercase',
        letterSpacing: '0.05em',
        color: '#94a3b8',
        marginBottom: 8,
      }}>
        {title}
      </div>
      {children}
    </div>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div style={{ marginBottom: 8 }}>
      <label style={{ display: 'block', fontSize: 12, color: '#64748b', marginBottom: 3 }}>
        {label}
      </label>
      {children}
    </div>
  );
}

const inputStyle: React.CSSProperties = {
  width: '100%',
  padding: '6px 10px',
  border: '1px solid #e2e8f0',
  borderRadius: 6,
  fontSize: 13,
  outline: 'none',
  boxSizing: 'border-box',
};

const btnStyle: React.CSSProperties = {
  padding: '6px 14px',
  borderRadius: 6,
  border: '1px solid #e2e8f0',
  background: '#fff',
  cursor: 'pointer',
  fontSize: 13,
  fontWeight: 500,
};
