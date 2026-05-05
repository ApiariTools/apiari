import styles from './Layout.module.css';

interface LayoutProps {
  sidebar: React.ReactNode;
  main: React.ReactNode;
}

export function Layout({ sidebar, main }: LayoutProps) {
  return (
    <div className={styles.root}>
      <aside className={styles.sidebar}>{sidebar}</aside>
      <main className={styles.main}>{main}</main>
    </div>
  );
}
