import type {ReactNode} from 'react';
import Link from '@docusaurus/Link';
import Translate, {translate} from '@docusaurus/Translate';
import useDocusaurusContext from '@docusaurus/useDocusaurusContext';
import useBaseUrl from '@docusaurus/useBaseUrl';
import Layout from '@theme/Layout';
import HomepageFeatures from '@site/src/components/HomepageFeatures';
import Heading from '@theme/Heading';

import styles from './index.module.css';

function HomepageShowcase() {
  const videoUrl = useBaseUrl('/video/example0.mp4');
  return (
    <section className={styles.showcase}>
      <div className="container">
        <video
          className={styles.showcaseVideo}
          autoPlay
          loop
          muted
          playsInline
          controls>
          <source src={videoUrl} type="video/mp4" />
        </video>
      </div>
    </section>
  );
}

function HomepageHeader() {
  const {siteConfig} = useDocusaurusContext();
  return (
    <header className={styles.heroBanner}>
      <div className="container">
        <Heading as="h1" className={styles.heroTitle}>
          {siteConfig.title}
        </Heading>
        <p className={styles.heroSubtitle}>
          <Translate id="homepage.tagline">
            The most customizable Wayland compositor with TypeScript(tsx).
          </Translate>
        </p>
        <div className={styles.buttons}>
          <Link className="button button--primary button--lg" to="/docs/intro">
            <Translate id="homepage.getStarted">Get Started</Translate>
          </Link>
        </div>
      </div>
    </header>
  );
}

export default function Home(): ReactNode {
  const {siteConfig} = useDocusaurusContext();
  return (
    <Layout
      title={siteConfig.title}
      description={translate({
        id: 'homepage.description',
        message: 'Documentation for the ShojiWM Wayland compositor.',
      })}>
      <HomepageHeader />
      <main>
        <HomepageShowcase />
        <HomepageFeatures />
      </main>
    </Layout>
  );
}
