import styles from './SDKs.module.css';

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

export default function SDKs(): JSX.Element {
  return (
    <section className={styles.section}>
      <div className={styles.container}>
        <h2 className={styles.heading}>Client SDKs</h2>
        <p className={styles.subheading}>All stdlib-only. Published to their native registries.</p>
        <div className={styles.items}>
          {sdks.map((sdk) => (
            <a key={sdk.name} href={sdk.href} className={styles.item}>
              {sdk.name}
            </a>
          ))}
        </div>
      </div>
    </section>
  );
}
