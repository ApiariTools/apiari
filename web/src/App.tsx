import { useEffect, useRef, useState } from 'react'
import Layout from './components/Layout/Layout'
import Sidebar from './components/Sidebar/Sidebar'
import BottomTabBar from './components/BottomTabBar/BottomTabBar'
import EmptyState from './components/EmptyState/EmptyState'
import WorkerDetailV2 from './components/WorkerDetailV2/WorkerDetailV2'
import { Bot, Wrench } from 'lucide-react'
import { listWorkersV2, connectWebSocket } from './api'
import type { WorkerV2 } from './types'
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

// Stub auto bots — Phase 3 will replace with real data
const STUB_AUTO_BOTS: SidebarItem[] = [
  { id: 'triage', name: 'Triage', status: 'idle', meta: 'signal' },
  { id: 'standup', name: 'Standup', status: 'running', meta: 'cron' },
]

export default function App() {
  const [selected, setSelected] = useState<SelectedEntity | null>(null)
  const [mobileTab, setMobileTab] = useState('workers')
  const [workers, setWorkers] = useState<WorkerV2[]>([])
  const [loading, setLoading] = useState(true)
  const pollRef = useRef<number | null>(null)

  const handleSelect = (type: EntityType, id: string) => {
    setSelected({ type, id })
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

    pollRef.current = window.setInterval(() => {
      fetchWorkers(false)
    }, 5000)

    return () => {
      cancelled = true
      if (pollRef.current !== null) {
        window.clearInterval(pollRef.current)
        pollRef.current = null
      }
    }
  }, [])

  // WebSocket — update workers on worker_v2_state events
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
    })
    return () => ws.close()
  }, [])

  const sidebarWorkers = workers.map(workerToSidebarItem)

  const mainContent = selected ? (
    selected.type === 'worker' ? (
      <WorkerDetailV2
        workspace={WORKSPACE}
        workerId={selected.id}
      />
    ) : (
      <PlaceholderDetail entity={selected} />
    )
  ) : (
    <EmptyState />
  )

  const tabs = [
    { id: 'auto_bots', label: 'Auto Bots', icon: <Bot size={20} /> },
    { id: 'workers', label: 'Workers', icon: <Wrench size={20} /> },
  ]

  if (loading) {
    // Render shell immediately; sidebar will populate once workers arrive
  }

  return (
    <Layout
      sidebar={
        <Sidebar
          selectedType={selected?.type ?? null}
          selectedId={selected?.id ?? null}
          onSelect={handleSelect}
          autoBots={STUB_AUTO_BOTS}
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
  )
}

function PlaceholderDetail({ entity }: { entity: SelectedEntity }) {
  return (
    <div style={{
      display: 'flex',
      alignItems: 'center',
      justifyContent: 'center',
      height: '100%',
      color: 'var(--text-faint)',
      fontSize: 'var(--font-size-small)',
      fontFamily: 'var(--font)',
    }}>
      Auto Bot: {entity.id}
    </div>
  )
}
