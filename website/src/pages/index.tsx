import Layout from '@theme/Layout';
import Hero from '../components/Hero';
import Problem from '../components/Problem';
import Performance from '../components/Performance';
import HowItWorks from '../components/HowItWorks';
import Providers from '../components/Providers';
import Integrations from '../components/Integrations';
import SDKs from '../components/SDKs';
import WaveDivider from '../components/WaveDivider';
import styles from './index.module.css';

export default function Home(): JSX.Element {
  return (
    <Layout title="beachcomber" description="One daemon. One cache. Every consumer reads from it.">
      <Hero />
      {/* Wave into Problem (dark section) */}
      <div className={styles.waveProblem}>
        <WaveDivider fillColor="var(--bc-problem-bg)" />
      </div>
      <Problem />
      {/* Wave out of Problem into Performance */}
      <div className={styles.wavePerformance}>
        <WaveDivider fillColor="var(--ifm-background-color)" />
      </div>
      <Performance />
      <HowItWorks />
      <Providers />
      <Integrations />
      <SDKs />
    </Layout>
  );
}
