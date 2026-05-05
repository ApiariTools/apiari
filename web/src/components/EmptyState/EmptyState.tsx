import { LayoutGrid } from 'lucide-react'
import styles from './EmptyState.module.css'

export default function EmptyState() {
  return (
    <div className={styles.container}>
      <LayoutGrid size={32} className={styles.icon} aria-hidden="true" />
      <p className={styles.heading}>Select something</p>
      <p className={styles.subtext}>Choose a worker or auto bot from the sidebar</p>
    </div>
  )
}
