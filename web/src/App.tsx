import { useState } from 'react'
import Layout from './components/Layout/Layout'
import Sidebar from './components/Sidebar/Sidebar'
import BottomTabBar from './components/BottomTabBar/BottomTabBar'
import EmptyState from './components/EmptyState/EmptyState'
import { Bot, Wrench } from 'lucide-react'
import './theme.css'

type EntityType = 'auto_bot' | 'worker'

interface SelectedEntity {
  type: EntityType
  id: string
}

export default function App() {
  const [selected, setSelected] = useState<SelectedEntity | null>(null)
  const [mobileTab, setMobileTab] = useState('workers')

  const handleSelect = (type: EntityType, id: string) => {
    setSelected({ type, id })
  }

  // Placeholder detail view — will be replaced in Phase 2
  const mainContent = selected ? (
    <PlaceholderDetail entity={selected} />
  ) : (
    <EmptyState />
  )

  const tabs = [
    { id: 'auto_bots', label: 'Auto Bots', icon: <Bot size={20} /> },
    { id: 'workers', label: 'Workers', icon: <Wrench size={20} /> },
  ]

  return (
    <Layout
      sidebar={
        <Sidebar
          selectedType={selected?.type ?? null}
          selectedId={selected?.id ?? null}
          onSelect={handleSelect}
          autoBots={STUB_AUTO_BOTS}
          workers={STUB_WORKERS}
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
      {entity.type === 'worker' ? 'Worker' : 'Auto Bot'}: {entity.id}
    </div>
  )
}

// Stub data — remove in Phase 2
const STUB_AUTO_BOTS = [
  { id: 'triage', name: 'Triage', status: 'idle', meta: 'signal' },
  { id: 'standup', name: 'Standup', status: 'running', meta: 'cron' },
]

const STUB_WORKERS = [
  { id: 'apiari-1', name: 'fix-auth-rate-limit', status: 'running', meta: 'swarm/fix-auth' },
  { id: 'apiari-2', name: 'update-deps', status: 'waiting', meta: 'swarm/deps' },
  { id: 'apiari-3', name: 'add-tests', status: 'failed', meta: 'swarm/tests' },
]
