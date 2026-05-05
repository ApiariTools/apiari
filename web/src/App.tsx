import { useEffect, useRef, useState } from 'react'
import Layout from './components/Layout/Layout'
import Sidebar from './components/Sidebar/Sidebar'
import BottomTabBar from './components/BottomTabBar/BottomTabBar'
import Dashboard from './components/Dashboard/Dashboard'
import WorkerDetailV2 from './components/WorkerDetailV2/WorkerDetailV2'
import AutoBotDetail from './components/AutoBotDetail/AutoBotDetail'
import ContextBotManager from './components/ContextBot/ContextBotManager'
import CommandPalette from './components/CommandPalette/CommandPalette'
import QuickDispatch from './components/QuickDispatch/QuickDispatch'
import { Bot, Wrench } from 'lucide-react'
import { getWorkspaces, listWorkersV2, listAutoBots, connectWebSocket, chatWithContextBot } from './api'
import type { WorkerV2, AutoBot, ContextBotContext, ContextBotSession } from './types'
import type { SidebarItem } from './components/Sidebar/Sidebar'
import './theme.css'

type EntityType = 'auto_bot' | 'worker'

interface SelectedEntity {
  type: EntityType
  id: string
}

function workerToSidebarItem(w: WorkerV2): SidebarItem {
  return {
    id: w.id,
    name: w.goal ?? w.branch ?? w.id,
    status: w.is_stalled ? 'stalled' : w.state,
    meta: w.branch ?? undefined,
  }
}

function autoBotToSidebarItem(b: AutoBot): SidebarItem {
  return {
    id: b.id,
    name: b.name,
    status: b.status,
    meta: b.trigger_type === 'cron' ? 'cron' : (b.signal_source ?? 'signal'),
  }
}

export default function App() {
  const [workspaces, setWorkspaces] = useState<string[]>([])
  const [workspace, setWorkspace] = useState<string>('')
  const [selected, setSelected] = useState<SelectedEntity | null>(null)
  const [mobileTab, setMobileTab] = useState('workers')
  const [workers, setWorkers] = useState<WorkerV2[]>([])
  const [autoBots, setAutoBots] = useState<AutoBot[]>([])
  const [loading, setLoading] = useState(true)
  const [contextSessions, setContextSessions] = useState<ContextBotSession[]>([])
  const [paletteOpen, setPaletteOpen] = useState(false)
  const [quickDispatchOpen, setQuickDispatchOpen] = useState(false)
  const workerPollRef = useRef<number | null>(null)
  const autoBotPollRef = useRef<number | null>(null)
  const workspaceRef = useRef<string>('')

  // Keep workspaceRef in sync so the WS handler always sees the latest value
  workspaceRef.current = workspace

  // Fetch workspace list on mount
  useEffect(() => {
    getWorkspaces()
      .then((ws) => {
        const names = ws.map((w) => w.name)
        setWorkspaces(names)
        if (names.length > 0) setWorkspace(names[0])
      })
      .catch(() => {
        // fallback: keep workspace empty
      })
  }, [])

  const handleSelect = (type: EntityType, id: string) => {
    setSelected({ type, id })
  }

  // ── Context bot handlers ────────────────────────────────────────────

  function openContextBot(context: ContextBotContext, title: string) {
    const id = crypto.randomUUID()
    setContextSessions((prev) => [
      ...prev,
      {
        id,
        context,
        title,
        messages: [],
        minimized: false,
        loading: false,
      },
    ])
  }

  async function handleContextBotSend(sessionId: string, message: string) {
    const session = contextSessions.find((s) => s.id === sessionId)
    if (!session) return

    // Add user message + set loading
    setContextSessions((prev) =>
      prev.map((s) =>
        s.id === sessionId
          ? {
              ...s,
              loading: true,
              messages: [
                ...s.messages,
                { role: 'user' as const, content: message, timestamp: new Date().toISOString() },
              ],
            }
          : s,
      ),
    )

    try {
      const res = await chatWithContextBot(workspace, message, session.context, sessionId)

      setContextSessions((prev) =>
        prev.map((s) =>
          s.id === sessionId
            ? {
                ...s,
                loading: false,
                messages: [
                  ...s.messages,
                  { role: 'assistant' as const, content: res.response, timestamp: new Date().toISOString() },
                ],
              }
            : s,
        ),
      )

      // If backend dispatched a worker, refresh workers and select it
      if (res.dispatched_worker_id) {
        try {
          const list = await listWorkersV2(workspace)
          setWorkers(list)
        } catch {
          // ignore
        }
        setSelected({ type: 'worker', id: res.dispatched_worker_id })
      }
    } catch {
      setContextSessions((prev) =>
        prev.map((s) =>
          s.id === sessionId
            ? {
                ...s,
                loading: false,
                messages: [
                  ...s.messages,
                  {
                    role: 'assistant' as const,
                    content: 'Sorry, something went wrong. Please try again.',
                    timestamp: new Date().toISOString(),
                  },
                ],
              }
            : s,
        ),
      )
    }
  }

  // Fetch workers when workspace changes, then poll every 5s
  useEffect(() => {
    if (!workspace) return
    let cancelled = false

    async function fetchWorkers(initial = false) {
      try {
        const list = await listWorkersV2(workspace)
        if (!cancelled) {
          setWorkers(list)
          if (initial) setLoading(false)
        }
      } catch {
        if (initial && !cancelled) setLoading(false)
      }
    }

    fetchWorkers(true)

    workerPollRef.current = window.setInterval(() => {
      fetchWorkers(false)
    }, 5000)

    return () => {
      cancelled = true
      if (workerPollRef.current !== null) {
        window.clearInterval(workerPollRef.current)
        workerPollRef.current = null
      }
    }
  }, [workspace])

  // Fetch auto bots when workspace changes, then poll every 15s
  useEffect(() => {
    if (!workspace) return
    let cancelled = false

    async function fetchAutoBots() {
      try {
        const list = await listAutoBots(workspace)
        if (!cancelled) setAutoBots(list)
      } catch {
        // ignore errors — sidebar just stays empty
      }
    }

    fetchAutoBots()

    autoBotPollRef.current = window.setInterval(() => {
      fetchAutoBots()
    }, 15000)

    return () => {
      cancelled = true
      if (autoBotPollRef.current !== null) {
        window.clearInterval(autoBotPollRef.current)
        autoBotPollRef.current = null
      }
    }
  }, [workspace])

  // WebSocket — update workers and auto bots on relevant events
  useEffect(() => {
    const ws = connectWebSocket((event) => {
      if (event.type === 'worker_v2_state') {
        const workerId = event.worker_id as string
        const state = event.state as WorkerV2['state']
        const label = event.label as string
        const props = (event.properties ?? {}) as Partial<WorkerV2>
        setWorkers((prev) =>
          prev.map((w) =>
            w.id === workerId
              ? { ...w, state, label, ...props }
              : w,
          ),
        )
      }

      if (event.type === 'auto_bot_run_started') {
        const autoBotId = event.auto_bot_id as string
        setAutoBots((prev) =>
          prev.map((b) =>
            b.id === autoBotId ? { ...b, status: 'running' as const } : b,
          ),
        )
      }

      if (event.type === 'auto_bot_run_finished') {
        // Refresh the full list so status reverts correctly
        if (workspaceRef.current) {
          listAutoBots(workspaceRef.current).then((list) => setAutoBots(list)).catch(() => {})
        }
      }
    })
    return () => ws.close()
  }, [])

  // Cmd+K / Ctrl+K opens command palette
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === 'k') {
        e.preventDefault()
        setPaletteOpen((open) => !open)
      }
    }
    window.addEventListener('keydown', handler)
    return () => window.removeEventListener('keydown', handler)
  }, [])

  // 'n' key opens quick dispatch (when not in an input)
  useEffect(() => {
    function handleKey(e: KeyboardEvent) {
      if (
        e.key === 'n' &&
        !e.metaKey &&
        !e.ctrlKey &&
        !(e.target instanceof HTMLInputElement) &&
        !(e.target instanceof HTMLTextAreaElement)
      ) {
        setQuickDispatchOpen(true)
      }
    }
    window.addEventListener('keydown', handleKey)
    return () => window.removeEventListener('keydown', handleKey)
  }, [])

  const sidebarWorkers = workers.map(workerToSidebarItem)
  const sidebarAutoBots = autoBots.map(autoBotToSidebarItem)

  const mainContent = !workspace ? (
    <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'center', height: '100%', color: 'var(--text-faint)', fontFamily: 'var(--font)', fontSize: '14px' }}>
      Loading workspaces...
    </div>
  ) : selected ? (
    selected.type === 'worker' ? (
      <WorkerDetailV2
        workspace={workspace}
        workerId={selected.id}
        onOpenContextBot={openContextBot}
      />
    ) : (
      <AutoBotDetail
        workspace={workspace}
        autoBotId={selected.id}
        onSelectWorker={(id) => setSelected({ type: 'worker', id })}
        onOpenContextBot={openContextBot}
      />
    )
  ) : (
    <Dashboard
      workspace={workspace}
      workers={workers}
      autoBots={autoBots}
      onSelectWorker={(id) => setSelected({ type: 'worker', id })}
      onSelectAutoBot={(id) => setSelected({ type: 'auto_bot', id })}
    />
  )

  const tabs = [
    { id: 'auto_bots', label: 'Auto Bots', icon: <Bot size={20} /> },
    { id: 'workers', label: 'Workers', icon: <Wrench size={20} /> },
  ]

  if (loading) {
    // Render shell immediately; sidebar will populate once data arrives
  }

  return (
    <>
      <Layout
        sidebar={
          <Sidebar
            selectedType={selected?.type ?? null}
            selectedId={selected?.id ?? null}
            onSelect={handleSelect}
            autoBots={sidebarAutoBots}
            workers={sidebarWorkers}
            workspaces={workspaces}
            workspace={workspace}
            onWorkspaceChange={(ws) => {
              setWorkspace(ws)
              setSelected(null)
            }}
            onQuickDispatch={() => setQuickDispatchOpen(true)}
          />
        }
        main={mainContent}
        bottomBar={
          <BottomTabBar
            tabs={tabs}
            activeTab={mobileTab}
            onTabChange={setMobileTab}
          />
        }
      />
      <ContextBotManager
        sessions={contextSessions}
        onSend={handleContextBotSend}
        onMinimize={(id) =>
          setContextSessions((prev) =>
            prev.map((s) => (s.id === id ? { ...s, minimized: !s.minimized } : s)),
          )
        }
        onClose={(id) =>
          setContextSessions((prev) => prev.filter((s) => s.id !== id))
        }
      />
      {paletteOpen && (
        <CommandPalette
          workers={workers}
          autoBots={autoBots}
          onSelectWorker={(id) => {
            setSelected({ type: 'worker', id })
            setPaletteOpen(false)
          }}
          onSelectAutoBot={(id) => {
            setSelected({ type: 'auto_bot', id })
            setPaletteOpen(false)
          }}
          onClose={() => setPaletteOpen(false)}
        />
      )}
      {quickDispatchOpen && (
        <QuickDispatch
          workspace={workspace}
          onClose={() => setQuickDispatchOpen(false)}
          onDispatched={(workerId) => {
            setQuickDispatchOpen(false)
            setSelected({ type: 'worker', id: workerId })
            listWorkersV2(workspace).then(setWorkers).catch(() => {})
          }}
        />
      )}
    </>
  )
}
