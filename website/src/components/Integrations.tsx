import {useIntersectionObserver} from '../hooks/useIntersectionObserver';
import styles from './Integrations.module.css';

const integrations = [
  'zsh',
  'bash',
  'fish',
  'tmux',
  'neovim',
  'starship',
  'polybar',
  'waybar',
  'sketchybar',
  'oh-my-posh',
];

const sdks = [
  {name: 'Rust', href: '/docs/ecosystem/rust-sdk'},
  {name: 'Python', href: '/docs/ecosystem/python-sdk'},
  {name: 'Node.js', href: '/docs/ecosystem/nodejs-sdk'},
  {name: 'Go', href: '/docs/ecosystem/go-sdk'},
  {name: 'Lua', href: '/docs/ecosystem/lua-sdk'},
  {name: 'Ruby', href: '/docs/ecosystem/ruby-sdk'},
  {name: 'C', href: '/docs/ecosystem/c-sdk'},
  {name: 'Shell', href: '/docs/ecosystem/shell'},
];

export default function Integrations(): JSX.Element {
  const [ref, isVisible] = useIntersectionObserver(0.1);

  return (
    <section className={styles.section}>
      <div
        className={`${styles.container} ${isVisible ? styles.visible : ''}`}
        ref={ref as React.RefObject<HTMLDivElement>}
      >
        <h2 className={styles.heading}>Ecosystem</h2>
        <p className={styles.subheading}>beachcomber is infrastructure, not a prompt theme</p>

        <div className={styles.columns}>
          <div className={styles.column}>
            <div className={styles.columnHeading}>Integrations</div>
            <div className={styles.pills}>
              {integrations.map((name) => (
                <span key={name} className={styles.pill}>
                  {name}
                </span>
              ))}
            </div>
          </div>

          <div className={styles.divider} aria-hidden="true" />

          <div className={styles.column}>
            <div className={styles.columnHeading}>Client SDKs</div>
            <p className={styles.columnNote}>All stdlib-only. Published to their native registries.</p>
            <div className={styles.pills}>
              {sdks.map((sdk) => (
                <a key={sdk.name} href={sdk.href} className={`${styles.pill} ${styles.pillLink}`}>
                  {sdk.name}
                </a>
              ))}
            </div>
          </div>
        </div>
      </div>
    </section>
  );
}
