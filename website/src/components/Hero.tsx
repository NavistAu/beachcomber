import {useIntersectionObserver} from '../hooks/useIntersectionObserver';
import HeroAnimation from './HeroAnimation';
import styles from './Hero.module.css';

const COPY = (
  <>
    Obsessively fast<sup>µs</sup> cache engine.<br />
    Lightspeed shells, reduced load.<br />
    Store anything, automate everything.
  </>
);

function toProblem(e: React.MouseEvent) {
  e.preventDefault();
  document.getElementById('problem')?.scrollIntoView({behavior: 'smooth'});
}

export default function Hero(): JSX.Element {
  const [ref, isVisible] = useIntersectionObserver(0.1);

  return (
    <section className={styles.hero} id="hero">
      <div className={styles.titleSpace}>
      <div
        className={`${styles.header} ${isVisible ? styles.visible : ''}`}
        ref={ref as React.RefObject<HTMLDivElement>}
      >
        <h1 className={styles.title}>beach<span className={styles.bin}>comb</span>er</h1>
        <p className={styles.tagline}>
          {COPY}
        </p>

        <div className={styles.metrics}>
          <a className={styles.metric} href="https://github.com/NavistAu/beachcomber" target="_blank" rel="noopener noreferrer">
            <img src="https://img.shields.io/github/stars/NavistAu/beachcomber?style=flat&label=GitHub%20Stars&color=0891b2" alt="GitHub Stars" className={styles.shieldBadge} />
          </a>
          <a className={styles.metric} href="https://crates.io/crates/beachcomber" target="_blank" rel="noopener noreferrer">
            <img src="https://img.shields.io/crates/d/beachcomber?style=flat&label=crates.io&color=0891b2" alt="crates.io downloads" className={styles.shieldBadge} />
          </a>
          <a className={styles.metric} href="https://www.npmjs.com/package/libbeachcomber" target="_blank" rel="noopener noreferrer">
            <img src="https://img.shields.io/npm/dt/libbeachcomber?style=flat&label=npm&color=0891b2" alt="npm downloads" className={styles.shieldBadge} />
          </a>
          <a className={styles.metric} href="https://pypi.org/project/libbeachcomber/" target="_blank" rel="noopener noreferrer">
            <img src="https://img.shields.io/pypi/dm/libbeachcomber?style=flat&label=PyPI&color=0891b2" alt="PyPI downloads" className={styles.shieldBadge} />
          </a>
        </div>

        <div className={styles.ctas}>
          <a className={styles.ctaPrimary} href="/docs/getting-started/installation">
            Read the Docs
          </a>
          <a
            className={styles.ctaSecondary}
            href="#problem"
            onClick={toProblem}
          >
            Learn More &darr;
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
          <a className={styles.registryLink} href="https://luarocks.org/modules/jhogendorn/libbeachcomber" target="_blank" rel="noopener noreferrer">LuaRocks</a>
        </div>
      </div>

      </div>

      {/* Gradient fade from header into animation */}
      <div className={styles.headerFade} aria-hidden="true" />

      <HeroAnimation />

      {/* a phone's hero ends with the copy and a way on, in place of the tagline and buttons */}
      <div className={styles.foot}>
        <p className={styles.lead}>{COPY}</p>
        <a className={styles.next} href="#problem" aria-label="Learn more" onClick={toProblem}>
          <svg viewBox="0 0 24 24" width="28" height="28" aria-hidden="true">
            <path d="M5 9l7 7 7-7" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round" />
          </svg>
        </a>
      </div>
    </section>
  );
}
