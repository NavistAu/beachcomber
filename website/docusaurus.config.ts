import {themes as prismThemes} from 'prism-react-renderer';
import type {Config} from '@docusaurus/types';
import type * as Preset from '@docusaurus/preset-classic';

// This runs in Node.js - Don't use client-side code here (browser APIs, JSX...)

const config: Config = {
  title: 'beachcomber',
  tagline: 'One daemon. One cache. Every consumer reads from it.',
  favicon: 'img/favicon.ico',

  // Future flags, see https://docusaurus.io/docs/api/docusaurus-config#future
  future: {
    v4: true, // Improve compatibility with the upcoming Docusaurus v4
  },

  url: 'https://beachcomber.sh',
  baseUrl: '/',

  organizationName: 'NavistAu',
  projectName: 'beachcomber',

  trailingSlash: false,
  onBrokenLinks: 'throw',

  markdown: {
    hooks: {
      onBrokenMarkdownLinks: 'warn',
    },
  },

  i18n: {
    defaultLocale: 'en',
    locales: ['en'],
  },

  presets: [
    [
      'classic',
      {
        docs: {
          sidebarPath: './sidebars.ts',
        },
        blog: false,
        theme: {
          customCss: './src/css/custom.css',
        },
      } satisfies Preset.Options,
    ],
  ],

  themeConfig: {
    image: 'img/docusaurus-social-card.jpg',
    colorMode: {
      defaultMode: 'light',
      disableSwitch: false,
      respectPrefersColorScheme: true,
    },
    navbar: {
      title: 'beachcomber',
      logo: {
        alt: 'beachcomber logo',
        src: 'img/logo.svg',
      },
      items: [
        {
          type: 'docSidebar',
          sidebarId: 'docsSidebar',
          position: 'left',
          label: 'Docs',
        },
        {
          type: 'docsVersionDropdown',
          position: 'right',
        },
        {
          href: 'https://github.com/NavistAu/beachcomber',
          label: 'GitHub',
          position: 'right',
        },
      ],
    },
    footer: {
      style: 'dark',
      links: [
        {
          title: 'Docs',
          items: [
            {
              label: 'Getting Started',
              to: '/docs/getting-started/installation',
            },
            {
              label: 'CLI Reference',
              to: '/docs/reference/cli-commands',
            },
            {
              label: 'Providers',
              to: '/docs/reference/built-in-providers',
            },
          ],
        },
        {
          title: 'Ecosystem',
          items: [
            {
              label: 'SDKs',
              to: '/docs/ecosystem/overview',
            },
            {
              label: 'GitHub',
              href: 'https://github.com/NavistAu/beachcomber',
            },
            {
              label: 'crates.io',
              href: 'https://crates.io/crates/beachcomber',
            },
          ],
        },
        {
          title: 'Navist',
          items: [
            {
              label: 'navist.com.au',
              href: 'https://navist.com.au',
            },
          ],
        },
      ],
      copyright: '<div style="display:flex;align-items:center;justify-content:center;gap:0.75rem;flex-wrap:wrap"><a href="https://navist.com.au" style="display:inline-flex;align-items:center"><img src="/img/navist-logo.png" alt="Navist" style="height:28px;opacity:0.7" /></a><span>beachcomber is a product of <a href="https://navist.com.au">Navist</a> — polymath technical consultancy. MIT License.</span></div>',
    },
    prism: {
      theme: prismThemes.github,
      darkTheme: prismThemes.dracula,
    },
  } satisfies Preset.ThemeConfig,
};

export default config;
