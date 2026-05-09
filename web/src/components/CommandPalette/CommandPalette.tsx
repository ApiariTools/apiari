import { useEffect, useRef, useState, useCallback } from 'react'
import type { WorkerV2, AutoBot } from '@apiari/types'
import { getWorkerTitle } from '../../utils/workerTitle'
import styles from './CommandPalette.module.css'

// ── Helpers ───────────────────────────────────────────────────────────────

function workerDotClass(state: WorkerV2['state']): string {
  switch (state) {
    case 'running': return styles.dotRunning
    case 'waiting': return styles.dotWaiting
    case 'stalled': return styles.dotStalled
    case 'done': return styles.dotDone
    default: return styles.dotIdle
  }
}

function autoBotDotClass(status: AutoBot['status']): string {
  switch (status) {
    case 'running': return styles.dotRunning
    case 'error': return styles.dotFailed
    default: return styles.dotIdle
  }
}

function matches(query: string, ...fields: (string | null | undefined)[]): boolean {
  if (!query) return true
  const q = query.toLowerCase()
  return fields.some((f) => f && f.toLowerCase().includes(q))
}

// ── Result types ──────────────────────────────────────────────────────────

type ResultItem =
  | { kind: 'worker'; worker: WorkerV2 }
  | { kind: 'auto_bot'; bot: AutoBot }

// ── Main component ────────────────────────────────────────────────────────

export interface CommandPaletteProps {
  workers: WorkerV2[]
  autoBots: AutoBot[]
  onSelectWorker: (id: string) => void
  onSelectAutoBot: (id: string) => void
  onClose: () => void
}

export default function CommandPalette({
  workers,
  autoBots,
  onSelectWorker,
  onSelectAutoBot,
  onClose,
}: CommandPaletteProps) {
  const [query, setQuery] = useState('')
  const [activeIndex, setActiveIndex] = useState(0)
  const inputRef = useRef<HTMLInputElement>(null)
  const listRef = useRef<HTMLDivElement>(null)

  // Autofocus input on open
  useEffect(() => {
    inputRef.current?.focus()
  }, [])

  // Build filtered results
  const filteredWorkers = workers.filter((w) =>
    matches(query, w.display_title, w.goal, w.branch, w.id),
  )
  const filteredBots = autoBots.filter((b) =>
    matches(query, b.name),
  )

  const results: ResultItem[] = [
    ...filteredWorkers.map((w): ResultItem => ({ kind: 'worker', worker: w })),
    ...filteredBots.map((b): ResultItem => ({ kind: 'auto_bot', bot: b })),
  ]

  // Reset active index when results change
  useEffect(() => {
    setActiveIndex(0)
  }, [query])

  const selectItem = useCallback(
    (item: ResultItem) => {
      if (item.kind === 'worker') {
        onSelectWorker(item.worker.id)
      } else {
        onSelectAutoBot(item.bot.id)
      }
    },
    [onSelectWorker, onSelectAutoBot],
  )

  // Keyboard navigation
  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Escape') {
      e.preventDefault()
      onClose()
      return
    }
    if (e.key === 'ArrowDown') {
      e.preventDefault()
      setActiveIndex((i) => Math.min(i + 1, results.length - 1))
      return
    }
    if (e.key === 'ArrowUp') {
      e.preventDefault()
      setActiveIndex((i) => Math.max(i - 1, 0))
      return
    }
    if (e.key === 'Enter') {
      e.preventDefault()
      const item = results[activeIndex]
      if (item) selectItem(item)
      return
    }
  }

  // Scroll active item into view
  useEffect(() => {
    const list = listRef.current
    if (!list) return
    const active = list.querySelector<HTMLElement>(`[data-active="true"]`)
    if (active) {
      active.scrollIntoView({ block: 'nearest' })
    }
  }, [activeIndex])

  // Click outside to close
  const overlayRef = useRef<HTMLDivElement>(null)
  const handleOverlayClick = (e: React.MouseEvent) => {
    if (e.target === overlayRef.current) onClose()
  }

  // Worker rows section offset
  const workerSectionStart = 0
  const botSectionStart = filteredWorkers.length

  return (
    <div
      className={styles.overlay}
      ref={overlayRef}
      onClick={handleOverlayClick}
      data-testid="command-palette-overlay"
      role="dialog"
      aria-modal="true"
      aria-label="Command palette"
    >
      <div className={styles.panel}>
        {/* Search input */}
        <div className={styles.inputRow}>
          <input
            ref={inputRef}
            className={styles.input}
            type="text"
            placeholder="Search workers, auto bots..."
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={handleKeyDown}
            aria-label="Search"
            data-testid="command-palette-input"
          />
        </div>

        {/* Results */}
        <div className={styles.results} ref={listRef} data-testid="command-palette-results">
          {results.length === 0 && (
            <div className={styles.empty}>No results</div>
          )}

          {/* Workers section */}
          {filteredWorkers.length > 0 && (
            <div className={styles.section}>
              <div className={styles.sectionLabel}>Workers</div>
              {filteredWorkers.map((w, i) => {
                const globalIdx = workerSectionStart + i
                const isActive = globalIdx === activeIndex
                const dotCls = workerDotClass(w.state)
                const name = getWorkerTitle(w)

                return (
                  <button
                    key={w.id}
                    className={`${styles.resultRow} ${isActive ? styles.resultRowActive : ''}`}
                    data-active={isActive ? 'true' : undefined}
                    onClick={() => selectItem({ kind: 'worker', worker: w })}
                    onMouseEnter={() => setActiveIndex(globalIdx)}
                    type="button"
                    data-testid="palette-worker-row"
                    aria-label={`Worker: ${name}`}
                  >
                    <span className={`${styles.dot} ${dotCls}`} aria-hidden="true" />
                    <span className={styles.resultName}>{name}</span>
                    <span className={styles.resultMeta}>{w.label || w.state}</span>
                    {w.branch && (
                      <span className={styles.resultSub}>{w.branch}</span>
                    )}
                  </button>
                )
              })}
            </div>
          )}

          {/* Auto Bots section */}
          {filteredBots.length > 0 && (
            <div className={styles.section}>
              <div className={styles.sectionLabel}>Auto Bots</div>
              {filteredBots.map((b, i) => {
                const globalIdx = botSectionStart + i
                const isActive = globalIdx === activeIndex
                const dotCls = autoBotDotClass(b.status)

                return (
                  <button
                    key={b.id}
                    className={`${styles.resultRow} ${isActive ? styles.resultRowActive : ''}`}
                    data-active={isActive ? 'true' : undefined}
                    onClick={() => selectItem({ kind: 'auto_bot', bot: b })}
                    onMouseEnter={() => setActiveIndex(globalIdx)}
                    type="button"
                    data-testid="palette-bot-row"
                    aria-label={`Auto bot: ${b.name}`}
                  >
                    <span className={`${styles.dot} ${dotCls}`} aria-hidden="true" />
                    <span className={styles.resultName}>{b.name}</span>
                    <span className={styles.resultMeta}>{b.status}</span>
                  </button>
                )
              })}
            </div>
          )}
        </div>
      </div>
    </div>
  )
}
