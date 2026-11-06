/**
 * The stock Docusaurus loader for `additionalLanguages`, plus template highlighting inside TOML strings:
 * comb's templates are Jinja inside TOML strings, which the TOML grammar alone shows as one flat string.
 */
import siteConfig from '@generated/docusaurus.config';

// The parts of a `{{ … }}` or `{% … %}` block.
const jinja = {
  delimiter: {pattern: /^\{[{%]-?|-?[}%]\}$/, alias: 'punctuation'},
  string: /'(?:\\.|[^'\\])*'|"(?:\\.|[^"\\])*"/,
  keyword: /\b(?:if|else|elif|endif|for|in|endfor|set|and|or|not|is|none|true|false)\b/,
  // a filter after a pipe, or anything called
  function: [{pattern: /(\|\s*)[a-z_]\w*/i, lookbehind: true}, /\b[a-z_]\w*(?=\s*\()/i],
  number: /\b\d+(?:\.\d+)?\b/,
  variable: /\b[a-z_]\w*(?:\.[a-z_]\w*)*/i,
  operator: /[|~+\-*/%<>=!]=?/,
  punctuation: /[()[\],:.]/,
};

function withTemplates(Prism) {
  // Shells the docs show without a grammar of their own read well as bash; a tmux.conf is shell-like too.
  for (const name of ['zsh', 'fish', 'tmux']) Prism.languages[name] = Prism.languages.bash;
  // A bare template: text with {{ }} and {% %} blocks in it.
  Prism.languages.jinja = {template: {pattern: /\{[{%][\s\S]*?[}%]\}/, inside: jinja}};
  const toml = Prism.languages.toml;
  if (!toml) return;
  const inside = {
    template: {pattern: /\{[{%][\s\S]*?[}%]\}/, inside: jinja},
    string: /[\s\S]+?(?=\{[{%]|$)/,
  };
  Prism.languages.insertBefore('toml', 'string', {
    // a TOML string with a template in it: the literal text stays a string, the template is broken up
    'template-string': {
      pattern: /'''[\s\S]*?'''|"""[\s\S]*?"""|'[^'\n]*'|"(?:\\.|[^"\\\n])*"/,
      greedy: true,
      inside,
    },
  });
}

export default function prismIncludeLanguages(PrismObject) {
  const {
    themeConfig: {prism},
  } = siteConfig;
  const {additionalLanguages} = prism;
  // Prism components work on the Prism instance on the window; mount ours there while they load.
  const PrismBefore = globalThis.Prism;
  globalThis.Prism = PrismObject;
  additionalLanguages.forEach((lang) => {
    if (lang === 'php') {
      // eslint-disable-next-line global-require
      require('prismjs/components/prism-markup-templating.js');
    }
    // eslint-disable-next-line global-require, import/no-dynamic-require
    require(`prismjs/components/prism-${lang}`);
  });
  withTemplates(PrismObject);
  delete globalThis.Prism;
  if (typeof PrismBefore !== 'undefined') {
    globalThis.Prism = PrismObject;
  }
}
