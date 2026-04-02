import {useIntersectionObserver} from '../hooks/useIntersectionObserver';
import styles from './Problem.module.css';

export default function Problem(): JSX.Element {
  const [ref, isVisible] = useIntersectionObserver(0.15);

  return (
    <section className={styles.section} id="problem">
      <div
        className={`${styles.container} ${isVisible ? styles.visible : ''}`}
        ref={ref as React.RefObject<HTMLDivElement>}
      >
        <div className={styles.label}>The problem</div>
        <h2 className={styles.heading}>
          Your terminal is doing{' '}
          <span className={styles.accent}>insane</span>{' '}
          amounts of duplicate work
        </h2>

        <div className={styles.prose}>
          <p>
            Open Activity Monitor on a typical dev machine and look at what&apos;s running. Every
            shell prompt has its own git status daemon. Your tmux status bar forks a shell process
            for every pane on a timer. Your editor has its own file watchers. Your prompt framework
            has another set. Each one independently computing the same answer to the same question:
            &quot;what branch am I on?&quot;
          </p>
          <p>
            Scale it up: 30 terminal panes means 30 git daemons, each with a thread pool — that&apos;s
            hundreds of threads. <code>fseventsd</code> is dispatching every filesystem change to 30
            separate watchers all monitoring the same <code>.git</code> directory. Hundreds of open
            file handles, constant CPU churn, and every single one of them returns the same answer.
          </p>
          <p>
            Nothing talks to anything else. Every consumer — prompts, status bars, editors,
            scripts — operates in total isolation, duplicating work that only needs to happen once.
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
