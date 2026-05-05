import { Bot, Cpu, MessageSquare, Plus } from 'lucide-react';
import styles from './BottomTabBar.module.css';

type Tab = 'auto_bots' | 'workers' | 'chat' | 'new';

interface BottomTabBarProps {
  activeTab: Tab;
  onTabChange: (tab: Tab) => void;
}

const TABS: { id: Tab; icon: React.ElementType; label: string }[] = [
  { id: 'auto_bots', icon: Bot, label: 'Auto Bots' },
  { id: 'workers', icon: Cpu, label: 'Workers' },
  { id: 'chat', icon: MessageSquare, label: 'Chat' },
  { id: 'new', icon: Plus, label: 'New' },
];

export function BottomTabBar({ activeTab, onTabChange }: BottomTabBarProps) {
  return (
    <nav className={styles.root}>
      {TABS.map(({ id, icon: Icon, label }) => (
        <button
          key={id}
          className={`${styles.tab} ${activeTab === id ? styles.active : ''}`}
          onClick={() => onTabChange(id)}
        >
          <Icon size={20} />
          <span className={styles.label}>{label}</span>
        </button>
      ))}
    </nav>
  );
}
