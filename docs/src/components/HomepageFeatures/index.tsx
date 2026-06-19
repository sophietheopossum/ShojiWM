import type {ReactNode} from 'react';
import clsx from 'clsx';
import Translate from '@docusaurus/Translate';
import Heading from '@theme/Heading';
import styles from './styles.module.css';

type FeatureItem = {
  title: ReactNode;
  description: ReactNode;
};

const FeatureList: FeatureItem[] = [
  {
    title: (
      <Translate id="homepage.feature.declarative.title">
        Declarative composition
      </Translate>
    ),
    description: (
      <Translate id="homepage.feature.declarative.description">
        Describe window chrome and layout with a React-like TSX API.
      </Translate>
    ),
  },
  {
    title: (
      <Translate id="homepage.feature.reactive.title">
        Reactive signals
      </Translate>
    ),
    description: (
      <Translate id="homepage.feature.reactive.description">
        The UI re-composes automatically when state changes.
      </Translate>
    ),
  },
  {
    title: (
      <Translate id="homepage.feature.effects.title">GPU effects</Translate>
    ),
    description: (
      <Translate id="homepage.feature.effects.description">
        Blur, shaders, and per-window transforms, with hot reload.
      </Translate>
    ),
  },
];

function Feature({title, description}: FeatureItem) {
  return (
    <div className={clsx('col col--4')}>
      <div className="padding-horiz--md">
        <Heading as="h3">{title}</Heading>
        <p>{description}</p>
      </div>
    </div>
  );
}

export default function HomepageFeatures(): ReactNode {
  return (
    <section className={styles.features}>
      <div className="container">
        <div className="row">
          {FeatureList.map((props, idx) => (
            <Feature key={idx} {...props} />
          ))}
        </div>
      </div>
    </section>
  );
}
