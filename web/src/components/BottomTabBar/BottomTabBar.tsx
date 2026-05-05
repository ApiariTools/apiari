import styles from './BottomTabBar.module.css'

export interface Tab {
  id: string
  label: string
  icon: React.ReactNode
}

export interface BottomTabBarProps {
  tabs: Tab[]
  activeTab: string
  onTabChange: (id: string) => void
}

export default function BottomTabBar({ tabs, activeTab, onTabChange }: BottomTabBarProps) {
  return (
    <nav className={styles.bar} aria-label="Mobile navigation">
      {tabs.map((tab) => (
        <button
          key={tab.id}
          className={`${styles.tab} ${activeTab === tab.id ? styles.tabActive : ''}`}
          onClick={() => onTabChange(tab.id)}
          type="button"
          aria-current={activeTab === tab.id ? 'page' : undefined}
        >
          {tab.icon}
          <span className={styles.label}>{tab.label}</span>
        </button>
      ))}
    </nav>
  )
}
