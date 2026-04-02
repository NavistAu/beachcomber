import styles from './Hero.module.css';

export default function Hero(): JSX.Element {
  return (
    <section className={styles.hero}>
      <h1 className={styles.title}>beachcomber</h1>
      <p className={styles.tagline}>
        One daemon. One cache. Every consumer reads from it.
      </p>

      <div className={styles.installBlock}>
        brew install beachcomber
      </div>

      <div className={styles.badges}>
        <a className={styles.badge} href="https://github.com/NavistAu/beachcomber">
          GitHub ★
        </a>
        <a className={styles.badge} href="https://github.com/NavistAu/beachcomber/stargazers">
          Stars
        </a>
      </div>

      <div className={styles.registryLinks}>
        <a className={styles.registryLink} href="https://crates.io/crates/beachcomber">crates.io</a>
        <a className={styles.registryLink} href="https://pypi.org/project/libbeachcomber/">PyPI</a>
        <a className={styles.registryLink} href="https://www.npmjs.com/package/libbeachcomber">npm</a>
        <a className={styles.registryLink} href="https://rubygems.org/gems/libbeachcomber">RubyGems</a>
        <a className={styles.registryLink} href="https://luarocks.org/modules/navist/libbeachcomber">LuaRocks</a>
      </div>

      <div className={styles.terminalDemo}>
        <div><span className={styles.prompt}>$</span> comb get git.branch . -f text</div>
        <div className={styles.output}>main</div>
        <br />
        <div><span className={styles.prompt}>$</span> comb get battery.percent -f text</div>
        <div className={styles.output}>85</div>
        <br />
        <div><span className={styles.prompt}>$</span> comb status</div>
        <div className={styles.output}>{'{"uptime_secs": 3642, "cache_entries": 12, "active_watchers": 3, "demand": 8}'}</div>
      </div>
    </section>
  );
}
