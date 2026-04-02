import {useIntersectionObserver} from '../hooks/useIntersectionObserver';
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
  const [ref, isVisible] = useIntersectionObserver(0.1);

  return (
    <section className={styles.section}>
      <div
        className={`${styles.container} ${isVisible ? styles.visible : ''}`}
        ref={ref as React.RefObject<HTMLDivElement>}
      >
        <h2 className={styles.heading}>How it works</h2>
        <p className={styles.description}>
          beachcomber is a single async daemon. It watches directories using native OS APIs, runs
          providers when files change or timers fire, and caches results in a shared in-memory map.
          Every consumer reads from the same cache via a Unix socket. One watcher. One computation.
          Infinite readers.
        </p>

        <div className={styles.terminalWrapper}>
          <div className={styles.terminalHeader}>
            <span className={`${styles.dot} ${styles.dotRed}`} />
            <span className={`${styles.dot} ${styles.dotYellow}`} />
            <span className={`${styles.dot} ${styles.dotGreen}`} />
            <span className={styles.terminalTitle}>Terminal</span>
          </div>
          <div className={styles.terminalBody}>
            <div className={styles.terminalLine}>
              <span className={styles.prompt}>$</span>
              <span className={styles.command}> comb get git.branch . -f text</span>
            </div>
            <div className={styles.terminalLine}>
              <span className={styles.output}>main</span>
              <span className={styles.annotation}> ← 15µs</span>
            </div>
            <div className={styles.terminalSpacer} />
            <div className={styles.terminalLine}>
              <span className={styles.prompt}>$</span>
              <span className={styles.command}> comb get battery.percent -f text</span>
            </div>
            <div className={styles.terminalLine}>
              <span className={styles.output}>85</span>
            </div>
            <div className={styles.terminalSpacer} />
            <div className={styles.terminalLine}>
              <span className={styles.prompt}>$</span>
              <span className={styles.command}> comb status</span>
            </div>
            <div className={styles.terminalLine}>
              <span className={styles.jsonBrace}>{'{'}</span>
              <span className={styles.jsonKey}>&quot;uptime_secs&quot;</span>
              <span className={styles.jsonColon}>: </span>
              <span className={styles.jsonNumber}>3642</span>
              <span className={styles.jsonComma}>, </span>
              <span className={styles.jsonKey}>&quot;cache_entries&quot;</span>
              <span className={styles.jsonColon}>: </span>
              <span className={styles.jsonNumber}>12</span>
              <span className={styles.jsonComma}>, </span>
              <span className={styles.jsonKey}>&quot;active_watchers&quot;</span>
              <span className={styles.jsonColon}>: </span>
              <span className={styles.jsonNumber}>3</span>
              <span className={styles.jsonComma}>, </span>
              <span className={styles.jsonKey}>&quot;demand&quot;</span>
              <span className={styles.jsonColon}>: </span>
              <span className={styles.jsonNumber}>8</span>
              <span className={styles.jsonBrace}>{'}'}</span>
            </div>
          </div>
        </div>

        <div className={styles.diagram}>
          <pre className={styles.diagramPre}>{diagram}</pre>
        </div>
      </div>
    </section>
  );
}
