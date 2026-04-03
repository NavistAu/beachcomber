import {useState} from 'react';
import {useIntersectionObserver} from '../hooks/useIntersectionObserver';
import styles from './SDKs.module.css';

interface InstallTab {
  id: string;
  label: string;
  command: string;
}

const installTabs: InstallTab[] = [
  {id: 'brew', label: 'Homebrew', command: 'brew install navistau/tap/beachcomber'},
  {id: 'npm', label: 'npm', command: 'npm install -g beachcomber'},
  {id: 'pip', label: 'pip', command: 'pip install beachcomber'},
  {id: 'cargo', label: 'Cargo', command: 'cargo install beachcomber'},
];

export default function SDKs(): JSX.Element {
  const [ref, isVisible] = useIntersectionObserver(0.1);
  const [activeTab, setActiveTab] = useState('brew');

  const activeInstall = installTabs.find(t => t.id === activeTab);

  return (
    <>
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
            <div className={styles.dots}>
              <span className={`${styles.dot} ${styles.dotRed}`} />
              <span className={`${styles.dot} ${styles.dotYellow}`} />
              <span className={`${styles.dot} ${styles.dotGreen}`} />
            </div>
            <div className={styles.tabs}>
              {installTabs.map(tab => (
                <button
                  key={tab.id}
                  className={`${styles.tab} ${activeTab === tab.id ? styles.tabActive : ''}`}
                  onClick={() => setActiveTab(tab.id)}
                >
                  {tab.label}
                </button>
              ))}
            </div>
          </div>
          <div className={styles.terminalBody}>
            <span className={styles.prompt}>$</span>
            <span className={styles.command}> {activeInstall?.command}</span>
          </div>
        </div>

        <div className={styles.links}>
          <a className={styles.link} href="/docs/getting-started/quick-start">
            Quick Start
          </a>
          <a className={styles.link} href="/docs/getting-started/installation">
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

    <section className={styles.pageFooter}>
      <div className={styles.pageFooterInner}>
        <div className={styles.footerCol}>
          <div className={styles.footerColTitle}>Docs</div>
          <a className={styles.footerColLink} href="/docs/getting-started/installation">Getting Started</a>
          <a className={styles.footerColLink} href="/docs/reference/cli-commands">CLI Reference</a>
          <a className={styles.footerColLink} href="/docs/reference/built-in-providers">Providers</a>
        </div>
        <div className={styles.footerCol}>
          <div className={styles.footerColTitle}>Ecosystem</div>
          <a className={styles.footerColLink} href="/docs/ecosystem/overview">SDKs</a>
          <a className={styles.footerColLink} href="https://github.com/NavistAu/beachcomber" target="_blank" rel="noopener noreferrer">GitHub</a>
          <a className={styles.footerColLink} href="https://crates.io/crates/beachcomber" target="_blank" rel="noopener noreferrer">crates.io</a>
        </div>
      </div>
    </section>
    </>
  );
}
