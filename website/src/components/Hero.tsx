import {useIntersectionObserver} from '../hooks/useIntersectionObserver';
import styles from './Hero.module.css';

export default function Hero(): JSX.Element {
  const [ref, isVisible] = useIntersectionObserver(0.1);

  return (
    <section className={styles.hero}>
      <div className={styles.glow} aria-hidden="true" />
      <div
        className={`${styles.content} ${isVisible ? styles.visible : ''}`}
        ref={ref as React.RefObject<HTMLDivElement>}
      >
        <h1 className={styles.title}>beachcomber</h1>
        <p className={styles.tagline}>
          A single daemon that caches your shell environment. Every prompt, status bar, and editor
          reads from one shared cache instead of recomputing everything independently.
        </p>

        <div className={styles.badges}>
          <a
            className={styles.badge}
            href="https://github.com/NavistAu/beachcomber"
            target="_blank"
            rel="noopener noreferrer"
          >
            GitHub
          </a>
          <a
            className={styles.badge}
            href="https://github.com/NavistAu/beachcomber/stargazers"
            target="_blank"
            rel="noopener noreferrer"
          >
            ★ Stars
          </a>
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
