import {useEffect, useRef, useState} from 'react';
import {useIntersectionObserver} from '../hooks/useIntersectionObserver';
import styles from './Problem.module.css';

function CountUp({target, duration}: {target: number; duration: number}): JSX.Element {
  const [count, setCount] = useState(0);
  const [ref, isVisible] = useIntersectionObserver(0.3);
  const hasStarted = useRef(false);

  useEffect(() => {
    if (!isVisible || hasStarted.current) return;
    hasStarted.current = true;

    const startTime = performance.now();
    const step = (now: number) => {
      const progress = Math.min((now - startTime) / duration, 1);
      setCount(Math.round((1 - Math.pow(1 - progress, 3)) * target));
      if (progress < 1) requestAnimationFrame(step);
    };
    requestAnimationFrame(step);
  }, [isVisible, target, duration]);

  return <span ref={ref as React.RefObject<HTMLSpanElement>}>{count.toLocaleString()}</span>;
}

export default function Problem(): JSX.Element {
  const [ref, isVisible] = useIntersectionObserver(0.15);

  return (
    <section className={`${styles.section} page`} id="problem">
      <div
        className={`${styles.container} ${isVisible ? styles.visible : ''}`}
        ref={ref as React.RefObject<HTMLDivElement>}
      >
        <h2 className={styles.heading}>
          Your terminal is doing{' '}
          <span className={styles.accent}>insane</span>{' '}
          amounts of duplicate work
        </h2>

        <div className={styles.prose}>
          <p>
            Every prompt runs its own git status. Every tmux pane forks a shell on a timer to read the
            battery. Your editor watches the same files as your prompt framework. They all ask what branch
            you&apos;re on, and none of them share the answer.
          </p>
          <p>
            Ten tmux windows split three ways is thirty shells: thirty git daemons, hundreds of threads,
            and <code>fseventsd</code> handing every change in <code>.git</code> to thirty watchers.
          </p>
          <p>
            <strong>beachcomber</strong> works it out once, and every tool reads the same answer.
          </p>
        </div>

        <div className={styles.callout}>
          <div className={styles.calloutNumber}>
            <CountUp target={2060} duration={1500} />
          </div>
          <p className={styles.calloutText}>
            answers from beachcomber in the time gitstatus gives one
          </p>
        </div>
      </div>
    </section>
  );
}
