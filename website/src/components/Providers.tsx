import {useIntersectionObserver} from '../hooks/useIntersectionObserver';
import styles from './Providers.module.css';

const providers = [
  {name: 'hostname', time: '~400ns'},
  {name: 'user', time: '~650ns'},
  {name: 'load', time: '~550ns'},
  {name: 'uptime', time: '~660ns'},
  {name: 'kubecontext', time: '<1µs'},
  {name: 'gcloud', time: '<1µs'},
  {name: 'aws', time: '<1µs'},
  {name: 'conda', time: '<1µs'},
  {name: 'terraform', time: '<1µs'},
  {name: 'python', time: '<1µs'},
  {name: 'asdf', time: '<1µs'},
  {name: 'direnv', time: 'varies'},
  {name: 'mise', time: 'varies'},
  {name: 'network', time: '~2ms'},
  {name: 'battery', time: '~6ms'},
];

const gitFields = ['branch', 'dirty', 'staged', 'unstaged', 'ahead', 'behind', 'stash'];

export default function Providers(): JSX.Element {
  const [ref, isVisible] = useIntersectionObserver(0.1);

  return (
    <section className={styles.section}>
      <div
        className={`${styles.container} ${isVisible ? styles.visible : ''}`}
        ref={ref as React.RefObject<HTMLDivElement>}
      >
        <div className={styles.label}>Built-in</div>
        <h2 className={styles.heading}>16 built-in providers</h2>
        <p className={styles.subheading}>Plus a script backend for anything else</p>
        <div className={styles.grid}>
          <div
            className={`${styles.card} ${styles.cardFeatured}`}
            style={{ animationDelay: '0ms' } as React.CSSProperties}
          >
            <div className={styles.cardFeaturedHeader}>
              <div className={styles.cardName}>git</div>
              <div className={styles.cardTime}>~5.6ms</div>
            </div>
            <div className={styles.cardFields}>
              {gitFields.map((f) => (
                <span key={f} className={styles.cardField}>{f}</span>
              ))}
            </div>
          </div>
          {providers.map((p, i) => (
            <div
              key={p.name}
              className={styles.card}
              style={{ animationDelay: `${(i + 1) * 30}ms` } as React.CSSProperties}
            >
              <div className={styles.cardName}>{p.name}</div>
              <div className={styles.cardTime}>{p.time}</div>
            </div>
          ))}
        </div>
      </div>
    </section>
  );
}
