import Layout from '@theme/Layout';
import Hero from '../components/Hero';

export default function Home(): JSX.Element {
  return (
    <Layout title="beachcomber" description="One daemon. One cache. Every consumer reads from it.">
      <Hero />
    </Layout>
  );
}
