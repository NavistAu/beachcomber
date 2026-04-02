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
  {name: 'git', time: '~5.6ms'},
  {name: 'battery', time: '~6ms'},
];

export default function Providers(): JSX.Element {
  return (
    <section className={styles.section}>
      <div className={styles.container}>
        <h2 className={styles.heading}>16 built-in providers</h2>
        <p className={styles.subheading}>Plus a script backend for anything else</p>
        <div className={styles.grid}>
          {providers.map((p) => (
            <div key={p.name} className={styles.card}>
              <div className={styles.cardName}>{p.name}</div>
              <div className={styles.cardTime}>{p.time}</div>
            </div>
          ))}
        </div>
      </div>
    </section>
  );
}
