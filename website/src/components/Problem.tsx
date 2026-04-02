import {useIntersectionObserver} from '../hooks/useIntersectionObserver';
import styles from './Problem.module.css';

export default function Problem(): JSX.Element {
  const [ref, isVisible] = useIntersectionObserver(0.15);

  return (
    <section className={styles.section}>
      <div
        className={`${styles.container} ${isVisible ? styles.visible : ''}`}
        ref={ref as React.RefObject<HTMLDivElement>}
      >
        <h2 className={styles.heading}>
          Your terminal is doing{' '}
          <span className={styles.accent}>insane</span>{' '}
          amounts of duplicate work
        </h2>

        <div className={styles.prose}>
          <p>
            You have 10 tmux windows open, each with 3 panes. Your fancy prompt tells you the git
            branch — but every single pane is forking its own process to compute this. That&apos;s 30
            processes, each spawning their own git status daemon with a pool of threads. Your tmux
            status line is doing the same. Your neovim statusline too. Your Claude Code status line
            too.
          </p>
          <p>
            Meanwhile, fseventsd is pegging a CPU core dispatching the same filesystem change event
            to 30 independent watchers — all monitoring the same <code>.git</code> directory. You
            can see it in Activity Monitor: hundreds of open file handles, CPU burn on a process
            that should be idle.
          </p>
          <p>
            Every consumer is independently asking the same questions about the same files with zero
            coordination.
          </p>
        </div>

        <div className={styles.callout}>
          <div className={styles.calloutBar} aria-hidden="true" />
          <div className={styles.calloutBody}>
            <p>
              <strong>beachcomber</strong> is a local memoization cache for shell environment state.
              One daemon watches your filesystem, computes state once, and serves it from memory.
              Providers automatically back off when not queried, and warm up again on demand. One
              watcher. One computation. Infinite readers.
            </p>
          </div>
        </div>
      </div>
    </section>
  );
}
