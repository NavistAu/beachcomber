import {useIntersectionObserver} from '../hooks/useIntersectionObserver';
import styles from './Performance.module.css';

const chartData = [
  {
    label: 'gitstatus (hot)',
    value: '30.9ms',
    width: '100%',
    barStyle: styles.chartBarSlow,
  },
  {
    label: 'git status (raw)',
    value: '5.6ms',
    width: '18.1%',
    barStyle: styles.chartBarMedium,
  },
  {
    label: 'beachcomber',
    value: '15µs',
    width: '0.05%',
    barStyle: styles.chartBarFast,
  },
];

const beforeStats = [
  {stat: '960 threads', desc: '30 shells running gitstatusd, all computing the same answer'},
  {stat: '500 forks / 10s', desc: 'tmux status bars forking processes to collect battery, hostname, and git data'},
  {stat: '2.5s CPU', desc: 'wasted every minute on duplicate environment queries across tools'},
];

const afterStats = [
  {stat: '1 daemon', desc: 'one beachcomber process serves every shell, editor, and status bar on the machine'},
  {stat: '15µs queries', desc: 'cached reads over a Unix socket — faster than a syscall to disk'},
  {stat: '45k req/sec', desc: 'sustained throughput on a single socket with zero coordination overhead'},
];

function BarChart(): JSX.Element {
  const [ref, isVisible] = useIntersectionObserver(0.3);

  return (
    <div className={styles.chart} ref={ref as React.RefObject<HTMLDivElement>}>
      {chartData.map((row) => (
        <div key={row.label} className={styles.chartRow}>
          <span className={styles.chartLabel}>{row.label}</span>
          <div className={styles.chartBarContainer}>
            <div
              className={`${styles.chartBar} ${row.barStyle}`}
              style={{width: isVisible ? row.width : '0'}}
            >
              <span className={styles.chartValue}>{row.value}</span>
            </div>
          </div>
        </div>
      ))}
    </div>
  );
}

function CalloutNumber(): JSX.Element {
  const [ref, isVisible] = useIntersectionObserver(0.3);

  return (
    <div className={styles.callout} ref={ref as React.RefObject<HTMLDivElement>}>
      <div
        className={styles.calloutNumber}
        style={{opacity: isVisible ? 1 : 0}}
      >
        2,060
      </div>
      <p className={styles.calloutText}>
        queries served by beachcomber in the time it takes gitstatus to return one result
      </p>
    </div>
  );
}

export default function Performance(): JSX.Element {
  return (
    <section className={styles.section}>
      <div className={styles.container}>
        <h2 className={styles.heading}>Measured performance</h2>

        <BarChart />

        <div className={styles.comparison}>
          <div className={styles.comparisonPanel}>
            <div className={styles.comparisonTitle}>Without beachcomber</div>
            {beforeStats.map((item) => (
              <div key={item.stat}>
                <div className={`${styles.comparisonStat} ${styles.comparisonStatBad}`}>
                  {item.stat}
                </div>
                <div className={styles.comparisonDesc}>{item.desc}</div>
              </div>
            ))}
          </div>

          <div className={styles.comparisonPanel}>
            <div className={styles.comparisonTitle}>With beachcomber</div>
            {afterStats.map((item) => (
              <div key={item.stat}>
                <div className={`${styles.comparisonStat} ${styles.comparisonStatGood}`}>
                  {item.stat}
                </div>
                <div className={styles.comparisonDesc}>{item.desc}</div>
              </div>
            ))}
          </div>
        </div>

        <CalloutNumber />
      </div>
    </section>
  );
}
