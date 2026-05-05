import { useState, useEffect } from 'react';
import { Layout } from './components/Layout/Layout';
import { Sidebar } from './components/Sidebar/Sidebar';
import { BottomTabBar } from './components/BottomTabBar/BottomTabBar';
import { EmptyState } from './components/EmptyState/EmptyState';

type EntityType = 'auto_bot' | 'worker';
type Tab = 'auto_bots' | 'workers' | 'chat' | 'new';

interface SelectedEntity {
  type: EntityType;
  id: string;
}

function PlaceholderDetail({ entity }: { entity: SelectedEntity }) {
  return (
    <div style={{
      display: 'flex',
      alignItems: 'center',
      justifyContent: 'center',
      height: '100%',
      flexDirection: 'column',
      gap: 8,
    }}>
      <div style={{ fontSize: 14, color: 'var(--text-faint)' }}>
        {entity.type === 'worker' ? 'Worker' : 'Auto Bot'}: {entity.id}
      </div>
    </div>
  );
}

export default function App() {
  const [selectedEntity, setSelectedEntity] = useState<SelectedEntity | null>(null);
  const [activeTab, setActiveTab] = useState<Tab>('auto_bots');
  const [isMobile, setIsMobile] = useState(window.innerWidth < 768);

  useEffect(() => {
    const handler = () => setIsMobile(window.innerWidth < 768);
    window.addEventListener('resize', handler);
    return () => window.removeEventListener('resize', handler);
  }, []);

  // Cmd+K palette stub, Cmd+J focus stub
  useEffect(() => {
    function handleKeyDown(e: KeyboardEvent) {
      if (e.repeat) return;
      const key = e.key.toLowerCase();
      if ((e.metaKey || e.ctrlKey) && key === 'k') {
        e.preventDefault();
        // Command palette — Phase 5
      }
      if ((e.metaKey || e.ctrlKey) && key === 'j') {
        e.preventDefault();
        // Focus input — Phase 4
      }
    }
    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, []);

  const handleSelect = (type: EntityType, id: string) => {
    setSelectedEntity({ type, id });
  };

  const mainContent = selectedEntity
    ? <PlaceholderDetail entity={selectedEntity} />
    : <EmptyState />;

  if (isMobile) {
    return (
      <div style={{ display: 'flex', flexDirection: 'column', height: '100%' }}>
        <div style={{ flex: 1, overflow: 'hidden' }}>
          {mainContent}
        </div>
        <BottomTabBar activeTab={activeTab} onTabChange={setActiveTab} />
      </div>
    );
  }

  return (
    <Layout
      sidebar={
        <Sidebar
          selectedId={selectedEntity?.id ?? null}
          onSelect={handleSelect}
        />
      }
      main={mainContent}
    />
  );
}
