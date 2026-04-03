import Layout from '@theme/Layout';
import Hero from '../components/Hero';
import Problem from '../components/Problem';
import Performance from '../components/Performance';
import HowItWorks from '../components/HowItWorks';
import Providers from '../components/Providers';
import Integrations from '../components/Integrations';
import SDKs from '../components/SDKs';

export default function Home(): JSX.Element {
  return (
    <Layout title="beachcomber" description="One daemon. One cache. Every consumer reads from it.">
      <Hero />
      <Problem />
      <Performance />
      <HowItWorks />
      <Providers />
      <Integrations />
      <SDKs />
    </Layout>
  );
}
