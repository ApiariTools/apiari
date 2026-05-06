import { useState } from 'react'
import { Menu } from 'lucide-react'
import styles from './Layout.module.css'

interface LayoutProps {
  sidebar: React.ReactNode
  main: React.ReactNode
  bottomBar?: React.ReactNode
}

export default function Layout({ sidebar, main, bottomBar }: LayoutProps) {
  const [sidebarOpen, setSidebarOpen] = useState(false)

  return (
    <div className={styles.layout}>
      {/* Desktop sidebar */}
      <div className={styles.sidebar}>
        {sidebar}
      </div>

      {/* iPad overlay sidebar */}
      {sidebarOpen && (
        <div className={styles.overlay}>
          <div className={styles.overlaySidebar}>
            {sidebar}
          </div>
          <div
            className={styles.overlayBackdrop}
            onClick={() => setSidebarOpen(false)}
            role="presentation"
          />
        </div>
      )}

      {/* iPad hamburger */}
      <button
        className={styles.hamburger}
        onClick={() => setSidebarOpen(true)}
        aria-label="Open sidebar"
        type="button"
      >
        <Menu size={18} />
      </button>

      {/* Main content */}
      <div className={styles.main}>
        {main}
      </div>

      {/* Mobile bottom bar */}
      {bottomBar && (
        <div className={styles.bottomBar}>
          {bottomBar}
        </div>
      )}
    </div>
  )
}
