import styles from './HowItWorks.module.css';

const diagram = `                ┌─────────────────────────────────────┐
                │          beachcomber daemon           │
                │                                       │
  filesystem ──►│  FSEvents/inotify                     │
  changes       │       │                               │
                │       ▼                               │
                │  Scheduler ──► Providers ──► Cache   │
                │                  git          157ns   │
                │                  battery      reads   │
                │                  network              │
                │                  hostname             │
                │                  ...                  │
                │                  scripts              │
                │                  (your own)           │
                │                                       │
                │  Unix Socket Server                   │
                └──────────────┬────────────────────────┘
                               │
                ┌──────────────┼────────────────────┐
                │              │                     │
           zsh prompt     tmux status           neovim
           bash prompt    polybar/waybar         lualine
           fish prompt    sketchybar             scripts
           starship       oh-my-posh             CI/automation`;

export default function HowItWorks(): JSX.Element {
  return (
    <section className={styles.section}>
      <div className={styles.container}>
        <h2 className={styles.heading}>How it works</h2>
        <p className={styles.description}>
          beachcomber is a single async daemon. It watches directories using native OS APIs, runs
          providers when files change or timers fire, and caches results in a shared in-memory map.
          Every consumer reads from the same cache via a Unix socket. One watcher. One computation.
          Infinite readers.
        </p>
        <div className={styles.diagram}>
          <pre className={styles.diagramPre}>{diagram}</pre>
        </div>
      </div>
    </section>
  );
}
