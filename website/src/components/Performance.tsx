import {useEffect, useRef, useState} from 'react';
import {useIntersectionObserver} from '../hooks/useIntersectionObserver';
import styles from './Performance.module.css';

// The chart plays each read out at SLOW times its real length: the bars all grow at one speed, so each takes
// as long to fill as its read takes, and the longest fills the row.
const SLOW = 100;
const chartData = [
  {label: 'gitstatus daemon', value: '30.9ms', ms: 30.9, barStyle: styles.chartBarSlow},
  {label: 'raw git status', value: '5.6ms', ms: 5.6, barStyle: styles.chartBarMedium},
  {label: 'beachcomber', value: '36µs', ms: 0.036, barStyle: styles.chartBarFast},
];
const longest = Math.max(...chartData.map(r => r.ms));

const beforeStats = [
  {stat: '960 threads', desc: '30 shells, each running gitstatusd'},
  {stat: '500 forks / 10s', desc: 'tmux status bars reading battery, host and git'},
  {stat: '2.5s CPU', desc: 'wasted a minute on duplicate queries'},
];

const afterStats = [
  {stat: '1 daemon', desc: 'serving every shell, editor and status bar'},
  {stat: '36µs reads', desc: 'client to daemon and back, on an M2 Max'},
  {stat: '45k req/s', desc: 'sustained, on a single socket'},
];

// The chart plays whenever it is scrolled down to, and stays filled when it is scrolled back up to: it empties
// only once it is wholly below the screen again, ready to play for the next reader coming down.
function BarChart(): JSX.Element {
  const ref = useRef<HTMLElement>(null);
  const [isVisible, setVisible] = useState(false);
  useEffect(() => {
    const seen = new IntersectionObserver(([e]) => {
      if (e.isIntersecting && e.intersectionRatio >= 0.5) setVisible(true);
      else if (!e.isIntersecting && e.boundingClientRect.top > 0) setVisible(false);
    }, {threshold: [0, 0.5]});
    seen.observe(ref.current!);
    return () => seen.disconnect();
  }, []);

  return (
    <figure className={styles.chart} ref={ref}>
      {chartData.map((row) => {
        const seconds = (row.ms * SLOW) / 1000;
        return (
          <div key={row.label} className={styles.chartRow}>
            <span className={styles.chartLabel}>{row.label}</span>
            <div className={styles.chartBarContainer}>
              <div
                className={`${styles.chartBar} ${row.barStyle}`}
                style={{
                  // the row keeps room after the bar for its value
                  width: isVisible ? `calc((100% - 4.5rem) * ${row.ms / longest})` : 0,
                  // empties at once, fills at the read's own pace
                  transitionDuration: isVisible ? `${seconds}s` : '0s',
                }}
              />
              <span
                className={styles.chartValue}
                style={{opacity: isVisible ? 1 : 0, transitionDelay: isVisible ? `${seconds}s` : '0s'}}
              >
                {row.value}
              </span>
            </div>
          </div>
        );
      })}
      <figcaption className={styles.chartCaption}>One read each, slowed down {SLOW}×</figcaption>
    </figure>
  );
}

export default function Performance(): JSX.Element {
  const [sectionRef, sectionVisible] = useIntersectionObserver(0.1);

  return (
    <section className={`${styles.section} page`}>
      <div
        className={`${styles.container} ${sectionVisible ? styles.visible : ''}`}
        ref={sectionRef as React.RefObject<HTMLDivElement>}
      >
        <h2 className={styles.heading}>Obsessive about speed</h2>

        <BarChart />

        <div className={styles.comparison}>
          {[
            ['Without beachcomber', beforeStats, styles.comparisonPanelBad, styles.comparisonStatBad],
            ['With beachcomber', afterStats, styles.comparisonPanelGood, styles.comparisonStatGood],
          ].map(([title, stats, panel, stat]) => (
            <div key={title as string} className={`${styles.comparisonPanel} ${panel}`}>
              <div className={styles.comparisonTitle}>{title as string}</div>
              {(stats as typeof beforeStats).map((item) => (
                <div key={item.stat} className={styles.comparisonItem}>
                  <div className={`${styles.comparisonStat} ${stat}`}>{item.stat}</div>
                  <div className={styles.comparisonDesc}>{item.desc}</div>
                </div>
              ))}
            </div>
          ))}
        </div>
      </div>
    </section>
  );
}
