import styles from './Problem.module.css';

const stats = [
  {
    number: '960',
    label: 'threads spawned by 30 shells running gitstatusd — all computing the same answer',
  },
  {
    number: '500',
    label: 'shell forks every 10 seconds from tmux status bars collecting battery, hostname, and git data',
  },
  {
    number: '30×',
    label: 'duplicate FSEvents registrations watching the same .git directory from independent daemons',
  },
  {
    number: '0',
    label: 'coordination between shells, editors, status bars, and prompts — all asking the same questions independently',
  },
];

export default function Problem(): JSX.Element {
  return (
    <section className={styles.section}>
      <div className={styles.container}>
        <h2 className={styles.heading}>The problem with your terminal</h2>
        <div className={styles.stats}>
          {stats.map((stat, i) => (
            <div key={i} className={styles.stat}>
              <div className={styles.statNumber}>{stat.number}</div>
              <div className={styles.statLabel}>{stat.label}</div>
            </div>
          ))}
        </div>
      </div>
    </section>
  );
}
