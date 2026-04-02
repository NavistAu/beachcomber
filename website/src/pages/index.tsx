import Layout from '@theme/Layout';
import Hero from '../components/Hero';
import Performance from '../components/Performance';
import Problem from '../components/Problem';

export default function Home(): JSX.Element {
  return (
    <Layout title="beachcomber" description="One daemon. One cache. Every consumer reads from it.">
      <Hero />
      <Problem />
      <Performance />
    </Layout>
  );
}
