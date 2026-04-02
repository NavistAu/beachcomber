import {useEffect, useRef, useState} from 'react';
import {useIntersectionObserver} from '../hooks/useIntersectionObserver';
import styles from './Performance.module.css';

const chartData = [
  {
    label: 'gitstatus daemon',
    value: '30.9ms',
    width: '100%',
    barStyle: styles.chartBarSlow,
  },
  {
    label: 'raw git status',
    value: '5.6ms',
    width: '18.1%',
    barStyle: styles.chartBarMedium,
  },
  {
    label: 'beachcomber',
    value: '15µs',
    width: '100%',
    minWidth: '4px',
    barStyle: styles.chartBarFast,
    isSliver: true,
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
            {row.isSliver ? (
              <div
                className={`${styles.chartBar} ${row.barStyle}`}
                style={{
                  width: isVisible ? '4px' : '0',
                  minWidth: isVisible ? '4px' : '0',
                  transition: 'width 1.2s cubic-bezier(0.25, 0.46, 0.45, 0.94) 0.6s',
                }}
              >
                <span className={styles.chartValueOutside}>{row.value}</span>
              </div>
            ) : (
              <div
                className={`${styles.chartBar} ${row.barStyle}`}
                style={{width: isVisible ? row.width : '0'}}
              >
                <span className={styles.chartValue}>{row.value}</span>
              </div>
            )}
          </div>
        </div>
      ))}
    </div>
  );
}

function CountUp({target, duration}: {target: number; duration: number}): JSX.Element {
  const [count, setCount] = useState(0);
  const [ref, isVisible] = useIntersectionObserver(0.3);
  const hasStarted = useRef(false);

  useEffect(() => {
    if (!isVisible || hasStarted.current) return;
    hasStarted.current = true;

    const startTime = performance.now();
    const step = (now: number) => {
      const elapsed = now - startTime;
      const progress = Math.min(elapsed / duration, 1);
      const eased = 1 - Math.pow(1 - progress, 3);
      setCount(Math.round(eased * target));
      if (progress < 1) {
        requestAnimationFrame(step);
      }
    };
    requestAnimationFrame(step);
  }, [isVisible, target, duration]);

  return (
    <span ref={ref as React.RefObject<HTMLSpanElement>}>
      {count.toLocaleString()}
    </span>
  );
}

export default function Performance(): JSX.Element {
  const [sectionRef, sectionVisible] = useIntersectionObserver(0.1);

  return (
    <section className={styles.section}>
      <div
        className={`${styles.container} ${sectionVisible ? styles.visible : ''}`}
        ref={sectionRef as React.RefObject<HTMLDivElement>}
      >
        <h2 className={styles.heading}>The speed difference is absurd</h2>

        <BarChart />

        <div className={styles.comparison}>
          <div className={`${styles.comparisonPanel} ${styles.comparisonPanelBad}`}>
            <div className={styles.comparisonTitle}>Without beachcomber</div>
            {beforeStats.map((item) => (
              <div key={item.stat} className={styles.comparisonItem}>
                <div className={`${styles.comparisonStat} ${styles.comparisonStatBad}`}>
                  {item.stat}
                </div>
                <div className={styles.comparisonDesc}>{item.desc}</div>
              </div>
            ))}
          </div>

          <div className={`${styles.comparisonPanel} ${styles.comparisonPanelGood}`}>
            <div className={styles.comparisonTitle}>With beachcomber</div>
            {afterStats.map((item) => (
              <div key={item.stat} className={styles.comparisonItem}>
                <div className={`${styles.comparisonStat} ${styles.comparisonStatGood}`}>
                  {item.stat}
                </div>
                <div className={styles.comparisonDesc}>{item.desc}</div>
              </div>
            ))}
          </div>
        </div>

        <div className={styles.callout}>
          <div className={styles.calloutNumber}>
            <CountUp target={2060} duration={1500} />
          </div>
          <p className={styles.calloutText}>
            queries served by beachcomber in the time it takes gitstatus to return one result
          </p>
        </div>
      </div>
    </section>
  );
}
