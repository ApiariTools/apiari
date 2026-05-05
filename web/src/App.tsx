import { useEffect, useRef, useState } from 'react'
import Layout from './components/Layout/Layout'
import Sidebar from './components/Sidebar/Sidebar'
import BottomTabBar from './components/BottomTabBar/BottomTabBar'
import EmptyState from './components/EmptyState/EmptyState'
import WorkerDetailV2 from './components/WorkerDetailV2/WorkerDetailV2'
import AutoBotDetail from './components/AutoBotDetail/AutoBotDetail'
import ContextBotManager from './components/ContextBot/ContextBotManager'
import { Bot, Wrench } from 'lucide-react'
import { listWorkersV2, listAutoBots, connectWebSocket, chatWithContextBot } from './api'
import type { WorkerV2, AutoBot, ContextBotContext, ContextBotSession } from './types'
import type { SidebarItem } from './components/Sidebar/Sidebar'
import './theme.css'

type EntityType = 'auto_bot' | 'worker'

interface SelectedEntity {
  type: EntityType
  id: string
}

const WORKSPACE = 'default'

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
  const [selected, setSelected] = useState<SelectedEntity | null>(null)
  const [mobileTab, setMobileTab] = useState('workers')
  const [workers, setWorkers] = useState<WorkerV2[]>([])
  const [autoBots, setAutoBots] = useState<AutoBot[]>([])
  const [loading, setLoading] = useState(true)
  const [contextSessions, setContextSessions] = useState<ContextBotSession[]>([])
  const workerPollRef = useRef<number | null>(null)
  const autoBotPollRef = useRef<number | null>(null)

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
      const res = await chatWithContextBot(WORKSPACE, message, session.context, sessionId)

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
          const list = await listWorkersV2(WORKSPACE)
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

  // Fetch workers on mount and poll every 5s
  useEffect(() => {
    let cancelled = false

    async function fetchWorkers(initial = false) {
      try {
        const list = await listWorkersV2(WORKSPACE)
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
  }, [])

  // Fetch auto bots on mount and poll every 15s
  useEffect(() => {
    let cancelled = false

    async function fetchAutoBots() {
      try {
        const list = await listAutoBots(WORKSPACE)
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
  }, [])

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
        listAutoBots(WORKSPACE).then((list) => setAutoBots(list)).catch(() => {})
      }
    })
    return () => ws.close()
  }, [])

  const sidebarWorkers = workers.map(workerToSidebarItem)
  const sidebarAutoBots = autoBots.map(autoBotToSidebarItem)

  const mainContent = selected ? (
    selected.type === 'worker' ? (
      <WorkerDetailV2
        workspace={WORKSPACE}
        workerId={selected.id}
        onOpenContextBot={openContextBot}
      />
    ) : (
      <AutoBotDetail
        workspace={WORKSPACE}
        autoBotId={selected.id}
        onSelectWorker={(id) => setSelected({ type: 'worker', id })}
        onOpenContextBot={openContextBot}
      />
    )
  ) : (
    <EmptyState />
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
    </>
  )
}
