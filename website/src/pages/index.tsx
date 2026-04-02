import Layout from '@theme/Layout';

export default function Home(): JSX.Element {
  return (
    <Layout title="beachcomber" description="One daemon. One cache. Every consumer reads from it.">
      <main style={{ padding: '4rem 0', textAlign: 'center' }}>
        <h1>beachcomber</h1>
        <p>One daemon. One cache. Every consumer reads from it.</p>
      </main>
    </Layout>
  );
}
