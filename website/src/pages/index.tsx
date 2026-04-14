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
    <Layout
      title="Shell state, cached."
      description="One daemon that caches your shell environment — git status, kubernetes context, battery, and more. Prompts, status bars, and editors read from a shared cache instead of forking a process per keystroke."
    >
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
