import {useIntersectionObserver} from '../hooks/useIntersectionObserver';
import styles from './Hero.module.css';

export default function Hero(): JSX.Element {
  const [ref, isVisible] = useIntersectionObserver(0.1);

  return (
    <section className={styles.hero}>
      <div className={styles.glow} aria-hidden="true" />
      <div className={styles.glowWarm} aria-hidden="true" />
      <div
        className={`${styles.content} ${isVisible ? styles.visible : ''}`}
        ref={ref as React.RefObject<HTMLDivElement>}
      >
        <h1 className={styles.title}>beachcomber</h1>
        <p className={styles.tagline}>
          A single daemon that caches your shell environment. Every prompt, status bar, and editor
          reads from one shared cache instead of recomputing everything independently.
        </p>

        <div className={styles.metrics}>
          <a
            className={styles.metric}
            href="https://github.com/NavistAu/beachcomber"
            target="_blank"
            rel="noopener noreferrer"
          >
            <img
              src="https://img.shields.io/github/stars/NavistAu/beachcomber?style=flat&label=GitHub%20Stars&color=0891b2"
              alt="GitHub Stars"
              className={styles.shieldBadge}
            />
          </a>
          <a
            className={styles.metric}
            href="https://crates.io/crates/beachcomber"
            target="_blank"
            rel="noopener noreferrer"
          >
            <img
              src="https://img.shields.io/crates/d/beachcomber?style=flat&label=crates.io&color=0891b2"
              alt="crates.io downloads"
              className={styles.shieldBadge}
            />
          </a>
          <a
            className={styles.metric}
            href="https://www.npmjs.com/package/libbeachcomber"
            target="_blank"
            rel="noopener noreferrer"
          >
            <img
              src="https://img.shields.io/npm/dt/libbeachcomber?style=flat&label=npm&color=0891b2"
              alt="npm downloads"
              className={styles.shieldBadge}
            />
          </a>
          <a
            className={styles.metric}
            href="https://pypi.org/project/libbeachcomber/"
            target="_blank"
            rel="noopener noreferrer"
          >
            <img
              src="https://img.shields.io/pypi/dm/libbeachcomber?style=flat&label=PyPI&color=0891b2"
              alt="PyPI downloads"
              className={styles.shieldBadge}
            />
          </a>
        </div>

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
              <span className={styles.command}> comb status</span>
            </div>
            <div className={styles.terminalLine}>
              <span className={styles.jsonBrace}>{'{'}</span>
              <span className={styles.jsonKey}>&quot;cache_entries&quot;</span>
              <span className={styles.jsonColon}>: </span>
              <span className={styles.jsonNumber}>12</span>
              <span className={styles.jsonComma}>, </span>
              <span className={styles.jsonKey}>&quot;active_watchers&quot;</span>
              <span className={styles.jsonColon}>: </span>
              <span className={styles.jsonNumber}>3</span>
              <span className={styles.jsonBrace}>{'}'}</span>
            </div>
          </div>
        </div>

        <div className={styles.registryLinks}>
          <a className={styles.registryLink} href="https://crates.io/crates/beachcomber" target="_blank" rel="noopener noreferrer">crates.io</a>
          <span className={styles.registrySep} aria-hidden="true">·</span>
          <a className={styles.registryLink} href="https://pypi.org/project/libbeachcomber/" target="_blank" rel="noopener noreferrer">PyPI</a>
          <span className={styles.registrySep} aria-hidden="true">·</span>
          <a className={styles.registryLink} href="https://www.npmjs.com/package/libbeachcomber" target="_blank" rel="noopener noreferrer">npm</a>
          <span className={styles.registrySep} aria-hidden="true">·</span>
          <a className={styles.registryLink} href="https://rubygems.org/gems/libbeachcomber" target="_blank" rel="noopener noreferrer">RubyGems</a>
          <span className={styles.registrySep} aria-hidden="true">·</span>
          <a className={styles.registryLink} href="https://luarocks.org/modules/navist/libbeachcomber" target="_blank" rel="noopener noreferrer">LuaRocks</a>
        </div>
      </div>
    </section>
  );
}
