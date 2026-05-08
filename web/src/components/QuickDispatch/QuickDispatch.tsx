import { useEffect, useRef, useState } from 'react'
import { getRepos, createWorkerV2, chatWithContextBot } from '../../api'
import type { Repo } from '../../types'
import styles from './QuickDispatch.module.css'

export interface QuickDispatchProps {
  workspace: string
  onClose: () => void
  onDispatched: (workerId: string) => void
}

type ReviewMode = 'local_first' | 'pr_first'
type AgentChoice = 'auto' | 'claude' | 'codex' | 'gemini'

const MODEL_OPTIONS: Record<Exclude<AgentChoice, 'auto'>, Array<{ value: string | null; label: string }>> = {
  claude: [
    { value: null, label: 'Default' },
    { value: 'opus', label: 'Opus' },
    { value: 'sonnet', label: 'Sonnet' },
    { value: 'haiku', label: 'Haiku' },
  ],
  codex: [
    { value: null, label: 'Default' },
    { value: 'gpt-5.5', label: 'GPT-5.5' },
    { value: 'gpt-5.4', label: 'GPT-5.4' },
    { value: 'gpt-5.4-mini', label: 'GPT-5.4 Mini' },
    { value: 'gpt-5.3-codex', label: 'GPT-5.3 Codex' },
    { value: 'o4-mini', label: 'o4-mini' },
    { value: 'o3', label: 'o3' },
  ],
  gemini: [
    { value: null, label: 'Default' },
    { value: 'gemini-2.5-pro', label: '2.5 Pro' },
    { value: 'gemini-2.5-flash', label: '2.5 Flash' },
    { value: 'gemini-2.0-flash', label: '2.0 Flash' },
  ],
}

export default function QuickDispatch({ workspace, onClose, onDispatched }: QuickDispatchProps) {
  const [intent, setIntent] = useState('')
  const [repos, setRepos] = useState<Repo[]>([])
  const [selectedRepo, setSelectedRepo] = useState<string | null>(null)
  const [reviewMode, setReviewMode] = useState<ReviewMode>('local_first')
  const [agent, setAgent] = useState<AgentChoice>('auto')
  const [model, setModel] = useState<string | null>(null)
  const [dispatching, setDispatching] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const textareaRef = useRef<HTMLTextAreaElement>(null)

  // Fetch repos on mount
  useEffect(() => {
    getRepos(workspace)
      .then((list) => {
        setRepos(list)
        if (list.length > 0 && selectedRepo === null) {
          setSelectedRepo(list[0].name)
        }
      })
      .catch(() => {
        // ignore — user can type repo name manually if needed
      })
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [workspace])

  // Autofocus textarea on mount
  useEffect(() => {
    textareaRef.current?.focus()
  }, [])

  // Escape closes
  useEffect(() => {
    function handleKey(e: KeyboardEvent) {
      if (e.key === 'Escape') {
        onClose()
      }
      if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
        if (intent.trim() && selectedRepo && !dispatching) {
          handleDispatch()
        }
      }
    }
    window.addEventListener('keydown', handleKey)
    return () => window.removeEventListener('keydown', handleKey)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [intent, selectedRepo, dispatching])

  useEffect(() => {
    if (agent === 'auto') {
      setModel(null)
      return
    }
    const options = MODEL_OPTIONS[agent]
    if (!options.some((option) => option.value === model)) {
      setModel(options[0]?.value ?? null)
    }
  }, [agent, model])

  async function handleDispatch() {
    if (!intent.trim() || !selectedRepo || dispatching) return
    setDispatching(true)
    setError(null)

    try {
      const worker = await createWorkerV2(workspace, {
        brief: {
          goal: intent.trim(),
          repo: selectedRepo,
          review_mode: reviewMode,
          context: {},
          constraints: [],
          scope: [],
          acceptance_criteria: [],
        },
        repo: selectedRepo,
        ...(agent !== 'auto' ? { agent, model } : {}),
      })

      // Fire-and-forget: enrich the brief with context bot in background
      chatWithContextBot(
        workspace,
        'Generate a detailed implementation brief for: ' + intent.trim(),
        {
          view: 'quick_dispatch',
          entity_id: null,
          entity_snapshot: { goal: intent.trim(), repo: selectedRepo },
        },
      ).catch(() => {
        // ignore — this is best-effort enrichment
      })

      onDispatched(worker.worker_id)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Dispatch failed. Please try again.')
      setDispatching(false)
    }
  }

  const canDispatch = intent.trim().length > 0 && selectedRepo !== null && !dispatching

  return (
    <div
      className={styles.overlay}
      data-testid="quick-dispatch-overlay"
      onMouseDown={(e) => {
        // Close when clicking directly on overlay background
        if (e.target === e.currentTarget) onClose()
      }}
    >
      <div
        className={styles.panel}
        role="dialog"
        aria-modal="true"
        aria-label="Quick dispatch"
        onMouseDown={(e) => e.stopPropagation()}
      >
        {/* Intent field */}
        <label className={styles.label} htmlFor="quick-dispatch-intent">
          What do you want to do?
        </label>
        <textarea
          id="quick-dispatch-intent"
          ref={textareaRef}
          className={styles.intentTextarea}
          rows={5}
          placeholder="Fix the rate limiting on the API endpoint..."
          value={intent}
          onChange={(e) => setIntent(e.target.value)}
          data-testid="intent-textarea"
        />

        {/* Repo pills */}
        <div className={styles.repoSection}>
          <span className={styles.label}>Repo</span>
          <div className={styles.pills} data-testid="repo-pills">
            {repos.map((repo) => (
              <button
                key={repo.name}
                type="button"
                className={`${styles.pill} ${selectedRepo === repo.name ? styles.pillSelected : ''}`}
                onClick={() => setSelectedRepo(repo.name)}
                aria-pressed={selectedRepo === repo.name}
                data-testid={`repo-pill-${repo.name}`}
              >
                {repo.name}
              </button>
            ))}
          </div>
        </div>

        {/* Review mode */}
        <div className={styles.reviewSection}>
          <span className={styles.label}>Worker</span>
          <div className={styles.pills} data-testid="agent-pills">
            {(['auto', 'claude', 'codex', 'gemini'] as AgentChoice[]).map((choice) => (
              <button
                key={choice}
                type="button"
                className={`${styles.pill} ${agent === choice ? styles.pillSelected : ''}`}
                onClick={() => setAgent(choice)}
                aria-pressed={agent === choice}
                data-testid={`agent-pill-${choice}`}
              >
                {choice === 'auto' ? 'Auto' : choice.charAt(0).toUpperCase() + choice.slice(1)}
              </button>
            ))}
          </div>
        </div>

        {agent !== 'auto' && (
          <div className={styles.reviewSection}>
            <span className={styles.label}>Model</span>
            <div className={styles.pills} data-testid="model-pills">
              {MODEL_OPTIONS[agent].map((option) => (
                <button
                  key={option.value ?? 'default'}
                  type="button"
                  className={`${styles.pill} ${model === option.value ? styles.pillSelected : ''}`}
                  onClick={() => setModel(option.value)}
                  aria-pressed={model === option.value}
                  data-testid={`model-pill-${option.value ?? 'default'}`}
                >
                  {option.label}
                </button>
              ))}
            </div>
          </div>
        )}

        <div className={styles.reviewSection}>
          <span className={styles.label}>Review mode</span>
          <div className={styles.pills} data-testid="review-mode-pills">
            <button
              type="button"
              className={`${styles.pill} ${reviewMode === 'local_first' ? styles.pillSelected : ''}`}
              onClick={() => setReviewMode('local_first')}
              aria-pressed={reviewMode === 'local_first'}
              data-testid="review-mode-local"
            >
              Local first
            </button>
            <button
              type="button"
              className={`${styles.pill} ${reviewMode === 'pr_first' ? styles.pillSelected : ''}`}
              onClick={() => setReviewMode('pr_first')}
              aria-pressed={reviewMode === 'pr_first'}
              data-testid="review-mode-pr"
            >
              PR first
            </button>
          </div>
        </div>

        {/* Error */}
        {error && (
          <p className={styles.errorMsg} role="alert" data-testid="dispatch-error">
            {error}
          </p>
        )}

        {/* Footer */}
        <div className={styles.footer}>
          <button
            type="button"
            className={styles.cancelBtn}
            onClick={onClose}
            data-testid="cancel-btn"
          >
            Cancel
          </button>
          <button
            type="button"
            className={styles.dispatchBtn}
            disabled={!canDispatch}
            onClick={handleDispatch}
            data-testid="dispatch-btn"
          >
            {dispatching && <span className={styles.spinner} aria-hidden="true" />}
            Dispatch
          </button>
        </div>
      </div>
    </div>
  )
}
