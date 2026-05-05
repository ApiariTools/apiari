import { LayoutGrid } from 'lucide-react';
import styles from './EmptyState.module.css';

export function EmptyState() {
  return (
    <div className={styles.root}>
      <LayoutGrid size={40} className={styles.icon} />
      <h2 className={styles.heading}>Select something to get started</h2>
      <p className={styles.subtext}>Choose a worker or auto bot from the sidebar</p>
    </div>
  );
}
