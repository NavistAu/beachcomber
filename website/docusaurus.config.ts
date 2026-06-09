import {themes as prismThemes} from 'prism-react-renderer';
import type {Config} from '@docusaurus/types';
import type * as Preset from '@docusaurus/preset-classic';

// This runs in Node.js - Don't use client-side code here (browser APIs, JSX...)

const config: Config = {
  title: 'beachcomber',
  tagline: 'One daemon that caches your shell environment — git status, kubernetes context, battery, and more. Prompts, status bars, and editors read from a shared cache instead of forking a process per keystroke.',
  favicon: 'img/favicon.svg',

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

  // Control the <title> shown by link previews (Discord, Slack, Twitter).
  titleDelimiter: '·',

  // Static head tags for metadata Docusaurus doesn't emit automatically.
  // og:title, og:description, og:image, twitter:image, twitter:card, and the
  // page description are all derived from the Layout props / themeConfig.image.
  headTags: [
    {tagName: 'meta', attributes: {property: 'og:site_name', content: 'beachcomber'}},
    {tagName: 'meta', attributes: {property: 'og:type', content: 'website'}},
    {tagName: 'meta', attributes: {property: 'og:image:type', content: 'image/png'}},
    {tagName: 'meta', attributes: {property: 'og:image:width', content: '1200'}},
    {tagName: 'meta', attributes: {property: 'og:image:height', content: '630'}},
    {
      tagName: 'meta',
      attributes: {
        property: 'og:image:alt',
        content: 'beachcomber — one daemon, one cache, every consumer reads from it.',
      },
    },
    // PNG favicon fallbacks for browsers that don't support SVG favicons
    // and for the Apple touch icon.
    {tagName: 'link', attributes: {rel: 'alternate icon', type: 'image/png', sizes: '32x32', href: '/img/favicon-32.png'}},
    {tagName: 'link', attributes: {rel: 'alternate icon', type: 'image/png', sizes: '64x64', href: '/img/favicon-64.png'}},
    {tagName: 'link', attributes: {rel: 'apple-touch-icon', sizes: '180x180', href: '/img/favicon-180.png'}},
    // Umami first-party tracker (renamed script, DNT on, scoped to prod domain)
    {
      tagName: 'script',
      attributes: {
        defer: 'true',
        src: 'https://stats.beachcomber.sh/s.js',
        'data-website-id': 'f1aec07f-0cf7-4ee5-9b69-f1060bc396c4',
        'data-do-not-track': 'true',
        'data-domains': 'beachcomber.sh',
      },
    },
    // Cloudflare Web Analytics beacon
    {
      tagName: 'script',
      attributes: {
        defer: 'true',
        src: 'https://static.cloudflareinsights.com/beacon.min.js',
        'data-cf-beacon': '{"token": "abe354cbd2964292a527d18635d2d484"}',
      },
    },
  ],

  markdown: {
    mermaid: true,
    hooks: {
      onBrokenMarkdownLinks: 'warn',
    },
  },

  themes: ['@docusaurus/theme-mermaid'],

  clientModules: [
    './src/clientModules/footerScroll.ts',
  ],

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
    image: 'img/beachcomber-social-card.png',
    metadata: [
      {name: 'keywords', content: 'beachcomber, shell prompt, zsh, bash, starship, powerlevel10k, tmux, git, kubernetes, status bar, daemon, cache, Rust'},
    ],
    colorMode: {
      defaultMode: 'light',
      disableSwitch: false,
      respectPrefersColorScheme: true,
    },
    navbar: {
      title: 'beachcomber',
      logo: {
        alt: 'beachcomber logo',
        src: 'img/beachcomber-icon-light.svg',
        srcDark: 'img/beachcomber-icon-dark.svg',
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
      links: [],
      copyright: 'beachcomber is a product of <a href="https://navist.com.au"><img src="/img/navist-logo.png" alt="Navist" style="height:20px;vertical-align:middle;position:relative;top:-2px;margin-left:4px" /></a> — a polymath technical consultancy. MIT License.',
    },
    prism: {
      theme: prismThemes.github,
      darkTheme: prismThemes.dracula,
    },
  } satisfies Preset.ThemeConfig,
};

export default config;
