import {useState} from 'react';
import IconCopy from '@theme/Icon/Copy';
import IconSuccess from '@theme/Icon/Success';
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

// Copies the install command, and shows a tick for a moment once it has.
function CopyButton({text}: {text: string}): JSX.Element {
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      className={styles.copy}
      aria-label={copied ? 'Copied' : 'Copy command'}
      title="Copy"
      onClick={() => navigator.clipboard.writeText(text).then(() => {
        setCopied(true);
        window.setTimeout(() => setCopied(false), 1500);
      })}
    >
      {copied ? <IconSuccess /> : <IconCopy />}
    </button>
  );
}

export default function SDKs(): JSX.Element {
  const [ref, isVisible] = useIntersectionObserver(0.1);
  const [activeTab, setActiveTab] = useState('brew');

  const activeInstall = installTabs.find(t => t.id === activeTab);

  return (
    <>
    <section className={`${styles.section} page`}>
      <div
        className={`${styles.container} ${isVisible ? styles.visible : ''}`}
        ref={ref as React.RefObject<HTMLDivElement>}
      >
        <h2 className={styles.heading}>Get started</h2>

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
            <div>
              <span className={styles.prompt}>$</span>
              <span className={styles.command}> {activeInstall?.command}</span>
            </div>
            <CopyButton text={activeInstall?.command ?? ''} />
          </div>
        </div>

        <div className={styles.links}>
          <a className={styles.quickStart} href="/docs/getting-started/set-up">
            Quick start &rarr;
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
          <a className={styles.footerColLink} href="/docs/providers/built-in-providers">Providers</a>
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
