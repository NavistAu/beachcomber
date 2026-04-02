import {useIntersectionObserver} from '../hooks/useIntersectionObserver';
import styles from './SDKs.module.css';

export default function SDKs(): JSX.Element {
  const [ref, isVisible] = useIntersectionObserver(0.1);

  return (
    <section className={styles.section}>
      <div
        className={`${styles.container} ${isVisible ? styles.visible : ''}`}
        ref={ref as React.RefObject<HTMLDivElement>}
      >
        <div className={styles.label}>Get started</div>
        <h2 className={styles.heading}>Get started</h2>
        <p className={styles.subheading}>
          The daemon starts automatically on first query. Integrate with your prompt, status bar, or
          editor to start seeing the benefits.
        </p>

        <div className={styles.terminalWrapper}>
          <div className={styles.terminalHeader}>
            <span className={`${styles.dot} ${styles.dotRed}`} />
            <span className={`${styles.dot} ${styles.dotYellow}`} />
            <span className={`${styles.dot} ${styles.dotGreen}`} />
          </div>
          <div className={styles.terminalBody}>
            <span className={styles.prompt}>$</span>
            <span className={styles.command}> brew install beachcomber</span>
          </div>
        </div>

        <div className={styles.links}>
          <a className={styles.link} href="/docs/quick-start">
            Quick Start
          </a>
          <a className={styles.link} href="/docs">
            Docs
          </a>
          <a
            className={styles.link}
            href="https://github.com/NavistAu/beachcomber"
            target="_blank"
            rel="noopener noreferrer"
          >
            GitHub
          </a>
          <a
            className={styles.link}
            href="https://crates.io/crates/beachcomber"
            target="_blank"
            rel="noopener noreferrer"
          >
            cargo install
          </a>
        </div>
      </div>
    </section>
  );
}
